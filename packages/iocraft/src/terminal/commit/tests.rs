use super::*;
use crate::prelude::*;
use futures::FutureExt;

#[derive(Clone, Default)]
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    blocked: Arc<AtomicBool>,
    attempted: Arc<AtomicBool>,
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.attempted.store(true, Ordering::Release);
        if self.blocked.load(Ordering::Acquire) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let length = bytes.len().min(2);
        self.bytes
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..length]);
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn session(capture: Capture) -> OutputSession {
    OutputSession::new(
        Box::new(capture),
        Box::new(io::sink()),
        false,
        false,
        Some(false),
        Output::Stdout,
    )
    .unwrap()
}

fn frame(canvas: Canvas, rows: u16) -> Operation {
    Operation::Frame {
        draft: Box::new(FrameDraft::new(canvas)),
        size: Some((20, rows)),
        screen: None,
        receipt: None,
    }
}

async fn barrier(session: &mut OutputSession) {
    let (sender, receiver) = oneshot::channel();
    session.enqueue(Operation::Barrier(sender)).unwrap();
    receiver.await.unwrap().unwrap();
}

#[derive(Debug)]
struct Source(ImageScope);

impl CanvasImageSource for Source {
    fn encoded_png(&self) -> &str {
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aFioAAAAASUVORK5CYII="
    }
    fn scope(&self) -> &ImageScope {
        &self.0
    }
}

#[test]
fn archived_main_images_survive_alternate_eviction_and_screen_switch() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
        let image = CanvasImage::new(Arc::new(Source(ImageScope::default())), 2, 2).unwrap();
        let mut document = Canvas::new(2, 2);
        document
            .subview_mut(0, 0, 0, 0, 2, 2)
            .set_image(0, 0, &image);
        let mut viewport = Canvas::new(20, 3);
        viewport
            .subview_mut(0, 0, 0, 0, 20, 3)
            .blit_region_from_clean(&document, 5, 1, 0, 1, 2, 1);
        let take_output =
            || String::from_utf8(std::mem::take(&mut *capture.bytes.lock().unwrap())).unwrap();
        let uploaded_id = |text: &str| -> u32 {
            text.split_once("a=t,f=100,t=d,i=")
                .unwrap()
                .1
                .split_once(',')
                .unwrap()
                .0
                .parse()
                .unwrap()
        };
        let deletion = |id| format!("a=d,d=I,i={id},");

        output
            .present(FrameDraft::new(viewport.clone()))
            .await
            .unwrap();
        let main = uploaded_id(&take_output());
        output
            .append_history(HistoryDraft::new(viewport.clone()))
            .await
            .unwrap();
        let archived = uploaded_id(&take_output());
        assert_eq!(archived, main + 2);
        output
            .present(FrameDraft::new(viewport.clone()).alternate_screen())
            .await
            .unwrap();
        let alternate = uploaded_id(&take_output());
        assert_ne!(main % 2, alternate % 2);

        let mut offscreen = Canvas::new(20, 10);
        offscreen
            .subview_mut(0, 0, 0, 0, 20, 10)
            .blit_region_from_clean(&document, 5, 8, 0, 0, 2, 2);
        output
            .present(FrameDraft::new(offscreen).alternate_screen())
            .await
            .unwrap();
        let evicted = take_output();
        assert!(evicted.contains(&deletion(alternate)));
        assert!(!evicted.contains("a=t,f=100"));
        output
            .present(FrameDraft::new(viewport).alternate_screen())
            .await
            .unwrap();
        let reentered = uploaded_id(&take_output());
        assert_eq!(reentered, alternate + 2);

        output
            .present(FrameDraft::new(Canvas::new(20, 3)))
            .await
            .unwrap();
        let returned = take_output();
        assert!(
            returned.find(&deletion(reentered)).unwrap() < returned.find("\x1b[?1049l").unwrap()
        );
        output.finish().await.unwrap();
        let finished = take_output();
        for id in [main, archived] {
            assert!(!evicted.contains(&deletion(id)));
            assert!(!returned.contains(&deletion(id)));
            assert!(!finished.contains(&deletion(id)));
        }
    });
}

