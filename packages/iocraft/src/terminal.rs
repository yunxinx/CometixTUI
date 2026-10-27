use crate::{
    canvas::{
        Canvas, CanvasPackedCellPools, CanvasPackedScreen, CanvasStyleTransitionCache,
        SelectionController, SelectionFocusMove, SelectionHoverOutcome, SelectionPressOutcome,
        SelectionReleaseOutcome,
    },
    context::ScrollbackReplay,
    element::Output,
};
use crossterm::{
    ExecutableCommand, QueueableCommand, cursor,
    event::{self, Event, EventStream},
    terminal,
};
use futures::{
    channel::{mpsc, oneshot},
    stream::{self, BoxStream, Stream, StreamExt},
    task::AtomicWaker,
};
use futures_timer::Delay;
use std::{
    collections::VecDeque,
    future::Future,
    io::{self, IsTerminal, Write, stdin},
    mem,
    pin::Pin,
    sync::{Arc, Mutex, Weak},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

// Re-exports for basic types.
pub use crossterm::event::{
    KeyCode, KeyEventKind, KeyEventState, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind,
};

const STDIN_RESUME_GAP: Duration = Duration::from_secs(5);

mod backend;
mod capability;
mod command;
mod events;
mod fullscreen;
mod input;
mod log_update;
mod mock;
mod query;
mod scroll_hint;

#[cfg(test)]
use backend::{
    PANIC_RESTORE_STATE, clear_canvas_inline, register_terminal_for_panic_restore,
    unregister_terminal_for_panic_restore,
};
use backend::{StdTerminal, TerminalImpl};
pub use capability::*;
#[cfg(test)]
use capability::{
    clear_terminal_sequence_with_env, has_cursor_up_viewport_yank_bug_with_env,
    is_synchronized_output_supported_with_env, is_xterm_js_with_env_and_xtversion,
    osc_color_query_sequence_with_env, supports_extended_keys_with_env,
};
use command::{TerminalWriter, write_ansi_command};
pub use events::*;
use events::{EventCellSnapshot, TerminalEventsShared};
pub use fullscreen::*;
pub use input::*;
pub use log_update::*;
use mock::MockTerminal;
pub use mock::MockTerminalConfig;
pub(crate) use mock::MockTerminalOutputStream;
pub use query::*;
pub use scroll_hint::*;

/// Outcome of routing a real fullscreen mouse event through a
/// [`SelectionController`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FullscreenSelectionEventOutcome {
    /// A left press started or changed selection state.
    Press(SelectionPressOutcome),
    /// A left drag updated selection focus/span.
    Drag,
    /// A release finished selection/click/link handling.
    Release(SelectionReleaseOutcome),
    /// No-button motion produced hover/lost-release handling.
    Hover(SelectionHoverOutcome),
    /// A non-left press reset the multi-click chain.
    NonLeftPress,
    /// A non-left release only finishes an active drag.
    NonLeftRelease {
        /// Whether an active drag was finished.
        finished_drag: bool,
    },
    /// A wheel event cleared an active selection before the caller scrolls.
    Wheel {
        /// Whether a selection existed and was cleared.
        cleared_selection: bool,
    },
    /// Event was not relevant to selection/click handling.
    Ignored,
}

/// Outcome of routing a fullscreen key event through selection behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenSelectionKeyOutcome {
    /// No selection was active or the key is not a press event.
    Ignored,
    /// The key intentionally preserves selection but does not otherwise act on
    /// it, such as Shift+PageUp or Option+Arrow navigation.
    Preserved,
    /// Selection was cleared because ordinary input occurred.
    Cleared,
    /// Copy should be performed by the caller. This covers Ctrl+C on legacy
    /// terminals where Ctrl+Shift+C is indistinguishable from Ctrl+C.
    CopyRequested,
    /// Keyboard selection extension moved, or attempted to move, focus.
    FocusMoved {
        /// Semantic movement requested by the key.
        movement: SelectionFocusMove,
        /// Whether the focus endpoint actually changed.
        moved: bool,
    },
}

