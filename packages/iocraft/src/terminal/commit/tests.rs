use super::*;
use crate::prelude::*;
use futures::FutureExt;

#[derive(Clone, Default)]
struct Capture {
    bytes: Arc<Mutex<Vec<u8>>>,
    blocked: Arc<AtomicBool>,
}

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
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
        replay: None,
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
fn handoff_stops_retirement_until_reacquired() {
    smol::block_on(async {
        let capture = Capture::default();
        let mut output = session(capture.clone());
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
