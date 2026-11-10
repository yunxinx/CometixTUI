use crate::unicode_linebreak::{BreakOpportunity, linebreaks_iter};
use core::fmt::{self, Display};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn is_whitespace_grapheme(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

pub(crate) fn trim_start_whitespace(text: &str) -> &str {
    let start = text
        .grapheme_indices(true)
        .find(|(_, grapheme)| !is_whitespace_grapheme(grapheme))
        .map_or(text.len(), |(offset, _)| offset);
    &text[start..]
}

/// A `SegmentedString` is a string consisting of multiple segments, which don't have to be
/// contiguous.
///
/// This is primarily used for wrapping text as the result will include information sufficient to
/// map output regions to input data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentedString<'a> {
    segments: Vec<&'a str>,
}

/// A `SegmentedStringLine` is a line of text after wrapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentedStringLine<'a> {
    pub segments: Vec<SegmentedStringLineSegment<'a>>,
    pub width: usize,
}

impl SegmentedStringLine<'_> {
    fn join<'b>(&self, buffer: &'b mut String) -> &'b str {
        buffer.clear();
        for segment in &self.segments {
            buffer.push_str(segment.text);
        }
        buffer
    }

    // Iterates over the graphemes of `text`, which must be the joined segment text, along with the
    // position of the segment each grapheme starts in.
    fn graphemes<'b>(&'b self, text: &'b str) -> impl Iterator<Item = (usize, usize, &'b str)> {
        let mut segment = 0;
        let mut segment_end = 0;
        text.grapheme_indices(true).map(move |(start, grapheme)| {
            while start >= segment_end {
                segment_end += self.segments[segment].text.len();
                segment += 1;
            }
            (segment - 1, start, grapheme)
        })
    }

    /// Calls `f` with consecutive spans of the line's text, along with the index of the segment each
    /// span came from. A grapheme spanning multiple segments is included whole in the span of the
    /// segment it starts in.
    pub(crate) fn for_each_grapheme_span(
        &self,
        buffer: &mut String,
        mut f: impl FnMut(&str, usize),
    ) {
        if let [segment] = self.segments.as_slice() {
            f(segment.text, segment.index);
            return;
        }
        let text = self.join(buffer);
        let mut span: Option<(usize, usize)> = None;
        for (segment, start, _) in self.graphemes(text) {
            match span {
                Some((span_segment, _)) if span_segment == segment => {}
                _ => {
                    if let Some((span_segment, span_start)) = span {
                        f(&text[span_start..start], self.segments[span_segment].index);
                    }
                    span = Some((segment, start));
                }
            }
        }
        if let Some((span_segment, span_start)) = span {
            f(&text[span_start..], self.segments[span_segment].index);
        }
    }

    /// Removes trailing whitespace from the line.
    pub fn trim_end(&mut self) {
        let len = self.len();
        while let Some(segment) = self.segments.last_mut() {
            segment.text = segment.text.trim_end();
            if !segment.text.is_empty() {
                break;
            }
            self.segments.pop();
        }
        if self.len() != len {
            self.recompute_widths();
        }
    }

    fn recompute_widths(&mut self) {
        let text = self.to_string();
        for segment in &mut self.segments {
            segment.width = 0;
        }
        let mut segment = 0;
        let mut segment_end = 0;
        let mut col = 0;
        // Measure the joined stream so escape sequences retain the canvas's zero-width semantics.
        for (start, _, columns) in crate::canvas::positioned_graphemes(&text, 0) {
            while start >= segment_end {
                segment_end += self.segments[segment].text.len();
                segment += 1;
            }
            self.segments[segment - 1].width += (columns.end - columns.start) as usize;
            col = columns.end as usize;
        }
        self.width = col;
    }

    fn len(&self) -> usize {
        self.segments.iter().map(|segment| segment.text.len()).sum()
    }
}

impl Display for SegmentedStringLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for segment in &self.segments {
            write!(f, "{}", segment)?;
        }
        Ok(())
    }
}

/// A `SegmentedStringLineSegment` is a segment making up part of a `SegmentedStringLine`, along
/// with information about where it came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SegmentedStringLineSegment<'a> {
    pub text: &'a str,
    pub index: usize,
    pub offset: usize,
    pub width: usize,
}

