use crate::{
    AnyElement, CanvasTextStyle, Color, Component, ComponentDrawer, ComponentUpdater, HandlerMut,
    Hook, Hooks, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, LayoutStyle, Overflow, Position,
    Props, Size, StyleOverlay, TerminalEvent, Weight, component,
    components::{TextDecoration, TextWrap, View},
    element,
    hooks::{Ref, State, UseMemo, UseRef, UseState, UseTerminalEvents},
    segmented_string::SegmentedString,
};
use core::{
    pin::Pin,
    task::{Poll, Waker},
};
use std::{
    ops::Range,
    sync::{Arc, OnceLock},
};
use unicode_segmentation::UnicodeSegmentation;

mod word_boundaries;

#[cfg(test)]
mod keyboard_tests;
#[cfg(test)]
mod style_tests;

trait UseCursorOverlay {
    fn use_cursor_overlay(&mut self) -> &mut CursorOverlayState;
}

impl UseCursorOverlay for Hooks<'_, '_> {
    fn use_cursor_overlay(&mut self) -> &mut CursorOverlayState {
        self.use_hook(CursorOverlayState::default)
    }
}

#[derive(Default)]
struct CursorOverlayState {
    col: u16,
    row: u16,
    char_width: u16,
    active: bool,
    physical_cursor: bool,
}

impl Hook for CursorOverlayState {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        if !self.active {
            return;
        }
        if !self.physical_cursor {
            let overlay = StyleOverlay {
                invert: Some(true),
                ..Default::default()
            };
            let mut canvas = drawer.canvas();
            for offset in 0..self.char_width {
                canvas.set_overlay(
                    self.col as isize + offset as isize,
                    self.row as isize,
                    overlay,
                );
            }
        }
        drawer
            .canvas()
            .declare_cursor(self.col as isize, self.row as isize, self.physical_cursor);
    }
}

/// A handle which can be used for imperative control of a [`TextInput`] component.
///
/// The handle retains the most recent nonempty line or word deletion for Ctrl+Y. Keep it in
/// a parent component to preserve that text across input unmounts and value replacements.
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn FormField(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// # let mut value = hooks.use_state(|| "".to_string());
/// # let initial_cursor_position = 0;
/// let mut handle = hooks.use_ref_default::<TextInputHandle>();
///
/// hooks.use_effect(
///     move || handle.write().set_cursor_offset(initial_cursor_position),
///     (),
/// );
///
/// element! {
///     View(
///         background_color: Color::DarkGrey,
///         width: 30,
///     ) {
///         TextInput(
///             has_focus: true,
///             value: value.to_string(),
///             on_change: move |new_value| value.set(new_value),
///             handle,
///         )
///     }
/// }
/// # }
/// ```
#[derive(Default)]
pub struct TextInputHandle {
    inner: Option<TextInputHandleInner>,
    /// An offset set while no input was mounted, applied by the next one to mount.
    pending_cursor_offset: Option<usize>,
    deleted_text: String,
}

struct TextInputHandleInner {
    cursor_offset: State<usize>,
    requested_cursor_offset: State<Option<usize>>,
}

impl TextInputHandle {
    /// Sets the cursor position to the specified offset.
    ///
    /// The offset is in bytes, not characters. Offsets past the end are clamped to the value's
    /// length. An offset inside a grapheme cluster is rounded down to that cluster's start.
    /// Set while the input is unmounted, the offset applies when an input mounts with this
    /// handle.
    pub fn set_cursor_offset(&mut self, offset: usize) {
        match &mut self.inner {
            Some(inner) if inner.requested_cursor_offset.try_get().is_some() => {
                inner.requested_cursor_offset.set(Some(offset));
            }
            _ => self.pending_cursor_offset = Some(offset),
        }
    }

    /// Gets the current cursor offset.
    ///
    /// The offset is in bytes, not characters.
    pub fn cursor_offset(&self) -> usize {
        self.inner
            .as_ref()
            .and_then(|inner| inner.cursor_offset.try_get())
            .or(self.pending_cursor_offset)
            .unwrap_or(0)
    }
}

/// Style overrides for a UTF-8 byte range in a [`TextInput`]'s current value.
///
/// Ranges must be nonempty, ordered, nonoverlapping, and bounded by extended grapheme
/// clusters in the current value. Invalid ranges panic. Styles affect drawing only;
/// wrapping, scrolling, editing, and cursor placement use the unchanged value.
/// If bidi reordering combines source graphemes, the resulting grapheme uses the
/// style of its first source character.
#[derive(Clone, Debug, PartialEq)]
pub struct TextInputStyleRange {
    /// The byte range to style, including any whitespace within it.
    pub range: Range<usize>,
    /// Overrides applied to the input's base style, beneath its cursor overlay.
    pub style: StyleOverlay,
}

/// The props which can be passed to the [`TextInput`] component.
#[non_exhaustive]
#[derive(Default, Props)]
pub struct TextInputProps {
    /// The color to make the text.
    pub color: Option<Color>,

    /// The weight (boldness) of the text.
    pub weight: Weight,

    /// The text decoration.
    pub decoration: TextDecoration,

    /// Whether the text is italicized.
    pub italic: bool,

    /// Whether the foreground and background colors are inverted.
    pub invert: bool,

    /// The current value.
    pub value: String,

    /// Style ranges derived from [`Self::value`]. Supply updated ranges with every value
    /// change; offsets are not tracked across edits. See [`TextInputStyleRange`].
    pub style_ranges: Vec<TextInputStyleRange>,

    /// True if the input has focus and should process keyboard input.
    pub has_focus: bool,

    /// The handler to invoke when the value changes.
    pub on_change: HandlerMut<'static, String>,

    /// The handler to invoke when input moves the cursor, with the new offset in bytes. An edit
    /// reports its value through [`Self::on_change`] first, so a parent that mirrors both sees
    /// them together, in the same event.
    pub on_change_cursor_offset: HandlerMut<'static, usize>,

    /// If true, the input will fill 100% of the height of its container and handle multiline input.
    /// If false, line breaks in pasted text are replaced with spaces.
    pub multiline: bool,

    /// An optional handle which can be used for imperative control of the input.
    pub handle: Option<Ref<TextInputHandle>>,

    /// When `true`, uses the terminal's native block cursor (**ratatui model**) instead
    /// of rendering the cursor via [`StyleOverlay`] (**ink model**, the default).
    ///
    /// - **ratatui model** (`true`): the terminal guarantees cursor contrast and
    ///   respects the user's cursor shape/color preferences.
    /// - **ink model** (`false`, default): the cursor is rendered as an inverted cell.
    ///   The physical cursor is positioned for IME but hidden.
    pub physical_cursor: bool,

    /// If true (requires [`Self::multiline`]), the input auto-grows its height
    /// to fit wrapped content instead of filling its container's height.
    pub auto_grow: bool,

    /// Maximum visible rows when auto-growing. Additional rows scroll with the cursor.
    pub max_height: Option<u16>,
}

trait UseSize<'a> {
    /// The size the input was last drawn at. A render that laid the text out at another size
    /// (the first one sees zero) is redone before its frame is written; `uses_height` says
    /// whether the layout read the height as well as the width.
    fn use_size(&mut self, uses_height: bool) -> (u16, u16);
}

impl<'a> UseSize<'a> for Hooks<'a, '_> {
    fn use_size(&mut self, uses_height: bool) -> (u16, u16) {
        let waker = self.component_waker().cloned();
        let hook = self.use_hook(UseSizeImpl::default);
        if hook.waker.is_none() {
            hook.waker = waker;
        }
        hook.rendered = Some((hook.size.0, uses_height.then_some(hook.size.1)));
        hook.size
    }
}

#[derive(Default)]
struct UseSizeImpl {
    size: (u16, u16),
    /// The width, and the height when it was read, of the last render's layout.
    rendered: Option<(u16, Option<u16>)>,
    resized: bool,
    waker: Option<Waker>,
}

impl Hook for UseSizeImpl {
    fn poll_change(mut self: Pin<&mut Self>, _cx: &mut core::task::Context) -> Poll<()> {
        if std::mem::take(&mut self.resized) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }

    fn settle_render_phase_change(&mut self, _waker: &Waker) -> bool {
        std::mem::take(&mut self.resized)
    }

    fn pre_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        // An input is laid out within a terminal, whose size fits a `u16`.
        let size = drawer.size();
        let size = (
            u16::try_from(size.width).unwrap_or(u16::MAX),
            u16::try_from(size.height).unwrap_or(u16::MAX),
        );
        self.size = size;
        let Some((width, height)) = self.rendered else {
            return;
        };
        if width != size.0 || height.is_some_and(|height| height != size.1) {
            self.resized = true;
            if let Some(waker) = &self.waker {
                waker.wake_by_ref();
            }
            drawer.request_layout_pass();
        }
    }
}

