use super::*;
use std::sync::Condvar;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_PENDING_OPERATIONS: usize = 64;

#[cfg(test)]
mod tests;

pub(super) type QueryOperation = Box<dyn FnOnce(&mut dyn Write) -> io::Result<()> + Send>;

pub(super) enum Operation {
    Query(QueryOperation),
    History(
        Box<HistoryDraft>,
        oneshot::Sender<io::Result<CommitReceipt>>,
    ),
    Frame {
        draft: Box<FrameDraft>,
        size: Option<(u16, u16)>,
        screen: Option<crate::context::AlternateScreenRequest>,
        replay: Option<ScrollbackReplay>,
        receipt: Option<oneshot::Sender<io::Result<FrameCommit>>>,
    },
    Control(String, Option<oneshot::Sender<io::Result<()>>>),
    PrintBatch(Vec<OutputMessage>, Output),
    Title(String),
    Bytes(Vec<u8>, bool),
    Flush(bool),
    ClearCanvas,
    ClearScreen,
    ClearTerminal,
    Mouse(bool),
    Keyboard(event::KeyboardEnhancementFlags),
    RawMode(bool),
    Resize,
    StdinResume,
    Resume,
    Suspend,
    Release(oneshot::Sender<io::Result<()>>),
    Reacquire(oneshot::Sender<io::Result<()>>),
    Barrier(oneshot::Sender<io::Result<()>>),
}

