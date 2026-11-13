use super::{SelectionClipboardPath, SelectionContext, UseContext};
use crate::terminal::OutputMessage as Message;
use crate::{
    Canvas, Clipboard, ClipboardMultiplexer, ComponentUpdater, Hook, Hooks, SelectionController,
    SelectionState,
};
use core::{
    pin::Pin,
    task::{Context, Poll, Waker},
};
use futures::{channel::oneshot, future::BoxFuture};
use std::io;
use std::sync::{Arc, Mutex};

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// `UseOutput` is a hook that allows you to write to stdout and stderr from a component. The
/// output will be appended to stdout or stderr, above the rendered component output.
///
/// Both `print` and `println` methods are available for writing output with or without newlines.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # use std::time::Duration;
/// #[component]
/// fn Example(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
///     let (stdout, stderr) = hooks.use_output();
///
///     hooks.use_future(async move {
///         stdout.println("Hello from iocraft to stdout!");
///         stderr.println("  And hello to stderr too!");
///
///         stdout.print("Working...");
///         for _ in 0..5 {
///             smol::Timer::after(Duration::from_secs(1)).await;
///             stdout.print(".");
///         }
///         stdout.println("\nDone!");
///     });
///
///     element! {
///         View(border_style: BorderStyle::Round, border_color: Color::Green) {
///             Text(content: "Hello, use_output!")
///         }
///     }
/// }
/// ```
pub trait UseOutput: private::Sealed {
    /// Gets handles which can be used to write to stdout and stderr.
    fn use_output(&mut self) -> (StdoutHandle, StderrHandle);
}

impl UseOutput for Hooks<'_, '_> {
    fn use_output(&mut self) -> (StdoutHandle, StderrHandle) {
        let clipboard = self
            .try_use_context::<Clipboard>()
            .map(|value| value.clone());
        let session = self
            .try_use_context::<crate::terminal::OutputSender>()
            .map(|context| context.clone());
        let output = self.use_hook(UseOutputImpl::default);
        output
            .state
            .lock()
            .expect("output handle is not poisoned")
            .session = session;
        output.clipboard = clipboard;
        (output.use_stdout(), output.use_stderr())
    }
}

#[derive(Default)]
struct UseOutputState {
    session: Option<crate::terminal::OutputSender>,
    closed: bool,
    changed: bool,
    queue: Vec<Message>,
    waker: Option<Waker>,
}

fn enqueue(state: &Mutex<UseOutputState>, message: Message) {
    let waker = {
        let mut state = state.lock().unwrap();
        if state.closed {
            return;
        }
        if let Some(session) = state.session.clone() {
            state.changed = true;
            let waker = state.waker.take();
            drop(state);
            session.submit(message);
            if let Some(waker) = waker {
                waker.wake();
            }
            return;
        }
        state.queue.push(message);
        state.waker.take()
    };
    if let Some(waker) = waker {
        waker.wake();
    }
}

#[derive(Default)]
struct OutputBatch {
    queue: Vec<Message>,
}

impl OutputBatch {
    fn exec(&mut self, updater: &mut ComponentUpdater) -> io::Result<()> {
        if self.queue.is_empty() || updater.terminal_mut().is_none() {
            return Ok(());
        }
        if self.queue.iter().any(Message::affects_visible_output) {
            updater.clear_terminal_output();
        }
        updater
            .terminal_mut()
            .expect("output admission has a terminal")
            .submit_output_batch(std::mem::take(&mut self.queue))
    }
}

/// A handle to write to stdout, obtained from [`UseOutput::use_output`].
#[derive(Clone)]
pub struct StdoutHandle {
    state: Arc<Mutex<UseOutputState>>,
    clipboard: Option<Clipboard>,
}

impl StdoutHandle {
    /// Reads the last complete local frame without retaining its canvas or component tree.
    pub fn committed_frame(&self) -> Option<Arc<crate::CommittedFrame>> {
        let session = self
            .state
            .lock()
            .expect("output handle is not poisoned")
            .session
            .clone()?;
        session.committed_frame()
    }