impl Display for SegmentedStringLineSegment<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.text)
    }
}

fn is_hard_line_break(grapheme: &str) -> bool {
    !grapheme.is_empty()
        && grapheme
            .chars()
            .all(crate::unicode_linebreak::is_hard_line_break)
}

struct Grapheme {
    start: usize,
    end: usize,
    is_whitespace: bool,
}

// A line under construction, as a byte range of the joined text.
#[derive(Clone, Copy)]
struct LineRange {
    start: usize,
    end: usize,
    width: usize,
}

impl LineRange {
    fn at(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
            width: 0,
        }
    }
}

impl<'a> SegmentedString<'a> {
    /// Wraps the string into lines of a given width without splitting extended grapheme clusters.
    pub fn wrap(&self, width: usize) -> Vec<SegmentedStringLine<'_>> {
        if self.segments.is_empty() {
            return vec![];
        }

        // Graphemes and line break rules can span segments, so wrap the joined text, then map each
        // line back onto the segments.
        let text: Cow<'_, str> = match self.segments.as_slice() {
            [segment] => Cow::Borrowed(segment),
            segments => Cow::Owned(segments.concat()),
        };
        let graphemes: Vec<Grapheme> = text
            .grapheme_indices(true)
            .map(|(start, grapheme)| Grapheme {
                start,
                end: start + grapheme.len(),
                is_whitespace: is_whitespace_grapheme(grapheme),
            })
            .collect();

        let mut ranges = Vec::new();
        let mut line = LineRange::at(0);
        let mut chunk_start = 0;
        let mut chunk_graphemes_start = 0;
        for (offset, opportunity) in linebreaks_iter(text.char_indices(), text.len()) {
            let chunk_graphemes_end = chunk_graphemes_start
                + graphemes[chunk_graphemes_start..].partition_point(|g| g.start < offset);
            let on_grapheme_boundary = offset == text.len()
                || graphemes
                    .get(chunk_graphemes_end)
                    .is_some_and(|g| g.start == offset);
            if opportunity == BreakOpportunity::Allowed && !on_grapheme_boundary {
                continue;
            }

            let mut chunk = &graphemes[chunk_graphemes_start..chunk_graphemes_end];
            if opportunity == BreakOpportunity::Mandatory
                && let Some((last, rest)) = chunk.split_last()
                && is_hard_line_break(&text[last.start..last.end])
            {
                chunk = rest;
            }
            let chunk_end = chunk.last().map_or(chunk_start, |g| g.end);
            let trailing_whitespace = chunk.iter().rev().take_while(|g| g.is_whitespace).count();
            let visible_end = chunk[..chunk.len() - trailing_whitespace]
                .last()
                .map_or(chunk_start, |g| g.end);
            let visible_width = crate::canvas::string_display_width_from_col(
                &text[chunk_start..visible_end],
                line.width,
            );

            if line.width + visible_width <= width {
                line.end = chunk_end;
                line.width += crate::canvas::string_display_width_from_col(
                    &text[chunk_start..chunk_end],
                    line.width,
                );
            } else {
                if line.width > 0 {
                    ranges.push(line);
                    line = LineRange::at(chunk_start);
                }
                for (i, g) in chunk.iter().enumerate() {
                    let grapheme = &text[g.start..g.end];
                    let mut grapheme_width =
                        crate::canvas::string_display_width_from_col(grapheme, line.width);
                    if i < chunk.len() - trailing_whitespace
                        && line.width > 0
                        && line.width + grapheme_width > width
                    {
                        ranges.push(line);
                        line = LineRange::at(g.start);
                        grapheme_width = crate::canvas::string_display_width_from_col(grapheme, 0);
                    }
                    line.end = g.end;
                    line.width += grapheme_width;
                }
            }

            if opportunity == BreakOpportunity::Mandatory {
                // We have to break here
                ranges.push(line);
                line = LineRange::at(offset);
            }
            chunk_start = offset;
            chunk_graphemes_start = chunk_graphemes_end;
        }

        // Add another line if the text ends with a line break. Empty text is also a single line.
        if ranges.is_empty() || text.ends_with(crate::unicode_linebreak::is_hard_line_break) {
            ranges.push(LineRange::at(text.len()));
        }

        self.lines_for_ranges(&ranges)
    }

    fn lines_for_ranges(&self, ranges: &[LineRange]) -> Vec<SegmentedStringLine<'a>> {
        let segment_starts: Vec<usize> = self
            .segments
            .iter()
            .scan(0, |start, segment| {
                let segment_start = *start;
                *start += segment.len();
                Some(segment_start)
            })
            .collect();
        let segment_end = |index: usize| segment_starts[index] + self.segments[index].len();

        let mut segment = 0;
        ranges
            .iter()
            .map(|range| {
                while segment + 1 < self.segments.len() && segment_end(segment) <= range.start {
                    segment += 1;
                }
                let mut line = SegmentedStringLine {
                    segments: vec![],
                    width: range.width,
                };
                for (index, (&segment_start, text)) in segment_starts
                    .iter()
                    .zip(&self.segments)
                    .enumerate()
                    .skip(segment)
                {
                    if segment_start >= range.end {
                        break;
                    }
                    let start = range.start.max(segment_start) - segment_start;
                    let end = range.end.min(segment_start + text.len()) - segment_start;
                    if start < end {
                        line.segments.push(SegmentedStringLineSegment {
                            text: &text[start..end],
                            index,
                            offset: start,
                            width: 0,
                        });
                    }
                }
                if line.segments.is_empty() {
                    let offset = range.start - segment_starts[segment];
                    line.segments.push(SegmentedStringLineSegment {
                        text: &self.segments[segment][offset..offset],
                        index: segment,
                        offset,
                        width: 0,
                    });
                }
                line.recompute_widths();
                line
            })
            .collect()
    }
}

