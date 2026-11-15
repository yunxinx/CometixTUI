#[path = "support/pty_output.rs"]
mod pty_output;

use iocraft::prelude::*;
#[cfg(windows)]
use portable_pty::ChildKiller;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: usize = 4096;

#[derive(Clone)]
struct Target {
    width: u16,
    generation: usize,
    cancelled: bool,
    token: HistoryTarget,
}

#[derive(Debug, Default)]
struct Image(ImageScope);

impl CanvasImageSource for Image {
    fn encoded_png(&self) -> &str {
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4H8DwHwAF8AJPyBbvuwAAAABJRU5ErkJggg=="
    }
    fn scope(&self) -> &ImageScope {
        &self.0
    }
}

#[derive(Default, Props)]
struct ReplayProps {
    directory: PathBuf,
}

#[component]
fn Replay(mut hooks: Hooks, props: &ReplayProps) -> impl Into<AnyElement<'static>> {
    let (width, height) = hooks.use_terminal_size();
    let mut text = hooks.use_state(|| String::from("seed"));
    let mut target = hooks.use_state(|| Target {
        width,
        generation: 0,
        cancelled: false,
        token: HistoryTarget::default(),
    });
    if target.read().width != width {
        let previous = target.read().clone();
        previous.token.cancel();
        target.set(Target {
            width,
            generation: previous.generation + 1,
            cancelled: false,
            token: HistoryTarget::default(),
        });
    }
    let mut application = hooks.use_app();
    let directory = props.directory.clone();
    hooks.use_terminal_events(move |event| {
        if let TerminalEvent::Key(key) = event
            && key.kind == KeyEventKind::Press
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char('c') => {
                    target.read().token.cancel();
                    target.write().cancelled = true;
                    std::fs::write(directory.join("cancelled"), b"accepted").unwrap();
                }
                KeyCode::Char('r') => {
                    let previous = target.read().clone();
                    previous.token.cancel();
                    target.set(Target {
                        generation: previous.generation + 1,
                        token: HistoryTarget::default(),
                        cancelled: false,
                        ..previous
                    });
                }
                KeyCode::Char('q') => application.exit(),
                _ => {}
            }
        }
    });
    let (stdout, _) = hooks.use_output();
    let directory = props.directory.clone();
    hooks.use_future(async move {
        let image = CanvasImage::new(Arc::new(Image::default()), 2, 2).unwrap();
        let mut completed = None;
        loop {
            let current = target.read().clone();
            if current.cancelled || completed == Some(current.generation) {
                smol::Timer::after(Duration::from_millis(5)).await;
                continue;
            }
            for start in (0..ROWS).step_by(32) {
                if target.read().generation != current.generation || target.read().cancelled {
                    break;
                }
                let mut canvas = Canvas::new(usize::from(current.width), 32);
                for row in 0..32 {
                    canvas
                        .subview_mut(0, 0, 0, 0, usize::from(current.width), 32)
                        .set_text(
                            0,
                            row as isize,
                            &format!("history-{}-{:04}", current.generation, start + row),
                            CanvasTextStyle::default(),
                        );
                }
                canvas
                    .subview_mut(0, 0, 0, 0, usize::from(current.width), 32)
                    .set_image(current.width as isize - 3, 0, &image);
                let mut draft = HistoryDraft::new(canvas).with_target(current.token.clone());
                if start == 0 && current.generation > 0 {
                    draft = draft.replace_scrollback();
                }
                match stdout.append_history(draft).await {
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => break,
                    Err(error) => panic!("history output: {error}"),
                }
                if start + 32 == ROWS {
                    completed = Some(current.generation);
                    std::fs::write(
                        directory.join("completed"),
                        format!("{}:{}", current.width, current.generation),
                    )
                    .unwrap();
                }
            }
        }
    });
    let directory = props.directory.clone();
    std::fs::write(directory.join("ready"), b"ready").unwrap();
    element! {
        View(width: width, height: height, flex_direction: FlexDirection::Column) {
            TextInput(value: text.to_string(), has_focus: true, multiline: true, auto_grow: true, max_height: height,
                physical_cursor: true,
                on_change: move |value: String| {
                    std::fs::write(directory.join("input"), value.as_bytes()).unwrap();
                    text.set(value);
                })
        }
    }
}