impl Operation {
    fn reject(self, error: &io::Error) {
        let rejected = || Err(io::Error::new(error.kind(), error.to_string()));
        match self {
            Self::Frame {
                receipt: Some(receipt),
                ..
            } => {
                let _ = receipt.send(Err(io::Error::new(error.kind(), error.to_string())));
            }
            Self::History(_, receipt) => {
                let _ = receipt.send(Err(io::Error::new(error.kind(), error.to_string())));
            }
            Self::PrintBatch(messages, _) => {
                for message in messages {
                    message.reject(error);
                }
            }
            Self::Control(_, Some(receipt))
            | Self::Release(receipt)
            | Self::Reacquire(receipt)
            | Self::Barrier(receipt) => {
                let _ = receipt.send(rejected());
            }
            _ => {}
        }
    }
    fn reserve(
        &mut self,
        memory: &Arc<OutputMemory>,
        control: &Arc<Allocation>,
    ) -> io::Result<Arc<Allocation>> {
        match self {
            Self::Frame { draft, .. } => {
                draft.canvas.output_allocations = draft.canvas.reserve_output(memory, false)?;
                draft.graphics = super::graphics::GraphicsPlan::reserve(&draft.canvas, memory)?;
                let bytes = draft.canvas.width() * draft.canvas.height()
                    + draft.hits.capacity() * size_of::<HitRegion>()
                    + draft
                        .views
                        .as_ref()
                        .map_or(0, |views| views.retained_bytes());
                draft.allocations.reserve(memory, bytes)?;
                for region in &mut draft.hits {
                    region.target.reserve(memory)?;
                }
                if let Some(coverage) = &mut draft.coverage {
                    coverage.reserve(memory)?;
                }
            }
            Self::History(draft, _) => {
                if let Some(coverage) = &mut draft.coverage {
                    coverage.reserve(memory)?;
                }
                draft.canvas.output_allocations = draft.canvas.reserve_output(memory, false)?;
                draft.graphics = super::graphics::GraphicsPlan::reserve(&draft.canvas, memory)?;
            }
            _ => {}
        }
        let bytes = self.retained_bytes();
        if bytes == 0 {
            Ok(control.clone())
        } else {
            memory.reserve(bytes)
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::History(..) | Self::Frame { .. } => 0,
            Self::Control(text, _) | Self::Title(text) => text.capacity(),
            Self::Bytes(bytes, _) => bytes.capacity(),
            Self::PrintBatch(messages, _) => {
                messages.capacity() * size_of::<OutputMessage>()
                    + messages
                        .iter()
                        .map(OutputMessage::retained_bytes)
                        .sum::<usize>()
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    Open,
    Releasing,
    Released,
    Reacquiring,
    Closing,
    Finished,
}

impl Admission {
    fn allows(self, operation: &Operation) -> bool {
        match operation {
            Operation::Reacquire(_) => self == Self::Released,
            _ => self == Self::Open,
        }
    }
    fn can_retire(self) -> bool {
        matches!(self, Self::Open | Self::Closing)
    }
}

struct Pending {
    operations: VecDeque<(Operation, Arc<Allocation>)>,
    admission: Admission,
    admission_error: Option<(io::ErrorKind, String)>,
    visual_revision: u64,
    capacity_revision: u64,
    active: bool,
    resumed: bool,
    finished: Option<Result<(), (io::ErrorKind, String)>>,
    snapshot: Option<EventCellSnapshot>,
    #[cfg(unix)]
    resume_signal: Option<super::backend::ResumeSignalListener>,
}

struct Shared {
    memory: Arc<OutputMemory>,
    control_allocation: Arc<Allocation>,
    pending: Mutex<Pending>,
    changed: Condvar,
    completed: AtomicWaker,
    abandoned: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct OutputSender {
    shared: Arc<Shared>,
    output: Output,
}

pub(super) struct OutputSession {
    shared: Arc<Shared>,
    pub(super) size: Option<(u16, u16)>,
    pub(super) screen: Option<crate::context::AlternateScreenRequest>,
    pub(super) base_fullscreen: bool,
    pub(super) raw_mode: bool,
    pub(super) replay: Option<ScrollbackReplay>,
    pub(super) raw_mode_supported: bool,
    pub(super) mock: bool,
    observed_visual_revision: u64,
    resume_restored: bool,
    waiting_capacity: Option<u64>,
    output: Output,
}

impl OutputSession {
    pub(super) fn new(
        destination: Box<dyn Write + Send>,
        alternate: Box<dyn Write + Send>,
        fullscreen: bool,
        mouse_capture: bool,
        keyboard_support: Option<bool>,
        output: Output,
    ) -> io::Result<Self> {
        Self::with_capacity(
            destination,
            alternate,
            fullscreen,
            mouse_capture,
            keyboard_support,
            output,
            OutputCapacity::default(),
        )
    }

    pub(super) fn with_capacity(
        destination: Box<dyn Write + Send>,
        alternate: Box<dyn Write + Send>,
        fullscreen: bool,
        mouse_capture: bool,
        keyboard_support: Option<bool>,
        output: Output,
        capacity: OutputCapacity,
    ) -> io::Result<Self> {
        // Complete the keyboard query before an event stream can acquire the input reader.
        let keyboard_support = Some(keyboard_support.unwrap_or_else(|| {
            stdin().is_terminal() && terminal::supports_keyboard_enhancement().unwrap_or(false)
        }));
        Self::spawn(
            fullscreen,
            output,
            terminal::size().ok(),
            stdin().is_terminal(),
            false,
            capacity,
            move |worker| {
                let destination = Box::new(ResumableWriter::new(destination, worker.clone()));
                let alternate = Box::new(ResumableWriter::new(alternate, worker.clone()));
                let backend = StdTerminal::new(
                    destination,
                    alternate,
                    fullscreen,
                    mouse_capture,
                    keyboard_support,
                )?;
                #[cfg(unix)]
                let backend = {
                    let mut backend = backend;
                    worker
                        .pending
                        .lock()
                        .expect("output initialization is not poisoned")
                        .resume_signal = backend.resume_signal.take();
                    worker.completed.wake();
                    backend
                };
                Ok(Box::new(backend))
            },
        )
    }

    pub(super) fn mock(backend: MockTerminal, output: Output) -> io::Result<Self> {
        let raw_mode = backend.is_raw_mode_enabled();
        let mut session = Self::spawn(
            backend.is_fullscreen(),
            output,
            backend.size(),
            true,
            true,
            OutputCapacity::default(),
            move |_| Ok(Box::new(backend)),
        )?;
        session.raw_mode = raw_mode;
        Ok(session)
    }

    fn spawn(
        fullscreen: bool,
        output: Output,
        size: Option<(u16, u16)>,
        raw_mode_supported: bool,
        mock: bool,
        capacity: OutputCapacity,
        create: impl FnOnce(Arc<Shared>) -> io::Result<Box<dyn TerminalImpl>> + Send + 'static,
    ) -> io::Result<Self> {
        let memory = OutputMemory::new(capacity);
        let control_allocation = memory.reserve(64 * 1024)?;
        let shared = Arc::new(Shared {
            memory,
            control_allocation,
            pending: Mutex::new(Pending {
                operations: VecDeque::with_capacity(MAX_PENDING_OPERATIONS),
                admission: Admission::Open,
                admission_error: None,
                visual_revision: 0,
                capacity_revision: 0,
                active: false,
                resumed: false,
                finished: None,
                snapshot: None,
                #[cfg(unix)]
                resume_signal: None,
            }),
            changed: Condvar::new(),
            completed: AtomicWaker::new(),
            abandoned: AtomicBool::new(false),
        });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("iocraft-output".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let backend = create(worker.clone())?;
                    let mut owner = OutputOwner {
                        backend,
                        previous: None,
                        saved_main: None,
                        base_fullscreen: fullscreen,
                        shared: worker.clone(),
                        synchronization: SynchronizedOutput::new(
                            !mock && is_synchronized_output_supported(),
                        ),
                        graphics: super::graphics::Graphics::new(worker.memory.clone()),
                        appended_newline: None,
                        sequence: 0,
                    };
                    let result = owner.run(&worker);
                    if result.is_err() {
                        worker.abandoned.store(true, Ordering::Release);
                    }
                    let cleanup = owner.cleanup();
                    drop(owner);
                    result.and(cleanup)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("terminal output worker panicked")));
                let mut pending = worker
                    .pending
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                pending.admission = Admission::Finished;
                let projection = pending.snapshot.take();
                let rejected = std::mem::take(&mut pending.operations);
                let result = result.map_err(|error| (error.kind(), error.to_string()));
                pending.finished =
                    Some(result.and(pending.admission_error.take().map_or(Ok(()), Err)));
                drop(pending);
                drop(projection);
                drop(rejected);
                worker.completed.wake();
            })?;
        Ok(Self {
            shared,
            size,
            screen: None,
            base_fullscreen: fullscreen,
            raw_mode: false,
            replay: None,
            raw_mode_supported,
            mock,
            output,
            observed_visual_revision: 0,
            resume_restored: false,
            waiting_capacity: None,
        })
    }

    pub(super) fn sender(&self) -> OutputSender {
        OutputSender {
            shared: self.shared.clone(),
            output: self.output,
        }
    }

    pub(super) fn take_visual_change(&mut self) -> bool {
        let revision = self
            .shared
            .pending
            .lock()
            .expect("output projection is not poisoned")
            .visual_revision;
        let changed = revision != self.observed_visual_revision;
        self.observed_visual_revision = revision;
        changed
    }

    pub(super) fn enqueue(&mut self, operation: Operation) -> io::Result<()> {
        self.sender().enqueue(operation)
    }

    pub(super) fn enqueue_frame(&mut self, operation: Operation) -> io::Result<bool> {
        let revision = self
            .shared
            .pending
            .lock()
            .expect("output status is not poisoned")
            .capacity_revision;
        match self.enqueue(operation) {
            Ok(()) => {
                self.waiting_capacity = None;
                Ok(true)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.waiting_capacity = Some(revision);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn poll_output_or_resume(&mut self, context: &mut Context<'_>) -> Poll<()> {
        self.shared.completed.register(context.waker());
        let pending = self
            .shared
            .pending
            .lock()
            .expect("output status is not poisoned");
        if self
            .waiting_capacity
            .is_some_and(|revision| revision != pending.capacity_revision)
        {
            self.waiting_capacity = None;
            return Poll::Ready(());
        }
        if pending.finished.is_some()
            || pending.visual_revision != self.observed_visual_revision
            || pending.resumed
        {
            return Poll::Ready(());
        }
        #[cfg(unix)]
        let mut pending = pending;
        #[cfg(unix)]
        if let Some(signal) = pending.resume_signal.as_mut() {
            return signal.poll_resumed(context);
        }
        Poll::Pending
    }

    pub(super) fn take_resumed(&mut self) -> bool {
        if std::mem::take(
            &mut self
                .shared
                .pending
                .lock()
                .expect("output status is not poisoned")
                .resumed,
        ) {
            self.resume_restored = true;
            return true;
        }
        #[cfg(unix)]
        {
            self.shared
                .pending
                .lock()
                .expect("output status is not poisoned")
                .resume_signal
                .as_ref()
                .is_some_and(|signal| signal.take_resumed())
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(super) fn resume(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.resume_restored) {
            Ok(())
        } else {
            self.enqueue(Operation::Resume)
        }
    }

    pub(super) fn check(&self) -> io::Result<()> {
        let pending = self
            .shared
            .pending
            .lock()
            .expect("output status is not poisoned");
        if let Some((kind, message)) = pending.admission_error.as_ref().or_else(|| {
            pending
                .finished
                .as_ref()
                .and_then(|result| result.as_ref().err())
        }) {
            return Err(io::Error::new(*kind, message.clone()));
        }
        Ok(())
    }

    pub(super) fn snapshot(&self) -> Option<EventCellSnapshot> {
        self.shared
            .pending
            .lock()
            .expect("output projection is not poisoned")
            .snapshot
            .clone()
    }

    pub(super) async fn finish(&mut self) -> io::Result<()> {
        {
            let mut pending = self
                .shared
                .pending
                .lock()
                .expect("output admission is not poisoned");
            if pending.admission != Admission::Finished {
                pending.admission = Admission::Closing;
            }
        }
        self.shared.changed.notify_one();
        futures::future::poll_fn(|context| {
            self.shared.completed.register(context.waker());
            let pending = self
                .shared
                .pending
                .lock()
                .expect("output completion is not poisoned");
            match &pending.finished {
                Some(Ok(())) => Poll::Ready(Ok(())),
                Some(Err((kind, message))) => {
                    Poll::Ready(Err(io::Error::new(*kind, message.clone())))
                }
                None => Poll::Pending,
            }
        })
        .await
    }
}

impl OutputSender {
    pub(crate) fn prepare_replay(
        &self,
        bytes: usize,
        prepare: impl FnOnce() -> ScrollbackReplay,
    ) -> Option<ScrollbackReplay> {
        match self.shared.memory.reserve(bytes) {
            Ok(allocation) => {
                let replay = prepare();
                Some(Arc::new(move |width| {
                    let _allocation = &allocation;
                    replay(width)
                }))
            }
            Err(error) => {
                self.fail_admission(&error);
                None
            }
        }
    }

    pub(crate) fn committed_frame(&self) -> Option<Arc<CommittedFrame>> {
        self.shared
            .pending
            .lock()
            .expect("output projection is not poisoned")
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.frame.clone())
    }

    pub(crate) fn append_history(
        &self,
        draft: HistoryDraft,
    ) -> futures::future::BoxFuture<'static, io::Result<CommitReceipt>> {
        self.append_history_with_errors(draft, false)
    }

    pub(crate) fn append_history_with_errors(
        &self,
        draft: HistoryDraft,
        report: bool,
    ) -> futures::future::BoxFuture<'static, io::Result<CommitReceipt>> {
        let (sender, receiver) = oneshot::channel();
        let accepted = self.enqueue(Operation::History(Box::new(draft), sender));
        if report && let Err(error) = &accepted {
            self.fail_admission(error);
        }
        Box::pin(async move {
            accepted?;
            receiver.await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal output is closed")
            })?
        })
    }
    pub(crate) fn submit(&self, message: OutputMessage) {
        if let Err(error) = self.enqueue(Operation::PrintBatch(vec![message], self.output)) {
            self.fail_admission(&error);
        }
    }

