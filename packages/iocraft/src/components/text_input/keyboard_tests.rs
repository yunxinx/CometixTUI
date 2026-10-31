use crate::prelude::*;
use futures::StreamExt;
use macro_rules_attribute::apply;
use smol_macros::test;

const CONTROL: KeyModifiers = KeyModifiers::CONTROL;
const NONE: KeyModifiers = KeyModifiers::NONE;
const ALT: KeyModifiers = KeyModifiers::ALT;

#[derive(Default, Props)]
struct EditingProbeProps {
    value: String,
    cursor: usize,
}

#[component]
fn EditingProbe(mut hooks: Hooks, props: &EditingProbeProps) -> impl Into<AnyElement<'static>> {
    let mut value = hooks.use_state(|| props.value.clone());
    let mut cursor = hooks.use_state(|| props.cursor);
    let mut shown = hooks.use_state(|| true);
    let mut focused = hooks.use_state(|| true);
    let mut handle = hooks.use_ref(|| {
        let mut handle = TextInputHandle::default();
        handle.set_cursor_offset(props.cursor);
        handle
    });
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut should_exit = hooks.use_state(|| false);
    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Key(KeyEvent { code, .. }) => match code {
            KeyCode::F(2) => {
                handle.write().set_cursor_offset(cursor.get());
                shown.set(!shown.get());
            }
            KeyCode::F(3) => {
                value.set(String::new());
                cursor.set(0);
                handle.write().set_cursor_offset(0);
            }
            KeyCode::F(4) => focused.set(!focused.get()),
            _ => {}
        },
        TerminalEvent::Resize(..) => should_exit.set(true),
        _ => {}
    });
    if should_exit.get() {
        system.exit();
    }
    element! {
        View(width: 100, flex_direction: FlexDirection::Column) {
            Text(content: format!("{:?}|{}", value.read().as_str(), cursor), wrap: TextWrap::NoWrap)
            #(shown.get().then(|| element! {
                View(width: 9) {
                    TextInput(
                        has_focus: focused.get(),
                        multiline: true,
                        auto_grow: true,
                        value: value.to_string(),
                        handle,
                        on_change: move |text| {
                            value.set(text);
                            cursor.set(handle.read().cursor_offset());
                        },
                        on_change_cursor_offset: move |offset| cursor.set(offset),
                    )
                }
            }))
        }
    }
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> TerminalEvent {
    let mut key = KeyEvent::new(KeyEventKind::Press, code);
    key.modifiers = modifiers;
    TerminalEvent::Key(key)
}

async fn assert_edits(
    value: &str,
    cursor: usize,
    events: impl IntoIterator<Item = TerminalEvent, IntoIter: Send + 'static>,
    expected: &[(&str, usize)],
) {
    let events = events.into_iter().chain([TerminalEvent::Resize(100, 24)]);
    let frames: Vec<_> = element!(EditingProbe(value, cursor))
        .mock_terminal_render_loop(MockTerminalConfig::with_delayed_events(events))
        .collect()
        .await;
    let mut edits: Vec<_> = frames
        .iter()
        .map(|canvas| canvas.to_string().lines().next().unwrap().to_string())
        .collect();
    edits.dedup();
    let expected: Vec<_> = expected
        .iter()
        .map(|(value, cursor)| format!("{value:?}|{cursor}"))
        .collect();
    assert_eq!(edits, expected);
}

#[apply(test!)]
async fn logical_line_edits_retain_the_last_nonempty_deletion_across_mounts() {
    use KeyCode::{Char, F};
    assert_edits(
        "first line wraps\nsecond",
        10,
        [
            key(Char('u'), CONTROL),
            key(Char('u'), CONTROL),
            key(F(2), NONE),
            key(F(2), NONE),
            key(Char('y'), CONTROL),
            key(Char('k'), CONTROL),
            key(Char('k'), CONTROL),
            key(Char('y'), CONTROL),
            key(Char('y'), CONTROL),
        ],
        &[
            ("first line wraps\nsecond", 10),
            (" wraps\nsecond", 0),
            ("first line wraps\nsecond", 10),
            ("first line\nsecond", 10),
            ("first linesecond", 10),
            ("first line\nsecond", 11),
            ("first line\n\nsecond", 12),
        ],
    )
    .await;
}

#[apply(test!)]
async fn word_aliases_share_unicode_punctuation_and_whitespace_boundaries() {
    use KeyCode::{Backspace, Char, Delete, Left, Right};
    let value = "one::中e\u{301} \n two";
    for (code, modifiers) in [(Char('b'), ALT), (Left, ALT), (Left, CONTROL)] {
        assert_edits(
            value,
            17,
            std::iter::repeat_n(key(code, modifiers), 6),
            &[
                (value, 17),
                (value, 14),
                (value, 8),
                (value, 5),
                (value, 3),
                (value, 0),
            ],
        )
        .await;
    }
    for (code, modifiers) in [(Char('f'), ALT), (Right, ALT), (Right, CONTROL)] {
        assert_edits(
            value,
            0,
            std::iter::repeat_n(key(code, modifiers), 6),
            &[
                (value, 0),
                (value, 3),
                (value, 5),
                (value, 8),
                (value, 11),
                (value, 17),
            ],
        )
        .await;
    }
    for (code, modifiers) in [(Char('w'), CONTROL), (Backspace, ALT), (Backspace, CONTROL)] {
        assert_edits(
            value,
            17,
            std::iter::repeat_n(key(code, modifiers), 6).chain([key(Char('y'), CONTROL)]),
            &[
                (value, 17),
                ("one::中e\u{301} \n ", 14),
                ("one::中", 8),
                ("one::", 5),
                ("one", 3),
                ("", 0),
                ("one", 3),
            ],
        )
        .await;
    }
    for (code, modifiers) in [(Char('d'), ALT), (Delete, ALT), (Delete, CONTROL)] {
        assert_edits(
            value,
            0,
            std::iter::repeat_n(key(code, modifiers), 6).chain([key(Char('y'), CONTROL)]),
            &[
                (value, 0),
                ("::中e\u{301} \n two", 0),
                ("中e\u{301} \n two", 0),
                ("e\u{301} \n two", 0),
                (" \n two", 0),
                ("", 0),
                (" \n two", 6),
            ],
        )
        .await;
    }
}

