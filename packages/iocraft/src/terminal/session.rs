use super::*;
use futures::future::BoxFuture;

/// Owns serialized terminal output for callers that prepare their own frames.
/// Dropping a receipt does not cancel an accepted operation. Await [`Self::finish`]
/// before returning the terminal to another application.
pub struct TerminalOutput {
    session: OutputSession,
}

impl TerminalOutput {
    /// Runs an exclusive startup query with raw input and the platform output adapter.
    /// The callback owns input reading for its duration. The returned future restores and
    /// flushes terminal modes before another input or output session may start.
    pub async fn probe<T: Send + 'static>(
        query: impl FnOnce(&mut dyn Write) -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        let mut session = OutputSession::new(
            super::platform::system_output(false)?,
            Box::new(io::sink()),
            false,
            false,
            Some(false),
            Output::Stdout,
        )?;
        session.enqueue(Operation::RawMode(true))?;
        let (sender, receiver) = oneshot::channel();
        session.enqueue(Operation::Query(Box::new(move |writer| {
            let result = query(writer).and_then(|value| writer.flush().map(|()| value));
            let status = result
                .as_ref()
                .map(|_| ())
                .map_err(|error| io::Error::new(error.kind(), error.to_string()));
            let _ = sender.send(result);
            status
        })))?;
        let result = receiver
            .await
            .map_err(|_| io::Error::other("terminal probe stopped"));
        let finish = session.finish().await;
        result
            .and_then(|result| result)
            .and_then(|value| finish.map(|()| value))
    }

    /// Creates an output owner using a supplied writer and known viewport dimensions.
    /// Input and keyboard negotiation remain with the caller of this frame-only interface.
    pub fn new(writer: impl Write + Send + 'static, columns: u16, rows: u16) -> io::Result<Self> {
        let mut session = OutputSession::new(
            Box::new(writer),
            Box::new(io::sink()),
            false,
            false,
            Some(false),
            Output::Stdout,
        )?;
        session.size = Some((columns, rows));
        Ok(Self { session })
    }

    /// Accepts a replaceable frame and reports its complete local commit or supersession.
    pub fn present(&mut self, draft: FrameDraft) -> BoxFuture<'static, io::Result<FrameCommit>> {
        let (sender, receiver) = oneshot::channel();
        let screen = draft
            .alternate
            .then_some(crate::context::AlternateScreenRequest {
                mouse_tracking: false,
            });
        let accepted = self.session.enqueue(Operation::Frame {
            draft: Box::new(draft),
            size: self.session.size,
            screen,
            replay: None,
            receipt: Some(sender),
        });
        Box::pin(async move {
            accepted?;
            receiver.await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal output is closed")
            })?
        })
    }

    /// Inserts history above the currently committed main-screen frame.
    pub fn append_history(
        &self,
        draft: HistoryDraft,
    ) -> BoxFuture<'static, io::Result<CommitReceipt>> {
        self.session.sender().append_history(draft)
    }

    /// Waits for previously accepted operations and their writer flush.
    pub fn flush(&mut self) -> BoxFuture<'static, io::Result<()>> {
        let (sender, receiver) = oneshot::channel();
        let accepted = self.session.enqueue(Operation::Barrier(sender));
        Box::pin(async move {
            accepted?;
            receiver.await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "terminal output is closed")
            })?
        })
    }

    /// Stops acceptance, settles accepted output, restores modes, and flushes the writer.
    pub async fn finish(&mut self) -> io::Result<()> {
        self.session.finish().await
    }
}