    /// Appends prepared history above the live frame and restores that frame and its cursor.
    /// The receipt resolves after the complete local write and flush.
    pub fn append_history(
        &self,
        draft: crate::HistoryDraft,
    ) -> BoxFuture<'static, io::Result<crate::CommitReceipt>> {
        let session = self
            .state
            .lock()
            .expect("output handle is not poisoned")
            .session
            .clone();
        match session {
            Some(session) => session.append_history_with_errors(draft, true),
            None => Box::pin(async {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "history output requires a terminal session",
                ))
            }),
        }
    }

    pub(crate) fn without_clipboard(mut self) -> Self {
        self.clipboard = None;
        self
    }

    /// Notification confidence, resolved by the shared clipboard policy.
    pub fn get_clipboard_path(&self) -> SelectionClipboardPath {
        self.clipboard
            .as_ref()
            .map(Clipboard::get_clipboard_path)
            .unwrap_or_default()
    }

    /// Queues raw bytes immediately. Resolves after the terminal writer accepts
    /// them. A bound Clipboard routes to its retained root queue, surviving
    /// child unmount. Unmounting that output owner reports BrokenPipe.
    pub fn write_control_sequence_and_wait<S: ToString>(
        &self,
        sequence: S,
    ) -> BoxFuture<'static, io::Result<()>> {
        let sequence = sequence.to_string();
        if let Some(root) = self.clipboard.as_ref().and_then(Clipboard::output) {
            return root.enqueue_control_sequence_and_wait(sequence);
        }
        self.enqueue_control_sequence_and_wait(sequence)
    }

    fn enqueue_control_sequence_and_wait(
        &self,
        sequence: String,
    ) -> BoxFuture<'static, io::Result<()>> {
        let (sender, receiver) = oneshot::channel();
        enqueue(
            &self.state,
            Message::StdoutControlAcknowledged(sequence, sender),
        );
        Box::pin(async move {
            receiver.await.unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal output owner was unmounted",
                ))
            })
        })
    }

    /// Queues a message to be written asynchronously to stdout, above the rendered component
    /// output.
    pub fn println<S: ToString>(&self, msg: S) {
        enqueue(&self.state, Message::Stdout(msg.to_string()));
    }

    /// Queues a message to be written asynchronously to stdout without a newline, above the
    /// rendered component output.
    pub fn print<S: ToString>(&self, msg: S) {
        let msg = msg.to_string();
        if msg.is_empty() {
            return;
        }
        enqueue(&self.state, Message::StdoutNoNewline(msg));
    }

    /// Queues a raw terminal control sequence to stdout.
    ///
    /// Unlike [`StdoutHandle::print`], this is non-visual output: it does not
    /// clear or reposition the retained render canvas. It is intended for OSC
    /// notifications, terminal progress, and other side-band terminal controls.
    pub fn write_control_sequence<S: ToString>(&self, sequence: S) {
        let sequence = sequence.to_string();
        if sequence.is_empty() {
            return;
        }
        enqueue(&self.state, Message::StdoutControl(sequence));
    }

    /// Sets the window title in output order, preserving the prior terminal title.
    /// The output owner restores it before handoff and normal finish.
    pub fn set_window_title(&self, title: impl Into<String>) {
        enqueue(&self.state, Message::WindowTitle(title.into()));
    }

    /// Queues an OSC 52 clipboard write to stdout.
    ///
    /// Unlike [`StdoutHandle::print`], this is a non-visual terminal control
    /// sequence: it does not clear or reposition the retained render canvas.
    /// This makes it suitable for fullscreen text-selection copy behavior.
    pub fn set_clipboard<S: ToString>(&self, text: S) {
        drop(self.set_clipboard_and_wait(text));
    }

    /// Accepts an ordered copy before starting any helper. Completes after helpers and
    /// terminal output have finished; component unmount does not cancel an accepted root copy.
    pub fn set_clipboard_and_wait<S: ToString>(
        &self,
        text: S,
    ) -> BoxFuture<'static, io::Result<()>> {
        let (sender, receiver) = oneshot::channel();
        let root = self
            .clipboard
            .as_ref()
            .and_then(Clipboard::output)
            .unwrap_or(self);
        enqueue(
            &root.state,
            Message::Copy(text.to_string(), self.clipboard.clone(), sender),
        );
        Box::pin(async move {
            receiver.await.unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "terminal output is closed",
                ))
            })
        })
    }

    /// Queues an OSC 52 clipboard write to stdout using an explicit
    /// multiplexer passthrough wrapper.
    pub fn set_clipboard_with_multiplexer<S: ToString>(
        &self,
        text: S,
        multiplexer: ClipboardMultiplexer,
    ) {
        enqueue(
            &self
                .clipboard
                .as_ref()
                .and_then(Clipboard::output)
                .unwrap_or(self)
                .state,
            Message::StdoutClipboard(text.to_string(), multiplexer),
        );
    }

    /// Copies a fullscreen [`SelectionState`] from a [`Canvas`] to the terminal
    /// clipboard without clearing the highlight. Returns the selected text.
    pub fn copy_selection_no_clear(&self, selection: &SelectionState, canvas: &Canvas) -> String {
        if !selection.has_selection() {
            return String::new();
        }
        let text = selection.selected_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Copies a fullscreen [`SelectionState`] from a [`Canvas`] to the terminal
    /// clipboard and clears the selection. Returns the selected text.
    pub fn copy_selection(&self, selection: &mut SelectionState, canvas: &Canvas) -> String {
        if !selection.has_selection() {
            return String::new();
        }
        let text = self.copy_selection_no_clear(selection, canvas);
        selection.clear();
        text
    }

    /// Copies from an app-level [`SelectionContext`] without clearing the
    /// highlight. Returns the selected text.
    ///
    /// This is the public app-level counterpart to CC Ink's
    /// `useSelection().copySelectionNoClear()`: the selection owner stays in a
    /// shared context while clipboard transport remains on `StdoutHandle`.
    pub fn copy_selection_context_no_clear(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
    ) -> String {
        let text = selection.copy_selection_no_clear_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Copies from an app-level [`SelectionContext`] and clears the highlight.
    /// Returns the selected text.
    pub fn copy_selection_context(&self, selection: &SelectionContext, canvas: &Canvas) -> String {
        let text = selection.copy_selection_text(canvas);
        if !text.is_empty() {
            self.set_clipboard(&text);
        }
        text
    }

    /// Runs CC Ink-style copy-on-select for an app-level [`SelectionContext`].
    pub fn copy_on_select_context(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard(&text);
        Some(text)
    }

    /// Runs app-level copy-on-select with an explicit multiplexer passthrough wrapper.
    pub fn copy_on_select_context_with_multiplexer(
        &self,
        selection: &SelectionContext,
        canvas: &Canvas,
        multiplexer: ClipboardMultiplexer,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard_with_multiplexer(&text, multiplexer);
        Some(text)
    }

    /// Runs CC Ink-style copy-on-select for a [`SelectionController`].
    ///
    /// When a selection has just settled, this queues an OSC 52 clipboard write
    /// and returns the copied text. Repeated calls for the same settled
    /// selection return `None` until a new drag/selection resets the controller's
    /// copy-on-select guard.
    pub fn copy_on_select(
        &self,
        selection: &mut SelectionController,
        canvas: &Canvas,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard(&text);
        Some(text)
    }

    /// Runs copy-on-select with an explicit multiplexer passthrough wrapper.
    pub fn copy_on_select_with_multiplexer(
        &self,
        selection: &mut SelectionController,
        canvas: &Canvas,
        multiplexer: ClipboardMultiplexer,
    ) -> Option<String> {
        let text = selection.copy_on_select_text(canvas)?;
        self.set_clipboard_with_multiplexer(&text, multiplexer);
        Some(text)
    }
}

/// A handle to write to stderr, obtained from [`UseOutput::use_output`].
#[derive(Clone)]
pub struct StderrHandle {
    state: Arc<Mutex<UseOutputState>>,
}

impl StderrHandle {
    /// Queues a message to be written asynchronously to stderr, above the rendered component
    /// output.
    pub fn println<S: ToString>(&self, msg: S) {
        enqueue(&self.state, Message::Stderr(msg.to_string()));
    }

    /// Queues a message to be written asynchronously to stderr without a newline, above the
    /// rendered component output.
    pub fn print<S: ToString>(&self, msg: S) {
        let msg = msg.to_string();
        if msg.is_empty() {
            return;
        }
        enqueue(&self.state, Message::StderrNoNewline(msg));
    }
}

#[derive(Default)]
struct UseOutputImpl {
    state: Arc<Mutex<UseOutputState>>,
    clipboard: Option<Clipboard>,
    pending: OutputBatch,
}

impl Drop for UseOutputImpl {
    fn drop(&mut self) {
        let queue = {
            let mut state = self.state.lock().unwrap();
            state.closed = true;
            std::mem::take(&mut state.queue)
        };
        // Dropping acknowledgements can wake consumers that enqueue output.
        drop(queue);
    }
}

impl Hook for UseOutputImpl {
    fn poll_change(self: Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        let waker = cx.waker().clone();
        let mut state = self.state.lock().unwrap();
        // Register-then-check (see UseState::poll_change): the Ready path
        // must stay armed so output queued after this frame's drain still
        // wakes the component under push-wake harvesting.
        let old_waker = state.waker.replace(waker);
        let result = if state.queue.is_empty() && !state.changed {
            Poll::Pending
        } else {
            Poll::Ready(())
        };
        drop(state);
        drop(old_waker);
        result
    }

    // Drain after every hook's `post_component_update`, so output queued by
    // effects (`use_effect`, or hooks built on it) is written in the same pass.
    // CC Ink's `writeRaw` is synchronous inside the effect; draining earlier
    // left anything queued by a later hook for the next pass, and the render
    // loop exits before that pass when the same render called `exit()`.
    fn post_component_effects(&mut self, updater: &mut ComponentUpdater) {
        {
            let mut state = self.state.lock().unwrap();
            state.changed = false;
            if state.queue.is_empty() || updater.terminal_mut().is_none() {
                return;
            }
            std::mem::swap(&mut self.pending.queue, &mut state.queue);
        }
        // Callbacks enqueue into state.queue for the next pass. Keep partial-line
        // bookkeeping with the output owner across batches.
        let result = self.pending.exec(updater);
        updater.record_terminal_result(result);
    }
}

impl UseOutputImpl {
    pub fn use_stdout(&mut self) -> StdoutHandle {
        StdoutHandle {
            state: self.state.clone(),
            clipboard: self.clipboard.clone(),
        }
    }

    pub fn use_stderr(&mut self) -> StderrHandle {
        StderrHandle {
            state: self.state.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::{StreamExt, task::noop_waker};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[test]
    fn test_formatting_and_waking_can_enqueue_output() {
        struct NestedMessage(StdoutHandle);
        impl std::fmt::Display for NestedMessage {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.println("inner");
                f.write_str("outer")
            }
        }
        struct WriteOnWake(StdoutHandle);
        impl std::task::Wake for WriteOnWake {
            fn wake(self: Arc<Self>) {
                self.0.println("wake");
            }
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut output = UseOutputImpl::default();
            let stdout = output.use_stdout();
            let waker = Waker::from(Arc::new(WriteOnWake(stdout.clone())));
            assert!(
                Pin::new(&mut output)
                    .poll_change(&mut core::task::Context::from_waker(&waker))
                    .is_pending()
            );
            stdout.println(NestedMessage(stdout.clone()));
            let messages: Vec<_> = output
                .state
                .lock()
                .unwrap()
                .queue
                .iter()
                .map(|message| match message {
                    Message::Stdout(content) => content.clone(),
                    _ => panic!("expected stdout message"),
                })
                .collect();
            sender.send(messages).unwrap();
        });
        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("reentrant output must not deadlock"),
            ["inner", "wake", "outer"]
        );
        worker.join().unwrap();
    }

    #[cfg(feature = "unstable-output-streams")]
    #[test]
    fn backend_output_accepts_reentrant_messages_in_order() {
        #[derive(Default)]
        struct Trace {
            stdout: Option<StdoutHandle>,
            updates: usize,
            writes: Vec<(usize, String)>,
            bytes: Vec<u8>,
        }

        struct ReentrantWriter(Arc<Mutex<Trace>>);
        impl io::Write for ReentrantWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let nested = {
                    let mut trace = self.0.lock().unwrap();
                    trace.bytes.extend_from_slice(bytes);
                    if bytes == b"outer" || bytes == b"nested" {
                        let update = trace.updates;
                        trace
                            .writes
                            .push((update, String::from_utf8(bytes.to_vec()).unwrap()));
                    }
                    (bytes == b"outer").then(|| trace.stdout.as_ref().unwrap().clone())
                };
                if let Some(stdout) = nested {
                    stdout.print("nested");
                }
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        #[component]
        fn FlushingComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let trace = hooks.use_context::<Arc<Mutex<Trace>>>().clone();
            let (stdout, _) = hooks.use_output();
            let updates = {
                let mut trace = trace.lock().unwrap();
                trace.stdout = Some(stdout.clone());
                trace.updates += 1;
                trace.updates
            };
            hooks.use_effect(move || stdout.print("outer"), ());
            if updates == 2 {
                hooks.use_context_mut::<SystemContext>().exit();
            }
            element!(View)
        }

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            for push_wake in [true, false] {
                crate::render::wake::set_push_wake_for_tests(Some(push_wake));
                let trace = Arc::new(Mutex::new(Trace::default()));
                smol::block_on(
                    element! {
                        ContextProvider(value: crate::Context::owned(trace.clone())) {
                            FlushingComponent
                        }
                    }
                    .render_loop()
                    .stdout(ReentrantWriter(trace.clone())),
                )
                .unwrap();
                crate::render::wake::set_push_wake_for_tests(None);
                let trace = trace.lock().unwrap();
                assert_eq!(
                    trace
                        .writes
                        .iter()
                        .map(|(_, text)| text.as_str())
                        .collect::<Vec<_>>(),
                    ["outer", "nested"]
                );
                assert!(
                    trace
                        .bytes
                        .windows(b"\x1b[1A\x1b[5C".len())
                        .any(|bytes| bytes == b"\x1b[1A\x1b[5C"),
                    "the next batch must resume after the previous partial line"
                );
            }
            sender.send(()).unwrap();
        });
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("writer output must not deadlock");
        worker.join().unwrap();
    }

    /// Queues side-band output from an effect and exits in the same (first)
    /// render. `use_effect` is declared after `use_output`, so the effect runs
    /// after that hook's `post_component_update`.
    #[component]
    fn EffectWritesThenExits(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (stdout, _) = hooks.use_output();
        hooks.use_effect(move || stdout.write_control_sequence("\x07"), ());
        system.exit();
        element!(Text(content: "bye"))
    }

    #[apply(test!)]
    async fn output_queued_by_effect_is_written_before_exit() {
        // CC Ink's `writeRaw` inside `useEffect` is synchronous, so a bell (or
        // any output) from the final commit reaches the terminal. Draining only
        // in `post_component_update` dropped it: the loop exited before the
        // next pass that would have flushed the queue.
        let (control_tx, control_rx) = futures::channel::mpsc::unbounded();
        let mut app = element!(EffectWritesThenExits);
        let mut frames = app
            .mock_terminal_render_loop(MockTerminalConfig::default().with_control_sink(control_tx));
        while frames.next().await.is_some() {}
        drop(frames);
        let writes: Vec<String> = control_rx.collect().await;
        assert_eq!(writes, vec!["\x07".to_string()]);
    }

    #[test]
    fn test_use_output_polling() {
        let mut use_output = UseOutputImpl::default();
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        // Empty no-newline prints are true no-ops: they must not wake the
        // render loop or clear the retained live canvas. This guards against
        // using `print("")` as a repaint workaround, which would otherwise call
        // clear_terminal_output() during UseOutputState::exec().
        let mut no_op_output = UseOutputImpl::default();
        let no_op_stdout = no_op_output.use_stdout();
        let no_op_stderr = no_op_output.use_stderr();
        no_op_stdout.print("");
        no_op_stderr.print("");
        assert_eq!(
            Pin::new(&mut no_op_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        let mut disabled_context_output = UseOutputImpl::default();
        let disabled_context_stdout = disabled_context_output.use_stdout();
        let disabled_context_canvas = Canvas::new(4, 1);
        assert_eq!(
            disabled_context_stdout.copy_selection_context_no_clear(
                &SelectionContext::disabled(),
                &disabled_context_canvas,
            ),
            ""
        );
        assert_eq!(
            Pin::new(&mut disabled_context_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending
        );

        let mut whitespace_output = UseOutputImpl::default();
        let whitespace_stdout = whitespace_output.use_stdout();
        let mut whitespace_canvas = Canvas::new(8, 1);
        whitespace_canvas.subview_mut(0, 0, 0, 0, 8, 1).set_text(
            0,
            0,
            "a   b",
            CanvasTextStyle::default(),
        );
        let mut whitespace_controller = SelectionController::new();
        whitespace_controller.selection_mut().start(1, 0);
        whitespace_controller.selection_mut().update(3, 0);
        whitespace_controller.selection_mut().finish();
        assert_eq!(
            whitespace_stdout.copy_on_select(&mut whitespace_controller, &whitespace_canvas),
            None
        );
        assert_eq!(
            Pin::new(&mut whitespace_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Pending,
            "whitespace-only copy-on-select should not queue an OSC 52 clipboard write"
        );

        let stdout = use_output.use_stdout();
        stdout.set_clipboard("copy");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        let mut canvas = Canvas::new(8, 1);
        canvas
            .subview_mut(0, 0, 0, 0, 8, 1)
            .set_text(0, 0, "copy", CanvasTextStyle::default());
        let mut selection = SelectionState::new();
        selection.start(1, 0);
        selection.update(3, 0);
        assert_eq!(stdout.copy_selection_no_clear(&selection, &canvas), "opy");
        assert!(selection.has_selection());
        assert_eq!(stdout.copy_selection(&mut selection, &canvas), "opy");
        assert!(!selection.has_selection());

        let mut controller = SelectionController::new();
        controller.selection_mut().start(1, 0);
        controller.selection_mut().update(3, 0);
        controller.selection_mut().finish();
        assert_eq!(
            stdout.copy_on_select(&mut controller, &canvas).as_deref(),
            Some("opy")
        );
        assert_eq!(stdout.copy_on_select(&mut controller, &canvas), None);
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        stdout.println("Hello, world!");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        let stderr = use_output.use_stderr();
        stderr.println("Hello, error!");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );

        // Test print methods
        stdout.print("Hello, ");
        stdout.print("world!");
        stderr.print("Error: ");
        stderr.print("test");
        stderr.print("Warning: ");
        stderr.print("print test");
        assert_eq!(
            Pin::new(&mut use_output)
                .poll_change(&mut core::task::Context::from_waker(&noop_waker())),
            Poll::Ready(())
        );
    }

    #[component]
    fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (stdout, stderr) = hooks.use_output();
        stdout.println("Hello, world!");
        stderr.println("Hello, error!");
        stdout.print("Testing ");
        stdout.print("print ");
        stdout.println("method!");
        stderr.print("Error: ");
        stderr.println("test");
        stderr.print("Warning: ");
        stderr.println("print test");
        system.exit();
        element!(View)
    }

    #[apply(test!)]
    async fn test_use_output() {
        element!(MyComponent).render_loop().await.unwrap();
    }
    #[test]
    fn control_ack_is_pending_until_write_and_closes_when_owner_unmounts() {
        use futures::FutureExt;
        let mut owner = UseOutputImpl::default();
        let stdout = owner.use_stdout();
        let mut ack = stdout.write_control_sequence_and_wait("\x1b]52;c;QQ==\x07");
        assert!((&mut ack).now_or_never().is_none());
        assert!(!owner.state.lock().unwrap().queue[0].affects_visible_output());
        drop(owner);
        assert_eq!(
            futures::executor::block_on(ack).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            futures::executor::block_on(stdout.write_control_sequence_and_wait("late"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[cfg(feature = "unstable-output-streams")]
    #[derive(Clone)]
    struct AckProof {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_control: bool,
    }
    #[cfg(feature = "unstable-output-streams")]
    impl io::Write for AckProof {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_control && bytes == b"\x1b]52;c;QQ==\x07" {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "controlled writer failure",
                ));
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "unstable-output-streams")]
    #[component]
    fn AckApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let proof = hooks.use_context::<AckProof>().clone();
        let (stdout, _) = hooks.use_output();
        let mut done = hooks.use_state(|| false);
        hooks.use_future(async move {
            let result = stdout
                .write_control_sequence_and_wait("\x1b]52;c;QQ==\x07")
                .await;
            if proof.fail_control {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            } else {
                result.unwrap();
                assert!(
                    proof
                        .bytes
                        .lock()
                        .unwrap()
                        .windows(b"\x1b]52;c;QQ==\x07".len())
                        .any(|w| w == b"\x1b]52;c;QQ==\x07")
                );
            }
            done.set(true);
        });
        if done.get() {
            hooks.use_context_mut::<SystemContext>().exit();
        }
        element!(View)
    }

    #[cfg(feature = "unstable-output-streams")]
    #[apply(test!)]
    async fn control_ack_observes_real_writer_bytes_and_failures() {
        for fail_control in [false, true] {
            let proof = AckProof {
                bytes: Arc::new(Mutex::new(Vec::new())),
                fail_control,
            };
            let result = element! {
                ContextProvider(value: crate::Context::owned(proof.clone())) { AckApp }
            }
            .render_loop()
            .stdout(proof)
            .await;
            if fail_control {
                assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            } else {
                result.unwrap();
            }
        }
    }
    struct NoProcessBackend;
    impl crate::ClipboardBackend for NoProcessBackend {
        fn execute(
            &self,
            _: &str,
            _: &[&str],
            _: &str,
            _: std::time::Duration,
        ) -> BoxFuture<'static, i32> {
            panic!("explicit multiplexer override must not invoke native or tmux processes")
        }
        fn is_kitty(&self) -> bool {
            true
        }
    }

    #[test]
    fn explicit_clipboard_wrapper_bypasses_provider_policy_and_fallback_remains_osc52() {
        let mut owner = UseOutputImpl::default();
        owner.clipboard = Some(Clipboard::new(Arc::new(NoProcessBackend)));
        let stdout = owner.use_stdout();
        stdout.set_clipboard_with_multiplexer("A", ClipboardMultiplexer::Screen);
        assert!(
            matches!(&owner.state.lock().unwrap().queue[0], Message::StdoutClipboard(text, ClipboardMultiplexer::Screen) if text == "A")
        );
        owner.clipboard = None;
        let plain = owner.use_stdout();
        plain.set_clipboard("A");
        assert!(
            matches!(&owner.state.lock().unwrap().queue[1], Message::Copy(text, None, _) if text == "A")
        );
        assert_eq!(plain.get_clipboard_path(), SelectionClipboardPath::Osc52);
    }

    #[component]
    fn ClipboardContextApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (stdout, _) = hooks.use_output();
        assert!(
            stdout.clipboard.is_some(),
            "output hook must capture its nearest Clipboard provider"
        );
        stdout.set_clipboard_with_multiplexer("A", ClipboardMultiplexer::Screen);
        hooks.use_context_mut::<SystemContext>().exit();
        element!(View)
    }

    #[apply(test!)]
    async fn clipboard_context_is_inherited_by_the_real_output_hook() {
        element! {
            ContextProvider(value: crate::Context::owned(Clipboard::new(Arc::new(NoProcessBackend)))) {
                ClipboardContextApp
            }
        }.render_loop().await.unwrap();
    }
    #[test]
    fn clipboard_root_route_survives_child_drop_and_closes_with_root_without_cycles() {
        use futures::FutureExt;
        let mut root = UseOutputImpl::default();
        let root_stdout = root.use_stdout();
        let service = Clipboard::new(Arc::new(NoProcessBackend)).with_output(root_stdout);
        let mut child = UseOutputImpl::default();
        child.clipboard = Some(service);
        let child_stdout = child.use_stdout();
        // Rebinding a contextual handle strips its service; no recursive route.
        let rebound = Clipboard::new(Arc::new(NoProcessBackend)).with_output(child_stdout.clone());
        assert!(rebound.output().unwrap().clipboard.is_none());
        drop(child);
        let mut ack = child_stdout.write_control_sequence_and_wait("after child unmount");
        assert!((&mut ack).now_or_never().is_none());
        assert_eq!(root.state.lock().unwrap().queue.len(), 1);
        drop(root);
        assert_eq!(
            futures::executor::block_on(ack).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            futures::executor::block_on(
                child_stdout.write_control_sequence_and_wait("after root unmount")
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[cfg(feature = "unstable-output-streams")]
    #[derive(Default)]
    struct DeferredClipboardBackend {
        release: Mutex<Option<oneshot::Receiver<()>>>,
    }
    #[cfg(feature = "unstable-output-streams")]
    impl crate::ClipboardBackend for DeferredClipboardBackend {
        fn execute(
            &self,
            _: &str,
            _: &[&str],
            _: &str,
            _: std::time::Duration,
        ) -> BoxFuture<'static, i32> {
            let release = self.release.lock().unwrap().take();
            Box::pin(async move {
                if let Some(release) = release {
                    release.await.unwrap();
                }
                0
            })
        }
        fn is_kitty(&self) -> bool {
            true
        }
    }
    #[cfg(feature = "unstable-output-streams")]
    #[derive(Clone)]
    struct LifetimeProof {
        output: AckProof,
        backend: Arc<DeferredClipboardBackend>,
        child_unmounted: Arc<std::sync::atomic::AtomicBool>,
        release: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    }
    #[cfg(feature = "unstable-output-streams")]
    #[derive(Clone)]
    struct ChildControl {
        visible: State<bool>,
        unmounted: Arc<std::sync::atomic::AtomicBool>,
    }
    #[cfg(feature = "unstable-output-streams")]
    struct MarkChildUnmount(Arc<std::sync::atomic::AtomicBool>);
    #[cfg(feature = "unstable-output-streams")]
    impl Hook for MarkChildUnmount {}
    #[cfg(feature = "unstable-output-streams")]
    impl Drop for MarkChildUnmount {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[cfg(feature = "unstable-output-streams")]
    #[component]
    fn ClipboardTransientChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let control = hooks.use_context::<ChildControl>().clone();
        let unmounted = control.unmounted.clone();
        hooks.use_hook(move || MarkChildUnmount(unmounted));
        let (stdout, _) = hooks.use_output();
        hooks.use_future(async move {
            stdout.set_clipboard("A");
            let mut visible = control.visible;
            visible.set(false);
        });
        element!(View)
    }
    #[cfg(feature = "unstable-output-streams")]
    #[component]
    fn ClipboardRetainedRoot(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let proof = hooks.use_context::<LifetimeProof>().clone();
        let (stdout, _) = hooks.use_output();
        let clipboard = hooks
            .use_state(|| Clipboard::new(proof.backend.clone()).with_output(stdout))
            .read()
            .clone();
        let visible = hooks.use_state(|| true);
        let mut done = hooks.use_state(|| false);
        let child_control = ChildControl {
            visible,
            unmounted: proof.child_unmounted.clone(),
        };
        hooks.use_future(async move {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !proof
                .child_unmounted
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "child did not unmount"
                );
                smol::Timer::after(std::time::Duration::from_millis(1)).await;
            }
            proof
                .release
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            while !proof
                .output
                .bytes
                .lock()
                .unwrap()
                .windows(b"52;c;QQ==".len())
                .any(|bytes| bytes == b"52;c;QQ==")
            {
                assert!(
                    std::time::Instant::now() < deadline,
                    "accepted clipboard did not finish"
                );
                smol::Timer::after(std::time::Duration::from_millis(1)).await;
            }
            assert!(
                proof
                    .output
                    .bytes
                    .lock()
                    .unwrap()
                    .windows(b"52;c;QQ==".len())
                    .any(|w| w == b"52;c;QQ==")
            );
            done.set(true);
        });
        if done.get() {
            hooks.use_context_mut::<SystemContext>().exit();
        }
        element! {
            ContextProvider(value: crate::Context::owned(clipboard)) {
                ContextProvider(value: crate::Context::owned(child_control)) {
                    #(if visible.get() { Some(element!(ClipboardTransientChild)) } else { None })
                }
            }
        }
    }
    #[cfg(feature = "unstable-output-streams")]
    #[apply(test!)]
    async fn accepted_clipboard_survives_child_unmount() {
        let output = AckProof {
            bytes: Arc::new(Mutex::new(Vec::new())),
            fail_control: false,
        };
        let (release, receiver) = oneshot::channel();
        let proof = LifetimeProof {
            release: Arc::new(Mutex::new(Some(release))),
            output: output.clone(),
            backend: Arc::new(DeferredClipboardBackend {
                release: Mutex::new(Some(receiver)),
            }),
            child_unmounted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        element! {
            ContextProvider(value: crate::Context::owned(proof)) { ClipboardRetainedRoot }
        }
        .render_loop()
        .stdout(output)
        .await
        .unwrap();
    }
    #[component]
    fn MixedCopyChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (stdout, _) = hooks.use_output();
        hooks.use_effect(
            move || {
                stdout.set_clipboard("A");
                stdout.set_clipboard_with_multiplexer("B", ClipboardMultiplexer::Screen);
            },
            (),
        );
        hooks.use_context_mut::<SystemContext>().exit();
        element!(View)
    }

    #[component]
    fn MixedCopyRoot(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (stdout, _) = hooks.use_output();
        let service = hooks.use_context::<Clipboard>().clone().with_output(stdout);
        element!(ContextProvider(value: crate::Context::owned(service)) { MixedCopyChild })
    }

    #[cfg(feature = "unstable-output-streams")]
    #[apply(test!)]
    async fn mixed_copy_transports_keep_root_acceptance_order() {
        let output = AckProof {
            bytes: Arc::new(Mutex::new(Vec::new())),
            fail_control: false,
        };
        let service = Clipboard::new(Arc::new(DeferredClipboardBackend::default()))
            .with_native_test_environment();
        element!(ContextProvider(value: crate::Context::owned(service)) { MixedCopyRoot })
            .render_loop()
            .stdout(output.clone())
            .await
            .unwrap();
        let bytes = output.bytes.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.find("52;c;QQ==").unwrap() < text.find("52;c;Qg==").unwrap());
    }

    #[cfg(feature = "unstable-output-streams")]
    #[test]
    fn mock_copy_preparation_yields_to_the_calling_executor() {
        let (done, result) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            smol::block_on(async {
                let (release, receiver) = oneshot::channel();
                let service = Clipboard::new(Arc::new(DeferredClipboardBackend {
                    release: Mutex::new(Some(receiver)),
                }))
                .with_native_test_environment();
                let mut app = element!(ContextProvider(value: crate::Context::owned(service)) { MixedCopyRoot });
                let render = async {
                    let mut frames = app.mock_terminal_render_loop(MockTerminalConfig::default());
                    while frames.next().await.is_some() {}
                };
                let helper = async {
                    smol::Timer::after(std::time::Duration::from_millis(10)).await;
                    release.send(()).unwrap();
                };
                futures::join!(render, helper);
            });
            done.send(()).unwrap();
        });
        result
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("mock preparation must release the runtime thread");
        thread.join().unwrap();
    }
}