struct PtyDeadline {
    stage: Arc<Mutex<&'static str>>,
    #[cfg(windows)]
    child: Arc<Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>>,
    completed: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    rows: u16,
}

impl PtyDeadline {
    fn new(rows: u16) -> Self {
        let stage = Arc::new(Mutex::new("preparing the scenario"));
        let current = stage.clone();
        #[cfg(windows)]
        let child = Arc::new(Mutex::new(None::<Box<dyn ChildKiller + Send + Sync>>));
        #[cfg(windows)]
        let timed_out_child = child.clone();
        let (completed, completion) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            if completion.recv_timeout(Duration::from_secs(90))
                == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            {
                let stage = *current.lock().unwrap();
                let _ = writeln!(
                    std::io::stderr(),
                    "scrollback PTY ({rows} rows) timed out during {stage}"
                );
                // Windows handles retain identity after reaping; Unix killers retain only a PID.
                // On Unix, exiting closes the controlling PTY and signals its foreground group.
                #[cfg(windows)]
                if let Some(child) = timed_out_child.lock().unwrap().as_mut() {
                    let _ = child.kill();
                }
                // Native ConPTY calls can block outside Rust's polling deadlines.
                std::process::exit(1);
            }
        });
        Self {
            stage,
            #[cfg(windows)]
            child,
            completed,
            thread: Some(thread),
            rows,
        }
    }

    fn at(&self, stage: &'static str) {
        *self.stage.lock().unwrap() = stage;
    }

    #[cfg(windows)]
    fn track_child(&self, child: Box<dyn ChildKiller + Send + Sync>) {
        *self.child.lock().unwrap() = Some(child);
    }

    #[track_caller]
    fn wait_for(&self, stage: &'static str, mut ready: impl FnMut() -> bool) {
        self.at(stage);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "PTY validation made no progress during {stage}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn report_failure(&self, error: &(dyn std::any::Any + Send)) {
        let stage = *self.stage.lock().unwrap();
        let message = error
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| error.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        let _ = writeln!(
            std::io::stderr(),
            "scrollback PTY ({} rows) failed during {stage}: {message}",
            self.rows
        );
    }
}