struct TextBufferRow {
    offset: usize,
    len: usize,
    width: usize,
}

#[derive(Default)]
struct TextBuffer {
    text: String,
    rows: Vec<TextBufferRow>,
    word_ranges: OnceLock<Vec<Range<usize>>>,
}

impl TextBuffer {
    fn row_count(&self) -> usize {
        self.rows.len()
    }

    fn max_row_width(&self) -> usize {
        self.rows.iter().map(|row| row.width).max().unwrap_or(0)
    }

    fn new<S: Into<String>>(text: S, width: usize) -> Self {
        let text = text.into();
        let s = SegmentedString::from(text.as_str());
        let lines = s.wrap(width);
        let mut rows: Vec<TextBufferRow> = Vec::with_capacity(lines.len());
        for line in lines {
            rows.push(TextBufferRow {
                offset: line
                    .segments
                    .first()
                    .map(|s| s.offset)
                    .unwrap_or_else(|| rows.last().map_or(0, |r| r.offset)),
                len: line.segments.first().map_or(0, |s| s.text.len()),
                width: line.width,
            });
        }
        Self {
            rows,
            text,
            word_ranges: OnceLock::new(),
        }
    }

    fn row_column_for_offset(&self, offset: usize) -> (usize, usize) {
        for (i, row) in self.rows.iter().enumerate() {
            if offset >= row.offset {
                let offset_in_row = offset - row.offset;
                if offset_in_row <= row.len {
                    let col = crate::canvas::string_display_width(&self.text[row.offset..offset]);
                    return (i, col);
                }
            }
        }
        (self.rows.len(), self.rows.last().map_or(0, |r| r.width))
    }

    fn left_of_offset(&self, offset: usize) -> usize {
        if offset == 0 {
            0
        } else {
            self.text[..offset]
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(i, _)| i)
        }
    }

    fn right_of_offset(&self, offset: usize) -> usize {
        if offset >= self.text.len() {
            self.text.len()
        } else {
            self.text[offset..]
                .grapheme_indices(true)
                .nth(1)
                .map_or(self.text.len(), |(i, _)| offset + i)
        }
    }

    fn offset_for_closest_column_in_row(&self, row: usize, col: usize) -> usize {
        let row = &self.rows[row];
        if col >= row.width {
            row.offset + row.len
        } else {
            let mut width = 0;
            for (idx, grapheme) in
                self.text[row.offset..row.offset + row.len].grapheme_indices(true)
            {
                if width >= col {
                    return row.offset + idx;
                }
                width += crate::canvas::string_display_width_from_col(grapheme, width);
            }
            row.offset + row.len
        }
    }

    fn above_offset(&self, offset: usize, col_preference: Option<usize>) -> usize {
        let (row, col) = self.row_column_for_offset(offset);
        if row == 0 {
            return offset;
        }
        self.offset_for_closest_column_in_row(row - 1, col_preference.unwrap_or(col))
    }

    fn below_offset(&self, offset: usize, col_preference: Option<usize>) -> usize {
        let (row, col) = self.row_column_for_offset(offset);
        if row + 1 >= self.rows.len() {
            return offset;
        }
        self.offset_for_closest_column_in_row(row + 1, col_preference.unwrap_or(col))
    }

    fn line_start_offset(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .rfind(|&(_, character)| crate::unicode_linebreak::is_hard_line_break(character))
            .map_or(0, |(index, character)| index + character.len_utf8())
    }

    fn line_end_offset(&self, offset: usize) -> usize {
        let end = self.text[offset..]
            .find(crate::unicode_linebreak::is_hard_line_break)
            .map_or(self.text.len(), |index| offset + index);
        clamp_to_grapheme_boundary(&self.text, end)
    }

    fn previous_word_offset(&self, offset: usize) -> usize {
        let ranges = self.word_ranges();
        let end = ranges.partition_point(|range| range.start < offset);
        ranges[..end].last().map_or(0, |range| range.start)
    }

    fn next_word_offset(&self, offset: usize) -> usize {
        let ranges = self.word_ranges();
        let start = ranges.partition_point(|range| range.end <= offset);
        ranges.get(start).map_or(self.text.len(), |range| range.end)
    }

    fn word_ranges(&self) -> &[Range<usize>] {
        self.word_ranges.get_or_init(|| {
            let mut ranges = word_boundaries::ranges(&self.text);
            ranges.retain(|range| !self.text[range.clone()].starts_with(char::is_whitespace));
            ranges
        })
    }
}

#[derive(Default, Props)]
struct TextBufferViewProps {
    color: Option<Color>,
    weight: Weight,
    underline: bool,
    italic: bool,
    invert: bool,
    auto_grow: bool,
    buffer: Arc<TextBuffer>,
    style_ranges: Vec<TextInputStyleRange>,
}

#[derive(Default)]
struct TextBufferView {
    text_style: CanvasTextStyle,
    buffer: Arc<TextBuffer>,
    style_ranges: Vec<TextInputStyleRange>,
}

impl Component for TextBufferView {
    type Props<'a> = TextBufferViewProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self::default()
    }

    fn update(
        &mut self,
        props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        self.text_style = CanvasTextStyle {
            color: props.color,
            weight: props.weight,
            underline: props.underline,
            italic: props.italic,
            invert: props.invert,
            ..Default::default()
        };
        self.buffer = props.buffer.clone();
        self.style_ranges = std::mem::take(&mut props.style_ranges);
        let mut boundaries = self
            .buffer
            .text
            .grapheme_indices(true)
            .map(|(offset, _)| offset)
            .chain(std::iter::once(self.buffer.text.len()));
        let mut boundary = 0;
        let mut previous_end = 0;
        for styled in &self.style_ranges {
            assert!(
                styled.range.start >= previous_end
                    && styled.range.start < styled.range.end
                    && styled.range.end <= self.buffer.text.len(),
                "TextInput style ranges must be ordered, nonempty, nonoverlapping, and within the current value"
            );
            for offset in [styled.range.start, styled.range.end] {
                while boundary < offset {
                    boundary = boundaries
                        .next()
                        .expect("the value ends at a grapheme boundary");
                }
                assert_eq!(
                    boundary, offset,
                    "TextInput style ranges must align to grapheme boundaries"
                );
            }
            previous_end = styled.range.end;
        }
        if props.auto_grow {
            // Auto-grow: explicit height (row count) and full parent width
            // instead of absolute positioning, so taffy's flex layout derives
            // the height from wrapped content. This avoids `set_measure_func`
            // and its `mark_dirty` side-effects entirely.
            let row_count = props.buffer.row_count().max(1);
            updater.set_layout_style_if_changed(
                LayoutStyle {
                    width: Size::Percent(100.0),
                    height: Size::Length(row_count as u32),
                    ..Default::default()
                }
                .into(),
            );
        } else {
            updater.set_layout_style_if_changed(
                LayoutStyle {
                    position: Position::Absolute,
                    top: 0.into(),
                    left: 0.into(),
                    ..Default::default()
                }
                .into(),
            );
        }
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        if drawer.zero_height_sibling_shares_y() {
            return;
        }
        let mut canvas = drawer.canvas();
        for (row_index, row) in self.buffer.rows.iter().enumerate() {
            if !canvas.row_is_visible(row_index as isize) {
                continue;
            }
            let first = self
                .style_ranges
                .partition_point(|styled| styled.range.end <= row.offset);
            let ranges = &self.style_ranges[first..];
            let line = &self.buffer.text[row.offset..row.offset + row.len];
            let visual = crate::bidi::reorder_bidi_text_with_offsets(
                line,
                crate::bidi::needs_software_bidi(),
            );
            let style_at = |offset| {
                let offset = row.offset + offset;
                let index = ranges.partition_point(|styled| styled.range.end <= offset);
                ranges
                    .get(index)
                    .filter(|styled| styled.range.contains(&offset))
                    .map(|styled| styled.style)
            };
            canvas.set_text_row_with_styles(
                row_index as isize,
                &visual.text,
                self.text_style,
                |offset| style_at(visual.source_offset(offset)),
            );
        }
    }
}

