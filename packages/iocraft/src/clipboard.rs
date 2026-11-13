use crate::ansi::MultiplexerPassthrough;
use crate::hooks::{SelectionClipboardPath, StdoutHandle};
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Host capabilities needed by the clipboard service, with no executor dependency.
pub trait ClipboardBackend: Send + Sync + 'static {
    /// Starts the process immediately, returning its eventual exit status.
    /// The output owner retains the future until the helper has exited or timed out.
    fn execute(
        &self,
        program: &str,
        args: &[&str],
        input: &str,
        timeout: Duration,
    ) -> BoxFuture<'static, i32>;
    /// Uses the application's resolved terminal identity.
    fn is_kitty(&self) -> bool;
}

/// Shared clipboard service. Provide one through a `ContextProvider` so all
/// `use_output` handles share the source module's Linux-tool cache.
#[derive(Clone)]
pub struct Clipboard {
    backend: Arc<dyn ClipboardBackend>,
    linux_copy: Arc<Mutex<LinuxCopy>>,
    output: Option<Arc<StdoutHandle>>,
    #[cfg(test)]
    environment: Option<Environment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinuxCopy {
    Unprobed,
    Unavailable,
    WlCopy,
    Xclip,
    Xsel,
}

#[derive(Clone)]
struct Environment {
    platform: &'static str,
    ssh: bool,
    tmux: bool,
    iterm: bool,
}
impl Environment {
    fn current() -> Self {
        let nonempty = |name| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        Self {
            platform: std::env::consts::OS,
            ssh: nonempty("SSH_CONNECTION"),
            tmux: nonempty("TMUX"),
            iterm: std::env::var("LC_TERMINAL").as_deref() == Ok("iTerm2"),
        }
    }
    fn get_clipboard_path(&self) -> SelectionClipboardPath {
        if self.platform == "macos" && !self.ssh {
            SelectionClipboardPath::Native
        } else if self.tmux {
            SelectionClipboardPath::TmuxBuffer
        } else {
            SelectionClipboardPath::Osc52
        }
    }
}

impl Clipboard {
    /// Creates a service whose clones share the native Linux-tool cache.
    pub fn new(backend: Arc<dyn ClipboardBackend>) -> Self {
        Self {
            backend,
            linux_copy: Arc::new(Mutex::new(LinuxCopy::Unprobed)),
            output: None,
            #[cfg(test)]
            environment: None,
        }
    }

    /// Binds clipboard control output to a retained root's existing queue.
    /// The handle's clipboard context is stripped so binding a contextual
    /// handle cannot create a recursive service/output route.
    pub fn with_output(mut self, output: StdoutHandle) -> Self {
        self.output = Some(Arc::new(output.without_clipboard()));
        self
    }

    pub(crate) fn output(&self) -> Option<&StdoutHandle> {
        self.output.as_deref()
    }

    /// Maps to CC `getClipboardPath`: confidence for the notification, not a
    /// promise that the terminal accepted the clipboard sequence.
    pub fn get_clipboard_path(&self) -> SelectionClipboardPath {
        Environment::current().get_clipboard_path()
    }

    #[cfg(test)]
    pub(crate) fn with_native_test_environment(mut self) -> Self {
        self.environment = Some(Environment {
            platform: "macos",
            ssh: false,
            tmux: false,
            iterm: false,
        });
        self
    }

    pub(crate) async fn prepare(&self, text: &str) -> PreparedClipboard {
        #[cfg(test)]
        let environment = self
            .environment
            .clone()
            .unwrap_or_else(Environment::current);
        #[cfg(not(test))]
        let environment = Environment::current();
        self.prepare_with_environment(text, environment).await
    }