    fn fail_admission(&self, error: &io::Error) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            return;
        }
        let mut pending = self
            .shared
            .pending
            .lock()
            .expect("output admission is not poisoned");
        pending
            .admission_error
            .get_or_insert((error.kind(), error.to_string()));
        if pending.admission != Admission::Finished {
            pending.admission = Admission::Closing;
        }
        drop(pending);
        self.shared.changed.notify_one();
        self.shared.completed.wake();
    }

    fn enqueue(&self, mut operation: Operation) -> io::Result<()> {
        let mut pending = self
            .shared
            .pending
            .lock()
            .expect("output admission is not poisoned");
        if !pending.admission.allows(&operation) {
            let error = io::Error::new(io::ErrorKind::BrokenPipe, "terminal output is closed");
            drop(pending);
            operation.reject(&error);
            return Err(error);
        }
        let replacing = matches!(
            (&operation, pending.operations.back()),
            (Operation::Frame { .. }, Some((Operation::Frame { .. }, _)))
        );
        if !replacing
            && pending.operations.len() >= MAX_PENDING_OPERATIONS
            && !matches!(&operation, Operation::Release(_) | Operation::Reacquire(_))
        {
            let error = io::Error::new(io::ErrorKind::WouldBlock, "terminal output queue is full");
            drop(pending);
            operation.reject(&error);
            return Err(error);
        }
        let allocation =
            match operation.reserve(&self.shared.memory, &self.shared.control_allocation) {
                Ok(allocation) => allocation,
                Err(error) => {
                    let error = if error.kind() == io::ErrorKind::OutOfMemory
                        && matches!(&operation, Operation::Frame { .. })
                        && (pending.active || !pending.operations.is_empty())
                    {
                        io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "terminal output capacity is in use",
                        )
                    } else {
                        error
                    };
                    drop(pending);
                    operation.reject(&error);
                    return Err(error);
                }
            };
        let replaced = replacing.then(|| pending.operations.pop_back()).flatten();
        if let (Operation::Frame { draft, .. }, Some((Operation::Frame { draft: old, .. }, _))) =
            (&mut operation, &replaced)
        {
            draft.paint |= old.paint;
            draft.canvas.inherit_unpainted_changes(&old.canvas);
        }
        if matches!(&operation, Operation::Release(_)) {
            pending.admission = Admission::Releasing;
        }
        if matches!(&operation, Operation::Reacquire(_)) {
            pending.admission = Admission::Reacquiring;
        }
        if matches!(&operation, Operation::History(..))
            || matches!(&operation, Operation::PrintBatch(messages, _) if messages.iter().any(OutputMessage::affects_visible_output))
        {
            pending.visual_revision += 1;
        }
        pending.operations.push_back((operation, allocation));
        drop(pending);
        if let Some((
            Operation::Frame {
                receipt: Some(receipt),
                ..
            },
            _,
        )) = replaced
        {
            let _ = receipt.send(Ok(FrameCommit::Superseded));
        }
        self.shared.changed.notify_one();
        self.shared.completed.wake();
        Ok(())
    }
}

