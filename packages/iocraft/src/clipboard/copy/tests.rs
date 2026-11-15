use super::*;
use crate::prelude::*;
use futures::FutureExt;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

type Action = Box<dyn FnOnce(StdoutHandle, AppHandle) + Send>;

#[derive(Clone)]
struct Scenario(Arc<Mutex<Option<Action>>>);

impl Scenario {
    fn new(action: impl FnOnce(StdoutHandle, AppHandle) + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Some(Box::new(action)))))
    }
}

#[component]
fn CopyApplication(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let scenario = hooks.use_context::<Scenario>().clone();
    let (stdout, _) = hooks.use_output();
    let application = hooks.use_app();
    hooks.use_effect(
        move || {
            scenario.0.lock().unwrap().take().unwrap()(stdout, application);
        },
        (),
    );
    element!(View)
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(7);
        self.0.lock().unwrap().extend_from_slice(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn cancelled_failed_and_oversized_preparations_publish_no_partial_clipboard() {
    smol::block_on(async {
        let (started, ready) = oneshot::channel();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = requests.clone();
        let scenario = Scenario::new(move |stdout, mut application| {
            let cancelled = stdout.prepare_clipboard(move |mut buffer| async move {
                buffer.push_str("cancelled prefix")?;
                started.send(()).unwrap();
                buffer.cancellation().cancelled().await;
                Ok(buffer)
            });
            let failed = stdout.prepare_clipboard(|mut buffer| async move {
                buffer.push_str("failed prefix")?;
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "cold source failed",
                ))
            });
            let oversized = stdout.prepare_clipboard(|mut buffer| async move {
                loop {
                    buffer.push_str(&"x".repeat(1024))?;
                }
            });
            let valid = stdout.prepare_clipboard(|mut buffer| async move {
                buffer.push_str("valid")?;
                Ok(buffer)
            });
            *accepted.lock().unwrap() = vec![cancelled, failed, oversized, valid];
            application.exit();
        });
        let output = Capture::default();
        let mut tree =
            element!(ContextProvider(value: crate::Context::owned(scenario)) { CopyApplication });
        let render = tree
            .render_loop()
            .stdout(output.clone())
            .output_capacity(OutputCapacity::new(96 * 1024, ()));
        let cancel = async {
            ready.await.unwrap();
            assert!(requests.lock().unwrap()[0].cancellation().cancel());
        };
        let (result, ()) = futures::join!(render, cancel);
        result.unwrap();
        let mut receipts = std::mem::take(&mut *requests.lock().unwrap()).into_iter();
        for expected in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::InvalidData,
            io::ErrorKind::OutOfMemory,
        ] {
            assert_eq!(receipts.next().unwrap().await.unwrap_err().kind(), expected);
        }
        let summary = receipts.next().unwrap().await.unwrap();
        assert_eq!(summary.characters, 5);
        assert_eq!(summary.result_bytes, 1024);
        assert_eq!(summary.helper_bytes, 0);
        assert_eq!(summary.protocol_bytes, 4096);
        let bytes = output.0.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.matches("52;c;").count(), 1);
        assert!(text.contains("52;c;dmFsaWQ="));
    });
}

struct Helpers {
    calls: Arc<Mutex<Vec<(String, String)>>>,
    gates: Mutex<BTreeMap<String, oneshot::Receiver<()>>>,
    started: Mutex<Option<oneshot::Sender<()>>>,
}

impl ClipboardBackend for Helpers {
    fn execute(
        &self,
        program: &str,
        _: &[&str],
        input: &str,
        _: Duration,
    ) -> BoxFuture<'static, i32> {
        self.calls
            .lock()
            .unwrap()
            .push((program.into(), input.into()));
        let gate = if program == "pbcopy" {
            self.gates.lock().unwrap().remove(input)
        } else {
            None
        };
        if let Some(started) = self.started.lock().unwrap().take() {
            started.send(()).unwrap();
        }
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.unwrap();
            }
            0
        })
    }
    fn is_kitty(&self) -> bool {
        false
    }
}

#[test]
fn preparation_and_helper_readiness_cannot_reverse_copy_order_or_cross_handoff() {
    smol::block_on(async {
        let (read_first, first_ready) = oneshot::channel();
        let (read_second, second_ready) = oneshot::channel();
        let (helper_first, first_helper_ready) = oneshot::channel();
        let (helper_second, second_helper_ready) = oneshot::channel();
        let (preparing, preparation_started) = oneshot::channel();
        let (helper_started, helper_running) = oneshot::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut clipboard = Clipboard::new(Arc::new(Helpers {
            calls: calls.clone(),
            gates: Mutex::new(BTreeMap::from([
                ("A".into(), first_helper_ready),
                ("B".into(), second_helper_ready),
            ])),
            started: Mutex::new(Some(helper_started)),
        }));
        clipboard.environment = Some(super::super::Environment {
            platform: "macos",
            ssh: false,
            tmux: true,
            iterm: false,
        });
        let output = Capture::default();
        let handed_off = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted = requests.clone();
        let external_output = output.clone();
        let external_handoff = handed_off.clone();
        let scenario = Scenario::new(move |stdout, mut application| {
            let first = stdout.prepare_clipboard(move |mut buffer| async move {
                preparing.send(()).unwrap();
                first_ready.await.unwrap();
                buffer.push_str("A")?;
                Ok(buffer)
            });
            let second = stdout.prepare_clipboard(move |mut buffer| async move {
                second_ready.await.unwrap();
                buffer.push_str("B")?;
                Ok(buffer)
            });
            *accepted.lock().unwrap() = vec![first, second];
            drop(application.suspend_terminal(move || {
                let late = stdout.prepare_clipboard(|buffer| async { Ok(buffer) });
                assert_eq!(
                    late.now_or_never().unwrap().unwrap_err().kind(),
                    io::ErrorKind::BrokenPipe
                );
                let before = external_output.0.lock().unwrap().clone();
                std::thread::sleep(Duration::from_millis(20));
                assert_eq!(*external_output.0.lock().unwrap(), before);
                external_handoff.store(true, Ordering::Release);
                application.exit();
            }));
        });
        let mut tree = element!(ContextProvider(value: crate::Context::owned(clipboard)) {
            ContextProvider(value: crate::Context::owned(scenario)) { CopyApplication }
        });
        let render = tree.render_loop().stdout(output.clone());
        let release = async {
            preparation_started.await.unwrap();
            read_second.send(()).unwrap();
            helper_second.send(()).unwrap();
            assert!(calls.lock().unwrap().is_empty());
            assert!(!handed_off.load(Ordering::Acquire));
            read_first.send(()).unwrap();
            helper_running.await.unwrap();
            assert_eq!(*calls.lock().unwrap(), [("pbcopy".into(), "A".into())]);
            assert!(!requests.lock().unwrap()[0].cancellation().cancel());
            helper_first.send(()).unwrap();
        };
        let (result, ()) = futures::join!(render, release);
        result.unwrap();
        assert!(handed_off.load(Ordering::Acquire));
        assert_eq!(
            *calls.lock().unwrap(),
            [
                ("pbcopy".into(), "A".into()),
                ("tmux".into(), "A".into()),
                ("pbcopy".into(), "B".into()),
                ("tmux".into(), "B".into()),
            ]
        );
        let receipts = std::mem::take(&mut *requests.lock().unwrap());
        for receipt in receipts {
            assert_eq!(receipt.await.unwrap().characters, 1);
        }
        let bytes = output.0.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.find("52;c;QQ==").unwrap() < text.find("52;c;Qg==").unwrap());
    });
}
