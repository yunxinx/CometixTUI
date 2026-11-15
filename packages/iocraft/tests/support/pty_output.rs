use std::io::Write;
use std::sync::{Arc, Mutex};

pub struct OutputDecoder {
    terminal: Arc<Mutex<avt::Vt>>,
    input: Arc<Mutex<Box<dyn Write + Send>>>,
    pending_utf8: Vec<u8>,
    query_offset: usize,
}

impl OutputDecoder {
    pub fn new(terminal: Arc<Mutex<avt::Vt>>, input: Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        Self {
            terminal,
            input,
            pending_utf8: Vec::new(),
            query_offset: 0,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.pending_utf8.extend_from_slice(bytes);
        let length = match std::str::from_utf8(&self.pending_utf8) {
            Ok(text) => text.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => panic!("invalid terminal UTF-8: {error}"),
        };
        let mut terminal = self.terminal.lock().unwrap();
        for character in std::str::from_utf8(&self.pending_utf8[..length])
            .unwrap()
            .chars()
        {
            terminal.feed(character);
            const CURSOR_QUERY: &[u8] = b"\x1b[6n";
            if character == char::from(CURSOR_QUERY[self.query_offset]) {
                self.query_offset += 1;
            } else {
                self.query_offset = usize::from(character == '\x1b');
            }
            if self.query_offset == CURSOR_QUERY.len() {
                self.query_offset = 0;
                // ConPTY's inherited cursor requires a host reply during startup and resize.
                let cursor = terminal.cursor();
                let mut input = self.input.lock().unwrap();
                write!(input, "\x1b[{};{}R", cursor.row + 1, cursor.col + 1).unwrap();
                input.flush().unwrap();
            }
        }
        self.pending_utf8.drain(..length);
    }

    pub fn finish(self) {
        assert!(
            self.pending_utf8.is_empty(),
            "PTY ended inside a UTF-8 character"
        );
    }
}
