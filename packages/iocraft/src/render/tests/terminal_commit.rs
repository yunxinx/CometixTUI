use futures::stream;
use iocraft::prelude::*;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct Observation {
    text: Arc<Mutex<String>>,
    bytes: Arc<Mutex<Vec<u8>>>,
    writable: Arc<AtomicBool>,
    stdout: Arc<Mutex<Option<StdoutHandle>>>,
}

struct SlowOutput(Observation);

impl Write for SlowOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.0.writable.load(Ordering::Acquire) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let count = bytes.len().min(2);
        self.0
            .bytes
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default, Props)]
struct EditorProps {
    observation: Observation,
}

#[component]
fn Editor(mut hooks: Hooks, props: &EditorProps) -> impl Into<AnyElement<'static>> {
    let mut text = hooks.use_state(|| String::from("中文 draft"));
    let mut application = hooks.use_app();
    hooks.use_terminal_events(move |event| {
        if matches!(event, TerminalEvent::Key(key) if key.code == KeyCode::Esc) {
            application.exit();
        }
    });
    let observation = props.observation.clone();
    element! {
        View(width: 20, height: 3) {
            TextInput(
                value: text.to_string(),
                has_focus: true,
                multiline: true,
                on_change: move |value: String| {
                    *observation.text.lock().unwrap() = value.clone();
                    text.set(value);
                },
            )
        }
    }
}

#[test]
fn editor_accepts_input_while_output_cannot_progress() {
    smol::block_on(async {
        let observation = Observation::default();
        let input_observation = observation.clone();
        let events = stream::unfold(0, move |index| {
            let observation = input_observation.clone();
            async move {
                smol::Timer::after(Duration::from_millis(20)).await;
                let code = match index {
                    0 => KeyCode::End,
                    1 => KeyCode::Char('!'),
                    _ => {
                        assert_eq!(*observation.text.lock().unwrap(), "中文 draft!");
                        observation.writable.store(true, Ordering::Release);
                        KeyCode::Esc
                    }
                };
                Some((
                    Ok(TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code))),
                    index + 1,
                ))
            }
        });
        element!(Editor(observation: observation.clone()))
            .render_loop()
            .stdout(SlowOutput(observation.clone()))
            .input_stream(events, false)
            .await
            .expect("output backpressure preserves editing and normal finish");
        let committed = observation.bytes.lock().unwrap().clone();
        assert!(!committed.is_empty());
        smol::Timer::after(Duration::from_millis(30)).await;
        assert_eq!(*observation.bytes.lock().unwrap(), committed);
    });
}

#[derive(Debug, Default)]
struct ImageSource(ImageScope);

impl CanvasImageSource for ImageSource {
    fn encoded_png(&self) -> &str {
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aFioAAAAASUVORK5CYII="
    }
    fn scope(&self) -> &ImageScope {
        &self.0
    }
}

#[test]
fn image_cells_preserve_coordinates_through_clipping_blitting_and_clearing() {
    let source = Arc::new(ImageSource::default());
    let image = CanvasImage::new(source, 4, 3).unwrap();
    let mut canvas = Canvas::new(5, 4);
    canvas
        .subview_mut(0, 0, 0, 0, 5, 4)
        .set_image(-2, -1, &image);
    let cell = canvas.cell(0, 0).unwrap().image.as_ref().unwrap();
    assert_eq!((cell.column, cell.row), (2, 1));
    let cropped = canvas.copy_region(0, 0, 2, 2);
    let mut destination = Canvas::new(6, 5);
    destination
        .subview_mut(0, 0, 0, 0, 6, 5)
        .blit_region_from(&cropped, 3, 2, 0, 0, 2, 2);
    let cell = destination.cell(4, 3).unwrap().image.as_ref().unwrap();
    assert_eq!((cell.column, cell.row), (3, 2));
    destination.clear_region(4, 3, 1, 1);
    assert!(destination.cell(4, 3).unwrap().image.is_none());
    destination
        .subview_mut(0, 0, 0, 0, 6, 5)
        .set_text(3, 2, "x", CanvasTextStyle::default());
    assert!(destination.cell(3, 2).unwrap().image.is_none());
}

struct OrderedClipboard {
    calls: Arc<Mutex<Vec<String>>>,
    release: Mutex<Option<futures::channel::oneshot::Receiver<()>>>,
}

impl ClipboardBackend for OrderedClipboard {
    fn execute(
        &self,
        _: &str,
        _: &[&str],
        input: &str,
        _: Duration,
    ) -> futures::future::BoxFuture<'static, i32> {
        self.calls.lock().unwrap().push(input.to_owned());
        let release = self.release.lock().unwrap().take();
        Box::pin(async move {
            if let Some(release) = release {
                release.await.unwrap();
            }
            0
        })
    }

    fn is_kitty(&self) -> bool {
        false
    }
}

#[component]
fn CopyAndExit(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (stdout, _) = hooks.use_output();
    if let Some(observation) = hooks.try_use_context::<Observation>() {
        *observation.stdout.lock().unwrap() = Some(stdout.clone());
    }
    hooks.use_effect(
        move || {
            stdout.set_clipboard("A");
            stdout.set_clipboard("B");
        },
        (),
    );
    hooks.use_context_mut::<SystemContext>().exit();
    element!(View)
}

#[test]
fn finish_waits_for_copies_in_acceptance_order() {
    use futures::FutureExt;
    smol::block_on(async {
        let (release, receiver) = futures::channel::oneshot::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let service = Clipboard::new(Arc::new(OrderedClipboard {
            calls: calls.clone(),
            release: Mutex::new(Some(receiver)),
        }))
        .with_native_test_environment();
        let observation = Observation::default();
        observation.writable.store(true, Ordering::Release);
        let mut app = element!(ContextProvider(value: iocraft::Context::owned(service)) {
            ContextProvider(value: iocraft::Context::owned(observation.clone())) { CopyAndExit }
        });
        let mut render = Box::pin(app.render_loop().stdout(SlowOutput(observation.clone())));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while calls.lock().unwrap().is_empty() {
            assert!(
                render.as_mut().now_or_never().is_none(),
                "finish must await the accepted helper"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "clipboard helper did not start"
            );
            smol::Timer::after(Duration::from_millis(1)).await;
        }
        assert_eq!(*calls.lock().unwrap(), ["A"]);
        assert!(render.as_mut().now_or_never().is_none());
        let stdout = observation.stdout.lock().unwrap().clone().unwrap();
        let late = stdout.set_clipboard_and_wait("late");
        assert_eq!(
            late.now_or_never()
                .expect("closed admission rejects immediately")
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        release.send(()).unwrap();
        render.await.unwrap();
        assert_eq!(*calls.lock().unwrap(), ["A", "B"]);
        let bytes = observation.bytes.lock().unwrap().clone();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let first = text.find("\x1b]52;c;QQ==\x07").unwrap();
        let second = text.find("\x1b]52;c;Qg==\x07").unwrap();
        assert!(first < second);
        smol::Timer::after(Duration::from_millis(30)).await;
        assert_eq!(*observation.bytes.lock().unwrap(), bytes);
    });
}