impl SelectionController {
    pub(crate) fn selection_focus_move_for_key(event: &KeyEvent) -> Option<SelectionFocusMove> {
        if !event.modifiers.contains(KeyModifiers::SHIFT)
            || event
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::META)
        {
            return None;
        }
        match event.code {
            KeyCode::Left => Some(SelectionFocusMove::Left),
            KeyCode::Right => Some(SelectionFocusMove::Right),
            KeyCode::Up => Some(SelectionFocusMove::Up),
            KeyCode::Down => Some(SelectionFocusMove::Down),
            KeyCode::Home => Some(SelectionFocusMove::LineStart),
            KeyCode::End => Some(SelectionFocusMove::LineEnd),
            _ => None,
        }
    }

    fn should_clear_selection_on_key(event: &KeyEvent) -> bool {
        let is_nav = matches!(
            event.code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        if is_nav
            && event.modifiers.intersects(
                KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::META | KeyModifiers::SUPER,
            )
        {
            return false;
        }
        true
    }

    /// Routes a decoded fullscreen key event through CC Ink-compatible
    /// selection behavior.
    ///
    /// When selection exists: Esc clears, Ctrl+C requests copy, Shift+arrow /
    /// Home / End extends focus, ordinary input clears, and modified navigation
    /// keys preserve the highlight. Callers remain responsible for performing
    /// clipboard writes when `CopyRequested` is returned.
    pub fn handle_fullscreen_key_event(
        &mut self,
        event: &KeyEvent,
        width: usize,
        height: usize,
    ) -> FullscreenSelectionKeyOutcome {
        if event.kind != KeyEventKind::Press || !self.has_selection() {
            return FullscreenSelectionKeyOutcome::Ignored;
        }

        if event.code == KeyCode::Esc {
            self.selection_mut().clear();
            return FullscreenSelectionKeyOutcome::Cleared;
        }

        if matches!(event.code, KeyCode::Char('c') | KeyCode::Char('C'))
            && event.modifiers.contains(KeyModifiers::CONTROL)
            && !event
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::META)
        {
            return FullscreenSelectionKeyOutcome::CopyRequested;
        }

        if let Some(movement) = Self::selection_focus_move_for_key(event) {
            let moved = self.selection_mut().move_focus_by(movement, width, height);
            return FullscreenSelectionKeyOutcome::FocusMoved { movement, moved };
        }

        if Self::should_clear_selection_on_key(event) {
            self.selection_mut().clear();
            FullscreenSelectionKeyOutcome::Cleared
        } else {
            FullscreenSelectionKeyOutcome::Preserved
        }
    }

    /// Routes a decoded fullscreen mouse event through the CC Ink-compatible
    /// selection/click/link state machine.
    ///
    /// `now_ms` is used for double/triple-click detection. `click_consumed` is
    /// only consulted for left-button releases: pass whether app-level click
    /// dispatch handled the click so hyperlink fallback can be suppressed.
    pub fn handle_fullscreen_mouse_event(
        &mut self,
        canvas: &Canvas,
        event: &FullscreenMouseEvent,
        now_ms: u64,
        click_consumed: bool,
    ) -> FullscreenSelectionEventOutcome {
        match event.kind {
            MouseEventKind::Down(event::MouseButton::Left) => {
                FullscreenSelectionEventOutcome::Press(self.handle_left_press(
                    canvas,
                    event.column as usize,
                    event.row as usize,
                    now_ms,
                    event.modifiers.contains(KeyModifiers::ALT),
                ))
            }
            MouseEventKind::Down(_) => {
                self.handle_non_left_press();
                FullscreenSelectionEventOutcome::NonLeftPress
            }
            MouseEventKind::Drag(event::MouseButton::Left) => {
                self.handle_drag(canvas, event.column as usize, event.row as usize);
                FullscreenSelectionEventOutcome::Drag
            }
            MouseEventKind::Drag(_) => FullscreenSelectionEventOutcome::Ignored,
            MouseEventKind::Up(event::MouseButton::Left) => {
                FullscreenSelectionEventOutcome::Release(self.handle_release_at(
                    canvas,
                    event.column as usize,
                    event.row as usize,
                    click_consumed,
                ))
            }
            MouseEventKind::Up(_) => FullscreenSelectionEventOutcome::NonLeftRelease {
                finished_drag: self.finish_if_dragging(),
            },
            MouseEventKind::Moved => FullscreenSelectionEventOutcome::Hover(
                self.handle_no_button_motion(event.column as usize, event.row as usize),
            ),
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                let cleared_selection = self.has_selection();
                if cleared_selection {
                    self.selection_mut().clear();
                }
                FullscreenSelectionEventOutcome::Wheel { cleared_selection }
            }
        }
    }
}

pub(crate) struct Terminal<'a> {
    inner: Box<dyn TerminalImpl + 'a>,
    output: Output,
    base_fullscreen: bool,
    event_stream: Option<BoxStream<'static, io::Result<TerminalEvent>>>,
    caller_input_stream: bool,
    pending_event: Option<io::Result<TerminalEvent>>,
    event_stream_wake: Arc<EventStreamWake>,
    subscribers: Vec<Weak<TerminalEventsShared>>,
    event_cell_snapshot: Option<EventCellSnapshot>,
    terminal_focus_state: Option<bool>,
    last_stdin_event_at: Option<Instant>,
    query_queue: VecDeque<PendingTerminalRequest>,
    pending_ctrl_c: Vec<Arc<SharedEventState>>,
    input_dispatched: bool,
    resized: bool,
    received_ctrl_c: bool,
    ignore_ctrl_c: bool,
    suspend_on_ctrl_z: bool,
    canvas_diff_planning: TerminalDiffPlanning,
    input_backend: TerminalInputBackend,
    raw_input_session_request: Option<TerminalRawInputSessionOptions>,
    synchronized_update_depth: usize,
    synchronized_update_started: bool,
    synchronized_update_supported: bool,
}

