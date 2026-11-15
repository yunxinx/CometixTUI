use super::Clipboard;
use crate::terminal::{Allocation, OutputMemory};
use futures::channel::oneshot;
use futures::future::{BoxFuture, poll_fn};
use futures::task::AtomicWaker;
use std::future::Future;
use std::io::{self, Write};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll};

const PREPARING: u8 = 0;
const CANCELLED: u8 = 1;
const COMMITTING: u8 = 2;

#[cfg(all(test, feature = "unstable-output-streams"))]
mod tests;

#[derive(Default)]
struct Cancellation {
    state: AtomicU8,
    waker: AtomicWaker,
}

/// Explicit cancellation of a copy which has not started a clipboard side effect.
#[derive(Clone, Default)]
pub struct ClipboardCancellation(Arc<Cancellation>);

impl ClipboardCancellation {
    /// Cancels preparation. Returns false once a helper or terminal write has started.
    /// A committing copy drains all its transports before the next copy or handoff.
    pub fn cancel(&self) -> bool {
        let cancelled = self
            .0
            .state
            .compare_exchange(PREPARING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if cancelled {
            self.0.waker.wake();
        }
        cancelled
    }

    /// Reports whether the preparation was explicitly cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == CANCELLED
    }

    /// Waits for cancellation so a producer can cancel and drain its own pending work.
    pub async fn cancelled(&self) {
        poll_fn(|context| {
            self.0.waker.register(context.waker());
            if self.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await
    }
}

/// Complete clipboard text, with capacity charged to the terminal's output envelope.
/// It holds no page canvases. The final text still costs memory proportional to its size.
pub struct ClipboardBuffer {
    text: String,
    memory: Arc<OutputMemory>,
    allocation: Option<Arc<Allocation>>,
    cancellation: ClipboardCancellation,
}

impl ClipboardBuffer {
    /// The cancellation capability for this preparation; dropping it does not cancel.
    pub fn cancellation(&self) -> ClipboardCancellation {
        self.cancellation.clone()
    }

    /// Appends text after reserving any growth, including the overlap during reallocation.
    pub fn push_str(&mut self, text: &str) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let needed = self
            .text
            .len()
            .checked_add(text.len())
            .ok_or(io::ErrorKind::OutOfMemory)?;
        if needed > self.text.capacity() {
            let capacity = needed.max(self.text.capacity().saturating_mul(2)).max(1024);
            let allocation = self.memory.reserve(capacity)?;
            let mut replacement = String::new();
            replacement
                .try_reserve_exact(capacity)
                .map_err(io::Error::other)?;
            replacement.push_str(&self.text);
            self.text = replacement;
            self.allocation = Some(allocation);
        }
        self.text.push_str(text);
        Ok(())
    }
}

/// Costs of a complete copy, separate from the producer's page reconstruction workspace.
#[derive(Clone, Copy, Debug)]
pub struct ClipboardSummary {
    /// Number of Unicode scalar values copied.
    pub characters: usize,
    /// Retained capacity of the final clipboard string.
    pub result_bytes: usize,
    /// Maximum input clone retained by the sequential native or tmux helper.
    pub helper_bytes: usize,
    /// Maximum base64 encoding block; the complete protocol is never collected.
    pub protocol_bytes: usize,
}

/// Receipt for an accepted copy. Dropping it does not cancel the operation.
pub struct ClipboardRequest {
    receiver: oneshot::Receiver<io::Result<ClipboardSummary>>,
    cancellation: ClipboardCancellation,
}

impl ClipboardRequest {
    /// Separates explicit cancellation from the lifetime of the requesting view.
    pub fn cancellation(&self) -> ClipboardCancellation {
        self.cancellation.clone()
    }
}

impl Future for ClipboardRequest {
    type Output = io::Result<ClipboardSummary>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver)
            .poll(context)
            .map(|result| result.unwrap_or_else(|_| Err(io::ErrorKind::BrokenPipe.into())))
    }
}