#[test]
fn handoff_stops_retirement_until_reacquired() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        let baseline = output.sender().main_screen_revision();
        let scope = ImageScope::default();
        let mut canvas = Canvas::new(20, 2);
        canvas.set_image_scope(Some(scope.clone()));
        for index in 0..40 {
            let image = CanvasImage::new(Arc::new(Source(scope.clone())), 1, 1).unwrap();
            canvas
                .subview_mut(0, 0, 0, 0, 20, 2)
                .set_image(index % 20, index / 20, &image);
        }
        output.enqueue(frame(canvas, 3)).unwrap();
        barrier(&mut output).await;
        capture.blocked.store(true, Ordering::Release);
        scope.retire();
        output.enqueue(frame(Canvas::new(20, 1), 3)).unwrap();
        let (sender, receiver) = oneshot::channel();
        output.enqueue(Operation::Release(sender)).unwrap();
        capture.blocked.store(false, Ordering::Release);
        receiver.await.unwrap().unwrap();
        let released = capture.bytes.lock().unwrap().clone();
        smol::Timer::after(Duration::from_millis(30)).await;
        assert_eq!(
            *capture.bytes.lock().unwrap(),
            released,
            "handoff owns the entire output channel"
        );
        let (sender, receiver) = oneshot::channel();
        output.enqueue(Operation::Reacquire(sender)).unwrap();
        receiver.await.unwrap().unwrap();
        assert!(output.sender().main_screen_revision() > baseline);
        output.finish().await.unwrap();
        let bytes = capture.bytes.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.matches("a=d,d=I").count(), 40);
    });
}

#[test]
fn finished_session_rejects_reacquire_without_a_pending_receipt() {
    smol::block_on(async {
        let mut output = session(Capture::default());
        output.finish().await.unwrap();
        let (sender, receiver) = oneshot::channel();
        assert_eq!(
            output
                .enqueue(Operation::Reacquire(sender))
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            receiver
                .now_or_never()
                .unwrap()
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    });
}

#[test]
fn dropping_queued_receipts_allows_reentrant_wakers() {
    struct Reenter(OutputSender, Arc<AtomicBool>);
    impl Wake for Reenter {
        fn wake(self: Arc<Self>) {
            self.0.submit(OutputMessage::Stdout("late".into()));
            self.1.store(true, Ordering::Release);
        }
    }
    let (done, completion) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let capture = Capture::default();
        capture.blocked.store(true, Ordering::Release);
        let mut output = session(capture);
        let woke = Arc::new(AtomicBool::new(false));
        let waker = Waker::from(Arc::new(Reenter(output.sender(), woke.clone())));
        let (sender, mut receiver) = oneshot::channel();
        output
            .enqueue(Operation::Control("queued".into(), Some(sender)))
            .unwrap();
        assert!(
            Pin::new(&mut receiver)
                .poll(&mut std::task::Context::from_waker(&waker))
                .is_pending()
        );
        drop(output);
        assert!(woke.load(Ordering::Acquire));
        done.send(()).unwrap();
    });
    completion
        .recv_timeout(Duration::from_secs(5))
        .expect("receipt destruction must happen outside the admission lock");
    thread.join().unwrap();
}

#[test]
fn history_preserves_equal_height_editor_and_prior_scrollback() {
    smol::block_on(async {
        for rows in 1..=3 {
            let capture = Capture::default();
            let mut output = session(capture.clone());
            let history = |label: &str| {
                let mut canvas = Canvas::new(20, 1);
                canvas.subview_mut(0, 0, 0, 0, 20, 1).set_text(
                    0,
                    0,
                    label,
                    CanvasTextStyle::default(),
                );
                canvas
            };
            output
                .sender()
                .append_history(HistoryDraft::new(history("older")))
                .await
                .unwrap();
            let mut editor = element!(View(width: 20, height: rows) { TextInput(value: "draft界", has_focus: true, multiline: true) });
            let canvas = editor.render(Some(20));
            output.enqueue(frame(canvas.clone(), rows)).unwrap();
            barrier(&mut output).await;
            output
                .sender()
                .append_history(HistoryDraft::new(history("inserted")))
                .await
                .unwrap();
            output.enqueue(frame(canvas, rows)).unwrap();
            barrier(&mut output).await;
            let bytes = capture.bytes.lock().unwrap().clone();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                !text.contains("\x1b[3J"),
                "an equal-height live frame cannot purge history"
            );
            let mut terminal = avt::Vt::builder()
                .size(20, usize::from(rows))
                .scrollback_limit(100)
                .build();
            terminal.feed_str(text);
            let contents = terminal.lines().map(|line| line.text()).collect::<Vec<_>>();
            assert_eq!(
                contents
                    .iter()
                    .filter(|line| line.contains("older"))
                    .count(),
                1
            );
            assert_eq!(
                contents
                    .iter()
                    .filter(|line| line.contains("inserted"))
                    .count(),
                1
            );
            assert_eq!(
                contents
                    .iter()
                    .filter(|line| line.contains("draft界"))
                    .count(),
                1,
                "the live editor never becomes history"
            );
            output.finish().await.unwrap();
        }
    });
}

