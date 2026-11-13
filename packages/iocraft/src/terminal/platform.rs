#[cfg(unix)]
use std::io::IsTerminal;
use std::io::{self, Write};

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
            return Ok(Box::new(file));
        }
    }
    if stderr {
        Ok(Box::new(io::stderr()))
    } else {
        Ok(Box::new(io::stdout()))
    }
}
