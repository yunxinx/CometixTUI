use iocraft::prelude::*;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

#[track_caller]
fn wait_for(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "PTY validation made no progress");
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn contents(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[test]
fn scrollback_input_resize_graphics_and_cancel_in_a_slow_pty() {
    if let Some(directory) = std::env::var_os("IOCRAFT_SCROLLBACK_CHILD") {
        smol::block_on(
            element!(Replay(directory: PathBuf::from(directory)))
                .render_loop()
                .ignore_ctrl_c(),
        )
        .unwrap();
        return;
    }
    for rows in 1..=3 {
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
        let pair = native_pty_system().openpty(size).unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        let paused = Arc::new(AtomicBool::new(true));
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let reading = paused.clone();
        let captured = bytes.clone();
        let reader_thread = std::thread::spawn(move || {
            let mut buffer = [0; 256];
            loop {
                while reading.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                let count = match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => count,
                };
                let mut bytes = captured.lock().unwrap();
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() <= 4 * 1024 * 1024, "bounded PTY capture");
                drop(bytes);
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "scrollback_input_resize_graphics_and_cancel_in_a_slow_pty",
            "--nocapture",
        ]);
        command.env("IOCRAFT_SCROLLBACK_CHILD", &directory.0);
        command.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            wait_for(|| directory.0.join("ready").exists());
            writer.write_all(b"\x1b[Ftyped").unwrap();
            writer.flush().unwrap();
            wait_for(|| contents(&directory.0.join("input")).as_deref() == Some("seedtyped"));
            for cols in [25, 60, 35] {
                pair.master.resize(PtySize { cols, ..size }).unwrap();
                std::thread::sleep(Duration::from_millis(90));
            }
            paused.store(false, Ordering::Release);
            wait_for(|| {
                contents(&directory.0.join("completed"))
                    .is_some_and(|value| value.starts_with("35:"))
            });
            let completed = contents(&directory.0.join("completed")).unwrap();
            let generation = completed.split_once(':').unwrap().1;
            let marker = format!("history-{generation}-4095");
            wait_for(|| String::from_utf8_lossy(&bytes.lock().unwrap()).contains(&marker));
            paused.store(true, Ordering::Release);
            writer.write_all(b"\x12").unwrap();
            writer.flush().unwrap();
            std::thread::sleep(Duration::from_millis(100));
            writer.write_all(b"\x03Z").unwrap();
            writer.flush().unwrap();
            wait_for(|| directory.0.join("cancelled").exists());
            wait_for(|| contents(&directory.0.join("input")).as_deref() == Some("seedtypedZ"));
            paused.store(false, Ordering::Release);
            writer.write_all(b"\x11").unwrap();
            writer.flush().unwrap();
            wait_for(|| child.try_wait().unwrap().is_some());
        }));
        paused.store(false, Ordering::Release);
        let _ = child.kill();
        let _ = child.wait();
        drop(writer);
        drop(pair.master);
        reader_thread.join().unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
        let bytes = bytes.lock().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("a=t,f=100"),
            "history includes native graphics"
        );
        assert_eq!(
            text.matches("\x1b[?2026h").count(),
            text.matches("\x1b[?2026l").count()
        );
        if let Some(report) = std::env::var_os("IOCRAFT_PTY_REPORT") {
            let report = PathBuf::from(report);
            std::fs::create_dir_all(&report).unwrap();
            std::fs::write(
                report.join(format!("scrollback-{}-{rows}.ansi", std::env::consts::OS)),
                &*bytes,
            )
            .unwrap();
        }
    }
}
