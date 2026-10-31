use super::*;

#[test]
fn enhanced_keys_preserve_codes_modifiers_and_event_kinds() {
    for (sequence, code, modifiers, kind) in [
        (
            "\x1b[A",
            KeyCode::Up,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ),
        (
            "\x1b[1;1:1A",
            KeyCode::Up,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ),
        (
            "\x1b[1;1:2A",
            KeyCode::Up,
            KeyModifiers::empty(),
            KeyEventKind::Repeat,
        ),
        (
            "\x1b[1;1:3A",
            KeyCode::Up,
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ),
        (
            "\x1b[1;1:1D",
            KeyCode::Left,
            KeyModifiers::empty(),
            KeyEventKind::Press,
        ),
        (
            "\x1b[1;1:3D",
            KeyCode::Left,
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ),
        (
            "\x1b[1;5:2C",
            KeyCode::Right,
            KeyModifiers::CONTROL,
            KeyEventKind::Repeat,
        ),
        (
            "\x1b[3;3:3~",
            KeyCode::Delete,
            KeyModifiers::ALT,
            KeyEventKind::Release,
        ),
        (
            "\x1b[13;2:1u",
            KeyCode::Enter,
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ),
        (
            "\x1b[97;1:3u",
            KeyCode::Char('a'),
            KeyModifiers::empty(),
            KeyEventKind::Release,
        ),
    ] {
        for split in 0..=sequence.len() {
            let mut parser = TerminalInputParser::new();
            let mut events = parser.feed_events(&sequence[..split]);
            events.extend(parser.feed_events(&sequence[split..]));
            events.extend(parser.flush_events());
            assert!(
                matches!(events.as_slice(), [TerminalEvent::Key(key)] if key.code == code && key.modifiers == modifiers && key.kind == kind),
                "{sequence:?}, split {split}: {events:?}"
            );
        }
    }
}

#[test]
fn device_attributes_accept_empty_parameters_without_becoming_keys() {
    for (sequence, expected) in [
        ("\x1b[?62;52c", vec![62, 52]),
        ("\x1b[?62;52;c", vec![62, 52, 0]),
        ("\x1b[?62;;52;c", vec![62, 0, 52, 0]),
    ] {
        for split in 0..=sequence.len() {
            let mut parser = TerminalInputParser::new();
            let mut events = parser.feed_events(&sequence[..split]);
            events.extend(parser.feed_events(&sequence[split..]));
            assert!(
                matches!(events.as_slice(), [TerminalEvent::Response(TerminalResponse::Da1 { params })] if params == &expected),
                "{sequence:?}, split {split}: {events:?}"
            );
        }
    }
}

#[test]
fn unsupported_control_sequences_are_not_editor_text() {
    for sequence in [
        "\x1b[?999;1z",
        "\x1b]999;payload\x07",
        "\x1bPunknown\x1b\\",
        "\x1b_Gi=1;OK\x1b\\",
        "\x1b[?",
        "\x1b[1;1:9A",
    ] {
        let mut parser = TerminalInputParser::new();
        let mut events = parser.feed_events(sequence);
        events.extend(parser.flush_events());
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, TerminalEvent::Key(_))),
            "{sequence:?}: {events:?}"
        );
    }
    let literal = "\x1b[1;1:1D\x1b[?62;52;c";
    let mut parser = TerminalInputParser::new();
    let events = parser.feed_events(&format!("\x1b[200~{literal}\x1b[201~"));
    assert!(matches!(events.as_slice(), [TerminalEvent::Paste(text)] if text == literal));
}