#[apply(test!)]
async fn grapheme_deletions_and_value_replacement_preserve_saved_text() {
    use KeyCode::{Backspace, Char, Delete, F, Home, Right};
    assert_edits(
        "saved",
        5,
        [
            key(Char('u'), CONTROL),
            TerminalEvent::Paste("replacement".into()),
            key(F(3), NONE),
            TerminalEvent::Paste("e\u{301}\u{1f468}\u{200d}\u{1f469}中".into()),
            key(Home, NONE),
            key(Char('d'), CONTROL),
            key(Delete, NONE),
            key(Right, NONE),
            key(Backspace, NONE),
            key(Char('d'), CONTROL),
            key(Char('y'), CONTROL),
        ],
        &[
            ("saved", 5),
            ("", 0),
            ("replacement", 11),
            ("", 0),
            ("e\u{301}\u{1f468}\u{200d}\u{1f469}中", 17),
            ("e\u{301}\u{1f468}\u{200d}\u{1f469}中", 0),
            ("\u{1f468}\u{200d}\u{1f469}中", 0),
            ("中", 0),
            ("中", 3),
            ("", 0),
            ("saved", 5),
        ],
    )
    .await;
}

#[apply(test!)]
async fn line_motion_ignores_wrapping_and_line_deletion_keeps_crlf_whole() {
    use KeyCode::{Char, End, Home};
    for (start, end, modifiers) in [(Home, End, NONE), (Char('a'), Char('e'), CONTROL)] {
        assert_edits(
            "first wraps wide\r\nlast",
            9,
            [
                key(start, modifiers),
                key(start, modifiers),
                key(end, modifiers),
                key(end, modifiers),
                key(Char('k'), CONTROL),
                key(Char('y'), CONTROL),
                key(Char('u'), CONTROL),
                key(Char('y'), CONTROL),
            ],
            &[
                ("first wraps wide\r\nlast", 9),
                ("first wraps wide\r\nlast", 0),
                ("first wraps wide\r\nlast", 16),
                ("first wraps widelast", 16),
                ("first wraps wide\r\nlast", 18),
                ("first wraps widelast", 16),
                ("first wraps wide\r\nlast", 18),
            ],
        )
        .await;
    }
}

#[apply(test!)]
async fn word_deletion_subdivides_unicode_words_without_splitting_graphemes() {
    assert_edits(
        "foo_bar can't 32.3",
        18,
        std::iter::repeat_n(key(KeyCode::Char('w'), CONTROL), 7),
        &[
            ("foo_bar can't 32.3", 18),
            ("foo_bar can't 32.", 17),
            ("foo_bar can't 32", 16),
            ("foo_bar can't ", 14),
            ("foo_bar can'", 12),
            ("foo_bar can", 11),
            ("foo_bar ", 8),
            ("", 0),
        ],
    )
    .await;
    assert_edits(
        "\u{1f1e8}\u{1f1f3}!\u{301}x",
        12,
        std::iter::repeat_n(key(KeyCode::Char('w'), CONTROL), 3)
            .chain([key(KeyCode::Char('y'), CONTROL)]),
        &[
            ("\u{1f1e8}\u{1f1f3}!\u{301}x", 12),
            ("\u{1f1e8}\u{1f1f3}!\u{301}", 11),
            ("\u{1f1e8}\u{1f1f3}", 8),
            ("", 0),
            ("\u{1f1e8}\u{1f1f3}", 8),
        ],
    )
    .await;
}

#[apply(test!)]
async fn unfocused_input_ignores_editing_aliases_and_keeps_saved_text() {
    use KeyCode::{Backspace, Char, Delete, End, F, Home, Left, Right};
    let shortcuts = [
        (Char('u'), CONTROL),
        (Char('k'), CONTROL),
        (Char('y'), CONTROL),
        (Char('w'), CONTROL),
        (Char('d'), CONTROL),
        (Char('a'), CONTROL),
        (Char('e'), CONTROL),
        (Char('b'), ALT),
        (Char('f'), ALT),
        (Char('d'), ALT),
        (Left, ALT),
        (Right, ALT),
        (Left, CONTROL),
        (Right, CONTROL),
        (Backspace, ALT),
        (Backspace, CONTROL),
        (Delete, ALT),
        (Delete, CONTROL),
        (Backspace, NONE),
        (Delete, NONE),
        (Home, NONE),
        (End, NONE),
    ];
    assert_edits(
        "saved",
        5,
        [
            key(Char('u'), CONTROL),
            TerminalEvent::Paste("draft".into()),
            key(F(4), NONE),
        ]
        .into_iter()
        .chain(shortcuts.map(|(code, modifiers)| key(code, modifiers)))
        .chain([key(F(4), NONE), key(Char('y'), CONTROL)]),
        &[("saved", 5), ("", 0), ("draft", 5), ("draftsaved", 10)],
    )
    .await;
}