#[derive(Clone)]
struct FaultCapture {
    bytes: Arc<Mutex<Vec<u8>>>,
    fault: Arc<Mutex<Option<(usize, io::ErrorKind)>>>,
    flush_blocked: Arc<AtomicBool>,
}

impl Write for FaultCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut output = self.bytes.lock().unwrap();
        let mut fault = self.fault.lock().unwrap();
        let mut length = bytes.len().min(7);
        if let Some((offset, kind)) = *fault {
            if output.len() == offset {
                *fault = None;
                return Err(kind.into());
            }
            length = length.min(offset.saturating_sub(output.len()));
        }
        output.extend_from_slice(&bytes[..length]);
        Ok(length)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.flush_blocked.swap(false, Ordering::AcqRel) {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn protocol_prefixes_continue_once_and_fatal_writes_only_restore() {
    smol::block_on(async {
        // Cross UTF-8, CSI, Kitty transmission, placement, OSC52 and flush boundaries.
        for fatal in [false, true] {
            for offset in 0..700 {
                let bytes = Arc::new(Mutex::new(Vec::new()));
                let capture = FaultCapture {
                    bytes: bytes.clone(),
                    fault: Arc::new(Mutex::new(Some((
                        offset,
                        if fatal {
                            io::ErrorKind::BrokenPipe
                        } else {
                            io::ErrorKind::WouldBlock
                        },
                    )))),
                    flush_blocked: Arc::new(AtomicBool::new(!fatal)),
                };
                let mut output = OutputSession::new(
                    Box::new(capture.clone()),
                    Box::new(io::sink()),
                    false,
                    false,
                    Some(false),
                    Output::Stdout,
                )
                .unwrap();
                let mut canvas = Canvas::new(20, 2);
                canvas.subview_mut(0, 0, 0, 0, 20, 2).set_text(
                    0,
                    0,
                    "中文終",
                    CanvasTextStyle::default(),
                );
                let source = Arc::new(Source(ImageScope::default()));
                let image = CanvasImage::new(source, 1, 1).unwrap();
                canvas
                    .subview_mut(0, 0, 0, 0, 20, 2)
                    .set_image(0, 1, &image);
                output.enqueue(frame(canvas, 3)).ok();
                let (sender, receiver) = oneshot::channel();
                output
                    .enqueue(Operation::PrintBatch(
                        vec![OutputMessage::Copy("完整复制".into(), None, sender)],
                        Output::Stdout,
                    ))
                    .ok();
                let receipt = receiver.await;
                let result = output.finish().await;
                let actual = bytes.lock().unwrap().clone();
                assert!(
                    actual.len() < 700,
                    "every emitted protocol byte is covered by the fault sweep"
                );
                if fatal && capture.fault.lock().unwrap().is_none() {
                    assert!(result.is_err());
                    if receipt.is_ok_and(|result| result.is_ok()) {
                        assert!(
                            actual
                                .windows(b"\x1b]52;c;".len())
                                .any(|part| part == b"\x1b]52;c;")
                        );
                    }
                    let text = String::from_utf8_lossy(&actual);
                    assert!(text.matches("a=t,f=100").count() <= 1);
                    assert!(text.matches("中文終").count() <= 1);
                } else {
                    result.unwrap();
                    receipt.unwrap().unwrap();
                    let text = std::str::from_utf8(&actual).unwrap();
                    assert_eq!(text.matches("中文終").count(), 1);
                    assert_eq!(text.matches("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aFioAAAAASUVORK5CYII=").count(), 1);
                    assert_eq!(text.matches("\x1b]52;c;").count(), 1);
                    assert_eq!(text.matches("a=t,f=100").count(), 1);
                }
                smol::Timer::after(Duration::from_millis(1)).await;
                assert_eq!(
                    *bytes.lock().unwrap(),
                    actual,
                    "finish leaves no late writer"
                );
            }
        }
    });
}

#[test]
fn history_receipt_keeps_coverage_capacity_until_its_last_reference() {
    smol::block_on(async {
        let mut output = OutputSession::with_capacity(
            Box::new(Capture::default()),
            Box::new(io::sink()),
            false,
            false,
            Some(false),
            Output::Stdout,
            OutputCapacity::new(128 * 1024, ()),
        )
        .unwrap();
        let history = || {
            HistoryDraft::new(Canvas::new(0, 0)).with_coverage(
                SourceCoverage::new(Arc::new(vec![7u8; 40 * 1024])).with_owned_bytes(40 * 1024),
            )
        };
        let receipt = output.sender().append_history(history()).await.unwrap();
        let retained = receipt.clone();
        drop(receipt);
        assert_eq!(
            output
                .sender()
                .append_history(history())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::OutOfMemory
        );
        assert_eq!(
            retained
                .coverage
                .as_ref()
                .unwrap()
                .get::<Vec<u8>>()
                .unwrap()
                .len(),
            40 * 1024
        );
        drop(retained);
        output.sender().append_history(history()).await.unwrap();
        output.finish().await.unwrap();
    });
}

#[test]
fn committed_hits_survive_backpressure_and_are_invalidated_by_handoff() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        let target = |label: &'static str| {
            let mut operation = frame(Canvas::new(20, 2), 3);
            if let Operation::Frame { draft, .. } = &mut operation {
                draft.hits.push(HitRegion {
                    column: 0,
                    row: 0,
                    width: 10,
                    height: 1,
                    target: SourceCoverage::new(Arc::new(label)),
                });
            }
            operation
        };
        output.enqueue(target("visible")).unwrap();
        barrier(&mut output).await;
        capture.blocked.store(true, Ordering::Release);
        output
            .enqueue(Operation::Control("\x1b]2;blocked\x07".into(), None))
            .unwrap();
        output.enqueue(target("new")).unwrap();
        assert_eq!(
            output.sender().committed_frame().unwrap().hit::<&str>(2, 0),
            Some(&"visible")
        );
        let (sender, receiver) = oneshot::channel();
        output.enqueue(Operation::Release(sender)).unwrap();
        capture.blocked.store(false, Ordering::Release);
        receiver.await.unwrap().unwrap();
        assert!(output.sender().committed_frame().is_none());
        let (sender, receiver) = oneshot::channel();
        output.enqueue(Operation::Reacquire(sender)).unwrap();
        receiver.await.unwrap().unwrap();
        assert!(output.sender().committed_frame().is_none());
        output.finish().await.unwrap();
    });
}

