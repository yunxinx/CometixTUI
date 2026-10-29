use super::*;

#[test]
fn os_raw_mode_failure_leaves_input_modes_untouched() {
    let output = TestWriter::default();
    let mut terminal = new_inline_term(output.clone(), 0);
    terminal.supports_keyboard_enhancement = true;
    terminal.mouse_capture = true;

    let error = terminal
        .apply_raw_mode_enabled_with(true, |enabled| {
            assert!(enabled);
            Err(io::Error::other("OS raw mode unavailable"))
        })
        .unwrap_err();

    assert_eq!(error.to_string(), "OS raw mode unavailable");
    assert!(output.buf.lock().unwrap().is_empty());
    assert!(!terminal.raw_mode_enabled);
    assert!(!terminal.enabled_keyboard_enhancement);
}

struct ModeWriter {
    output: TestWriter,
    fail_flush_at: usize,
    flushes: usize,
}

impl Write for ModeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        if self.flushes == self.fail_flush_at {
            Err(io::Error::other("mode output failed"))
        } else {
            Ok(())
        }
    }
}

fn terminal_with_mode_failure(output: TestWriter, fail_flush_at: usize) -> StdTerminal<'static> {
    let mut terminal = new_inline_term(TestWriter::default(), 0);
    terminal.dest = TerminalWriter::ansi(Box::new(ModeWriter {
        output,
        fail_flush_at,
        flushes: 0,
    }));
    terminal.supports_keyboard_enhancement = true;
    terminal.mouse_capture = true;
    terminal
}

fn assert_input_modes_restored(output: &str) {
    for sequence in ["\x1b[?2004l", "\x1b[?1004l", "\x1b[?1000l", "\x1b[<1u"] {
        assert!(
            output.contains(sequence),
            "missing {sequence:?} in {output:?}"
        );
    }
}

#[test]
fn input_mode_output_failures_roll_back_os_and_terminal_modes() {
    // Each enable command can reach the terminal before its flush fails.
    for fail_flush_at in 1..=4 {
        let output = TestWriter::default();
        let mut terminal = terminal_with_mode_failure(output.clone(), fail_flush_at);
        let mut os_modes = Vec::new();
        let error = terminal
            .apply_raw_mode_enabled_with(true, |enabled| {
                os_modes.push(enabled);
                Ok(())
            })
            .unwrap_err();

        assert_eq!(error.to_string(), "mode output failed");
        assert_eq!(os_modes, [true, false]);
        assert!(!terminal.raw_mode_enabled);
        assert!(!terminal.enabled_keyboard_enhancement);
        let bytes = output.buf.lock().unwrap().clone();
        assert_input_modes_restored(&String::from_utf8(bytes).unwrap());
    }
}

#[test]
fn input_mode_rollback_continues_after_os_cleanup_failure_and_can_retry() {
    let output = TestWriter::default();
    let mut terminal = terminal_with_mode_failure(output.clone(), 4);
    let mut os_modes = Vec::new();
    let error = terminal
        .apply_raw_mode_enabled_with(true, |enabled| {
            os_modes.push(enabled);
            if enabled {
                Ok(())
            } else {
                Err(io::Error::other("OS cleanup failed"))
            }
        })
        .unwrap_err();

    assert_eq!(error.to_string(), "mode output failed");
    assert_eq!(os_modes, [true, false]);
    assert!(
        terminal.raw_mode_enabled,
        "failed cleanup must remain retryable"
    );
    let bytes = output.buf.lock().unwrap().clone();
    assert_input_modes_restored(&String::from_utf8(bytes).unwrap());

    terminal
        .apply_raw_mode_enabled_with(false, |enabled| {
            assert!(!enabled);
            Ok(())
        })
        .unwrap();
    assert!(!terminal.raw_mode_enabled);
    assert!(!terminal.enabled_keyboard_enhancement);
    let bytes = output.buf.lock().unwrap().clone();
    assert_eq!(
        String::from_utf8(bytes)
            .unwrap()
            .matches("\x1b[<1u")
            .count(),
        1
    );
}
