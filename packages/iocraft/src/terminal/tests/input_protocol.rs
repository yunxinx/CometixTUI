use super::*;

#[test]
fn enter_encodings_preserve_submission_and_newline_keys() {
    for (sequence, code, modifiers) in [
        ("\r", KeyCode::Enter, KeyModifiers::NONE),
        ("\n", KeyCode::Char('j'), KeyModifiers::CONTROL),
        ("\x1b\r", KeyCode::Enter, KeyModifiers::ALT),
        ("\x1b[13;2u", KeyCode::Enter, KeyModifiers::SHIFT),
        ("\x1b[13;3u", KeyCode::Enter, KeyModifiers::ALT),
        ("\x1b[13;5u", KeyCode::Enter, KeyModifiers::CONTROL),
        ("\x1b[106;5u", KeyCode::Char('j'), KeyModifiers::CONTROL),
        ("\x1b[27;2;13~", KeyCode::Enter, KeyModifiers::SHIFT),
        ("\x1b[27;3;13~", KeyCode::Enter, KeyModifiers::ALT),
        ("\x1bOM", KeyCode::Enter, KeyModifiers::NONE),
        ("\x1b[57414;2u", KeyCode::Enter, KeyModifiers::SHIFT),
    ] {
        let chunks = (0..=sequence.len())
            .map(|split| vec![&sequence[..split], &sequence[split..]])
            .chain(std::iter::once(
                (0..sequence.len())
                    .map(|index| &sequence[index..index + 1])
                    .collect(),
            ));
        for chunks in chunks {
            let mut parser = TerminalInputParser::new();
            let mut events = Vec::new();
            for chunk in chunks {
                events.extend(parser.feed_events(chunk));
            }
            events.extend(parser.flush_events());
            assert!(
                matches!(events.as_slice(), [TerminalEvent::Key(key)] if key.code == code && key.modifiers == modifiers && key.kind == KeyEventKind::Press),
                "{sequence:?}: {events:?}"
            );
        }
    }
}

#[test]
fn disambiguated_keypad_and_function_keys_keep_legacy_navigation() {
    for (sequence, code) in [
        ("\x1b[57416u", KeyCode::Char(',')),
        ("\x1b[57417u", KeyCode::Left),
        ("\x1b[57418u", KeyCode::Right),
        ("\x1b[57419u", KeyCode::Up),
        ("\x1b[57420u", KeyCode::Down),
        ("\x1b[57421u", KeyCode::PageUp),
        ("\x1b[57422u", KeyCode::PageDown),
        ("\x1b[57423u", KeyCode::Home),
        ("\x1b[57424u", KeyCode::End),
        ("\x1b[57425u", KeyCode::Insert),
        ("\x1b[57426u", KeyCode::Delete),
        ("\x1b[57427u", KeyCode::KeypadBegin),
        ("\x1b[1;1:1P", KeyCode::F(1)),
        ("\x1b[1;1:1Q", KeyCode::F(2)),
        ("\x1b[13;1:1~", KeyCode::F(3)),
        ("\x1b[1;1:1S", KeyCode::F(4)),
    ] {
        for split in 0..=sequence.len() {
            let mut parser = TerminalInputParser::new();
            let mut events = parser.feed_events(&sequence[..split]);
            events.extend(parser.feed_events(&sequence[split..]));
            assert!(
                matches!(events.as_slice(), [TerminalEvent::Key(key)] if key.code == code && key.modifiers.is_empty() && key.kind == KeyEventKind::Press),
                "{sequence:?}: {events:?}"
            );
        }
    }
}

#[test]
fn enhanced_keys_preserve_codes_modifiers_and_event_kinds() {
    for (sequence, code, modifiers, kind) in [
        (
            "\x1b[1;2:1P",
            KeyCode::F(1),
            KeyModifiers::SHIFT,
            KeyEventKind::Press,
        ),
        (
            "\x1b[1;3:2Q",
            KeyCode::F(2),
            KeyModifiers::ALT,
            KeyEventKind::Repeat,
        ),
        (
            "\x1b[1;5:3S",
            KeyCode::F(4),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        ),
        (
            "\x1b[57417;2:2u",
            KeyCode::Left,
            KeyModifiers::SHIFT,
            KeyEventKind::Repeat,
        ),
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