#[test]
fn mouse_snapshot_tracks_visual_changes_independently_of_committed_hits() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        let target = |canvas, label| {
            let mut operation = frame(canvas, 3);
            if let Operation::Frame { draft, .. } = &mut operation {
                draft.hits.push(HitRegion {
                    column: 0,
                    row: 0,
                    width: 10,
                    height: 1,
                    target: SourceCoverage::new(Arc::new(label)),
                });
            }
            operation
        };
        let mut canvas = Canvas::new(20, 2);
        output.enqueue(target(canvas.clone(), "initial")).unwrap();
        barrier(&mut output).await;
        let initial = output.projection().unwrap();
        assert!(initial.cells.is_none());
        assert_eq!(initial.frame.hit::<&str>(0, 0), Some(&"initial"));

        output.enqueue(Operation::Mouse(true)).unwrap();
        barrier(&mut output).await;
        let initial_cells = output.projection().unwrap().cells.unwrap();
        assert!(initial_cells.cell_is_blank(0, 0));
        capture.bytes.lock().unwrap().clear();
        output
            .enqueue(target(canvas.clone(), "same pixels"))
            .unwrap();
        barrier(&mut output).await;
        let unchanged = output.projection().unwrap();
        assert!(capture.bytes.lock().unwrap().is_empty());
        assert_eq!(unchanged.frame.hit::<&str>(0, 0), Some(&"same pixels"));
        assert!(Arc::ptr_eq(&initial_cells, &unchanged.cells.unwrap()));

        canvas.declare_cursor(1, 0, true);
        output
            .enqueue(target(canvas.clone(), "moved caret"))
            .unwrap();
        barrier(&mut output).await;
        assert!(!capture.bytes.lock().unwrap().is_empty());
        let moved_caret = output.projection().unwrap().cells.unwrap();
        assert!(Arc::ptr_eq(&initial_cells, &moved_caret));

        canvas
            .subview_mut(0, 0, 0, 0, 20, 2)
            .set_text(0, 0, "content", CanvasTextStyle::default());
        output.enqueue(target(canvas, "painted")).unwrap();
        barrier(&mut output).await;
        let painted = output.projection().unwrap().cells.unwrap();
        assert!(!painted.cell_is_blank(0, 0));

        output.enqueue(Operation::Mouse(false)).unwrap();
        barrier(&mut output).await;
        let disabled = output.projection().unwrap();
        assert!(disabled.cells.is_none());
        assert_eq!(disabled.frame.hit::<&str>(0, 0), Some(&"painted"));
        output.finish().await.unwrap();
    });
}

