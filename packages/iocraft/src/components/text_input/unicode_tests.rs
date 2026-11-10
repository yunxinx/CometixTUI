use super::TextBuffer;
use crate::prelude::*;
use futures::{Stream, StreamExt};
use macro_rules_attribute::apply;
use smol_macros::test;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default, Props)]
struct WrappedCursorProps {
    handle_out: Arc<Mutex<Option<Ref<TextInputHandle>>>>,
}

#[component]
fn WrappedCursor(mut hooks: Hooks, props: &WrappedCursorProps) -> impl Into<AnyElement<'static>> {
    let handle = hooks.use_ref(|| {
        let mut handle = TextInputHandle::default();
        handle.set_cursor_offset(4);
        handle
    });
    *props.handle_out.lock().unwrap() = Some(handle);
    let mut width = hooks.use_state(|| 5_u16);
    let mut should_exit = hooks.use_state(|| false);
    let mut system = hooks.use_context_mut::<SystemContext>();
    hooks.use_terminal_events(move |event| match event {
        TerminalEvent::Resize(columns, _) => width.set(columns),
        TerminalEvent::Key(KeyEvent {
            code: KeyCode::Esc, ..
        }) => should_exit.set(true),
        _ => {}
    });
    if should_exit.get() {
        system.exit();
    }
    element! {
        View(width: u32::from(width.get()), height: 3) {
            TextInput(
                value: "abcd\nabcdefgh".to_string(), multiline: true,
                has_focus: true, physical_cursor: true, handle,
            )
        }
    }
}

async fn expect_cursor(
    frames: &mut (impl Stream<Item = Canvas> + Unpin),
    position: (usize, usize),
    text: &str,
) {
    while let Some(frame) = frames.next().await {
        if frame.to_string() == text
            && frame
                .cursor_declaration()
                .is_some_and(|cursor| (cursor.x, cursor.y) == position)
        {
            return;
        }
    }
    panic!("input ended before cursor reached {position:?}");
}

#[apply(test!)]
async fn test_text_buffer_keeps_hard_break_offsets_and_soft_wrap_affinity() {
    for separator in [
        "\r\n", "\r", "\n", "\u{b}", "\u{c}", "\u{85}", "\u{2028}", "\u{2029}",
    ] {
        let text = format!("ab{separator}{separator}cd{separator}");
        let buffer = TextBuffer::new(&text, 10);
        let rows: Vec<_> = buffer
            .rows
            .iter()
            .map(|row| {
                (
                    &text[row.offset..row.offset + row.len],
                    row.offset,
                    row.width,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("ab", 0, 2),
                ("", 2 + separator.len(), 0),
                ("cd", 2 + 2 * separator.len(), 2),
                ("", text.len(), 0),
            ],
            "{separator:?}"
        );
    }
    let empty = TextBuffer::new("", 4);
    assert_eq!(empty.row_count(), 1);
    assert_eq!(empty.row_column_for_offset(0), (0, 0));

    let handle_out = Arc::new(Mutex::new(None));
    let mut app = element!(WrappedCursor(handle_out: handle_out.clone()));
    let render = async {
        let (sender, events) = futures::channel::mpsc::unbounded();
        let frames = app.mock_terminal_render_loop(MockTerminalConfig::with_events(events));
        futures::pin_mut!(frames);
        let text = "abcd\nabcd\nefgh\n";
        expect_cursor(&mut frames, (4, 0), text).await;
        let mut handle = handle_out.lock().unwrap().expect("mounted input handle");
        let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        sender.unbounded_send(key(KeyCode::Down)).unwrap();
        expect_cursor(&mut frames, (4, 1), text).await;
        assert_eq!(handle.read().cursor_offset(), 9);
        sender.unbounded_send(key(KeyCode::Down)).unwrap();
        expect_cursor(&mut frames, (4, 2), text).await;
        assert_eq!(handle.read().cursor_offset(), 13);
        sender.unbounded_send(key(KeyCode::Up)).unwrap();
        expect_cursor(&mut frames, (4, 1), text).await;
        assert_eq!(handle.read().cursor_offset(), 9);

        // An explicit request for the same byte offset selects the following row's start.
        handle.write().set_cursor_offset(9);
        expect_cursor(&mut frames, (0, 2), text).await;
        assert_eq!(handle.read().cursor_offset(), 9);

        handle.write().set_cursor_offset(4);
        expect_cursor(&mut frames, (4, 0), text).await;
        sender.unbounded_send(key(KeyCode::Down)).unwrap();
        expect_cursor(&mut frames, (4, 1), text).await;
        sender.unbounded_send(TerminalEvent::Resize(10, 3)).unwrap();
        expect_cursor(&mut frames, (4, 1), "abcd\nabcdefgh\n\n").await;
        assert_eq!(handle.read().cursor_offset(), 9);
        // Widening removed the soft boundary; shrinking again must not revive its old affinity.
        sender.unbounded_send(TerminalEvent::Resize(5, 3)).unwrap();
        expect_cursor(&mut frames, (0, 2), text).await;
        assert_eq!(handle.read().cursor_offset(), 9);
        sender.unbounded_send(key(KeyCode::Esc)).unwrap();
        while frames.next().await.is_some() {}
    };
    smol::future::or(render, async {
        smol::Timer::after(Duration::from_secs(3)).await;
        panic!("wrapped cursor did not reach its expected row");
    })
    .await;
}
