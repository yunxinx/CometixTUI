#[cfg(unix)]
use std::io::IsTerminal;
use std::io::{self, Write};

#[cfg(unix)]
struct TerminalFile(std::fs::File);

#[cfg(unix)]
impl Write for TerminalFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd;

        match self.0.write(bytes) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut descriptor = libc::pollfd {
                    fd: self.0.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // The output owner must regain control promptly when the reader stalls.
                // SAFETY: descriptor is live and poll only accesses this one entry.
                match unsafe { libc::poll(&mut descriptor, 1, 2) } {
                    -1 => Err(io::Error::last_os_error()),
                    0 => Err(error),
                    _ => self.0.write(bytes),
                }
            }
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub(crate) fn system_output(stderr: bool) -> io::Result<Box<dyn Write + Send>> {
    #[cfg(unix)]
    {
        use std::ffi::{CStr, OsStr};
        use std::os::unix::{ffi::OsStrExt, fs::OpenOptionsExt};
        let descriptor = if stderr {
            libc::STDERR_FILENO
        } else {
            libc::STDOUT_FILENO
        };
        let terminal = if stderr {
            io::stderr().is_terminal()
        } else {
            io::stdout().is_terminal()
        };
        if terminal {
            let mut path = [0; 4096];
            // SAFETY: the live standard descriptor and writable path buffer are valid.
            let result = unsafe { libc::ttyname_r(descriptor, path.as_mut_ptr(), path.len()) };
            if result != 0 {
                return Err(io::Error::from_raw_os_error(result));
            }
            // SAFETY: successful ttyname_r returns a NUL-terminated path in this buffer.
            let path = unsafe { CStr::from_ptr(path.as_ptr()) };
            let file = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(OsStr::from_bytes(path.to_bytes()))?;
            return Ok(Box::new(TerminalFile(file)));
        }
    }
    if stderr {
        Ok(Box::new(io::stderr()))
    } else {
        Ok(Box::new(io::stdout()))
    }
}