#[test]
fn hidden_fullscreen_changes_skip_output_and_mouse_snapshot_until_resize() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        let alternate_frame = |canvas, rows| {
            let mut operation = frame(canvas, rows);
            if let Operation::Frame { screen, .. } = &mut operation {
                *screen = Some(crate::context::AlternateScreenRequest {
                    mouse_tracking: true,
                });
            }
            operation
        };
        let mut canvas = Canvas::new(20, 4);
        output.enqueue(alternate_frame(canvas.clone(), 2)).unwrap();
        barrier(&mut output).await;
        let initial = output.projection().unwrap().cells.unwrap();
        capture.bytes.lock().unwrap().clear();
        canvas.subview_mut(0, 0, 0, 0, 20, 4).set_text(
            0,
            3,
            "revealed",
            CanvasTextStyle::default(),
        );
        output.enqueue(alternate_frame(canvas.clone(), 2)).unwrap();
        barrier(&mut output).await;
        assert!(capture.bytes.lock().unwrap().is_empty());
        let unchanged = output.projection().unwrap().cells.unwrap();
        assert!(Arc::ptr_eq(&initial, &unchanged));
        assert!(unchanged.cell_is_blank(0, 3));

        output.enqueue(alternate_frame(canvas, 4)).unwrap();
        barrier(&mut output).await;
        let resized = output.projection().unwrap().cells.unwrap();
        assert!(!resized.cell_is_blank(0, 3));
        assert!(
            std::str::from_utf8(&capture.bytes.lock().unwrap())
                .unwrap()
                .contains("revealed")
        );
        output.finish().await.unwrap();
    });
}

#[test]
fn frame_capacity_backpressure_preserves_the_accepted_target_and_recovers() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = OutputSession::with_capacity(
            Box::new(capture.clone()),
            Box::new(io::sink()),
            false,
            false,
            Some(false),
            Output::Stdout,
            OutputCapacity::new(144 * 1024, ()),
        )
        .unwrap();
        barrier(&mut output).await;
        capture.blocked.store(true, Ordering::Release);
        output
            .enqueue(Operation::Bytes(vec![b'p'; 24 * 1024], true))
            .unwrap();
        let target = |bytes| {
            let mut operation = frame(Canvas::new(1, 1), 3);
            if let Operation::Frame { draft, .. } = &mut operation {
                draft.coverage =
                    Some(SourceCoverage::new(Arc::new(vec![0u8; bytes])).with_owned_bytes(bytes));
            }
            operation
        };
        assert!(output.enqueue_frame(target(20 * 1024)).unwrap());
        assert!(!output.enqueue_frame(target(40 * 1024)).unwrap());
        capture.blocked.store(false, Ordering::Release);
        barrier(&mut output).await;
        let committed = output.sender().committed_frame().unwrap();
        assert_eq!(
            committed
                .receipt
                .coverage
                .as_ref()
                .unwrap()
                .get::<Vec<u8>>()
                .unwrap()
                .len(),
            20 * 1024
        );
        drop(committed);
        assert!(output.enqueue_frame(target(40 * 1024)).unwrap());
        barrier(&mut output).await;
        assert_eq!(
            output
                .sender()
                .committed_frame()
                .unwrap()
                .receipt
                .coverage
                .as_ref()
                .unwrap()
                .get::<Vec<u8>>()
                .unwrap()
                .len(),
            40 * 1024
        );
        output.finish().await.unwrap();
    });
}

