use crossterm::{ExecutableCommand, terminal};
use std::io::{self, Write};

pub(super) struct OutputWriter<'a> {
    pub(super) synchronization: &'a mut SynchronizedOutput,
    pub(super) backend: &'a mut dyn super::TerminalImpl,
    pub(super) render_destination: bool,
}

impl Write for OutputWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !bytes.is_empty() {
            self.synchronization.start(self.backend.dest())?;
        }
        if self.render_destination {
            self.backend.dest().write(bytes)
        } else {
            self.backend.alt().write(bytes)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.render_destination {
            self.backend.dest().flush()
        } else {
            self.backend.alt().flush()
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct OutputContext {
    pub(super) atomic: bool,
}

#[derive(Default, PartialEq, Eq)]
enum Transaction {
    #[default]
    Idle,
    CleanupRequired,
    Started,
}

pub(super) struct SynchronizedOutput {
    pub(super) emit_markers: bool,
    pub(super) atomic_capability: bool,
    depth: usize,
    transaction: Transaction,
}

impl SynchronizedOutput {
    pub(super) fn new(atomic_capability: bool) -> Self {
        Self {
            emit_markers: true,
            atomic_capability,
            depth: 0,
            transaction: Transaction::Idle,
        }
    }

    pub(super) fn enter(&mut self) {
        self.depth += 1;
    }

    pub(super) fn start(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        if !self.emit_markers || self.depth == 0 || self.transaction == Transaction::Started {
            return Ok(());
        }
        if self.transaction == Transaction::CleanupRequired {
            return Err(io::Error::other("synchronized output failed to begin"));
        }
        // A partial command or failed flush can already have changed terminal mode.
        self.transaction = Transaction::CleanupRequired;
        writer.execute(terminal::BeginSynchronizedUpdate)?;
        self.transaction = Transaction::Started;
        Ok(())
    }

    pub(super) fn context(&self) -> OutputContext {
        OutputContext {
            atomic: self.atomic_capability && self.transaction == Transaction::Started,
        }
    }

    pub(super) fn leave(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        self.depth = self
            .depth
            .checked_sub(1)
            .expect("synchronized scopes are balanced");
        if self.depth != 0 || self.transaction == Transaction::Idle {
            return Ok(());
        }
        self.transaction = Transaction::Idle;
        let result = writer.execute(terminal::EndSynchronizedUpdate).map(|_| ());
        if result.is_err() {
            // Keep the original error while trying to close a partially written end marker.
            let _ = writer.execute(terminal::EndSynchronizedUpdate);
        }
        result
    }
}