impl<'a> From<&'a str> for SegmentedString<'a> {
    fn from(text: &'a str) -> Self {
        [text].into_iter().collect()
    }
}

impl<'a> FromIterator<&'a str> for SegmentedString<'a> {
    fn from_iter<T: IntoIterator<Item = &'a str>>(iter: T) -> Self {
        SegmentedString {
            segments: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrapping_keeps_graphemes_across_segments_and_trimming() {
        // Each tuple records the rendered text, width and original segment/byte ranges.
        for (parts, width, expected) in [
            (
                vec!["👩", "\u{200d}💻b"],
                3,
                vec![("👩‍💻b", 3, vec![(0, 0, "👩", 2), (1, 0, "\u{200d}💻b", 1)])],
            ),
            (
                vec!["🇺", "🇸 ", "\u{301}x"],
                1,
                vec![
                    ("🇺🇸", 2, vec![(0, 0, "🇺", 2), (1, 0, "🇸", 0)]),
                    (" \u{301}", 1, vec![(1, 4, " ", 1), (2, 0, "\u{301}", 0)]),
                    ("x", 1, vec![(2, 2, "x", 1)]),
                ],
            ),
            (
                vec!["a \u{301}b"],
                1,
                vec![
                    ("a", 1, vec![(0, 0, "a", 1)]),
                    (" \u{301}", 1, vec![(0, 1, " \u{301}", 1)]),
                    ("b", 1, vec![(0, 4, "b", 1)]),
                ],
            ),
            (
                vec!["a ", "\u{301}b"],
                1,
                vec![
                    ("a", 1, vec![(0, 0, "a", 1)]),
                    (" \u{301}", 1, vec![(0, 1, " ", 1), (1, 0, "\u{301}", 0)]),
                    ("b", 1, vec![(1, 2, "b", 1)]),
                ],
            ),
        ] {
            let text: SegmentedString = parts.iter().copied().collect();
            let lines = text.wrap(width);
            assert_eq!(lines.len(), expected.len(), "{parts:?}");
            for (mut line, (content, width, sources)) in lines.into_iter().zip(expected) {
                assert_eq!((line.to_string(), line.width), (content.to_string(), width));
                assert_eq!(
                    line.segments
                        .iter()
                        .map(|s| (s.index, s.offset, s.text, s.width))
                        .collect::<Vec<_>>(),
                    sources,
                );
                line.trim_end();
                assert_eq!((line.to_string(), line.width), (content.to_string(), width));
            }
        }

        for parts in [vec!["\u{600}", " "], vec!["\u{600} "]] {
            let text: SegmentedString = parts.into_iter().collect();
            let mut line = text.wrap(4).pop().unwrap();
            assert_eq!(line.width, 1);
            line.trim_end();
            assert_eq!((line.to_string(), line.width), ("\u{600}".to_string(), 0));
            assert_eq!(line.segments.len(), 1);
            assert_eq!((line.segments[0].index, line.segments[0].offset), (0, 0));
        }

        use crate::{
            ElementExt,
            components::{MixedText, MixedTextContent, Text, TextWrap, View},
            element,
        };
        let text = element! {
            View(width: 1) { Text(wrap: TextWrap::WrapTrim, content: " \u{301}b") }
        }
        .render(None);
        let mixed = element! {
            View(width: 1) {
                MixedText(wrap: TextWrap::WrapTrim, contents: vec![
                    MixedTextContent::new(" "), MixedTextContent::new("\u{301}b")
                ])
            }
        }
        .render(None);
        assert_eq!(text.to_string(), " \u{301}\nb\n");
        assert_eq!(mixed.to_string(), " \u{301}\nb\n");
    }

    #[test]
    fn test_segmented_string_wrap() {
        {
            let segmented_string = SegmentedString::from("Hello, world! This is a test string.");
            let lines = segmented_string.wrap(12);
            assert_eq!(
                lines,
                vec![
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "Hello, ",
                            index: 0,
                            offset: 0,
                            width: 7
                        },],
                        width: 7,
                    },
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "world! This ",
                            index: 0,
                            offset: 7,
                            width: 12
                        },],
                        width: 12,
                    },
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "is a test ",
                            index: 0,
                            offset: 19,
                            width: 10
                        },],
                        width: 10,
                    },
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "string.",
                            index: 0,
                            offset: 29,
                            width: 7
                        },],
                        width: 7,
                    }
                ],
            );
        }

        {
            let segmented_string = SegmentedString::from("foo bar");
            let lines = segmented_string
                .wrap(0)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(lines, vec!["f", "o", "o ", "b", "a", "r"]);
        }

        {
            let segmented_string = SegmentedString::from("Hello, world! This is a test string.");
            let lines = segmented_string
                .wrap(12)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec!["Hello, ", "world! This ", "is a test ", "string.",]
            );
        }

        {
            let segmented_string = SegmentedString::from("Hello, world! This is a test string.");
            let lines = segmented_string
                .wrap(11)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec!["Hello, ", "world! This ", "is a test ", "string.",]
            );
        }

        {
            let segmented_string =
                SegmentedString::from("Hello, thisisalongunbreakablemultiline str.");
            let lines = segmented_string
                .wrap(12)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec!["Hello, ", "thisisalongu", "nbreakablemu", "ltiline str.",]
            );
        }

        {
            let segmented_string =
                SegmentedString::from("Hello, this\nstring\nhas\nnewlines in it.\n\n");
            let lines = segmented_string
                .wrap(11)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec![
                    "Hello, this",
                    "string",
                    "has",
                    "newlines in ",
                    "it.",
                    "",
                    ""
                ]
            );
        }

        {
            let segmented_string: SegmentedString =
                ["this is ", "a wrapping test"].into_iter().collect();
            let lines = segmented_string
                .wrap(14)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(lines, vec!["this is a ", "wrapping test"]);
        }

        {
            // CC structured text wraps the flattened plain string once. A
            // long later segment must consume the first segment's remaining
            // width rather than starting its own width counter at zero.
            let segmented_string: SegmentedString =
                ["(", "notes.ipynb", "@cell)"].into_iter().collect();
            let lines = segmented_string
                .wrap(7)
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>();
            assert_eq!(lines, vec!["(notes.", "ipynb@c", "ell)"]);
        }

        {
            let segmented_string = SegmentedString::from("a\tb");
            let lines = segmented_string.wrap(8);
            assert_eq!(
                lines,
                vec![
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "a\t",
                            index: 0,
                            offset: 0,
                            width: 8,
                        }],
                        width: 8,
                    },
                    SegmentedStringLine {
                        segments: vec![SegmentedStringLineSegment {
                            text: "b",
                            index: 0,
                            offset: 2,
                            width: 1,
                        }],
                        width: 1,
                    },
                ]
            );
        }
    }
}