/// `TextInput` is a component that can receive text input from the user.
///
/// It will fill the available width and display the current value. Typically, you will want to
/// render it in a [`View`](crate::components::View) component of the desired text field size.
///
/// Home/End and Ctrl+A/E move to logical line boundaries, independent of wrapping. Ctrl+U/K
/// delete to those boundaries, or the adjacent newline when already at a boundary. Alt+B/F,
/// Alt+Left/Right and Ctrl+Left/Right move by Unicode words, subdividing ASCII punctuation
/// except underscores and grouping adjacent separators. Ctrl+W, Alt+Backspace and
/// Ctrl+Backspace delete backward by a word; Alt+D, Alt+Delete and Ctrl+Delete delete forward.
/// These deletions include intervening whitespace and always preserve extended graphemes.
///
/// Ctrl+Y repeatedly inserts the most recent nonempty line or word deletion. Backspace,
/// Delete and Ctrl+D delete one grapheme without changing the saved text. Saved text belongs
/// to the [`TextInputHandle`], or to the component when no handle is supplied. Replacing the
/// value preserves it; retaining a handle in the caller also preserves it across unmounts.
/// All editing keys require [`TextInputProps::has_focus`].
///
/// # Example
///
/// ```
/// # use iocraft::prelude::*;
/// # #[component]
/// # fn FormField(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
/// let mut value = hooks.use_state(|| "".to_string());
///
/// element! {
///     View(
///         border_style: BorderStyle::Round,
///         border_color: Color::Blue,
///     ) {
///         View(width: 15) {
///             Text(content: "Input: ")
///         }
///         View(
///             background_color: Color::DarkGrey,
///             width: 30,
///         ) {
///             TextInput(
///                 has_focus: true,
///                 value: value.to_string(),
///                 on_change: move |new_value| value.set(new_value),
///             )
///         }
///     }
/// }
/// # }
/// ```
#[component]
pub fn TextInput(mut hooks: Hooks, props: &mut TextInputProps) -> impl Into<AnyElement<'static>> {
    let multiline = props.multiline;
    // Auto-grow only applies to multiline inputs; single-line keeps its
    // fixed one-row height and horizontal scrolling.
    let auto_grow = props.auto_grow && multiline;
    let has_focus = props.has_focus;
    let wrap = if multiline {
        TextWrap::Wrap
    } else {
        TextWrap::NoWrap
    };

    let mut prev_value = hooks.use_state(|| "".to_string());
    let mut cursor_offset = hooks.use_state(|| 0usize);
    let mut requested_cursor_offset = hooks.use_state(|| None);
    let mut scroll_offset_row = hooks.use_state(|| 0usize);
    let mut scroll_offset_col = hooks.use_state(|| 0usize);
    let mut vertical_movement_col_preference = hooks.use_state(|| None);
    let (width, height) = hooks.use_size(!auto_grow);
    let (width, height) = (usize::from(width), usize::from(height));

    let local_handle = hooks.use_ref_default::<TextInputHandle>();
    let mut handle = props.handle.unwrap_or(local_handle);
    let pending_cursor_offset = {
        let mut handle = handle.write();
        handle.inner = Some(TextInputHandleInner {
            cursor_offset,
            requested_cursor_offset,
        });
        handle.pending_cursor_offset.take()
    };

    let max_text_width = if wrap == TextWrap::Wrap {
        // Reserve the last column for the cursor.
        width.max(1) - 1
    } else {
        usize::MAX
    };

    let buffer = hooks.use_memo(
        {
            let text = props.value.clone();
            move || Arc::new(TextBuffer::new(text, max_text_width))
        },
        (&props.value, max_text_width),
    );

    let visible_rows = buffer
        .row_count()
        .max(1)
        .min(props.max_height.unwrap_or(u16::MAX).max(1) as usize);
    let height = if auto_grow { visible_rows } else { height };
    let max_scroll_row = buffer.row_count().saturating_sub(height);
    if scroll_offset_row.get() > max_scroll_row {
        scroll_offset_row.set(max_scroll_row);
    }

    // Input sets the cursor along with the value it edits; a value the parent changed on its own
    // moves the cursor by the edit that most likely produced it.
    if props.value.as_str() != prev_value.read().as_str() {
        let new_cursor_offset =
            new_cursor_offset(&prev_value.read(), cursor_offset.get(), &props.value);
        if cursor_offset != new_cursor_offset {
            cursor_offset.set(new_cursor_offset);
        }
        prev_value.set(props.value.clone());
    }

    // Update the cursor position if the user requested it.
    let requested = requested_cursor_offset.get();
    if requested.is_some() {
        requested_cursor_offset.set(None);
    }
    if let Some(requested) = requested.or(pending_cursor_offset) {
        let requested = clamp_to_grapheme_boundary(&props.value, requested);
        if cursor_offset != requested {
            cursor_offset.set(requested);
        }
    }

    let safe_cursor_offset = clamp_to_grapheme_boundary(&props.value, cursor_offset.get());
    if safe_cursor_offset != cursor_offset.get() {
        cursor_offset.set(safe_cursor_offset);
    }

    let (cursor_row, mut cursor_col) = buffer.row_column_for_offset(safe_cursor_offset);

    // If we're wrapping, don't let the cursor go past the visible area. No non-whitespace
    // characters will extend that far.
    if wrap == TextWrap::Wrap && cursor_col >= width && width > 0 {
        cursor_col = width - 1;
    }

    // Update the offset if the cursor is out of bounds.
    if width > 0 && height > 0 {
        if cursor_row >= scroll_offset_row.get() + height {
            scroll_offset_row.set(cursor_row - height + 1);
        } else if cursor_row < scroll_offset_row.get() {
            scroll_offset_row.set(cursor_row);
        }
        if auto_grow {
            // Content wraps into extra rows instead of scrolling sideways.
            if scroll_offset_col.get() != 0 {
                scroll_offset_col.set(0);
            }
        } else if cursor_col >= scroll_offset_col.get() + width {
            scroll_offset_col.set(cursor_col - width + 1);
        } else if cursor_col < scroll_offset_col.get() {
            scroll_offset_col.set(cursor_col);
        }

        // Scroll back when the content shrinks so it isn't cut off.
        let content_width = buffer.max_row_width();
        let max_scroll_offset_col = (content_width + 1).saturating_sub(width);
        if scroll_offset_col.get() > max_scroll_offset_col {
            scroll_offset_col.set(max_scroll_offset_col);
        }
    }

    // Cursor overlay: post-render styling at the cursor position, replacing the old
    // extra absolute-positioned background block view. The hook applies the overlay in
    // post_component_draw — after TextBufferView has drawn the text — so the cursor is a
    // pure style layer that doesn't interfere with the text content.
    let cursor_overlay = hooks.use_cursor_overlay();
    cursor_overlay.active = has_focus;
    cursor_overlay.col =
        u16::try_from(cursor_col.saturating_sub(scroll_offset_col.get())).unwrap_or(u16::MAX);
    cursor_overlay.row =
        u16::try_from(cursor_row.saturating_sub(scroll_offset_row.get())).unwrap_or(u16::MAX);
    cursor_overlay.physical_cursor = props.physical_cursor;
    cursor_overlay.char_width = if has_focus && safe_cursor_offset < props.value.len() {
        props.value[safe_cursor_offset..]
            .graphemes(true)
            .next()
            .map(|grapheme| crate::canvas::grapheme_width(grapheme).max(1) as u16)
            .unwrap_or(1)
    } else {
        1
    };

    hooks.use_propagated_terminal_events({
        let buffer = buffer.clone();
        let mut value = props.value.clone();
        let mut on_change = props.on_change.take();
        let mut on_change_cursor_offset = props.on_change_cursor_offset.take();
        move |event| {
            if !has_focus {
                return;
            }
            let propagated = event;
            let event = propagated.event().clone();
            let cursor = clamp_to_grapheme_boundary(&value, cursor_offset.get());
            // The cursor after an edit of `value`.
            let mut edited = None;
            let mut deleted_range = None;
            let mut save_deleted_text = false;
            let mut handled = false;

            match event {
                TerminalEvent::Paste(text) => {
                    // Bracketed paste delivers the entire pasted text as one event, so
                    // we insert it in a single pass: one on_change, one re-render —
                    // instead of one per character. In single-line mode, newlines are
                    // replaced with spaces (matching typical browser input behavior).
                    let text = if multiline {
                        text
                    } else {
                        let mut normalized = String::with_capacity(text.len());
                        let mut chars = text.chars().peekable();
                        while let Some(ch) = chars.next() {
                            match ch {
                                '\r' => {
                                    if matches!(chars.peek(), Some('\n')) {
                                        chars.next();
                                    }
                                    normalized.push(' ');
                                }
                                '\n' => normalized.push(' '),
                                other => normalized.push(other),
                            }
                        }
                        normalized
                    };
                    value.insert_str(cursor, &text);
                    edited = Some(cursor + text.len());
                    vertical_movement_col_preference.set(None);
                    handled = true;
                }
                TerminalEvent::Key(KeyEvent {
                    code,
                    kind,
                    modifiers,
                    ..
                }) if kind != KeyEventKind::Release
                    && modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !is_altgr_char(code, modifiers) =>
                {
                    let control = modifiers.contains(KeyModifiers::CONTROL);
                    let alt = modifiers.contains(KeyModifiers::ALT);
                    match code {
                        KeyCode::Char('a') if control => {
                            cursor_offset.set(buffer.line_start_offset(cursor));
                            vertical_movement_col_preference.set(None);
                            handled = true;
                        }
                        KeyCode::Char('e') if control => {
                            cursor_offset.set(buffer.line_end_offset(cursor));
                            vertical_movement_col_preference.set(None);
                            handled = true;
                        }
                        KeyCode::Char('u') if control => {
                            save_deleted_text = true;
                            let start = buffer.line_start_offset(cursor);
                            deleted_range = Some(if start == cursor {
                                buffer.left_of_offset(cursor)..cursor
                            } else {
                                start..cursor
                            });
                            handled = true;
                        }
                        KeyCode::Char('k') if control => {
                            save_deleted_text = true;
                            let end = buffer.line_end_offset(cursor);
                            deleted_range = Some(if end == cursor {
                                cursor..buffer.right_of_offset(cursor)
                            } else {
                                cursor..end
                            });
                            handled = true;
                        }
                        KeyCode::Char('y') if control => {
                            let handle = handle.read();
                            if !handle.deleted_text.is_empty() {
                                value.insert_str(cursor, &handle.deleted_text);
                                edited = Some(cursor + handle.deleted_text.len());
                            }
                            vertical_movement_col_preference.set(None);
                            handled = true;
                        }
                        KeyCode::Left | KeyCode::Char('b') if code == KeyCode::Left || alt => {
                            cursor_offset.set(buffer.previous_word_offset(cursor));
                            vertical_movement_col_preference.set(None);
                            handled = true;
                        }
                        KeyCode::Right | KeyCode::Char('f') if code == KeyCode::Right || alt => {
                            cursor_offset.set(buffer.next_word_offset(cursor));
                            vertical_movement_col_preference.set(None);
                            handled = true;
                        }
                        KeyCode::Backspace | KeyCode::Char('w')
                            if code == KeyCode::Backspace || control =>
                        {
                            deleted_range = Some(buffer.previous_word_offset(cursor)..cursor);
                            save_deleted_text = true;
                            handled = true;
                        }
                        KeyCode::Delete | KeyCode::Char('d') if code == KeyCode::Delete || alt => {
                            deleted_range = Some(cursor..buffer.next_word_offset(cursor));
                            save_deleted_text = true;
                            handled = true;
                        }
                        KeyCode::Char('d') if control => {
                            deleted_range = Some(cursor..buffer.right_of_offset(cursor));
                            handled = true;
                        }
                        _ => {}
                    }
                }
                TerminalEvent::Key(KeyEvent {
                    code,
                    kind,
                    modifiers,
                    ..
                }) if kind != KeyEventKind::Release
                    && (!modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        || is_altgr_char(code, modifiers)) =>
                {
                    let mut clear_vertical_movement_col_preference = true;

                    match code {
                        KeyCode::Char(c) => {
                            value.insert(cursor, c);
                            edited = Some(cursor + c.len_utf8());
                            handled = true;
                        }
                        KeyCode::Backspace | KeyCode::Delete => {
                            deleted_range = Some(if code == KeyCode::Backspace {
                                buffer.left_of_offset(cursor)..cursor
                            } else {
                                cursor..buffer.right_of_offset(cursor)
                            });
                            handled = true;
                        }
                        KeyCode::Enter if multiline => {
                            value.insert(cursor, '\n');
                            edited = Some(cursor + 1);
                            handled = true;
                        }
                        KeyCode::Left => {
                            cursor_offset.set(buffer.left_of_offset(cursor));
                            handled = true;
                        }
                        KeyCode::Right => {
                            cursor_offset.set(buffer.right_of_offset(cursor));
                            handled = true;
                        }
                        KeyCode::Up if multiline => {
                            clear_vertical_movement_col_preference = false;
                            if vertical_movement_col_preference.get().is_none() {
                                let (_, col) = buffer.row_column_for_offset(cursor);
                                vertical_movement_col_preference.set(Some(col));
                            }
                            cursor_offset.set(
                                buffer.above_offset(cursor, vertical_movement_col_preference.get()),
                            );
                            handled = true;
                        }
                        KeyCode::Down if multiline => {
                            clear_vertical_movement_col_preference = false;
                            if vertical_movement_col_preference.get().is_none() {
                                let (_, col) = buffer.row_column_for_offset(cursor);
                                vertical_movement_col_preference.set(Some(col));
                            }
                            cursor_offset.set(
                                buffer.below_offset(cursor, vertical_movement_col_preference.get()),
                            );
                            handled = true;
                        }
                        KeyCode::Home => {
                            cursor_offset.set(buffer.line_start_offset(cursor));
                            handled = true;
                        }
                        KeyCode::End => {
                            cursor_offset.set(buffer.line_end_offset(cursor));
                            handled = true;
                        }
                        _ => {
                            clear_vertical_movement_col_preference = false;
                        }
                    }

                    if clear_vertical_movement_col_preference {
                        vertical_movement_col_preference.set(None);
                    }
                }
                _ => {}
            }
            if let Some(range) = deleted_range {
                if !range.is_empty() {
                    if save_deleted_text {
                        handle.write().deleted_text = value[range.clone()].to_string();
                    }
                    edited = Some(range.start);
                    value.replace_range(range, "");
                }
                vertical_movement_col_preference.set(None);
            }
            // An edit can join adjacent graphemes, so publish its cursor at a boundary in
            // the resulting value, together with the value it belongs to.
            let moved = clamp_to_grapheme_boundary(&value, edited.unwrap_or(cursor_offset.get()));
            if cursor_offset != moved {
                cursor_offset.set(moved);
            }
            if edited.is_some() {
                prev_value.set(value.clone());
                on_change(value.clone());
            }
            if moved != cursor {
                on_change_cursor_offset(moved);
            }
            if handled {
                propagated.stop_propagation();
            }
        }
    });

    element! {
        View(overflow: Overflow::Hidden, width: 100pct, height: if auto_grow { Size::Length(visible_rows as u32) } else if multiline { Size::Percent(100.0) } else { Size::Length(1) }, position: Position::Relative) {
            View(position: Position::Absolute, top: -i32::try_from(scroll_offset_row.get()).unwrap_or(i32::MAX), left: -i32::try_from(scroll_offset_col.get()).unwrap_or(i32::MAX)) {
                TextBufferView(
                    buffer,
                    style_ranges: props.style_ranges.clone(),
                    color: props.color,
                    weight: props.weight,
                    underline: props.decoration == TextDecoration::Underline,
                    italic: props.italic,
                    invert: props.invert,
                    auto_grow: false,
                )
            }
        }
    }
}