    async fn prepare_with_environment(
        &self,
        text: &str,
        environment: Environment,
    ) -> PreparedClipboard {
        if !environment.ssh {
            self.copy_native(text, environment.platform).await;
        }
        let tmux = environment.tmux
            && self
                .backend
                .execute(
                    "tmux",
                    if environment.iterm {
                        &["load-buffer", "-"]
                    } else {
                        &["load-buffer", "-w", "-"]
                    },
                    text,
                    Duration::from_secs(2),
                )
                .await
                == 0;
        PreparedClipboard {
            multiplexer: if tmux {
                MultiplexerPassthrough::Tmux
            } else {
                MultiplexerPassthrough::None
            },
            is_kitty: self.backend.is_kitty(),
        }
    }

    async fn copy_native(&self, text: &str, platform: &str) {
        let timeout = Duration::from_secs(2);
        let cached = *self
            .linux_copy
            .lock()
            .expect("clipboard policy is not poisoned");
        let command: Option<(&str, &[&str])> = match platform {
            "macos" => Some(("pbcopy", &[])),
            "windows" => Some(("clip", &[])),
            "linux" => match cached {
                LinuxCopy::Unavailable | LinuxCopy::Unprobed => None,
                LinuxCopy::WlCopy => Some(("wl-copy", &[])),
                LinuxCopy::Xclip => Some(("xclip", &["-selection", "clipboard"])),
                LinuxCopy::Xsel => Some(("xsel", &["--clipboard", "--input"])),
            },
            _ => None,
        };
        if let Some((program, arguments)) = command {
            self.backend
                .execute(program, arguments, text, timeout)
                .await;
        } else if platform == "linux" && cached == LinuxCopy::Unprobed {
            let winner = if self.backend.execute("wl-copy", &[], text, timeout).await == 0 {
                LinuxCopy::WlCopy
            } else if self
                .backend
                .execute("xclip", &["-selection", "clipboard"], text, timeout)
                .await
                == 0
            {
                LinuxCopy::Xclip
            } else if self
                .backend
                .execute("xsel", &["--clipboard", "--input"], text, timeout)
                .await
                == 0
            {
                LinuxCopy::Xsel
            } else {
                LinuxCopy::Unavailable
            };
            *self
                .linux_copy
                .lock()
                .expect("clipboard policy is not poisoned") = winner;
        }
    }
}

pub(crate) struct PreparedClipboard {
    pub(crate) multiplexer: MultiplexerPassthrough,
    pub(crate) is_kitty: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{FutureExt, channel::oneshot, executor::block_on};
    use std::collections::VecDeque;