#[test]
fn dropped_history_receipt_does_not_hide_render_loop_admission_failure() {
    smol::block_on(async {
        let mut output = session(Capture::default());
        let draft = HistoryDraft::new(Canvas::new(1, 1))
            .with_coverage(SourceCoverage::new(Arc::new(())).with_owned_bytes(65 * 1024 * 1024));
        drop(output.sender().append_history_with_errors(draft, true));
        assert_eq!(
            output.check().unwrap_err().kind(),
            io::ErrorKind::OutOfMemory
        );
        assert_eq!(
            output.finish().await.unwrap_err().kind(),
            io::ErrorKind::OutOfMemory
        );
    });
}

#[test]
fn superseded_history_does_not_purge_or_advance_coverage() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        barrier(&mut output).await;
        capture.attempted.store(false, Ordering::Release);
        capture.blocked.store(true, Ordering::Release);
        output.enqueue(frame(Canvas::new(20, 1), 3)).unwrap();
        while !capture.attempted.load(Ordering::Acquire) {
            smol::Timer::after(Duration::from_millis(1)).await;
        }
        let target = HistoryTarget::default();
        let old = output.sender().append_history(
            HistoryDraft::new(Canvas::new(20, 1))
                .with_target(target.clone())
                .replace_scrollback()
                .with_coverage(SourceCoverage::new(Arc::new(1_u64))),
        );
        target.cancel();
        let mut canvas = Canvas::new(20, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 20, 1)
            .set_text(0, 0, "latest", CanvasTextStyle::default());
        let latest = output.sender().append_history(
            HistoryDraft::new(canvas).with_coverage(SourceCoverage::new(Arc::new(2_u64))),
        );
        capture.blocked.store(false, Ordering::Release);
        assert_eq!(old.await.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let receipt = latest.await.unwrap();
        assert_eq!(receipt.coverage.unwrap().get::<u64>(), Some(&2));
        output.finish().await.unwrap();
        let bytes = capture.bytes.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("\x1b[3J"));
        assert_eq!(text.matches("latest").count(), 1);
    });
}

#[test]
fn identical_editor_frames_preserve_the_terminal_cursor() {
    smol::block_on(async {
        for alternate in [false, true] {
            let capture = Capture::default();
            let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
            let mut editor = element!(View(width: 20, height: 3) {
                TextInput(value: "draft界", has_focus: true, physical_cursor: true, multiline: true)
            });
            let mut canvas = editor.render(Some(20));
            canvas.clear_damage();
            assert!(canvas.cursor_declaration().is_some());
            let mut moved = canvas.clone();
            moved.declare_cursor(1, 1, true);
            for canvas in [canvas, moved] {
                for repeat in 0..3 {
                    let draft = FrameDraft::new(canvas.clone());
                    output
                        .present(if alternate {
                            draft.alternate_screen()
                        } else {
                            draft
                        })
                        .await
                        .unwrap();
                    let bytes = std::mem::take(&mut *capture.bytes.lock().unwrap());
                    if repeat > 0 {
                        assert!(
                            bytes.is_empty(),
                            "unchanged editor output moved the IME anchor: {bytes:?}"
                        );
                    } else {
                        assert!(!bytes.is_empty(), "a changed caret must reach the terminal");
                    }
                }
            }
            output.finish().await.unwrap();
        }
    });
}

