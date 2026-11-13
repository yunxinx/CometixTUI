use super::{ClipboardMultiplexer, Output, TerminalImpl};
use crossterm::{QueueableCommand, cursor};
use futures::channel::oneshot;
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

pub(crate) enum OutputMessage {
    WindowTitle(String),
    Stdout(String),
    StdoutNoNewline(String),
    StdoutClipboard(String, ClipboardMultiplexer),
    Copy(
        String,
        Option<crate::Clipboard>,
        oneshot::Sender<io::Result<()>>,
    ),
    StdoutControl(String),
    StdoutControlAcknowledged(String, oneshot::Sender<io::Result<()>>),
    Stderr(String),
    StderrNoNewline(String),
}

impl OutputMessage {
    pub(super) fn reject(self, error: &io::Error) {
        if let Self::Copy(_, _, receipt) | Self::StdoutControlAcknowledged(_, receipt) = self {
            let _ = receipt.send(Err(io::Error::new(error.kind(), error.to_string())));
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>()
            + match self {
                Self::Copy(text, _, _) => text.capacity().saturating_mul(2) + 4096,
                Self::WindowTitle(text)
                | Self::Stdout(text)
                | Self::StdoutNoNewline(text)
                | Self::Stderr(text)
                | Self::StderrNoNewline(text)
                | Self::StdoutClipboard(text, _)
                | Self::StdoutControl(text)
                | Self::StdoutControlAcknowledged(text, _) => text.capacity(),
            }
    }

    pub(crate) fn affects_visible_output(&self) -> bool {
        !matches!(
            self,
            OutputMessage::WindowTitle(_)
                | OutputMessage::StdoutClipboard(..)
                | OutputMessage::Copy(..)
                | OutputMessage::StdoutControl(_)
                | OutputMessage::StdoutControlAcknowledged(..)
        )
    }
}

fn write_terminal_text(output: &mut dyn Write, text: &str) -> io::Result<()> {
    let mut start = 0;
    for (offset, _) in text.match_indices('\n') {
        output.write_all(&text.as_bytes()[start..offset])?;
        if offset == 0 || text.as_bytes()[offset - 1] != b'\r' {
            output.write_all(b"\r")?;
        }
        output.write_all(b"\n")?;
        start = offset + 1;
    }
    output.write_all(&text.as_bytes()[start..])
}

fn skip_ansi_string_control(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let mut saw_escape = false;
    for ch in chars.by_ref() {
        if ch == '\u{7}' {
            break;
        }
        if saw_escape && ch == '\\' {
            break;
        }
        saw_escape = ch == '\u{1b}';
    }
}

fn skip_ansi_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(kind) = chars.next() else {
        return;
    };
    match kind {
        '[' => {
            for ch in chars.by_ref() {
                let code = ch as u32;
                if (0x40..=0x7e).contains(&code) {
                    break;
                }
            }
        }
        ']' | 'P' | '_' | '^' | 'X' => skip_ansi_string_control(chars),
        _ => {}
    }
}

fn advance_printable_width(col: u16, char_width: usize, terminal_width: Option<usize>) -> u16 {
    if char_width == 0 {
        return col;
    }

    let Some(terminal_width) = terminal_width.filter(|w| *w > 0) else {
        return (col as usize)
            .saturating_add(char_width)
            .min(u16::MAX as usize) as u16;
    };

    // `col == terminal_width` represents the VT pending-wrap state after a
    // printable ended exactly at the right margin. The next printable first
    // wraps to the following row before drawing at column 0.
    let start_col = if col as usize >= terminal_width {
        0
    } else {
        col as usize
    };
    let advanced = start_col.saturating_add(char_width);
    if advanced <= terminal_width {
        // Preserve `terminal_width` as the pending-wrap marker.
        advanced as u16
    } else {
        (advanced % terminal_width) as u16
    }
}

fn advance_terminal_column(col: u16, text: &str, terminal_width: Option<usize>) -> u16 {
    let mut col = col;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_ansi_escape(&mut chars),
            '\r' | '\n' => col = 0,
            '\t' => {
                let next_tab = ((col as usize / 8) + 1) * 8;
                let delta = next_tab.saturating_sub(col as usize);
                col = advance_printable_width(col, delta, terminal_width);
            }
            '\u{8}' => col = col.saturating_sub(1),
            _ if ch.is_control() => {}
            _ => {
                col = advance_printable_width(col, ch.width().unwrap_or(0), terminal_width);
            }
        }
    }
    col
}