type Producer =
    Box<dyn FnOnce(ClipboardBuffer) -> BoxFuture<'static, io::Result<ClipboardBuffer>> + Send>;

pub(crate) struct CopyPreparation {
    producer: Producer,
    clipboard: Option<Clipboard>,
    receipt: oneshot::Sender<io::Result<ClipboardSummary>>,
    cancellation: ClipboardCancellation,
    memory: Option<Arc<OutputMemory>>,
    pub(crate) retained_bytes: usize,
}

impl CopyPreparation {
    pub(crate) fn new<F, Fut>(producer: F, clipboard: Option<Clipboard>) -> (Self, ClipboardRequest)
    where
        F: FnOnce(ClipboardBuffer) -> Fut + Send + 'static,
        Fut: Future<Output = io::Result<ClipboardBuffer>> + Send + 'static,
    {
        let (receipt, receiver) = oneshot::channel();
        let cancellation = ClipboardCancellation::default();
        (
            Self {
                producer: Box::new(move |buffer| Box::pin(producer(buffer))),
                clipboard,
                receipt,
                cancellation: cancellation.clone(),
                memory: None,
                retained_bytes: size_of::<F>() + size_of::<Fut>() + 4096,
            },
            ClipboardRequest {
                receiver,
                cancellation,
            },
        )
    }

    pub(crate) fn admit(&mut self, memory: &Arc<OutputMemory>) {
        self.memory = Some(memory.clone());
    }

    pub(crate) fn reject(self, error: &io::Error) {
        let _ = self
            .receipt
            .send(Err(io::Error::new(error.kind(), error.to_string())));
    }

    pub(crate) fn execute(self, output: &mut dyn Write) -> io::Result<()> {
        let Some(memory) = self.memory else {
            self.reject(&io::Error::new(
                io::ErrorKind::BrokenPipe,
                "clipboard preparation requires a terminal output owner",
            ));
            return Ok(());
        };
        let buffer = ClipboardBuffer {
            text: String::new(),
            memory: memory.clone(),
            allocation: None,
            cancellation: self.cancellation.clone(),
        };
        let prepared = if self.cancellation.is_cancelled() {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            futures::executor::block_on((self.producer)(buffer))
        };
        let prepared = prepared.and_then(|buffer| {
            let helper_bytes = if self.clipboard.is_some() {
                buffer.text.len()
            } else {
                0
            };
            let helper = memory.reserve(helper_bytes)?;
            if self
                .cancellation
                .0
                .state
                .compare_exchange(PREPARING, COMMITTING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(io::ErrorKind::Interrupted.into());
            }
            Ok((buffer, helper, helper_bytes))
        });
        let (buffer, _helper, helper_bytes) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self.receipt.send(Err(error));
                return Ok(());
            }
        };
        let summary = ClipboardSummary {
            characters: if buffer.text.trim().is_empty() {
                0
            } else {
                buffer.text.chars().count()
            },
            result_bytes: buffer.text.capacity(),
            helper_bytes,
            protocol_bytes: 4096,
        };
        if buffer.text.trim().is_empty() {
            let _ = self.receipt.send(Ok(summary));
            return Ok(());
        }
        let result = write_copy(output, &buffer.text, self.clipboard.as_ref());
        let reported = result
            .as_ref()
            .map(|()| summary)
            .map_err(|error| io::Error::new(error.kind(), error.to_string()));
        let _ = self.receipt.send(reported);
        result
    }
}

pub(crate) fn write_copy(
    output: &mut dyn Write,
    text: &str,
    clipboard: Option<&Clipboard>,
) -> io::Result<()> {
    let prepared = clipboard.map(|clipboard| futures::executor::block_on(clipboard.prepare(text)));
    crate::ansi::write_clipboard(
        output,
        text,
        prepared
            .as_ref()
            .map_or(crate::ansi::MultiplexerPassthrough::None, |prepared| {
                prepared.multiplexer
            }),
        prepared.is_some_and(|prepared| prepared.is_kitty),
    )
    .and_then(|()| output.flush())
}