#[test]
fn attached_history_follows_main_screen_transition_under_backpressure() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
        let mut alternate = frame(Canvas::new(20, 3), 3);
        if let Operation::Frame { screen, .. } = &mut alternate {
            *screen = Some(crate::context::AlternateScreenRequest {
                mouse_tracking: false,
            });
        }
        output.enqueue(alternate).unwrap();
        barrier(&mut output).await;
        capture.bytes.lock().unwrap().clear();

        let canvas = |text| {
            let mut canvas = Canvas::new(20, 1);
            canvas
                .subview_mut(0, 0, 0, 0, 20, 1)
                .set_text(0, 0, text, CanvasTextStyle::default());
            canvas
        };
        capture.blocked.store(true, Ordering::Release);
        output
            .enqueue(Operation::Control("\x1b]2;blocked\x07".into(), None))
            .unwrap();
        let previous_target = HistoryTarget::default();
        let mut superseded = frame(canvas("obsolete input"), 3);
        if let Operation::Frame { draft, .. } = &mut superseded {
            draft.history = Some(Box::new(
                HistoryDraft::new(canvas("obsolete history"))
                    .replace_scrollback()
                    .with_target(previous_target.clone()),
            ));
        }
        output.enqueue(superseded).unwrap();
        previous_target.cancel();
        let mut returning = frame(canvas("returning"), 3);
        if let Operation::Frame { draft, .. } = &mut returning {
            draft.history = Some(Box::new(
                HistoryDraft::new(canvas("archived")).replace_scrollback(),
            ));
        }
        output.enqueue(returning).unwrap();
        output.enqueue(frame(canvas("latest input"), 3)).unwrap();
        capture.blocked.store(false, Ordering::Release);
        barrier(&mut output).await;

        let bytes = capture.bytes.lock().unwrap().clone();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("obsolete"));
        let leave = text.find("\x1b[?1049l").unwrap();
        let archived = text.find("archived").unwrap();
        let latest = text.find("latest input").unwrap();
        let history_begin = text[..archived].rfind("\x1b[?2026h").unwrap();
        let history_end = text[archived..].find("\x1b[?2026l").unwrap() + archived;
        assert!(leave < history_begin && history_end < latest);
        assert!(text[archived..history_end].contains("returning"));
        output.finish().await.unwrap();
    });
}

#[test]
fn history_batch_preserves_the_editor_until_its_atomic_replacement() {
    smol::block_on(async {
        for (replace, graphics) in [(false, false), (true, false), (false, true), (true, true)] {
            let capture = Capture::default();
            let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
            let mut editor = element!(View(width: 20, height: 3) {
                TextInput(value: "draft界", has_focus: true, physical_cursor: true, multiline: true)
            });
            output
                .present(FrameDraft::new(editor.render(Some(20))))
                .await
                .unwrap();
            capture.bytes.lock().unwrap().clear();
            let mut history = Canvas::new(20, 2);
            history.subview_mut(0, 0, 0, 0, 20, 2).set_text(
                0,
                0,
                "history",
                CanvasTextStyle::default(),
            );
            if graphics {
                let image =
                    CanvasImage::new(Arc::new(Source(ImageScope::default())), 1, 1).unwrap();
                history
                    .subview_mut(0, 0, 0, 0, 20, 2)
                    .set_image(0, 1, &image);
            }
            let draft = HistoryDraft::new(history);
            output
                .append_history(if replace {
                    draft.replace_scrollback()
                } else {
                    draft
                })
                .await
                .unwrap();
            let bytes = capture.bytes.lock().unwrap().clone();
            let text = std::str::from_utf8(&bytes).unwrap();
            let begin = text
                .find("\x1b[?2026h")
                .expect("history replacement has a presentation boundary");
            assert!(
                !text[..begin].contains("\x1b["),
                "history clear is visible before replacement: {text:?}"
            );
            assert_eq!(
                text[..begin].contains("\x1b_Ga=t"),
                graphics,
                "image upload precedes visible replacement"
            );
            assert!(
                text.ends_with("\x1b[?2026l"),
                "editor restoration must complete the same update: {text:?}"
            );
            assert_eq!(text.matches("\x1b[?2026h").count(), 1);
            assert!(text.contains("history"));
            assert!(text.contains("draft界"));
            output.finish().await.unwrap();
        }
    });
}