pub(super) fn write_batch(
    terminal: &mut dyn TerminalImpl,
    output: Output,
    appended_newline: &mut Option<u16>,
    messages: Vec<OutputMessage>,
) -> io::Result<()> {
    let visible = messages.iter().any(OutputMessage::affects_visible_output);
    let width = terminal.size().map(|(width, _)| usize::from(width));
    if visible {
        if let Some(column) = *appended_newline {
            terminal
                .dest()
                .queue(cursor::MoveUp(1))?
                .queue(cursor::MoveRight(column))?;
        }
        terminal.dest().flush()?;
    }
    let mut column = appended_newline.unwrap_or(0);
    for message in messages {
        let (text, stdout, newline) = match message {
            OutputMessage::WindowTitle(title) => {
                terminal.set_window_title(&title)?;
                continue;
            }
            OutputMessage::Stdout(text) => (text, true, true),
            OutputMessage::StdoutNoNewline(text) => (text, true, false),
            OutputMessage::Stderr(text) => (text, false, true),
            OutputMessage::StderrNoNewline(text) => (text, false, false),
            OutputMessage::Copy(text, clipboard, receipt) => {
                let prepared = clipboard
                    .as_ref()
                    .map(|clipboard| futures::executor::block_on(clipboard.prepare(&text)));
                let result = crate::ansi::write_clipboard(
                    terminal.dest(),
                    &text,
                    prepared
                        .as_ref()
                        .map_or(crate::ansi::MultiplexerPassthrough::None, |prepared| {
                            prepared.multiplexer
                        }),
                    prepared.is_some_and(|prepared| prepared.is_kitty),
                )
                .and_then(|()| terminal.dest().flush());
                let reported = result
                    .as_ref()
                    .copied()
                    .map_err(|error| io::Error::new(error.kind(), error.to_string()));
                let _ = receipt.send(reported);
                result?;
                continue;
            }
            OutputMessage::StdoutClipboard(text, multiplexer) => {
                crate::ansi::osc52_clipboard_for_multiplexer(
                    terminal.dest(),
                    &text,
                    multiplexer.into(),
                )?;
                terminal.dest().flush()?;
                continue;
            }
            OutputMessage::StdoutControl(sequence) => {
                terminal.write_control_sequence(&sequence)?;
                continue;
            }
            OutputMessage::StdoutControlAcknowledged(sequence, receipt) => {
                let result = terminal.write_control_sequence(&sequence);
                let reported = result
                    .as_ref()
                    .copied()
                    .map_err(|error| io::Error::new(error.kind(), error.to_string()));
                let _ = receipt.send(reported);
                result?;
                continue;
            }
        };
        let destination = if stdout == matches!(output, Output::Stdout) {
            terminal.dest()
        } else {
            terminal.alt()
        };
        write_terminal_text(destination, &text)?;
        if newline {
            destination.write_all(b"\r\n")?;
            column = 0;
        } else {
            column = advance_terminal_column(column, &text, width);
        }
    }
    if visible {
        terminal.dest().flush()?;
        terminal.alt().flush()?;
        *appended_newline = if column > 0 {
            terminal.dest().write_all(b"\r\n")?;
            width
                .filter(|width| usize::from(column) >= *width)
                .map_or(Some(column), |_| None)
        } else {
            None
        };
        terminal.dest().flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn normalize_terminal_newlines(text: &str) -> String {
        let mut output = Vec::new();
        write_terminal_text(&mut output, text).unwrap();
        String::from_utf8(output).unwrap()
    }
    #[test]
    fn test_normalize_terminal_newlines_preserves_carriage_return_alignment() {
        assert_eq!(normalize_terminal_newlines("plain"), "plain");
        assert_eq!(normalize_terminal_newlines("a\nb"), "a\r\nb");
        assert_eq!(normalize_terminal_newlines("a\r\nb"), "a\r\nb");
        assert_eq!(normalize_terminal_newlines("\nDone!"), "\r\nDone!");
    }

    #[test]
    fn test_advance_terminal_column_tracks_print_continuation_without_dsr() {
        assert_eq!(advance_terminal_column(0, "Working...", None), 10);
        assert_eq!(advance_terminal_column(10, ".", None), 11);
        assert_eq!(advance_terminal_column(11, "\nDone!", None), 5);
        assert_eq!(advance_terminal_column(0, "中\t", None), 8);
    }

    #[test]
    fn test_advance_terminal_column_wraps_with_terminal_width() {
        assert_eq!(advance_terminal_column(0, "abcdef", Some(10)), 6);
        assert_eq!(advance_terminal_column(6, "abcdef", Some(10)), 2);
        assert_eq!(advance_terminal_column(0, "1234567890", Some(10)), 10);
        assert_eq!(advance_terminal_column(10, ".", Some(10)), 1);
        assert_eq!(advance_terminal_column(0, "12345\nabc", Some(10)), 3);
    }

    #[test]
    fn test_advance_terminal_column_ignores_ansi_controls() {
        assert_eq!(advance_terminal_column(0, "\x1b[31mred\x1b[0m", None), 3);
        assert_eq!(
            advance_terminal_column(
                0,
                "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\",
                None,
            ),
            4
        );
        assert_eq!(advance_terminal_column(3, "\x08!", None), 3);
    }
}