impl Drop for PtyDeadline {
    fn drop(&mut self) {
        let _ = self.completed.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Directory(PathBuf);

impl Directory {
    fn set_output_paused(&self, paused: bool) -> io::Result<()> {
        let path = self.0.join("output-paused");
        if paused {
            std::fs::write(path, b"paused")
        } else {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct GatedOutput {
    directory: PathBuf,
    output: io::Stdout,
    capture: io::BufWriter<std::fs::File>,
    captured_bytes: usize,
    reported_block: bool,
}

impl GatedOutput {
    fn check_writable(&mut self) -> io::Result<()> {
        // ConPTY needs its output drained to deliver native input. Block at the writer boundary.
        if self.directory.join("output-paused").try_exists()? {
            if !self.reported_block {
                std::fs::write(self.directory.join("output-blocked"), b"blocked")?;
                self.reported_block = true;
            }
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if self.reported_block {
            std::fs::remove_file(self.directory.join("output-blocked"))?;
            self.reported_block = false;
        }
        Ok(())
    }
}

impl Write for GatedOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check_writable()?;
        let count = self.output.write(bytes)?;
        self.captured_bytes += count;
        assert!(
            self.captured_bytes <= 4 * 1024 * 1024,
            "bounded application output capture"
        );
        // ConPTY can consume control sequences instead of forwarding the original bytes.
        self.capture.write_all(&bytes[..count])?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.check_writable()?;
        self.output.flush()?;
        self.capture.flush()
    }
}

fn contents(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn assert_graphics_and_balanced_updates(bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    assert!(
        text.contains("a=t,f=100"),
        "history includes native graphics"
    );
    assert_eq!(
        text.matches("\x1b[?2026h").count(),
        text.matches("\x1b[?2026l").count()
    );
}

#[test]
fn scrollback_input_resize_graphics_and_cancel_in_a_slow_pty() {
    if let Some(directory) = std::env::var_os("IOCRAFT_SCROLLBACK_CHILD") {
        let directory = PathBuf::from(directory);
        let output = GatedOutput {
            directory: directory.clone(),
            output: io::stdout(),
            capture: io::BufWriter::new(
                std::fs::File::create(directory.join("application-output.ansi")).unwrap(),
            ),
            captured_bytes: 0,
            reported_block: false,
        };
        smol::block_on(
            element!(Replay(directory: directory))
                .render_loop()
                .stdout(output)
                .ignore_ctrl_c(),
        )
        .unwrap();
        return;
    }
    for rows in 1..=3 {
        let deadline = PtyDeadline::new(rows);
        let directory = Directory(
            std::env::temp_dir().join(format!("iocraft-scrollback-{}-{rows}", std::process::id())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        let size = PtySize {
            rows,
            cols: 40,
            pixel_width: 0,
            pixel_height: 0,
        };
        deadline.at("opening the native PTY");
        let pair = native_pty_system().openpty(size).unwrap();
        deadline.at("cloning the PTY output reader");
        let mut reader = pair.master.try_clone_reader().unwrap();
        deadline.at("taking the PTY input writer");
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let terminal = Arc::new(Mutex::new(avt::Vt::new(40, usize::from(rows))));
        let mut decoder = pty_output::OutputDecoder::new(terminal.clone(), writer.clone());
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let captured = bytes.clone();
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0; 256];
            loop {
                let count = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                decoder.feed(&buffer[..count]);
                let mut bytes = captured.lock().unwrap();
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() <= 4 * 1024 * 1024, "bounded PTY capture");
                drop(bytes);
                std::thread::sleep(Duration::from_millis(1));
            }
            decoder.finish();
        });
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "scrollback_input_resize_graphics_and_cancel_in_a_slow_pty",
            "--nocapture",
        ]);
        command.env("IOCRAFT_SCROLLBACK_CHILD", &directory.0);
        command.env("TERM", "xterm-256color");
        deadline.at("spawning the child");
        let mut child = pair.slave.spawn_command(command).unwrap();
        #[cfg(windows)]
        deadline.track_child(child.clone_killer());
        deadline.at("releasing the slave PTY");
        drop(pair.slave);
        let send_input = |bytes: &[u8]| {
            let mut writer = writer.lock().unwrap();
            writer.write_all(bytes).unwrap();
            writer.flush().unwrap();
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            deadline.wait_for("startup", || directory.0.join("ready").exists());
            deadline.wait_for("the initial editor frame", || {
                terminal
                    .lock()
                    .unwrap()
                    .view()
                    .any(|line| line.text().contains("seed"))
            });
            directory.set_output_paused(true).unwrap();
            deadline.at("requesting the first paused replay");
            send_input(b"\x12");
            deadline.wait_for("blocking the first replay output", || {
                directory.0.join("output-blocked").exists()
            });
            deadline.at("writing input while output is paused");
            send_input(b"\x1b[Ftyped");
            deadline.wait_for("accepting input while output is paused", || {
                contents(&directory.0.join("input")).as_deref() == Some("seedtyped")
            });
            assert!(directory.0.join("output-blocked").exists());
            directory.set_output_paused(false).unwrap();
            for cols in [25, 60, 35] {
                deadline.at("resizing the host terminal");
                terminal
                    .lock()
                    .unwrap()
                    .resize(usize::from(cols), usize::from(rows));
                deadline.at("resizing the native PTY");
                pair.master.resize(PtySize { cols, ..size }).unwrap();
                std::thread::sleep(Duration::from_millis(90));
            }
            deadline.wait_for("committing resized history", || {
                contents(&directory.0.join("completed"))
                    .is_some_and(|value| value.starts_with("35:"))
            });
            let completed = contents(&directory.0.join("completed")).unwrap();
            let generation = completed.split_once(':').unwrap().1;
            let marker = format!("history-{generation}-4095");
            deadline.wait_for("receiving resized history", || {
                String::from_utf8_lossy(&bytes.lock().unwrap()).contains(&marker)
            });
            directory.set_output_paused(true).unwrap();
            deadline.at("requesting replay while output is paused");
            send_input(b"\x12");
            deadline.wait_for("blocking replay output before cancellation", || {
                directory.0.join("output-blocked").exists()
            });
            deadline.at("writing cancellation while output is paused");
            send_input(b"\x03Z");
            deadline.wait_for("accepting cancellation", || {
                directory.0.join("cancelled").exists()
            });
            deadline.wait_for("accepting input after cancellation", || {
                contents(&directory.0.join("input")).as_deref() == Some("seedtypedZ")
            });
            assert!(directory.0.join("output-blocked").exists());
            directory.set_output_paused(false).unwrap();
            deadline.at("requesting normal exit");
            send_input(b"\x11");
            deadline.wait_for("normal child exit", || {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "{status:?}");
                    true
                } else {
                    false
                }
            });
        }));
        if let Err(error) = &result {
            deadline.report_failure(error.as_ref());
            let progress = || {
                format!(
                    "ready={}, input={:?}, cancelled={}, completed={:?}, output_blocked={}",
                    directory.0.join("ready").exists(),
                    contents(&directory.0.join("input")),
                    directory.0.join("cancelled").exists(),
                    contents(&directory.0.join("completed")),
                    directory.0.join("output-blocked").exists(),
                )
            };
            let before = progress();
            let _ = writeln!(std::io::stderr(), "before resuming output: {before}");
            deadline.at("resuming output after failure");
            let _ = directory.set_output_paused(false);
            let drain_deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < drain_deadline
                && !(directory.0.join("cancelled").exists()
                    && contents(&directory.0.join("input")).as_deref() == Some("seedtypedZ"))
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = writeln!(std::io::stderr(), "after resuming output: {}", progress());
        }
        let _ = directory.set_output_paused(false);
        deadline.at("checking child exit before cleanup");
        if !matches!(child.try_wait(), Ok(Some(_))) {
            deadline.at("terminating the child");
            let _ = child.kill();
        }
        deadline.at("reaping the child");
        let _ = child.wait();
        drop(writer);
        deadline.at("closing the native PTY");
        drop(pair.master);
        deadline.at("joining the output reader");
        reader_thread.join().unwrap();
        deadline.at("checking captured output");
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
        let application_output =
            std::fs::read(directory.0.join("application-output.ansi")).unwrap();
        assert_graphics_and_balanced_updates(&application_output);
        let bytes = bytes.lock().unwrap();
        #[cfg(unix)]
        assert_graphics_and_balanced_updates(&bytes);
        if let Some(report) = std::env::var_os("IOCRAFT_PTY_REPORT") {
            let report = PathBuf::from(report);
            std::fs::create_dir_all(&report).unwrap();
            std::fs::write(
                report.join(format!("scrollback-{}-{rows}.ansi", std::env::consts::OS)),
                &*bytes,
            )
            .unwrap();
            std::fs::write(
                report.join(format!(
                    "scrollback-{}-{rows}-application.ansi",
                    std::env::consts::OS
                )),
                application_output,
            )
            .unwrap();
        }
    }
}
