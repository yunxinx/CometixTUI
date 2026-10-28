use super::*;

#[test]
fn default_output_sends_markers_without_atomic_capability() {
    let (writer, bytes) = new_test_writer();
    let mut terminal = Terminal::new(
        Box::new(writer),
        Box::new(io::sink()),
        Output::Stdout,
        false,
        false,
        None,
    )
    .unwrap();
    terminal.synchronized_output.atomic_capability = false;
    bytes.lock().unwrap().clear();
    terminal
        .synchronized_update(|terminal| terminal.write_all(b"frame"))
        .unwrap();
    let output = bytes.lock().unwrap().clone();
    assert_eq!(output, b"\x1b[?2026hframe\x1b[?2026l");
}

#[test]
fn screen_transitions_and_final_cursor_share_one_transaction() {
    let (writer, bytes) = new_test_writer();
    let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
    let mut backend = new_inline_term_with_size(writer, 0, (40, 8));
    backend.raw_mode_enabled = true;
    terminal.inner = Box::new(backend);
    for request in [
        Some(crate::context::AlternateScreenRequest {
            mouse_tracking: true,
        }),
        None,
    ] {
        bytes.lock().unwrap().clear();
        terminal
            .synchronized_update(|terminal| {
                assert!(terminal.set_dynamic_alternate_screen(request)?);
                terminal.write_all(b"FIRST_FRAME")?;
                terminal.position_cursor(Some(CursorDeclaration {
                    x: 2,
                    y: 0,
                    visible: true,
                }))
            })
            .unwrap();
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.starts_with("\x1b[?2026h"), "{output:?}");
        assert!(output.ends_with("\x1b[?2026l"), "{output:?}");
        let screen = if request.is_some() {
            "\x1b[?1049h"
        } else {
            "\x1b[?1049l"
        };
        assert!(output.find(screen).unwrap() < output.find("FIRST_FRAME").unwrap());
        let mouse = if request.is_some() {
            "\x1b[?1000h"
        } else {
            "\x1b[?1000l"
        };
        assert!(output.find(mouse).unwrap() < output.find("FIRST_FRAME").unwrap());
        let cursor = if request.is_some() {
            "\x1b[1;3H"
        } else {
            "\x1b[3G"
        };
        assert!(
            output.find("FIRST_FRAME").unwrap() < output.rfind(cursor).unwrap(),
            "{output:?}"
        );
        if request.is_some() {
            for mouse_tracking in [false, true] {
                bytes.lock().unwrap().clear();
                terminal
                    .synchronized_update(|terminal| {
                        assert!(!terminal.set_dynamic_alternate_screen(Some(
                            crate::context::AlternateScreenRequest { mouse_tracking }
                        ))?);
                        Ok(())
                    })
                    .unwrap();
                let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
                assert!(output.starts_with("\x1b[?2026h"));
                assert!(output.ends_with("\x1b[?2026l"));
                assert!(output.contains(if mouse_tracking {
                    "\x1b[?1000h"
                } else {
                    "\x1b[?1000l"
                }));
                assert!(!output.contains("1049"));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Failure {
    WriteAfter(usize),
    FlushAt(usize),
}

struct FailingWriter {
    output: TestWriter,
    failure: Option<Failure>,
    flushes: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(Failure::WriteAfter(remaining)) = self.failure {
            if remaining == 0 {
                self.failure = None;
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "injected write failure",
                ));
            }
            let count = remaining.min(bytes.len());
            self.failure = Some(Failure::WriteAfter(remaining - count));
            return self.output.write(&bytes[..count]);
        }
        self.output.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if matches!(self.failure, Some(Failure::FlushAt(index)) if self.flushes == index) {
            self.failure = None;
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected flush failure",
            ));
        }
        Ok(())
    }
}

#[test]
fn partial_begin_body_end_and_flush_failures_propagate_and_close_the_transaction() {
    let begin = b"\x1b[?2026h";
    let body = b"frame";
    let end = b"\x1b[?2026l";
    let failures = (0..begin.len() + body.len() + end.len())
        .map(Failure::WriteAfter)
        .chain([Failure::FlushAt(1), Failure::FlushAt(2)]);
    for failure in failures {
        let (output, bytes) = new_test_writer();
        let writer = FailingWriter {
            output,
            failure: Some(failure),
            flushes: 0,
        };
        let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
        let mut backend = new_inline_term_with_size(TestWriter::default(), 0, (40, 8));
        backend.dest = TerminalWriter::ansi(Box::new(writer));
        terminal.inner = Box::new(backend);
        let error = terminal
            .synchronized_update(|terminal| terminal.write_all(body))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
        let output = bytes.lock().unwrap().clone();
        assert!(output.ends_with(end), "{output:?}");
        if matches!(failure, Failure::WriteAfter(offset) if offset < begin.len())
            || matches!(failure, Failure::FlushAt(1))
        {
            assert!(!output.windows(body.len()).any(|window| window == body));
        }
        bytes.lock().unwrap().clear();
        terminal
            .synchronized_update(|terminal| terminal.write_all(body))
            .unwrap();
        let output = bytes.lock().unwrap().clone();
        assert_eq!(output, [begin.as_slice(), body, end].concat());
    }
}

