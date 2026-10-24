use crossterm::{Command, QueueableCommand};
use std::{fmt, io, io::Write, ops::Deref, ops::DerefMut};

pub(super) fn write_ansi_command(
    writer: &mut (impl Write + ?Sized),
    command: impl Command,
) -> io::Result<()> {
    struct Ansi<C>(C);

    impl<C: Command> fmt::Display for Ansi<C> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            self.0.write_ansi(formatter)
        }
    }

    write!(writer, "{}", Ansi(command))
}

/// Keeps system command execution at the output boundary. Protocol tests use
/// ANSI serialization so commands target their captured stream, not the host console.
pub(super) struct TerminalWriter<'a> {
    inner: Box<dyn Write + Send + 'a>,
    #[cfg(test)]
    ansi_only: bool,
}

impl<'a> TerminalWriter<'a> {
    pub(super) fn new(inner: Box<dyn Write + Send + 'a>) -> Self {
        Self {
            inner,
            #[cfg(test)]
            ansi_only: false,
        }
    }

    #[cfg(test)]
    pub(super) fn ansi(inner: Box<dyn Write + Send + 'a>) -> Self {
        Self {
            inner,
            ansi_only: true,
        }
    }

    pub(super) fn queue(&mut self, command: impl Command) -> io::Result<&mut Self> {
        #[cfg(test)]
        if self.ansi_only {
            write_ansi_command(&mut *self.inner, command)?;
            return Ok(self);
        }
        self.inner.queue(command)?;
        Ok(self)
    }

    pub(super) fn execute(&mut self, command: impl Command) -> io::Result<&mut Self> {
        self.queue(command)?;
        self.inner.flush()?;
        Ok(self)
    }
}

impl<'a> Deref for TerminalWriter<'a> {
    type Target = dyn Write + Send + 'a;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl DerefMut for TerminalWriter<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
    }
}