#[test]
fn finishing_alternate_screen_removes_the_hidden_interactive_frame() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
        let mut history = element!(Text(content: "saved history"));
        output
            .append_history(HistoryDraft::new(history.render(Some(20))))
            .await
            .unwrap();
        let mut main = element!(View(width: 20, flex_direction: FlexDirection::Column) {
            Text(content: "transient menu")
            TextInput(value: "draft", has_focus: true, physical_cursor: true)
        });
        output
            .present(FrameDraft::new(main.render(Some(20))))
            .await
            .unwrap();
        let mut alternate = element!(Text(content: "fullscreen reply"));
        output
            .present(FrameDraft::new(alternate.render(Some(20))).alternate_screen())
            .await
            .unwrap();
        output.finish().await.unwrap();
        let bytes = capture.bytes.lock().unwrap().clone();
        let mut terminal = avt::Vt::builder().size(20, 8).scrollback_limit(100).build();
        terminal.feed_str(std::str::from_utf8(&bytes).unwrap());
        let text = terminal
            .lines()
            .map(|line| line.text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("saved history"),
            "committed history survives exit: {text:?}"
        );
        assert!(
            !text.contains("transient menu"),
            "hidden interactive frame reappeared on exit: {text:?}"
        );
        assert!(!text.contains("draft"));
        assert!(!text.contains("fullscreen reply"));
    });
}

#[test]
fn screen_transitions_publish_the_frame_and_cursor_atomically() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
        let mut editor = element!(View(width: 20, height: 3) {
            TextInput(value: "draft界", has_focus: true, physical_cursor: true, multiline: true)
        });
        let canvas = editor.render(Some(20));
        output
            .present(FrameDraft::new(canvas.clone()))
            .await
            .unwrap();
        for alternate in [true, false] {
            capture.bytes.lock().unwrap().clear();
            let draft = FrameDraft::new(canvas.clone());
            output
                .present(if alternate {
                    draft.alternate_screen()
                } else {
                    draft
                })
                .await
                .unwrap();
            let bytes = capture.bytes.lock().unwrap().clone();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.starts_with("\x1b[?2026h"),
                "screen transition precedes its frame: {text:?}"
            );
            assert!(text.ends_with("\x1b[?2026l"));
            assert_eq!(text.matches("\x1b[?2026h").count(), 1);
            assert!(text.contains(if alternate {
                "\x1b[?1049h"
            } else {
                "\x1b[?1049l"
            }));
            assert!(
                text.contains(if alternate { "\x1b[1;8H" } else { "\x1b[8G" }),
                "declared input caret must be restored: {text:?}"
            );
        }
        output.finish().await.unwrap();
    });
}

#[test]
fn identical_damaged_frame_repairs_terminal_cells() {
    smol::block_on(async {
        for alternate in [false, true] {
            let capture = Capture::default();
            let mut output = TerminalOutput::new(capture.clone(), 20, 8).unwrap();
            let mut editor = element!(View(width: 20, height: 3) {
                TextInput(value: "draft", has_focus: true, physical_cursor: true, multiline: true)
            });
            let mut canvas = editor.render(Some(20));
            canvas.clear_damage();
            let draft = |canvas| {
                let draft = FrameDraft::new(canvas);
                if alternate {
                    draft.alternate_screen()
                } else {
                    draft
                }
            };
            output.present(draft(canvas.clone())).await.unwrap();
            let mut terminal = avt::Vt::new(20, 8);
            terminal.feed_str(std::str::from_utf8(&capture.bytes.lock().unwrap()).unwrap());
            terminal.feed_str("\x1b[s\x1b[1;1HWRONG\x1b[u");
            capture.bytes.lock().unwrap().clear();
            let mut damaged = canvas.clone();
            damaged.mark_damage(DamageRegion {
                x: 0,
                y: 0,
                width: 5,
                height: 1,
            });
            output.present(draft(damaged)).await.unwrap();
            let repair = capture.bytes.lock().unwrap().clone();
            terminal.feed_str(std::str::from_utf8(&repair).unwrap());
            assert!(
                terminal.line(0).text().starts_with("draft"),
                "damage must restore otherwise-identical text: {repair:?}"
            );
            output.present(draft(canvas.clone())).await.unwrap();
            capture.bytes.lock().unwrap().clear();
            output.present(draft(canvas)).await.unwrap();
            assert!(
                capture.bytes.lock().unwrap().is_empty(),
                "settled undamaged frames leave the caret untouched"
            );
            output.finish().await.unwrap();
        }
    });
}