#[test]
fn nested_scopes_and_unwind_close_only_the_outer_transaction() {
    let (writer, bytes) = new_test_writer();
    let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
    terminal.inner = Box::new(new_inline_term_with_size(writer, 0, (40, 8)));
    terminal
        .synchronized_update(|terminal| {
            terminal.write_all(b"outer")?;
            terminal.synchronized_update(|terminal| terminal.write_all(b"inner"))?;
            terminal.write_all(b"tail")
        })
        .unwrap();
    let output = bytes.lock().unwrap().clone();
    assert_eq!(output, b"\x1b[?2026houterinnertail\x1b[?2026l");
    bytes.lock().unwrap().clear();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = terminal.synchronized_update(|terminal| {
            terminal.synchronized_update(|terminal| {
                terminal.write_all(b"inner")?;
                panic!("injected unwind");
            })
        });
    }));
    assert!(unwind.is_err());
    let output = bytes.lock().unwrap().clone();
    assert_eq!(output, b"\x1b[?2026hinner\x1b[?2026l");
}

#[test]
fn identical_screen_requests_and_nonraw_mouse_changes_emit_nothing() {
    let (writer, bytes) = new_test_writer();
    let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
    terminal.inner = Box::new(new_inline_term_with_size(writer, 0, (40, 8)));
    terminal
        .synchronized_update(|terminal| {
            assert!(!terminal.set_dynamic_alternate_screen(None)?);
            terminal.write_all(b"")
        })
        .unwrap();
    assert!(bytes.lock().unwrap().is_empty());
    terminal
        .synchronized_update(|terminal| {
            terminal.set_dynamic_alternate_screen(Some(
                crate::context::AlternateScreenRequest {
                    mouse_tracking: false,
                },
            ))?;
            Ok(())
        })
        .unwrap();
    bytes.lock().unwrap().clear();
    for mouse_tracking in [false, true, true, false] {
        terminal
            .synchronized_update(|terminal| {
                assert!(!terminal.set_dynamic_alternate_screen(Some(
                    crate::context::AlternateScreenRequest { mouse_tracking }
                ))?);
                Ok(())
            })
            .unwrap();
        assert!(bytes.lock().unwrap().is_empty());
    }
}

#[component]
fn AnimatedMemo(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut tick = hooks.use_state(|| 0usize);
    let mut exit = hooks.use_state(|| false);
    if exit.get() {
        hooks.use_context_mut::<SystemContext>().exit();
    }
    hooks.use_terminal_events(move |event| {
        if let TerminalEvent::Key(key) = event {
            if key.code == KeyCode::Char('x') {
                exit.set(true);
            } else {
                tick += 1;
            }
        }
    });
    let body = if tick.get() < 5 {
        "STATIC_SENTINEL"
    } else {
        "CHANGED_SENTINEL"
    };
    element! {
        View(flex_direction: FlexDirection::Column) {
            Memo(memo_key: body.to_string(), compare: memo_key_eq as MemoComparator) {
                Text(content: body)
            }
            Text(content: format!("animation {}", tick.get()))
        }
    }
}

#[test]
fn animation_frames_leave_stable_memo_text_unwritten() {
    crate::render::set_retained_blit_for_tests(Some(true));
    let (writer, bytes) = new_test_writer();
    let (sender, events) = mpsc::unbounded();
    let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
    terminal.inner = Box::new(ScriptedTerminal {
        inner: new_inline_term_with_size(writer, 0, (40, 8)),
        events: Some(events.boxed()),
    });
    let mut frames = Vec::new();
    let mut offset = 0;
    let result = smol::block_on(crate::render::terminal_render_loop(
        &mut element!(AnimatedMemo),
        terminal,
        None,
        Some(Box::new(|_| {
            let output = bytes.lock().unwrap().clone();
            frames.push(String::from_utf8(output[offset..].to_vec()).unwrap());
            offset = output.len();
            let key = if frames.len() < 7 { 'a' } else { 'x' };
            let _ = sender.unbounded_send(TerminalEvent::Key(KeyEvent::new(
                KeyEventKind::Press,
                KeyCode::Char(key),
            )));
        })),
    ));
    crate::render::set_retained_blit_for_tests(None);
    result.unwrap();
    assert!(frames[0].contains("STATIC_SENTINEL"), "{frames:?}");
    for (index, frame) in frames.iter().enumerate().take(5).skip(2) {
        assert!(!frame.contains("STATIC_SENTINEL"), "{frames:?}");
        assert!(
            frame.contains(&index.to_string()),
            "animation must advance: {frames:?}"
        );
    }
    assert!(frames[5].contains("CHANGED_SENTINEL"), "{frames:?}");
    let mut terminal_screen = avt::Vt::new(40, 8);
    terminal_screen.feed_str(&frames.concat());
    assert!(
        terminal_screen
            .line(0)
            .text()
            .starts_with("CHANGED_SENTINEL")
    );
    assert!(terminal_screen.line(1).text().starts_with("animation 6"));
}