/// Returns whether a keypress is using AltGr, always false for platforms other than Windows.
/// Windows reports AltGr (used for characters like '@', '{' and '€' on many keyboard layouts)
/// as Ctrl+Alt, so these key presses should be typed rather than treated as shortcuts.
/// Letters and digits are excluded, since they are used for shortcuts.
fn is_altgr_char(code: KeyCode, modifiers: KeyModifiers) -> bool {
    cfg!(windows)
        && modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && matches!(code, KeyCode::Char(c) if !c.is_control() && !c.is_ascii_alphanumeric())
}

fn clamp_to_grapheme_boundary(value: &str, offset: usize) -> usize {
    if offset >= value.len() {
        return value.len();
    }
    value
        .grapheme_indices(true)
        .take_while(|(index, _)| *index <= offset)
        .last()
        .map_or(0, |(index, _)| index)
}

fn new_cursor_offset(prev_value: &str, cursor_offset: usize, value: &str) -> usize {
    let cursor_offset = clamp_to_grapheme_boundary(prev_value, cursor_offset);
    let prev_head = &prev_value[..cursor_offset];
    let prev_tail = &prev_value[cursor_offset..];
    let has_same_head = value
        .get(..cursor_offset)
        .is_some_and(|head| head == prev_head);
    let has_same_tail = value.ends_with(prev_tail);

    let offset = if value.len() >= prev_value.len() && has_same_head && has_same_tail {
        // insertion (or no change)
        cursor_offset + (value.len() - prev_value.len())
    } else if value.len() < prev_value.len() && has_same_tail {
        // backspace, which also wins when a deletion would explain the value as well
        cursor_offset.saturating_sub(prev_value.len() - value.len())
    } else if value.len() < prev_value.len() && has_same_head {
        // deletion
        cursor_offset
    } else {
        // unknown, put the cursor at the end
        value.len()
    };
    clamp_to_grapheme_boundary(value, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::stream::{self, StreamExt};
    use macro_rules_attribute::apply;
    use smol_macros::test;

    #[apply(test!)]
    async fn test_input_style_ranges_preserve_wrapping_and_cursor() {
        let value = "ab e\u{301}中 xyz";
        let render = |styled| {
            element! {
                CursorProbe(
                    initial_value: value,
                    width: 7,
                    height: 2,
                    multiline: true,
                    physical_cursor: true,
                    weight: Weight::Bold,
                    style_ranges: if styled {
                        vec![TextInputStyleRange {
                            range: 3..9,
                            style: StyleOverlay {
                                color: Some(Some(Color::Cyan)),
                                ..Default::default()
                            },
                        }]
                    } else {
                        Vec::new()
                    },
                )
            }
        };
        let events = || {
            MockTerminalConfig::with_delayed_events([
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Right)),
                TerminalEvent::Resize(80, 24),
            ])
        };
        let plain: Vec<_> = render(false)
            .mock_terminal_render_loop(events())
            .collect()
            .await;
        let styled: Vec<_> = render(true)
            .mock_terminal_render_loop(events())
            .collect()
            .await;
        let plain = plain.last().unwrap();
        let styled = styled.last().unwrap();
        assert_eq!(styled.to_string(), plain.to_string());
        assert_eq!(styled.cursor_declaration(), plain.cursor_declaration());
        assert_eq!(styled.to_string(), "ab e\u{301}中\nxyz\n");
        for column in [3, 4] {
            let style = styled.resolved_text_style(column, 0).unwrap();
            assert_eq!(style.color, Some(Color::Cyan));
            assert_eq!(style.weight, Weight::Bold);
        }
        assert_eq!(styled.resolved_text_style(0, 1).unwrap().color, None);
    }

    #[test]
    fn test_input_style_ranges_preserve_bidirectional_text() {
        let render = |styled| {
            element! {
                View(width: 20, height: 1, background_color: Color::Red) {
                    TextInput(
                        value: "אבגabc",
                        has_focus: true,
                        physical_cursor: true,
                        style_ranges: if styled {
                            vec![TextInputStyleRange {
                                range: 0..2,
                                style: StyleOverlay {
                                    color: Some(Some(Color::Cyan)),
                                    background_color: Some(None),
                                    ..Default::default()
                                },
                            }]
                        } else { Vec::new() },
                    )
                }
            }
            .render(None)
        };
        let plain = render(false);
        let styled = render(true);
        assert_eq!(styled.to_string(), plain.to_string());
        assert_eq!(styled.cursor_declaration(), plain.cursor_declaration());
        for column in 0..6 {
            let cell = styled.cell(column, 0).unwrap();
            assert_eq!(
                styled.resolved_text_style(column, 0).unwrap().color,
                if cell.text() == Some("א") {
                    Some(Color::Cyan)
                } else {
                    None
                }
            );
        }
        let changes = plain.diff(&styled);
        let styled_cell = changes
            .iter()
            .find_map(|change| {
                change
                    .added
                    .as_ref()
                    .filter(|cell| cell.cell.text() == Some("א"))
            })
            .expect("styled cell changes");
        assert_eq!(styled_cell.cell.background_color, None);
        assert_eq!(styled_cell.overlay, None);
    }

    #[test]
    fn test_input_style_ranges_follow_canvas_tab_stops_and_control_sequences() {
        for (value, end, last_column, expected) in [
            ("a\tb tail", 3, 8, "  a     b tail\n"),
            ("a\x1b[31mb tail", 7, 3, "  ab tail\n"),
        ] {
            let canvas = element! {
                View(width: 20, height: 1, padding_left: 2) {
                    TextInput(value, style_ranges: vec![TextInputStyleRange {
                        range: 0..end,
                        style: StyleOverlay { color: Some(Some(Color::Cyan)), ..Default::default() },
                    }])
                }
            }.render(None);
            assert_eq!(canvas.to_string(), expected);
            for column in 2..=last_column {
                assert_eq!(
                    canvas.resolved_text_style(column, 0).unwrap().color,
                    Some(Color::Cyan)
                );
            }
            for column in last_column + 1..last_column + 6 {
                assert_eq!(canvas.resolved_text_style(column, 0).unwrap().color, None);
            }
        }
    }

    #[derive(Default, Props)]
    struct StyledEditorProbeProps {
        styled: bool,
        physical_cursor: bool,
    }

    #[component]
    fn StyledEditorProbe(
        mut hooks: Hooks,
        props: &StyledEditorProbeProps,
    ) -> impl Into<AnyElement<'static>> {
        let mut value = hooks.use_state(String::new);
        let mut color = hooks.use_state(|| Color::Cyan);
        let mut exit = hooks.use_state(|| false);
        let mut system = hooks.use_context_mut::<SystemContext>();
        let (width, _) = hooks.use_terminal_size();
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(key) = event {
                match key.code {
                    KeyCode::F(2) => color.set(Color::Magenta),
                    KeyCode::F(3) => value.set("e\u{301}中".to_string()),
                    KeyCode::Esc => exit.set(true),
                    _ => {}
                }
            }
        });
        if exit.get() {
            system.exit();
        }
        let style_ranges = if props.styled && !value.read().is_empty() {
            vec![TextInputStyleRange {
                range: 0..value.read().len(),
                style: StyleOverlay {
                    color: Some(Some(color.get())),
                    ..Default::default()
                },
            }]
        } else {
            Vec::new()
        };
        element! {
            View(width) {
                TextInput(
                    value: value.to_string(),
                    style_ranges,
                    on_change: move |text| value.set(text),
                    has_focus: true,
                    physical_cursor: props.physical_cursor,
                    multiline: true,
                    auto_grow: true,
                    max_height: Some(2),
                )
            }
        }
    }

    #[apply(test!)]
    async fn test_input_style_ranges_follow_edits_restore_resize_and_scroll() {
        let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        let events = || {
            MockTerminalConfig::with_delayed_events([
                TerminalEvent::Paste("ab e\u{301}中\nsecond line\nlast".to_string()),
                key(KeyCode::Left),
                key(KeyCode::Backspace),
                key(KeyCode::Home),
                key(KeyCode::Delete),
                key(KeyCode::Up),
                key(KeyCode::Up),
                TerminalEvent::Resize(12, 5),
                key(KeyCode::F(2)),
                key(KeyCode::F(3)),
                key(KeyCode::Home),
                key(KeyCode::Right),
                key(KeyCode::Backspace),
                key(KeyCode::Delete),
                key(KeyCode::Esc),
            ])
            .with_size(7, 5)
        };
        for physical_cursor in [false, true] {
            let plain: Vec<_> = element!(StyledEditorProbe(styled: false, physical_cursor))
                .mock_terminal_render_loop(events())
                .collect()
                .await;
            let styled: Vec<_> = element!(StyledEditorProbe(styled: true, physical_cursor))
                .mock_terminal_render_loop(events())
                .collect()
                .await;
            let observations = |canvases: &[Canvas]| {
                let mut observations: Vec<_> = canvases
                    .iter()
                    .map(|canvas| (canvas.to_string(), canvas.cursor_declaration()))
                    .collect();
                observations.dedup();
                observations
            };
            assert_eq!(observations(&styled), observations(&plain));
            let restored = styled
                .iter()
                .find(|canvas| canvas.to_string() == "e\u{301}中\n")
                .expect("restored value scrolls back into view");
            for column in [0, 1] {
                assert_eq!(
                    restored.resolved_text_style(column, 0).unwrap().color,
                    Some(Color::Magenta)
                );
            }
            assert!(styled.iter().any(|canvas| canvas.to_string() == "中\n"));
            assert!(styled.last().unwrap().to_string().trim().is_empty());
        }
    }

    #[test]
    fn test_input_style_ranges_reject_invalid_current_value_boundaries() {
        for range in [1..3, 0..2, 0..7, 3..3] {
            let result = std::panic::catch_unwind(|| {
                element! {
                    View(width: 10, height: 1) {
                        TextInput(value: "e\u{301}中", style_ranges: vec![TextInputStyleRange {
                            range,
                            style: StyleOverlay::default(),
                        }])
                    }
                }
                .render(None);
            });
            assert!(result.is_err(), "invalid ranges must fail before drawing");
        }
    }

    #[derive(Default, Props)]
    struct MyComponentProps {
        initial_value: String,
    }

    #[component]
    fn MyComponent(mut hooks: Hooks, props: &MyComponentProps) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut value = hooks.use_state(|| props.initial_value.clone());

        if value.read().contains("!") {
            system.exit();
        }

        element! {
            View(height: 1, width: 11, padding_left: 1) {
                TextInput(
                    has_focus: true,
                    value: value.to_string(),
                    on_change: move |new_value| value.set(new_value),
                )
            }
        }
    }

    #[component]
    fn MyMultilineComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut value = hooks.use_state(|| "".to_string());

        if value.read().contains("!") {
            system.exit();
        }

        element! {
            View(height: 3, width: 11, padding_left: 1) {
                TextInput(
                    has_focus: true,
                    value: value.to_string(),
                    on_change: move |new_value| value.set(new_value),
                    multiline: true,
                )
            }
        }
    }

    #[derive(Default, Props)]
    struct CursorProbeProps {
        style_ranges: Vec<TextInputStyleRange>,
        weight: Weight,
        initial_value: String,
        width: Size,
        height: Size,
        multiline: bool,
        physical_cursor: bool,
    }

    #[component]
    fn CursorProbe(mut hooks: Hooks, props: &CursorProbeProps) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Resize(..)) {
                should_exit.set(true);
            }
        });
        if should_exit.get() {
            system.exit();
        }

        element! {
            View(width: props.width, height: props.height) {
                TextInput(
                    has_focus: true,
                    value: props.initial_value.clone(),
                    style_ranges: props.style_ranges.clone(),
                    weight: props.weight,
                    on_change: |_| {},
                    multiline: props.multiline,
                    physical_cursor: props.physical_cursor,
                )
            }
        }
    }

    #[component]
    fn SubmitKeyOwner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let (mut value, mut submitted) = *hooks.use_context::<(State<String>, State<String>)>();
        hooks.use_propagated_terminal_events(move |event| {
            if matches!(event.event(), TerminalEvent::Key(key) if key.code == KeyCode::Enter && key.kind != KeyEventKind::Release) {
                submitted.set(value.to_string());
                value.set(String::new());
                event.stop_propagation();
            }
        });
        element!(View(width: 0, height: 0))
    }

    #[component]
    fn ControlledInput(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut value = hooks.use_state(String::new);
        let submitted = hooks.use_state(String::new);
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| {
            if matches!(event, TerminalEvent::Resize(..)) {
                should_exit.set(true);
            }
        });
        if should_exit.get() {
            system.exit();
        }
        element! {
            ContextProvider(value: Context::owned((value, submitted))) {
                View(width: 20, flex_direction: FlexDirection::Column) {
                    Text(content: format!("sent={}", submitted))
                    SubmitKeyOwner
                    TextInput(
                        has_focus: true,
                        physical_cursor: true,
                        multiline: true,
                        auto_grow: true,
                        max_height: Some(3),
                        value: value.to_string(),
                        on_change: move |text| value.set(text),
                    )
                }
            }
        }
    }

    #[apply(test!)]
    async fn test_caller_input_stream_preserves_utf8_paste_and_key_order() {
        let input = "hi早\x1b[200~粘贴\x1b[201~\x1b[D!\rx";
        let bytes: Vec<_> = input
            .as_bytes()
            .iter()
            .map(|byte| Ok(vec![*byte]))
            .collect();
        let stream = TerminalRawInputFallibleEventStream::new_utf8(stream::iter(bytes))
            .chain(stream::iter([Ok(TerminalEvent::Resize(80, 24))]));
        let frames: Vec<_> = element!(ControlledInput)
            .mock_terminal_render_loop(MockTerminalConfig::default().with_input_stream(stream))
            .collect()
            .await;
        assert_eq!(frames.last().unwrap().to_string(), "sent=hi早粘!贴\nx\n");
    }

    #[apply(test!)]
    async fn test_caller_input_stream_paints_last_key_before_eof() {
        // Unlike stream::iter, unfold panics if polled after returning None.
        let stream = stream::unfold(false, |sent| async move {
            if sent {
                None
            } else {
                Some((
                    Ok(TerminalEvent::Key(KeyEvent::new(
                        KeyEventKind::Press,
                        KeyCode::Char('a'),
                    ))),
                    true,
                ))
            }
        });
        let frames: Vec<_> = element!(ControlledInput)
            .mock_terminal_render_loop(MockTerminalConfig::default().with_input_stream(stream))
            .map(|canvas| canvas.to_string())
            .collect()
            .await;
        assert_eq!(frames, ["sent=\n\n", "sent=\na\n"]);
    }

    #[apply(test!)]
    async fn test_controlled_input_commits_between_batched_keys() {
        let events = [
            KeyCode::Char('h'),
            KeyCode::Char('i'),
            KeyCode::Enter,
            KeyCode::Char('x'),
        ]
        .into_iter()
        .map(|code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code)))
        .chain([TerminalEvent::Resize(80, 24)]);
        let frames: Vec<_> = element!(ControlledInput)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(events)))
            .collect()
            .await;
        assert_eq!(frames.last().unwrap().to_string(), "sent=hi\nx\n");
    }

    #[component]
    fn MirroredInput(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut value = hooks.use_state(String::new);
        let mut cursor = hooks.use_state(|| 0usize);
        let mut shown = hooks.use_state(|| false);
        let mut handle = hooks.use_ref_default::<TextInputHandle>();
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut should_exit = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| match event {
            TerminalEvent::Key(KeyEvent {
                code: KeyCode::Tab, ..
            }) => {
                if !shown.get() {
                    handle.write().set_cursor_offset(cursor.get());
                }
                shown.set(!shown.get());
            }
            TerminalEvent::Resize(..) => should_exit.set(true),
            _ => {}
        });
        if should_exit.get() {
            system.exit();
        }
        element! {
            View(width: 16, flex_direction: FlexDirection::Column) {
                Text(content: format!("{}|{}", value, cursor))
                #(shown.get().then(|| element! {
                    TextInput(
                        has_focus: true,
                        multiline: true,
                        auto_grow: true,
                        max_height: Some(6),
                        value: value.to_string(),
                        on_change: move |text| value.set(text),
                        on_change_cursor_offset: move |offset| cursor.set(offset),
                        handle,
                    )
                }))
            }
        }
    }

    #[apply(test!)]
    async fn test_input_reports_its_cursor_with_its_value_and_mounts_at_its_width() {
        // The first frame relies on the same-frame layout pass, which requires push-wake.
        crate::render::wake::set_push_wake_for_tests(Some(true));
        let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        let events = [
            key(KeyCode::Tab),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('b')),
            key(KeyCode::Left),
            TerminalEvent::Paste(" two words".to_string()),
            key(KeyCode::Tab),
            key(KeyCode::Tab),
            TerminalEvent::Resize(80, 24),
        ];
        let canvases: Vec<_> = element!(MirroredInput)
            .mock_terminal_render_loop(MockTerminalConfig::with_delayed_events(events))
            .collect()
            .await;
        crate::render::wake::set_push_wake_for_tests(None);
        let frames: Vec<_> = canvases.iter().map(Canvas::to_string).collect();
        // Each edit's frame already carries the cursor it leaves behind.
        let mut mirrored: Vec<&str> = frames
            .iter()
            .map(|frame| frame.lines().next().unwrap_or_default())
            .collect();
        mirrored.dedup();
        assert_eq!(mirrored, ["|0", "a|1", "ab|2", "ab|1", "a two wordsb|11"]);
        // Mounted again with its value, the input lays it out at its width from the first frame.
        let hidden = frames
            .iter()
            .rposition(|frame| frame.lines().count() == 1)
            .expect("the input is hidden before it mounts again");
        assert!(
            frames[hidden + 1..]
                .iter()
                .all(|frame| frame.lines().count() == 2),
            "{frames:?}"
        );
        // The offset set while it was unmounted places its cursor.
        let cursor = canvases.last().unwrap().cursor_declaration().unwrap();
        assert_eq!((cursor.x, cursor.y), (11, 1));
    }

    #[apply(test!)]
    async fn test_input_moves_and_deletes_whole_graphemes() {
        let value = "a\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}b\u{1f1e8}\u{1f1f3}c";
        let key = |code| TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, code));
        let events = [key(KeyCode::Tab), TerminalEvent::Paste(value.to_string())]
            .into_iter()
            .chain(
                [
                    KeyCode::Left,
                    KeyCode::Left,
                    KeyCode::Left,
                    KeyCode::Left,
                    KeyCode::Right,
                    KeyCode::Right,
                    KeyCode::Right,
                    KeyCode::Backspace,
                    KeyCode::Left,
                    KeyCode::Backspace,
                    KeyCode::Home,
                    KeyCode::Delete,
                    KeyCode::Delete,
                    KeyCode::Delete,
                ]
                .into_iter()
                .map(key),
            )
            .chain([TerminalEvent::Paste(value.to_string())])
            .chain(
                [
                    KeyCode::Home,
                    KeyCode::Right,
                    KeyCode::Delete,
                    KeyCode::Right,
                    KeyCode::Delete,
                ]
                .into_iter()
                .map(key),
            )
            .chain([TerminalEvent::Resize(80, 24)]);
        let canvases: Vec<_> = element!(MirroredInput)
            .mock_terminal_render_loop(MockTerminalConfig::with_delayed_events(events))
            .collect()
            .await;
        let frames: Vec<_> = canvases.iter().map(Canvas::to_string).collect();
        let mut mirrored: Vec<_> = frames
            .iter()
            .map(|frame| frame.lines().next().unwrap_or_default())
            .collect();
        mirrored.dedup();
        assert_eq!(
            mirrored,
            [
                "|0".to_string(),
                format!("{value}|29"),
                format!("{value}|28"),
                format!("{value}|20"),
                format!("{value}|19"),
                format!("{value}|1"),
                format!("{value}|19"),
                format!("{value}|20"),
                format!("{value}|28"),
                "a\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}bc|20".to_string(),
                "a\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}bc|19".to_string(),
                "abc|1".to_string(),
                "abc|0".to_string(),
                "bc|0".to_string(),
                "c|0".to_string(),
                "|0".to_string(),
                format!("{value}|29"),
                format!("{value}|0"),
                format!("{value}|1"),
                "ab\u{1f1e8}\u{1f1f3}c|1".to_string(),
                "ab\u{1f1e8}\u{1f1f3}c|2".to_string(),
                "abc|2".to_string(),
            ]
        );
    }

    #[apply(test!)]
    async fn test_auto_grow_scrolls_at_limit_and_resets_after_submit() {
        let frames: Vec<_> = element!(ControlledInput)
            .mock_terminal_render_loop(MockTerminalConfig::with_delayed_events([
                TerminalEvent::Paste("one\ntwo\nthree\nfour\nfive".to_string()),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                TerminalEvent::Resize(80, 24),
            ]))
            .collect()
            .await;
        let scrolled = frames
            .iter()
            .find(|frame| frame.to_string() == "sent=\nthree\nfour\nfive\n")
            .expect("the capped input must scroll to its final three rows");
        assert_eq!(scrolled.height(), 4);
        let cursor = scrolled.cursor_declaration().unwrap();
        assert_eq!((cursor.x, cursor.y, cursor.visible), (4, 3, true));
        let last = frames.last().unwrap();
        assert!(last.to_string().ends_with("five\nx\n"));
        let cursor = last.cursor_declaration().unwrap();
        assert_eq!((cursor.x, cursor.y), (1, last.height() - 1));
    }

    #[apply(test!)]
    async fn test_text_input() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('f'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('f'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n", " foo! \n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    /// A focused TextInput must declare the physical cursor position on the canvas so
    /// the renderer can anchor the terminal cursor (and thus the IME pre-edit window)
    /// to the caret.
    #[apply(test!)]
    async fn test_text_input_declares_physical_cursor() {
        let canvases: Vec<_> = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('f'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                ],
            )))
            .collect::<Vec<_>>()
            .await;
        use crate::canvas::CursorDeclaration;
        let cd = |x, y| CursorDeclaration {
            x,
            y,
            visible: false,
        };
        // Initial frame: empty value, caret at the input's origin. MyComponent has
        // padding_left: 1, so the input's canvas-absolute origin is column 1.
        assert_eq!(canvases[0].cursor_declaration(), Some(cd(1, 0)));
        // After typing "f" then "!", the caret has advanced past the typed text.
        let last = canvases.last().unwrap();
        assert_eq!(last.cursor_declaration(), Some(cd(3, 0)));
    }

    #[apply(test!)]
    async fn test_text_input_paste() {
        // A bracketed paste delivers the whole text as one event and one insertion.
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Paste("foo".to_string()),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n", " foo! \n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    #[apply(test!)]
    async fn test_text_input_paste_strips_newlines_in_single_line_mode() {
        // Pasting multi-line text into a single-line input must not be interpreted
        // as pressing Enter; newlines are replaced with spaces.
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Paste("a\r\nb".to_string()),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n", " a b! \n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    #[apply(test!)]
    async fn test_text_input_initial_value() {
        let actual = element! {
            MyComponent(initial_value: "foo")
        }
        .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
            vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('!'))),
            ],
        )))
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .await;
        let expected = vec![" foo \n", " foo! \n"];
        assert_eq!(actual, expected);
    }

    #[apply(test!)]
    async fn test_text_input_overflow() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Repeat, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('x'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n", " xxxxxxxx! \n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    #[apply(test!)]
    async fn test_text_input_kanji() {
        let actual = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('一'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('一'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('二'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('二'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n", " 一二! \n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    #[apply(test!)]
    async fn test_text_input_wide_cursor_beyond_u16_on_first_draw() {
        // The first frame relies on the same-frame layout pass, which requires push-wake.
        crate::render::wake::set_push_wake_for_tests(Some(true));
        #[component]
        fn WideCursor(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let handle = hooks.use_ref(|| {
                let mut handle = TextInputHandle::default();
                handle.set_cursor_offset(65_536);
                handle
            });
            element! {
                View(width: 20) {
                    TextInput(
                        value: format!("{}中", "x".repeat(65_536)),
                        has_focus: true,
                        handle,
                    )
                }
            }
        }
        let canvas = element!(WideCursor).render(Some(20));
        crate::render::wake::set_push_wake_for_tests(None);
        assert_eq!(canvas.cursor_declaration().unwrap().x, 19);
    }

    #[apply(test!)]
    async fn test_text_input_scrolls_beyond_u16_coordinates() {
        // The first frame relies on the same-frame layout pass, which requires push-wake.
        crate::render::wake::set_push_wake_for_tests(Some(true));
        let value = format!("{}TAIL", "x".repeat(65_532));
        let frames = element!(CursorProbe(initial_value: value.clone(), width: 20, height: 1))
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter([
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Home)),
                TerminalEvent::Resize(80, 24),
            ])))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(frames[0].to_string(), "xxxxxxxxxxxxxxxTAIL \n");
        assert_eq!(frames[0].cursor_declaration().unwrap().x, 19);
        assert_eq!(frames.last().unwrap().to_string(), "xxxxxxxxxxxxxxxxxxxx\n");
        assert_eq!(frames.last().unwrap().cursor_declaration().unwrap().x, 0);

        let frames = element!(MyComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter([
                TerminalEvent::Paste(value),
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
            ])))
            .collect::<Vec<_>>()
            .await;
        assert_eq!(frames.last().unwrap().to_string(), " xxxxTAIL! \n");

        let frames = element!(CursorProbe(
            initial_value: format!("{}TAIL", "x\n".repeat(65_536)),
            width: 20,
            height: 2,
            multiline: true,
        ))
        .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter([
            TerminalEvent::Resize(80, 24),
        ])))
        .collect::<Vec<_>>()
        .await;
        crate::render::wake::set_push_wake_for_tests(None);
        assert_eq!(frames[0].to_string(), "x\nTAIL \n");
        assert_eq!(frames[0].cursor_declaration().unwrap().y, 1);
    }

    #[apply(test!)]
    async fn test_text_input_unwrapped_multiline_value_scrolls_back_when_longest_line_shrinks() {
        // The first frame relies on the same-frame layout pass, which requires push-wake.
        crate::render::wake::set_push_wake_for_tests(Some(true));
        #[component]
        fn ShrinkingInput(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
            let mut value = hooks.use_state(|| "abcd\nabcdefghij".to_string());
            let mut system = hooks.use_context_mut::<SystemContext>();
            let mut should_exit = hooks.use_state(|| false);
            hooks.use_terminal_events(move |event| {
                if matches!(event, TerminalEvent::Resize(..)) {
                    should_exit.set(true);
                }
            });
            if should_exit.get() {
                system.exit();
            }
            element! {
                View(width: 6) {
                    TextInput(
                        has_focus: true,
                        value: value.to_string(),
                        on_change: move |new_value| value.set(new_value),
                    )
                }
            }
        }

        let events = std::iter::repeat_n(
            TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Backspace)),
            5,
        )
        .chain([TerminalEvent::Resize(80, 24)]);
        let frames = element!(ShrinkingInput)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(events)))
            .map(|canvas| canvas.to_string())
            .collect::<Vec<_>>()
            .await;
        crate::render::wake::set_push_wake_for_tests(None);

        assert_eq!(frames.first().map(String::as_str), Some("fghij \n"));
        assert_eq!(frames.last().map(String::as_str), Some("abcde \n"));
    }

    #[apply(test!)]
    async fn test_text_input_multiline_newline() {
        let actual = element!(MyMultilineComponent)
            .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
                vec![
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('f'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('f'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('o'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('\n'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('\n'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('!'))),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Release, KeyCode::Char('!'))),
                ],
            )))
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .await;
        let expected = ["  \n\n\n", " foo\n ! \n\n"];
        assert_eq!(actual.first().map(String::as_str), Some(expected[0]));
        assert_eq!(actual.last().map(String::as_str), Some(expected[1]));
    }

    /// `auto_grow` derives the input's height from its wrapped row count
    /// instead of filling the container, so the rendered row count follows the
    /// content. Matches upstream #225.
    #[test]
    fn test_text_input_auto_grow_height_follows_wrapped_rows() {
        // The first frame relies on the same-frame layout pass, which requires push-wake.
        crate::render::wake::set_push_wake_for_tests(Some(true));
        #[derive(Default, Props)]
        struct AutoGrowProps {
            value: String,
        }

        #[component]
        fn AutoGrowInput(props: &AutoGrowProps) -> impl Into<AnyElement<'static>> {
            element! {
                View(width: 6, height: 4) {
                    TextInput(
                        has_focus: false,
                        multiline: true,
                        auto_grow: true,
                        value: props.value.clone(),
                        on_change: |_| {},
                    )
                }
            }
        }

        let one_row = element!(AutoGrowInput(value: "a".to_string()))
            .render(None)
            .to_string();
        let three_rows = element!(AutoGrowInput(value: "one two six".to_string()))
            .render(None)
            .to_string();
        crate::render::wake::set_push_wake_for_tests(None);

        // Height tracks the wrapped row count rather than staying fixed.
        assert_eq!(one_row.lines().filter(|l| !l.is_empty()).count(), 1);
        assert_eq!(three_rows.lines().filter(|l| !l.is_empty()).count(), 3);
    }

    #[apply(test!)]
    async fn test_default_cursor_uses_pure_inversion_without_forced_white() {
        let canvases: Vec<_> = element! {
            CursorProbe(initial_value: "".to_string(), width: 3, height: 1)
        }
        .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
            TerminalEvent::Resize(80, 24),
        ])))
        .collect()
        .await;

        let first = &canvases[0];
        let style = first
            .resolved_text_style(0, 0)
            .expect("cursor overlay missing");
        assert!(style.invert, "default cursor should invert the cell");
        assert_eq!(
            style.color, None,
            "default cursor should not force white fg"
        );
    }

    #[apply(test!)]
    async fn test_physical_cursor_mode_no_overlay_but_visible_declaration() {
        use crate::canvas::CursorDeclaration;
        let canvases: Vec<_> = element! {
            CursorProbe(initial_value: "".to_string(), width: 3, height: 1, physical_cursor: true)
        }
        .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
            TerminalEvent::Resize(80, 24),
        ])))
        .collect()
        .await;

        let first = &canvases[0];
        assert_eq!(
            first.resolved_text_style(0, 0),
            None,
            "physical_cursor mode should NOT apply any overlay"
        );
        assert_eq!(
            first.cursor_declaration(),
            Some(CursorDeclaration {
                x: 0,
                y: 0,
                visible: true,
            }),
            "physical_cursor mode should declare visible cursor"
        );
    }

    #[apply(test!)]
    async fn test_cursor_overlay_covers_both_cells_of_wide_character() {
        for (value, column) in [
            ("一二", 2),
            ("a1\u{fe0f}\u{20e3}", 1),
            ("a\u{1f1e8}\u{1f1f3}", 1),
        ] {
            let canvases: Vec<_> = element! {
                CursorProbe(initial_value: value.to_string(), width: 6, height: 1)
            }
            .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Left)),
                TerminalEvent::Resize(80, 24),
            ])))
            .collect()
            .await;

            let last = canvases.last().unwrap();
            assert!(last.resolved_text_style(column, 0).unwrap().invert);
            assert!(last.resolved_text_style(column + 1, 0).unwrap().invert);
        }
    }

    #[apply(test!)]
    async fn test_cursor_overlay_is_visible_at_multiline_trailing_newline_eof() {
        let canvases: Vec<_> = element! {
            CursorProbe(initial_value: "a\n".to_string(), width: 4, height: 2, multiline: true)
        }
        .mock_terminal_render_loop(MockTerminalConfig::with_events(stream::iter(vec![
            TerminalEvent::Resize(80, 24),
        ])))
        .collect()
        .await;

        let first = &canvases[0];
        let style = first
            .resolved_text_style(0, 1)
            .expect("expected cursor overlay on trailing newline row");
        assert!(style.invert);
    }

    #[test]
    fn test_text_buffer_cursor_movement() {
        let buffer = TextBuffer::new("foo\nbar baz", 10);
        assert_eq!(buffer.left_of_offset(2), 1);
        assert_eq!(buffer.right_of_offset(2), 3);
        assert_eq!(buffer.above_offset(2, None), 2);
        assert_eq!(buffer.below_offset(2, None), 6);
        assert_eq!(buffer.below_offset(2, Some(5)), 9);
        assert_eq!(buffer.above_offset(5, None), 1);
        assert_eq!(buffer.below_offset(5, None), 5);
        assert_eq!(buffer.above_offset(5, Some(6)), 3);
    }

    #[test]
    fn test_text_buffer_vertical_movement_uses_grapheme_columns() {
        let buffer = TextBuffer::new("12345\na\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}bc", 10);
        assert_eq!(buffer.below_offset(3, None), 25);
        assert_eq!(buffer.above_offset(25, None), 3);
        assert_eq!(buffer.below_offset(2, None), 25);
        assert_eq!(buffer.below_offset(3, Some(4)), 26);

        let buffer = TextBuffer::new("a\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}bc\n12345", 10);
        assert_eq!(buffer.above_offset(25, None), 19);
        assert_eq!(buffer.below_offset(19, None), 25);

        let buffer = TextBuffer::new("0123456789\na\tbc", 20);
        assert_eq!(buffer.below_offset(9, None), 14);

        let buffer = TextBuffer::new("a\tbc\n0123456789", 20);
        assert_eq!(buffer.above_offset(14, None), 3);
    }

    #[test]
    fn test_test_buffer_row_column_for_offset() {
        assert_eq!(
            TextBuffer::new("一二!", 10).row_column_for_offset(7),
            (0, 5)
        );

        assert_eq!(
            TextBuffer::new("foo bar bazqux", 10).row_column_for_offset(9),
            (1, 1)
        );

        assert_eq!(
            TextBuffer::new("1234512345", 10).row_column_for_offset(10),
            (0, 10)
        );

        assert_eq!(
            TextBuffer::new("12345123451", 10).row_column_for_offset(11),
            (1, 1)
        );

        assert_eq!(
            TextBuffer::new("asd asd asd", 10).row_column_for_offset(11),
            (1, 3)
        );

        assert_eq!(
            TextBuffer::new("12345123 5 ", 10).row_column_for_offset(11),
            (0, 11)
        );

        assert_eq!(
            TextBuffer::new("asd\n", 10).row_column_for_offset(4),
            (1, 0)
        );
    }

    #[test]
    fn test_new_cursor_offset() {
        let mixed_text = "你好世界，Hello World";
        let cursor_offset = mixed_text.find('W').unwrap();
        assert_eq!(
            new_cursor_offset(mixed_text, cursor_offset, "你好世界，Hello aWorld"),
            cursor_offset + 1
        );

        assert_eq!(new_cursor_offset("", 0, "foo"), 3);
        assert_eq!(new_cursor_offset("foo", 3, "foobar"), 6);
        assert_eq!(new_cursor_offset("foobar", 3, "foobar"), 3);
        assert_eq!(new_cursor_offset("foobar", 3, "fooar"), 3);
        assert_eq!(new_cursor_offset("foobar", 3, "fooasdbar"), 6);
        assert_eq!(new_cursor_offset("a\n", 0, "\n"), 0);
        assert_eq!(new_cursor_offset("asddasd\nasdasd", 3, "asdasd\nasdasd"), 2);
    }

    #[test]
    fn test_cursor_offsets_are_clamped_to_grapheme_boundaries() {
        assert_eq!(clamp_to_grapheme_boundary("éx", 0), 0);
        assert_eq!(clamp_to_grapheme_boundary("éx", 1), 0);
        assert_eq!(clamp_to_grapheme_boundary("éx", 2), 2);
        assert_eq!(clamp_to_grapheme_boundary("éx", 99), 3);

        assert_eq!(new_cursor_offset("éx", 1, "é!x"), 4);

        for offset in 0..3 {
            assert_eq!(clamp_to_grapheme_boundary("e\u{0301}x", offset), 0);
        }
        assert_eq!(clamp_to_grapheme_boundary("e\u{0301}x", 3), 3);
        assert_eq!(clamp_to_grapheme_boundary("e\u{0301}x", 99), 4);
        assert_eq!(new_cursor_offset("e\u{0301}x", 1, "e\u{0301}!x"), 5);
        assert_eq!(new_cursor_offset("ex", 1, "e\u{0301}x"), 3);
        assert_eq!(new_cursor_offset("e\n\u{0301}x", 2, "e\u{0301}x"), 0);
    }

    #[cfg(windows)]
    #[test]
    fn test_is_altgr_char() {
        let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(is_altgr_char(KeyCode::Char('@'), ctrl_alt));
        assert!(is_altgr_char(KeyCode::Char('€'), ctrl_alt));
        assert!(!is_altgr_char(KeyCode::Char('a'), ctrl_alt));
        assert!(!is_altgr_char(KeyCode::Char('1'), ctrl_alt));
        assert!(!is_altgr_char(KeyCode::Char('@'), KeyModifiers::CONTROL));
    }

    #[cfg(not(windows))]
    #[test]
    fn test_is_altgr_char() {
        let ctrl_alt = KeyModifiers::CONTROL | KeyModifiers::ALT;
        assert!(!is_altgr_char(KeyCode::Char('@'), ctrl_alt));
    }
}