impl Drop for OutputSession {
    fn drop(&mut self) {
        self.shared.abandoned.store(true, Ordering::Release);
        let mut pending = self
            .shared
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.admission = Admission::Closing;
        let rejected = std::mem::take(&mut pending.operations);
        drop(pending);
        drop(rejected);
        self.shared.changed.notify_one();
    }
}

struct ResumableWriter {
    inner: Box<dyn Write + Send>,
    shared: Arc<Shared>,
}

impl ResumableWriter {
    fn new(inner: Box<dyn Write + Send>, shared: Arc<Shared>) -> Self {
        Self { inner, shared }
    }

    fn retry<T>(
        &mut self,
        mut action: impl FnMut(&mut dyn Write) -> io::Result<T>,
    ) -> io::Result<T> {
        loop {
            match action(self.inner.as_mut()) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) && !self.shared.abandoned.load(Ordering::Acquire) =>
                {
                    std::thread::park_timeout(Duration::from_millis(2));
                }
                result => {
                    if result.is_err() {
                        self.shared.abandoned.store(true, Ordering::Release);
                    }
                    return result;
                }
            }
        }
    }
}

impl Write for ResumableWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.retry(|writer| writer.write(&bytes[..bytes.len().min(4096)]));
        if matches!(result, Ok(0)) && !bytes.is_empty() {
            self.shared.abandoned.store(true, Ordering::Release);
        }
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        self.retry(|writer| writer.flush())
    }
}