#[test]
fn unverified_multiplexers_do_not_inherit_outer_atomic_capability() {
    for environment in [
        vec![("TERM_PROGRAM", "herdr"), ("KITTY_WINDOW_ID", "1")],
        vec![("TERM_PROGRAM", "unknown"), ("TERM", "xterm-kitty")],
        vec![("TERM_PROGRAM", "ghostty"), ("TMUX", "server")],
        vec![("TERM_PROGRAM", "ghostty"), ("STY", "server")],
    ] {
        assert!(!synchronized_output_env(&environment));
    }
}

#[test]
fn scroll_patches_require_capability_active_transaction_and_valid_geometry() {
    for (atomic_capability, emit_markers, scoped, columns, expected_patch) in [
        (false, true, true, 10, false),
        (true, true, true, 10, true),
        (true, false, true, 10, false),
        (true, true, false, 10, false),
        (true, true, true, 9, false),
    ] {
        let mut previous = Canvas::new(10, 5);
        let mut next = Canvas::new(10, 5);
        for (canvas, labels) in [
            (&mut previous, ["top", "one", "two", "three", "bottom"]),
            (&mut next, ["top", "two", "three", "new", "bottom"]),
        ] {
            for (row, label) in labels.into_iter().enumerate() {
                canvas.subview_mut(0, 0, 0, 0, 10, 5).set_text(
                    0,
                    row as isize,
                    label,
                    CanvasTextStyle::default(),
                );
            }
        }
        next.set_scroll_hint(ScrollHint {
            top: 1,
            bottom: 3,
            delta: 1,
        });
        let (writer, bytes) = new_test_writer();
        let mut backend = new_fullscreen_term(writer, 0, 5);
        backend.size = Some((columns, 5));
        backend.prev_size_on_write = backend.size;
        let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
        terminal.inner = Box::new(backend);
        terminal.synchronized_output.atomic_capability = atomic_capability;
        terminal.synchronized_output.emit_markers = emit_markers;
        if scoped {
            terminal
                .synchronized_update(|terminal| terminal.write_canvas(Some(&previous), &next))
                .unwrap();
        } else {
            terminal.write_canvas(Some(&previous), &next).unwrap();
        }
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(output.contains("\x1b[2;4r"), expected_patch, "{output:?}");
        assert_eq!(output.starts_with("\x1b[?2026h"), scoped && emit_markers);
        if columns == 10 {
            let mut screen = avt::Vt::new(10, 5);
            let mut setup = Vec::new();
            previous
                .write_ansi_without_final_newline(&mut setup)
                .unwrap();
            screen.feed_str(&String::from_utf8(setup).unwrap());
            screen.feed_str(&output);
            for (row, label) in ["top", "two", "three", "new", "bottom"].iter().enumerate() {
                assert_eq!(screen.line(row).text().trim_end(), *label);
            }
        }
    }
}

#[derive(Default, Props)]
struct FailingFrameProps {
    alternate: bool,
}

#[component]
fn FailingFrame(mut hooks: Hooks, props: &FailingFrameProps) -> impl Into<AnyElement<'static>> {
    let (stdout, _) = hooks.use_output();
    let alternate = props.alternate;
    hooks.use_effect(
        move || {
            if !alternate {
                stdout.print("output");
            }
        },
        (),
    );
    if alternate {
        element!(AlternateScreen { Text(content: "alternate") }).into_any()
    } else {
        element!(Text(content: "main")).into_any()
    }
}

#[test]
fn rendering_propagates_transition_and_output_hook_errors_without_panicking() {
    for alternate in [false, true] {
        let (output, bytes) = new_test_writer();
        let mut backend = new_inline_term_with_size(TestWriter::default(), 0, (40, 8));
        backend.dest = TerminalWriter::ansi(Box::new(FailingWriter {
            output,
            failure: Some(Failure::WriteAfter(3)),
            flushes: 0,
        }));
        let (mut terminal, _) = Terminal::mock(MockTerminalConfig::default());
        terminal.inner = Box::new(ScriptedTerminal {
            inner: backend,
            events: None,
        });
        let result = smol::block_on(crate::render::terminal_render_loop(
            &mut element!(FailingFrame(alternate)),
            terminal,
            None,
            None,
        ));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.contains("\x1b[?2026l"), "{output:?}");
    }
}