// Crossterm keeps the first Pending poll's waker in its background reader.
// A stable bridge lets nonblocking peeks share that reader with later waits.
struct EventStreamWake {
    waker: AtomicWaker,
}

impl Default for EventStreamWake {
    fn default() -> Self {
        Self {
            waker: AtomicWaker::new(),
        }
    }
}

impl Wake for EventStreamWake {
    fn wake(self: Arc<Self>) {
        self.waker.wake();
    }
}

impl<'a> Terminal<'a> {
    pub fn new(
        stdout: Box<dyn Write + Send + 'a>,
        stderr: Box<dyn Write + Send + 'a>,
        output: Output,
        fullscreen: bool,
        mouse_capture: bool,
        keyboard_support: Option<bool>,
    ) -> io::Result<Self> {
        // dest is the render destination, alt is the other stream
        let (dest, alt) = match output {
            Output::Stdout => (stdout, stderr),
            Output::Stderr => (stderr, stdout),
        };
        Ok(Self {
            inner: Box::new(StdTerminal::new(
                dest,
                alt,
                fullscreen,
                mouse_capture,
                keyboard_support,
            )?),
            output,
            base_fullscreen: fullscreen,
            event_stream: None,
            caller_input_stream: false,
            pending_event: None,
            event_stream_wake: Arc::new(EventStreamWake::default()),
            subscribers: Vec::new(),
            event_cell_snapshot: None,
            terminal_focus_state: None,
            last_stdin_event_at: Some(Instant::now()),
            query_queue: VecDeque::new(),
            pending_ctrl_c: Vec::new(),
            input_dispatched: false,
            resized: false,
            received_ctrl_c: false,
            ignore_ctrl_c: false,
            suspend_on_ctrl_z: false,
            canvas_diff_planning: TerminalDiffPlanning::Baseline,
            input_backend: TerminalInputBackend::Crossterm,
            raw_input_session_request: None,
            synchronized_update_depth: 0,
            synchronized_update_started: false,
            synchronized_update_supported: is_synchronized_output_supported(),
        })
    }

    pub fn enable_mouse_capture(&mut self) -> io::Result<()> {
        self.inner.set_mouse_capture(true)
    }

    pub fn disable_mouse_capture(&mut self) -> io::Result<()> {
        self.inner.set_mouse_capture(false)
    }

    pub fn set_keyboard_enhancement_flags(
        &mut self,
        flags: event::KeyboardEnhancementFlags,
    ) -> io::Result<()> {
        self.inner.set_keyboard_enhancement_flags(flags)
    }

    pub fn ignore_ctrl_c(&mut self) {
        self.ignore_ctrl_c = true;
    }

    pub fn suspend_on_ctrl_z(&mut self) {
        self.suspend_on_ctrl_z = true;
    }

    pub fn exit_on_ctrl_c(&self) -> bool {
        !self.ignore_ctrl_c
    }

    pub fn is_raw_mode_supported(&self) -> bool {
        self.inner.is_raw_mode_supported()
    }

    pub fn is_raw_mode_enabled(&self) -> bool {
        self.inner.is_raw_mode_enabled()
    }

    pub fn set_raw_mode_enabled(&mut self, raw_mode_enabled: bool) -> io::Result<()> {
        self.inner.set_raw_mode_enabled(raw_mode_enabled)
    }

    pub fn set_canvas_diff_planning(&mut self, planning: TerminalDiffPlanning) {
        self.canvas_diff_planning = planning;
    }

    pub fn canvas_diff_planning(&self) -> TerminalDiffPlanning {
        self.canvas_diff_planning
    }