    type Call = (String, Vec<String>, String, Duration);
    #[derive(Default)]
    struct Backend {
        calls: Mutex<Vec<Call>>,
        replies: Mutex<VecDeque<BoxFuture<'static, i32>>>,
    }
    impl ClipboardBackend for Backend {
        fn execute(
            &self,
            program: &str,
            arguments: &[&str],
            input: &str,
            timeout: Duration,
        ) -> BoxFuture<'static, i32> {
            self.calls.lock().unwrap().push((
                program.into(),
                arguments
                    .iter()
                    .map(|argument| argument.to_string())
                    .collect(),
                input.into(),
                timeout,
            ));
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Box::pin(async { 0 }))
        }
        fn is_kitty(&self) -> bool {
            true
        }
    }
    impl Backend {
        fn reply(&self, code: i32) {
            self.replies
                .lock()
                .unwrap()
                .push_back(Box::pin(async move { code }));
        }
        fn programs(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|call| call.0.clone())
                .collect()
        }
    }
    fn environment(platform: &'static str, ssh: bool, tmux: bool, iterm: bool) -> Environment {
        Environment {
            platform,
            ssh,
            tmux,
            iterm,
        }
    }
    fn sequence(prepared: PreparedClipboard, text: &str) -> String {
        let mut output = Vec::new();
        crate::ansi::write_clipboard(&mut output, text, prepared.multiplexer, prepared.is_kitty)
            .unwrap();
        String::from_utf8(output).unwrap()
    }

    #[test]
    fn clipboard_preparation_waits_for_native_before_tmux_and_terminal_output() {
        let backend = Arc::new(Backend::default());
        let (release, reply) = oneshot::channel();
        backend
            .replies
            .lock()
            .unwrap()
            .push_back(Box::pin(async move { reply.await.unwrap() }));
        let service = Clipboard::new(backend.clone());
        let mut prepared = Box::pin(
            service.prepare_with_environment("copy", environment("macos", false, true, false)),
        );
        assert!(backend.programs().is_empty());
        assert!(prepared.as_mut().now_or_never().is_none());
        assert_eq!(backend.programs(), ["pbcopy"]);
        release.send(0).unwrap();
        assert_eq!(
            sequence(block_on(prepared), "copy"),
            "\x1bPtmux;\x1b\x1b]52;c;Y29weQ==\x07\x1b\\"
        );
        assert_eq!(backend.programs(), ["pbcopy", "tmux"]);
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls[1].1, ["load-buffer", "-w", "-"]);
        assert!(
            calls
                .iter()
                .all(|call| call.2 == "copy" && call.3 == Duration::from_secs(2))
        );
    }

    #[test]
    fn clipboard_iterm_failure_returns_raw_kitty_sequence_and_ssh_skips_native() {
        let backend = Arc::new(Backend::default());
        backend.reply(1);
        let service = Clipboard::new(backend.clone());
        assert_eq!(
            sequence(
                block_on(
                    service.prepare_with_environment("A", environment("macos", true, true, true))
                ),
                "A"
            ),
            "\x1b]52;c;QQ==\x1b\\"
        );
        assert_eq!(backend.programs(), ["tmux"]);
        assert_eq!(backend.calls.lock().unwrap()[0].1, ["load-buffer", "-"]);
        assert_eq!(
            sequence(
                block_on(
                    service.prepare_with_environment("", environment("macos", true, false, false))
                ),
                ""
            ),
            "\x1b]52;c;\x1b\\"
        );
        assert_eq!(
            environment("macos", false, true, false).get_clipboard_path(),
            SelectionClipboardPath::Native
        );
        assert_eq!(
            environment("linux", false, false, false).get_clipboard_path(),
            SelectionClipboardPath::Osc52
        );
        assert_eq!(
            environment("macos", true, true, false).get_clipboard_path(),
            SelectionClipboardPath::TmuxBuffer
        );
    }

    #[test]
    fn clipboard_linux_probe_order_and_cached_success_or_failure() {
        for (replies, expected, winner) in [
            (vec![0], vec!["wl-copy"], LinuxCopy::WlCopy),
            (vec![1, 0], vec!["wl-copy", "xclip"], LinuxCopy::Xclip),
            (
                vec![1, 1, 0],
                vec!["wl-copy", "xclip", "xsel"],
                LinuxCopy::Xsel,
            ),
            (
                vec![1, 1, 1],
                vec!["wl-copy", "xclip", "xsel"],
                LinuxCopy::Unavailable,
            ),
        ] {
            let backend = Arc::new(Backend::default());
            for reply in replies {
                backend.reply(reply);
            }
            let service = Clipboard::new(backend.clone());
            block_on(
                service.prepare_with_environment("one", environment("linux", false, false, false)),
            );
            assert_eq!(backend.programs(), expected);
            assert_eq!(*service.linux_copy.lock().unwrap(), winner);
            let count = backend.calls.lock().unwrap().len();
            backend.reply(1);
            block_on(
                service.prepare_with_environment("two", environment("linux", false, false, false)),
            );
            assert_eq!(
                backend.calls.lock().unwrap().len(),
                count + usize::from(winner != LinuxCopy::Unavailable)
            );
            assert_eq!(*service.linux_copy.lock().unwrap(), winner);
        }
    }

    #[test]
    fn clipboard_windows_uses_clip_without_shell_or_arguments() {
        let backend = Arc::new(Backend::default());
        let service = Clipboard::new(backend.clone());
        block_on(
            service.prepare_with_environment("text", environment("windows", false, false, false)),
        );
        assert_eq!(backend.programs(), ["clip"]);
        assert!(backend.calls.lock().unwrap()[0].1.is_empty());
    }
}
