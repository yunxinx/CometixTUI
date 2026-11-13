use iocraft::prelude::*;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[component]
fn Editor(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (width, height) = hooks.use_terminal_size();
    let mut text = hooks.use_state(|| String::from("seed"));
    let mut alternate = hooks.use_state(|| false);
    let mut application = hooks.use_app();
    let mut system = hooks.use_context_mut::<SystemContext>();
    if alternate.get() {
        system.request_alternate_screen(false);
    }
    hooks.use_terminal_events(move |event| {
        if let TerminalEvent::Key(key) = event
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
                alternate.set(!alternate.get());
            }
            if key.code == KeyCode::Char('q') {
                application.exit();
            }
        }
    });
    element! {
        View(width: u32::from(width), height: u32::from(height), flex_direction: FlexDirection::Row) {
            Text(content: format!("{}{width}:", if alternate.get() { "A" } else { "M" }))
            View(flex_grow: 1.0_f32) {
                TextInput(value: text.to_string(), has_focus: true, multiline: true, max_height: height,
                    on_change: move |value| text.set(value))
            }
        }
    }
}

struct Output {
    receiver: Receiver<Vec<u8>>,
    terminal: Arc<Mutex<avt::Vt>>,
    transcript: Vec<u8>,
    closed: bool,
}

impl Output {
    fn receive(&mut self, duration: Duration) {
        match self.receiver.recv_timeout(duration) {
            Ok(bytes) => {
                assert!(
                    self.transcript.len() + bytes.len() <= 2 * 1024 * 1024,
                    "PTY output is bounded"
                );
                self.transcript.extend_from_slice(&bytes);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.closed = true,
        }
    }

    fn wait_for_sequence(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !self
            .transcript
            .windows(expected.len())
            .any(|bytes| bytes == expected)
        {
            assert!(
                !self.closed && Instant::now() < deadline,
                "missing sequence {expected:?}; bytes={:?}",
                String::from_utf8_lossy(&self.transcript)
            );
            self.receive(Duration::from_millis(25));
        }
    }

    fn wait_for(&mut self, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let visible = self
                .terminal
                .lock()
                .unwrap()
                .view()
                .map(|line| line.text())
                .collect::<Vec<_>>()
                .join("\n");
            if visible.contains(expected) {
                return;
            }
            assert!(!self.closed, "PTY closed before {expected:?}: {visible:?}");
            assert!(
                Instant::now() < deadline,
                "missing {expected:?}; screen={visible:?}; bytes={:?}",
                String::from_utf8_lossy(&self.transcript)
            );
            self.receive(Duration::from_millis(25));
        }
    }
}

#[test]
fn real_editor_roundtrip_in_one_row_pty() {
    run_editor_in_native_pty(1, "real_editor_roundtrip_in_one_row_pty");
}

#[test]
fn real_editor_roundtrip_in_two_row_pty() {
    run_editor_in_native_pty(2, "real_editor_roundtrip_in_two_row_pty");
}

#[test]
fn real_editor_roundtrip_in_three_row_pty() {
    run_editor_in_native_pty(3, "real_editor_roundtrip_in_three_row_pty");
}

fn run_editor_in_native_pty(rows: u16, test_name: &str) {
    if std::env::var_os("IOCRAFT_PTY_CHILD").is_some() {
        println!();
        smol::block_on(element!(Editor).render_loop()).unwrap();
        return;
    }
    {
        let size = PtySize {
            rows,
            cols: 24,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .expect("native PTY is required");
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args(["--exact", test_name, "--nocapture", "--test-threads=1"]);
        command.env("IOCRAFT_PTY_CHILD", "1");
        command.env("TERM", "xterm-256color");
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let terminal = Arc::new(Mutex::new(avt::Vt::new(24, usize::from(rows))));
        let reader_terminal = terminal.clone();
        let reply_writer = writer.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            let mut pending_utf8 = Vec::new();
            let mut query_offset = 0;
            let mut total_bytes = 0;
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                total_bytes += count;
                assert!(total_bytes <= 2 * 1024 * 1024, "PTY capture is bounded");
                pending_utf8.extend_from_slice(&bytes[..count]);
                let length = match std::str::from_utf8(&pending_utf8) {
                    Ok(text) => text.len(),
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(error) => panic!("invalid terminal UTF-8: {error}"),
                };
                {
                    let mut terminal = reader_terminal.lock().unwrap();
                    for character in std::str::from_utf8(&pending_utf8[..length])
                        .unwrap()
                        .chars()
                    {
                        terminal.feed(character);
                        const CURSOR_QUERY: &[u8] = b"\x1b[6n";
                        if character == char::from(CURSOR_QUERY[query_offset]) {
                            query_offset += 1;
                        } else {
                            query_offset = usize::from(character == '\x1b');
                        }
                        if query_offset == CURSOR_QUERY.len() {
                            query_offset = 0;
                            // ConPTY's inherited cursor requires a live host reply, including during resize.
                            let cursor = terminal.cursor();
                            let mut writer = reply_writer.lock().unwrap();
                            let response = format!("\x1b[{};{}R", cursor.row + 1, cursor.col + 1);
                            writer.write_all(response.as_bytes()).unwrap();
                            writer.flush().unwrap();
                        }
                    }
                }
                pending_utf8.drain(..length);
                if sender.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
            assert!(
                pending_utf8.is_empty(),
                "PTY ended inside a UTF-8 character"
            );
        });
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("spawn real Cometix editor");
        drop(pair.slave);
        let mut output = Output {
            receiver,
            terminal,
            transcript: Vec::new(),
            closed: false,
        };
        let send_input = |bytes: &[u8]| {
            let mut writer = writer.lock().unwrap();
            writer.write_all(bytes).unwrap();
            writer.flush().unwrap();
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            output.wait_for("M24:seed");
            send_input("\x1b[F界".as_bytes());
            output.wait_for("M24:seed界");
            send_input(b"\x14");
            output.wait_for("A24:seed界");
            output.wait_for_sequence(b"\x1b[?1049h");
            output
                .terminal
                .lock()
                .unwrap()
                .resize(30, usize::from(rows));
            pair.master.resize(PtySize { cols: 30, ..size }).unwrap();
            output.wait_for("A30:seed界");
            send_input(b"\x14");
            output.wait_for("M30:seed界");
            output.wait_for_sequence(b"\x1b[?1049l");
            send_input(b"q");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "{status:?}");
                    break;
                }
                assert!(Instant::now() < deadline, "editor did not finish");
                output.receive(Duration::from_millis(10));
            }
            output.wait_for_sequence(b"\x1b[?25h");
        }));
        let _ = child.kill();
        let _ = child.wait();
        drop(writer);
        drop(pair.master);
        let drain_deadline = Instant::now() + Duration::from_secs(5);
        while !output.closed && Instant::now() < drain_deadline {
            output.receive(Duration::from_millis(10));
        }
        let report = std::env::var_os("IOCRAFT_PTY_REPORT").map(std::path::PathBuf::from);
        if let Some(report) = report {
            std::fs::create_dir_all(&report).unwrap();
            std::fs::write(
                report.join(format!("{}-{rows}-rows.ansi", std::env::consts::OS)),
                &output.transcript,
            )
            .unwrap();
        }
        assert!(output.closed, "PTY capture did not reach EOF");
        assert!(
            output.terminal.lock().unwrap().cursor().visible,
            "exit restores cursor visibility"
        );
        drop(output);
        reader_thread.join().unwrap();
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