struct OutputOwner {
    backend: Box<dyn TerminalImpl>,
    previous: Option<Canvas>,
    saved_main: Option<Canvas>,
    base_fullscreen: bool,
    synchronization: SynchronizedOutput,
    shared: Arc<Shared>,
    graphics: super::graphics::Graphics,
    appended_newline: Option<u16>,
    sequence: u64,
}

impl OutputOwner {
    fn cleanup(&mut self) -> io::Result<()> {
        if self.shared.abandoned.load(Ordering::Acquire) {
            let _ = self.backend.dest().write_all(b"\x18\x1b\\\x1b[0m");
        }
        let synchronization = self.synchronization.restore(self.backend.dest());
        let graphics = self
            .graphics
            .cleanup(self.backend.is_fullscreen(), self.backend.dest());
        let terminal = self.backend.finish();
        synchronization.and(graphics).and(terminal)
    }

    fn run(&mut self, shared: &Shared) -> io::Result<()> {
        loop {
            let operation = {
                let mut pending = shared.pending.lock().expect("output queue is not poisoned");
                while pending.operations.is_empty()
                    && pending.admission != Admission::Closing
                    && !(pending.admission.can_retire()
                        && self
                            .graphics
                            .retirement_pending(self.backend.is_fullscreen()))
                {
                    pending = shared
                        .changed
                        .wait(pending)
                        .expect("output queue is not poisoned");
                }
                match pending.operations.pop_front() {
                    Some(operation) => {
                        pending.active = true;
                        Some(operation)
                    }
                    None if pending.admission.can_retire()
                        && self
                            .graphics
                            .retirement_pending(self.backend.is_fullscreen()) =>
                    {
                        None
                    }
                    None => return Ok(()),
                }
            };
            let Some((operation, allocation)) = operation else {
                self.graphics
                    .drain_retired(self.backend.is_fullscreen(), self.backend.dest())?;
                self.backend.flush()?;
                continue;
            };
            self.execute(operation)?;
            drop(allocation);
            let mut pending = shared
                .pending
                .lock()
                .expect("output status is not poisoned");
            pending.active = false;
            pending.capacity_revision += 1;
            drop(pending);
            shared.completed.wake();
        }
    }