    pub fn set_input_backend(&mut self, backend: TerminalInputBackend) {
        self.input_backend = backend;
        self.raw_input_session_request = None;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn input_backend(&self) -> TerminalInputBackend {
        self.input_backend
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn raw_input_session_request(&self) -> Option<TerminalRawInputSessionOptions> {
        self.raw_input_session_request
    }

    pub fn is_fullscreen(&self) -> bool {
        self.inner.is_fullscreen()
    }

    pub fn set_dynamic_alternate_screen(
        &mut self,
        request: Option<crate::context::AlternateScreenRequest>,
    ) -> io::Result<bool> {
        if self.base_fullscreen {
            return Ok(false);
        }
        self.inner.set_dynamic_alternate_screen(request)
    }

    pub fn refresh_size(&mut self) {
        self.inner.refresh_size()
    }

    pub fn size(&self) -> Option<(u16, u16)> {
        self.inner.size()
    }

    pub(crate) fn terminal_focus_state(&self) -> Option<bool> {
        self.terminal_focus_state
    }

    pub fn clear_canvas(&mut self) -> io::Result<()> {
        if self.inner.clear_canvas_would_write() {
            self.ensure_synchronized_update_started()?;
        }
        self.inner.clear_canvas()
    }

    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.ensure_synchronized_update_started()?;
        self.inner.clear_screen()
    }

    pub fn clear_terminal(&mut self) -> io::Result<()> {
        self.ensure_synchronized_update_started()?;
        self.inner.clear_terminal()
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn write_canvas(&mut self, prev: Option<&Canvas>, canvas: &Canvas) -> io::Result<()> {
        if self.inner.write_canvas_would_write(prev, canvas) {
            self.ensure_synchronized_update_started()?;
        }
        let decstbm_safe = self.synchronized_update_supported && self.synchronized_update_started;
        self.inner.set_decstbm_safe(decstbm_safe);
        self.inner.write_canvas(prev, canvas)
    }

    pub(crate) fn set_scrollback_replay(&mut self, replay: Option<ScrollbackReplay>) {
        self.inner.set_scrollback_replay(replay);
    }

    /// Puts back the main-screen rows a shorter terminal cut below a displaced cursor, from
    /// the previous inline frame.
    pub(crate) fn restore_inline_rows_after_height_shrink(
        &mut self,
        prev: &Canvas,
    ) -> io::Result<()> {
        if !self.inner.restore_inline_rows_would_write() {
            return Ok(());
        }
        self.ensure_synchronized_update_started()?;
        self.inner.restore_inline_rows_after_height_shrink(prev)
    }

    pub(crate) fn write_canvas_after_repaint_check(
        &mut self,
        prev: Option<&Canvas>,
        canvas: &Canvas,
    ) -> io::Result<()> {
        // The render loop has already classified this frame as a repaint. Avoid
        // running the terminal backend's preflight row scan only to repeat the
        // same retained-canvas scan inside write_canvas().
        self.ensure_synchronized_update_started()?;
        let decstbm_safe = self.synchronized_update_supported && self.synchronized_update_started;
        self.inner.set_decstbm_safe(decstbm_safe);
        self.inner.write_canvas(prev, canvas)
    }

    /// Updates the retained screen-buffer metadata used to annotate mouse events
    /// with CC Ink-style blank-cell information.
    ///
    /// Skipped — and any stale snapshot dropped — while the terminal is not
    /// reporting the mouse. The snapshot is an O(canvas) scan per frame and
    /// only ever serves mouse events; inline mode never enables tracking (CC
    /// does so only inside its alternate screen), so on a long inline
    /// session it was a quarter of every frame spent on events that could
    /// not arrive.
    pub fn set_event_cell_snapshot(&mut self, canvas: &Canvas) {
        if !self.inner.mouse_events_possible() {
            self.event_cell_snapshot = None;
            return;
        }
        self.event_cell_snapshot = Some(EventCellSnapshot::from_canvas(canvas));
    }

    #[cfg(test)]
    pub(crate) fn has_event_cell_snapshot(&self) -> bool {
        self.event_cell_snapshot.is_some()
    }

    fn annotate_terminal_event(
        mut event: TerminalEvent,
        snapshot: Option<&EventCellSnapshot>,
    ) -> TerminalEvent {
        if let TerminalEvent::FullscreenMouse(mouse) = &mut event {
            mouse.cell_is_blank = snapshot
                .map(|snapshot| snapshot.cell_is_blank(mouse.column, mouse.row))
                .unwrap_or(false);
        }
        event
    }

    /// Positions the physical cursor per the declaration's visibility flag.
    /// See [`Canvas::declare_cursor`].
    pub fn position_cursor(
        &mut self,
        declaration: Option<crate::canvas::CursorDeclaration>,
    ) -> io::Result<()> {
        if self.inner.position_cursor_would_write(declaration) {
            self.ensure_synchronized_update_started()?;
        }
        self.inner.position_cursor(declaration)
    }

    /// Writes a raw OSC 52 clipboard sequence to the terminal.
    ///
    /// This is intentionally outside synchronized-update framing: it is not a
    /// screen repaint and mirrors CC Ink's selection-copy path, which writes the
    /// clipboard escape sequence directly to stdout after extracting selected
    /// text. Clipboard transport fallbacks such as tmux buffers or native tools
    /// remain application-level policy.
    ///
    /// Like [`Self::write_control_sequence`], the bytes are flushed at once so
    /// the copy lands even when no repaint follows.
    pub fn set_clipboard(&mut self, text: &str) -> io::Result<()> {
        let dest = self.inner.dest();
        crate::ansi::osc52_clipboard(dest, text)?;
        dest.flush()
    }

    /// Writes an OSC 52 clipboard sequence using an explicit multiplexer
    /// passthrough wrapper. This mirrors CC Ink's tmux/screen passthrough
    /// helpers; native clipboard and tmux-buffer loading remain app policy.
    pub fn set_clipboard_with_multiplexer(
        &mut self,
        text: &str,
        multiplexer: ClipboardMultiplexer,
    ) -> io::Result<()> {
        if multiplexer == ClipboardMultiplexer::None {
            self.set_clipboard(text)
        } else {
            let dest = self.inner.dest();
            crate::ansi::osc52_clipboard_for_multiplexer(dest, text, multiplexer.into())?;
            dest.flush()
        }
    }

    /// Writes a raw non-visual terminal control sequence directly to stdout.
    ///
    /// This intentionally bypasses synchronized-update framing and retained
    /// canvas clearing, matching CC Ink's TerminalWriteProvider side channel for
    /// OSC notifications, terminal progress, and BEL-like controls.
    pub fn write_control_sequence(&mut self, sequence: &str) -> io::Result<()> {
        self.inner.write_control_sequence(sequence)
    }

    /// Sets the terminal window/tab title.
    ///
    /// Like [`Self::write_control_sequence`], this is a non-visual side-band
    /// write outside synchronized-update framing. The backend decides how to
    /// deliver it: OSC 0 on Unix-likes, and crossterm's `SetTitle` on Windows so
    /// legacy conhost without VT support still receives `SetConsoleTitleW`.
    pub fn set_title(&mut self, title: &str) -> io::Result<()> {
        self.inner.set_title(title)
    }

    /// Sends a terminal query on the render output side band and returns a
    /// future for its response.
    ///
    /// Pair this with [`Self::flush_terminal_queries`] to bound unsupported
    /// queries without timeouts. Parsed [`TerminalEvent::Response`] events are
    /// routed automatically while the render loop waits, or can be delivered
    /// manually with [`Self::on_terminal_response`] in custom integrations.
    ///
    /// Sending a query starts the terminal event stream so backends that
    /// surface [`TerminalEvent::Response`] do not require a separate keyboard
    /// input subscriber. Custom raw-stdin frontends can also feed responses
    /// manually with [`Self::on_terminal_response`].
    pub fn send_terminal_query(
        &mut self,
        query: TerminalQuery,
    ) -> io::Result<PendingTerminalQuery> {
        self.start_event_stream()?;
        let (sender, receiver) = oneshot::channel();
        self.query_queue.push_back(PendingTerminalRequest::Query {
            matcher: query.matcher.clone(),
            sender,
        });
        self.write_control_sequence(query.request())?;
        Ok(PendingTerminalQuery { receiver })
    }

    /// Sends the DA1 flush sentinel for pending terminal queries.
    ///
    /// This also starts the terminal event stream so backends that surface
    /// [`TerminalEvent::Response`] can resolve the sentinel round trip without
    /// a separate input subscriber.
    pub fn flush_terminal_queries(&mut self) -> io::Result<PendingTerminalFlush> {
        self.start_event_stream()?;
        let (sender, receiver) = oneshot::channel();
        self.query_queue
            .push_back(PendingTerminalRequest::Sentinel { sender });
        self.write_control_sequence(da1_query_sequence())?;
        Ok(PendingTerminalFlush { receiver })
    }

    /// Dispatches a parsed terminal response to this terminal's query queue.
    #[allow(dead_code)]
    pub fn on_terminal_response(&mut self, response: TerminalResponse) {
        if let TerminalResponse::Xtversion { name } = &response {
            set_xtversion_name(name.clone());
        }
        dispatch_terminal_query_response(&mut self.query_queue, response);
    }

    pub fn received_ctrl_c(&self) -> bool {
        self.received_ctrl_c
    }

    /// Applies the default Ctrl+C behavior after component hooks have had a chance
    /// to consume the event via propagation. A pending Ctrl+C exits only if none of
    /// the component callbacks called `stop_propagation()`.
    pub fn resolve_pending_ctrl_c(&mut self) {
        if self.ignore_ctrl_c || self.received_ctrl_c {
            self.pending_ctrl_c.clear();
            return;
        }
        if self
            .pending_ctrl_c
            .drain(..)
            .any(|state| !state.is_propagation_stopped())
        {
            self.received_ctrl_c = true;
        }
    }

    /// Returns `true` (and clears the flag) if the process was resumed from suspension
    /// (SIGCONT) since the last call. The render loop uses this to trigger a full
    /// terminal reinitialization and redraw.
    pub fn take_resumed(&mut self) -> bool {
        self.inner.take_resumed()
    }

    /// Re-applies terminal modes and resets cached output state after a resume from
    /// suspension. See [`TerminalImpl::reinitialize_after_resume`].
    pub fn reinitialize_after_resume(&mut self) -> io::Result<()> {
        self.inner.reinitialize_after_resume()
    }

    /// Releases raw/input modes so another process (or direct writes) can own
    /// the terminal. Dropping the event stream releases crossterm's input
    /// reader while the terminal is handed off; it is recreated by
    /// [`Terminal::reacquire_terminal`]. Caller-owned streams retain their buffered
    /// events and are not polled until the handoff finishes.
    pub fn release_terminal(&mut self) -> io::Result<()> {
        if !self.caller_input_stream {
            self.event_stream = None;
        }
        self.inner.release_terminal_modes()
    }

    /// Restores terminal modes and the input event stream after
    /// [`Terminal::release_terminal`].
    pub fn reacquire_terminal(&mut self) -> io::Result<()> {
        self.inner.reacquire_terminal_modes()?;
        self.start_event_stream()
    }

    /// Returns a mutable reference to the stdout handle.
    pub fn stdout(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        match self.output {
            Output::Stdout => self.inner.dest(),
            Output::Stderr => self.inner.alt(),
        }
    }

    /// Returns a mutable reference to the stderr handle.
    pub fn stderr(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        match self.output {
            Output::Stdout => self.inner.alt(),
            Output::Stderr => self.inner.dest(),
        }
    }

    /// Returns a mutable reference to the render output handle (stdout or stderr based on output setting).
    pub fn render_output(&mut self) -> &mut dyn Write {
        let _ = self.ensure_synchronized_update_started();
        self.inner.dest()
    }

    fn ensure_synchronized_update_started(&mut self) -> io::Result<()> {
        if self.synchronized_update_supported
            && self.synchronized_update_depth > 0
            && !self.synchronized_update_started
        {
            self.inner
                .dest()
                .execute(terminal::BeginSynchronizedUpdate)?;
            self.synchronized_update_started = true;
        }
        Ok(())
    }

    fn end_synchronized_update(&mut self) -> io::Result<()> {
        self.synchronized_update_depth = self.synchronized_update_depth.saturating_sub(1);
        if self.synchronized_update_depth == 0 && self.synchronized_update_started {
            self.synchronized_update_started = false;
            self.inner.dest().execute(terminal::EndSynchronizedUpdate)?;
        }
        Ok(())
    }

    /// Wraps a series of terminal updates in a synchronized update block, making sure to end the
    /// synchronized update even if there is an error or panic.
    pub fn synchronized_update<F>(&mut self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut Self) -> io::Result<()>,
    {
        let mut t = SynchronizedUpdate::begin(self);
        let result = f(t.inner.as_deref_mut().unwrap());
        result.and(t.finish())
    }

    pub(crate) fn set_input_stream(
        &mut self,
        stream: BoxStream<'static, io::Result<TerminalEvent>>,
    ) -> io::Result<()> {
        self.inner.set_raw_mode_enabled(true)?;
        self.event_stream = Some(stream);
        self.caller_input_stream = true;
        Ok(())
    }

    pub fn start_event_stream(&mut self) -> io::Result<()> {
        if self.event_stream.is_none() {
            match self.input_backend {
                TerminalInputBackend::Crossterm => {
                    self.event_stream = Some(self.inner.event_stream()?);
                }
                TerminalInputBackend::RawInput(options) => {
                    self.raw_input_session_request = Some(options);
                    self.event_stream = Some(stream::pending().boxed());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn take_input_dispatched(&mut self) -> bool {
        std::mem::take(&mut self.input_dispatched)
    }

    /// Whether a size-changing resize event arrived since the last call.
    pub(crate) fn take_resized(&mut self) -> bool {
        std::mem::take(&mut self.resized)
    }

    /// Peeks at already available input without dispatching it before props settle.
    pub(crate) fn has_pending_input(&mut self) -> bool {
        if self.pending_event.is_none()
            && let Poll::Ready(Some(event)) = self.poll_event_stream_now()
        {
            self.pending_event = Some(event);
        }
        self.pending_event.as_ref().is_some_and(|event| {
            matches!(event, Ok(TerminalEvent::Key(_) | TerminalEvent::Paste(_)))
                && !event
                    .as_ref()
                    .is_ok_and(|event| self.event_requests_suspend(event))
        })
    }

    fn event_requests_suspend(&self, event: &TerminalEvent) -> bool {
        self.suspend_on_ctrl_z
            && matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char(ch),
                    kind: KeyEventKind::Press,
                    modifiers,
                    ..
                }) if ch.eq_ignore_ascii_case(&'z')
                    && modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            )
    }

    fn poll_event_stream_now(&mut self) -> Poll<Option<io::Result<TerminalEvent>>> {
        let Some(event_stream) = self.event_stream.as_mut() else {
            return Poll::Ready(None);
        };
        let stream_waker = Waker::from(self.event_stream_wake.clone());
        let mut context = Context::from_waker(&stream_waker);
        event_stream.as_mut().poll_next(&mut context)
    }

    pub async fn wait(&mut self) -> io::Result<()> {
        use futures::future::{Either, poll_fn, select};

        let event_cell_snapshot = self.event_cell_snapshot.clone();
        loop {
            // Race the next terminal event against the resume (SIGCONT) signal so
            // the render loop wakes immediately when the process is foregrounded,
            // even with no pending input.
            let inner = &mut self.inner;
            let next = if let Some(event) = self.pending_event.take() {
                Some(event)
            } else {
                let Some(event_stream) = self.event_stream.as_mut() else {
                    poll_fn(|cx| inner.poll_resumed(cx)).await;
                    return Ok(());
                };
                let event_stream_wake = self.event_stream_wake.clone();
                let stream_waker = Waker::from(event_stream_wake.clone());
                let next_event = poll_fn(move |cx| {
                    event_stream_wake.waker.register(cx.waker());
                    let mut stream_context = Context::from_waker(&stream_waker);
                    event_stream.as_mut().poll_next(&mut stream_context)
                });
                match select(next_event, poll_fn(|cx| inner.poll_resumed(cx))).await {
                    Either::Left((next, _)) => next,
                    Either::Right(((), _)) => return Ok(()),
                }
            };
            let Some(event) = next else {
                // Propagate stream exhaustion so applications can shut down
                // when their terminal disappears.
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal event stream ended",
                ));
            };
            let event = event?;
            let now = Instant::now();
            if self
                .last_stdin_event_at
                .is_some_and(|last| now.duration_since(last) > STDIN_RESUME_GAP)
            {
                let _ = self.inner.reassert_after_stdin_resume();
            }
            self.last_stdin_event_at = Some(now);
            let is_ctrl_c = matches!(
                event,
                TerminalEvent::Key(KeyEvent {
                    code: KeyCode::Char('c'),
                    kind: KeyEventKind::Press,
                    modifiers,
                    ..
                }) if modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            );
            if self.event_requests_suspend(&event) {
                // Opt-in CC Ink/Claude Code Ctrl+Z behavior: hand the
                // terminal back to the shell, stop the process on Unix, and
                // let the SIGCONT repair path repaint from a clean slate
                // after `fg`. Generic iocraft apps receive Ctrl+Z as normal
                // input unless they explicitly enable this policy.
                let _ = self.inner.suspend();
                return Ok(());
            }
            match &event {
                TerminalEvent::FocusGained => self.terminal_focus_state = Some(true),
                TerminalEvent::FocusLost => self.terminal_focus_state = Some(false),
                TerminalEvent::Response(response) => {
                    if let TerminalResponse::Xtversion { name } = response {
                        set_xtversion_name(name.clone());
                    }
                    dispatch_terminal_query_response(&mut self.query_queue, response.clone());
                }
                _ => {}
            }
            // CC Ink `App.tsx` "Failsafe: if we receive input, the terminal
            // must be focused": a key or paste that arrives after a
            // focus-out means the focus-in was dropped (some emulators
            // do), so the input stands in for it. Focus subscribers see
            // the synthesized focus-in before the input itself, the way
            // CC's `setTerminalFocused(true)` notifies its store first.
            if self.terminal_focus_state == Some(false)
                && matches!(event, TerminalEvent::Key(_) | TerminalEvent::Paste(_))
            {
                self.terminal_focus_state = Some(true);
                let _ = Self::dispatch_to_subscribers(
                    &mut self.subscribers,
                    TerminalEvent::FocusGained,
                );
            }
            let is_resize = matches!(event, TerminalEvent::Resize(..));
            self.input_dispatched =
                matches!(event, TerminalEvent::Key(_) | TerminalEvent::Paste(_));
            if let TerminalEvent::Resize(width, height) = &event {
                let next_size = (*width, *height);
                if self.inner.size() == Some(next_size) {
                    // Match CC Ink's resize handler: same-dimension resize
                    // events are terminal-settling noise. Do not wake the
                    // render loop or reset/reassert modes redundantly.
                    continue;
                }
                self.inner.set_size_from_resize_event(*width, *height);
                self.resized = true;
                let _ = self.inner.reassert_after_resize();
            }
            let event = Self::annotate_terminal_event(event, event_cell_snapshot.as_ref());

            // Dispatch to the interested subscribers first — Ctrl+C is a
            // normal event.
            let (delivered, shared_state) =
                Self::dispatch_to_subscribers(&mut self.subscribers, event);

            if is_ctrl_c && !self.ignore_ctrl_c {
                if delivered {
                    // Defer default exit until the render loop has polled component
                    // hooks, giving propagation-aware listeners a chance to consume it.
                    self.pending_ctrl_c.push(shared_state);
                } else {
                    // No component can consume it, so preserve the default behavior.
                    self.received_ctrl_c = true;
                }
            }
            if self.received_ctrl_c || is_resize || self.input_dispatched {
                return Ok(());
            }
        }
    }

    /// Queues `event` on every subscriber whose interest covers it and wakes
    /// them. A subscriber whose interest does not cover the event is left
    /// untouched: no clone, no queue entry, no wake. Returns whether anyone
    /// received it and the propagation state the deferred Ctrl+C check reads.
    /// Takes the field rather than `self` so `wait` can call it while it
    /// holds the event stream.
    fn dispatch_to_subscribers(
        subscribers: &mut Vec<Weak<TerminalEventsShared>>,
        event: TerminalEvent,
    ) -> (bool, Arc<SharedEventState>) {
        let shared_state = Arc::new(SharedEventState::default());
        let mut delivered = false;
        subscribers.retain(|subscriber| {
            let Some(subscriber) = subscriber.upgrade() else {
                return false;
            };
            if !subscriber.interest().covers(&event) {
                return true;
            }
            delivered = true;
            let mut inner = subscriber.inner.lock().unwrap();
            inner
                .pending
                .push_back((event.clone(), shared_state.clone()));
            if let Some(waker) = inner.waker.take() {
                waker.wake();
            }
            true
        });
        (delivered, shared_state)
    }

    pub fn events(&mut self) -> io::Result<TerminalEvents> {
        self.start_event_stream()?;
        let shared = Arc::new(TerminalEventsShared::new());
        self.subscribers.push(Arc::downgrade(&shared));
        Ok(TerminalEvents { shared })
    }
}

impl Terminal<'static> {
    pub fn mock(mut config: MockTerminalConfig) -> (Self, MockTerminalOutputStream) {
        let ignore_ctrl_c = config.ignore_ctrl_c;
        let suspend_on_ctrl_z = config.suspend_on_ctrl_z;
        let canvas_diff_planning = config.canvas_diff_planning;
        let input_backend = config.input_backend;
        let input_stream = config.input_stream.take();
        let caller_input_stream = input_stream.is_some();
        let (term, output_stream) = MockTerminal::new(config);
        let base_fullscreen = term.fullscreen;
        (
            Self {
                inner: Box::new(term),
                output: Output::Stdout,
                base_fullscreen,
                event_stream: input_stream,
                caller_input_stream,
                pending_event: None,
                event_stream_wake: Arc::new(EventStreamWake::default()),
                subscribers: Vec::new(),
                event_cell_snapshot: None,
                terminal_focus_state: None,
                last_stdin_event_at: Some(Instant::now()),
                query_queue: VecDeque::new(),
                pending_ctrl_c: Vec::new(),
                input_dispatched: false,
                resized: false,
                received_ctrl_c: false,
                ignore_ctrl_c,
                suspend_on_ctrl_z,
                canvas_diff_planning,
                input_backend,
                raw_input_session_request: None,
                synchronized_update_depth: 0,
                synchronized_update_started: false,
                synchronized_update_supported: true,
            },
            output_stream,
        )
    }

    #[cfg(test)]
    pub(crate) fn mock_with_event_error(error: io::Error) -> Self {
        let (mut terminal, _output) = Self::mock(MockTerminalConfig::default());
        terminal.event_stream = Some(stream::once(async move { Err(error) }).boxed());
        terminal
    }
}

impl Write for Terminal<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.ensure_synchronized_update_started()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Synchronized update terminal guard.
/// Enters synchronized update on creation, exits when finished or dropped.
pub(crate) struct SynchronizedUpdate<'a, 'b> {
    inner: Option<&'a mut Terminal<'b>>,
}

impl<'a, 'b> SynchronizedUpdate<'a, 'b> {
    pub fn begin(terminal: &'a mut Terminal<'b>) -> Self {
        terminal.synchronized_update_depth += 1;
        Self {
            inner: Some(terminal),
        }
    }

    fn finish(mut self) -> io::Result<()> {
        self.inner.take().unwrap().end_synchronized_update()
    }
}

impl Drop for SynchronizedUpdate<'_, '_> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.as_deref_mut() {
            let _ = inner.end_synchronized_update();
        }
    }
}

#[cfg(test)]
mod tests;