    fn invalidate_projection(&self) {
        let snapshot = self
            .shared
            .pending
            .lock()
            .expect("output projection is not poisoned")
            .snapshot
            .take();
        drop(snapshot);
    }

    fn execute(&mut self, operation: Operation) -> io::Result<()> {
        if matches!(
            &operation,
            Operation::ClearCanvas
                | Operation::ClearScreen
                | Operation::ClearTerminal
                | Operation::Release(_)
                | Operation::Resume
                | Operation::Suspend
        ) {
            self.invalidate_projection();
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .expect("a terminal session cannot exhaust commit sequences");
        match operation {
            Operation::Query(query) => query(self.backend.dest())?,
            Operation::History(draft, receipt) => {
                let result = self.write_history(&draft.canvas, draft.graphics);
                let reported = match &result {
                    Ok(()) => Ok(CommitReceipt {
                        sequence: self.sequence,
                        coverage: draft.coverage,
                    }),
                    Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
                };
                let _ = receipt.send(reported);
                result?;
            }
            Operation::PrintBatch(messages, output) => {
                if messages.iter().any(OutputMessage::affects_visible_output)
                    && self.previous.take().is_some()
                {
                    self.backend.clear_canvas()?;
                }
                super::print::write_batch(
                    self.backend.as_mut(),
                    output,
                    &mut self.appended_newline,
                    messages,
                )?;
            }
            Operation::Frame {
                mut draft,
                size,
                screen,
                replay,
                receipt,
            } => {
                let canvas = &draft.canvas;
                if let Some((width, height)) = size {
                    self.backend.set_size_from_resize_event(width, height);
                }
                self.backend.set_scrollback_replay(replay);
                let result: io::Result<()> = (|| {
                    if let Some(previous) = &self.previous {
                        self.backend
                            .restore_inline_rows_after_height_shrink(previous)?;
                    }
                    if !self.base_fullscreen {
                        let plan = self.backend.plan_screen_transition(screen);
                        if matches!(plan.transition, ScreenTransition::Leave) {
                            self.graphics.leave_alternate(self.backend.dest())?;
                        }
                        if self.backend.apply_screen_transition(plan)? {
                            if self.backend.is_fullscreen() {
                                self.saved_main = self.previous.take();
                            } else {
                                self.previous = self.saved_main.take();
                            }
                        }
                    }
                    let resolved = self.graphics.prepare(
                        canvas,
                        self.backend.is_fullscreen(),
                        self.backend.dest(),
                        std::mem::take(&mut draft.graphics),
                    )?;
                    if !draft.paint && self.previous.as_ref() == Some(&resolved) {
                        return self.backend.flush();
                    }
                    self.synchronization.enter();
                    let write = (|| {
                        self.synchronization.start(self.backend.dest())?;
                        let synchronization = &mut self.synchronization;
                        let graphics = &mut self.graphics;
                        let memory = self.shared.memory.clone();
                        let mut prepared_history = Vec::new();
                        self.backend.write_canvas_with_replay(
                            self.previous.as_ref(),
                            &resolved,
                            synchronization.context(),
                            &mut |replay, width, prepare, writer| {
                                if prepare {
                                    let result = synchronization.leave(writer).and_then(|()| {
                                        for mut history in replay(width) {
                                            history.output_allocations =
                                                history.reserve_output(&memory, false)?;
                                            let plan = super::graphics::GraphicsPlan::reserve(
                                                &history, &memory,
                                            )?;
                                            prepared_history.push((history, plan));
                                        }
                                        Ok(())
                                    });
                                    synchronization.enter();
                                    return result;
                                }
                                let end = synchronization.leave(writer);
                                let result = end.and_then(|()| {
                                    for (history, plan) in prepared_history.drain(..) {
                                        graphics.append_archival(&history, writer, Some(plan))?;
                                    }
                                    Ok(())
                                });
                                synchronization.enter();
                                result?;
                                synchronization.start(writer)
                            },
                        )?;
                        self.backend.position_cursor(canvas.cursor_declaration())
                    })();
                    let end = self.synchronization.leave(self.backend.dest());
                    write.and(end)?;
                    self.backend.flush()?;
                    self.previous = Some(resolved);
                    Ok(())
                })();
                if result.is_ok() {
                    let mut snapshot = EventCellSnapshot::from_canvas(&draft.canvas);
                    snapshot.frame = Some(Arc::new(CommittedFrame {
                        receipt: CommitReceipt {
                            sequence: self.sequence,
                            coverage: draft.coverage.clone(),
                        },
                        hits: draft.hits.into(),
                        views: draft.views,
                        _allocations: draft.allocations,
                    }));
                    let previous = self
                        .shared
                        .pending
                        .lock()
                        .expect("output projection is not poisoned")
                        .snapshot
                        .replace(snapshot);
                    drop(previous);
                }
                if let Some(receipt) = receipt {
                    let reported = match &result {
                        Ok(()) => Ok(FrameCommit::Committed(CommitReceipt {
                            sequence: self.sequence,
                            coverage: draft.coverage,
                        })),
                        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
                    };
                    let _ = receipt.send(reported);
                }
                result?;
                self.graphics
                    .commit(self.backend.is_fullscreen(), self.backend.dest())?;
            }
            Operation::Control(sequence, receipt) => {
                let result = self.backend.write_control_sequence(&sequence);
                if let Some(receipt) = receipt {
                    let reported = result
                        .as_ref()
                        .copied()
                        .map_err(|error| io::Error::new(error.kind(), error.to_string()));
                    let _ = receipt.send(reported);
                }
                result?;
            }
            Operation::Title(title) => self.backend.set_title(&title)?,
            Operation::Bytes(bytes, destination) => {
                if destination {
                    self.backend.dest().write_all(&bytes)?;
                } else {
                    self.backend.alt().write_all(&bytes)?;
                }
            }
            Operation::Flush(destination) => {
                if destination {
                    self.backend.dest().flush()?;
                } else {
                    self.backend.alt().flush()?;
                }
            }
            Operation::ClearCanvas => {
                self.backend.clear_canvas()?;
                self.previous = None;
            }
            Operation::ClearScreen => {
                self.backend.clear_screen()?;
                self.previous = None;
            }
            Operation::ClearTerminal => {
                self.backend.clear_terminal()?;
                self.previous = None;
                self.saved_main = None;
            }
            Operation::Mouse(enabled) => self.backend.set_mouse_capture(enabled)?,
            Operation::Keyboard(flags) => self.backend.set_keyboard_enhancement_flags(flags)?,
            Operation::RawMode(enabled) => {
                if self.backend.is_raw_mode_supported() {
                    self.backend.set_raw_mode_enabled(enabled)?;
                }
            }
            Operation::Resize => self.backend.reassert_after_resize()?,
            Operation::StdinResume => self.backend.reassert_after_stdin_resume()?,
            Operation::Suspend => {
                self.backend.suspend()?;
                self.graphics.invalidate();
                self.previous = None;
                self.saved_main = None;
                let mut pending = self
                    .shared
                    .pending
                    .lock()
                    .expect("output status is not poisoned");
                #[cfg(unix)]
                if let Some(signal) = &pending.resume_signal {
                    let _ = signal.take_resumed();
                }
                pending.resumed = true;
                drop(pending);
                self.shared.completed.wake();
            }
            Operation::Resume => {
                self.backend.reinitialize_after_resume()?;
                self.graphics.invalidate();
                self.previous = None;
                self.saved_main = None;
            }
            Operation::Release(receipt) => {
                let graphics = if self.backend.is_fullscreen() {
                    self.graphics.leave_alternate(self.backend.dest())
                } else {
                    Ok(())
                };
                let result = graphics
                    .and(self.backend.release_terminal_modes())
                    .and_then(|()| self.backend.flush());
                self.graphics.invalidate();
                self.previous = None;
                self.saved_main = None;
                let reported = result
                    .as_ref()
                    .copied()
                    .map_err(|error| io::Error::new(error.kind(), error.to_string()));
                if result.is_ok() {
                    let mut pending = self
                        .shared
                        .pending
                        .lock()
                        .expect("output admission is not poisoned");
                    if pending.admission == Admission::Releasing {
                        pending.admission = Admission::Released;
                    }
                }
                let _ = receipt.send(reported);
                result?;
            }
            Operation::Barrier(receipt) => {
                let result = self.backend.flush();
                let _ = receipt.send(
                    result
                        .as_ref()
                        .copied()
                        .map_err(|error| io::Error::new(error.kind(), error.to_string())),
                );
                result?;
            }
            Operation::Reacquire(receipt) => {
                let result = self
                    .backend
                    .reacquire_terminal_modes()
                    .and_then(|()| self.backend.flush());
                let reported = result
                    .as_ref()
                    .copied()
                    .map_err(|error| io::Error::new(error.kind(), error.to_string()));
                if result.is_ok() {
                    let mut pending = self
                        .shared
                        .pending
                        .lock()
                        .expect("output admission is not poisoned");
                    if pending.admission == Admission::Reacquiring {
                        pending.admission = Admission::Open;
                    }
                }
                let _ = receipt.send(reported);
                result?;
            }
        }
        Ok(())
    }

    fn write_history(
        &mut self,
        canvas: &Canvas,
        plan: super::graphics::GraphicsPlan,
    ) -> io::Result<()> {
        if self.backend.is_fullscreen() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "history rows require the main screen",
            ));
        }
        if self.previous.is_some() {
            self.backend.clear_canvas()?;
        }
        self.graphics
            .append_archival(canvas, self.backend.dest(), Some(plan))?;
        if let Some(previous) = &self.previous {
            self.backend
                .write_canvas(None, previous, OutputContext::default())?;
            self.backend
                .position_cursor(previous.cursor_declaration())?;
        }
        self.backend.flush()
    }
}

impl Drop for OutputOwner {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.shared.abandoned.store(true, Ordering::Release);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.cleanup()));
        }
    }
}
