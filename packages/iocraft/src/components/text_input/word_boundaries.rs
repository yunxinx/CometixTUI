use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WordClass {
    Whitespace,
    Separator,
    Text,
}

impl WordClass {
    fn for_grapheme(grapheme: &str) -> Self {
        let character = grapheme.chars().next().expect("graphemes are nonempty");
        if character.is_whitespace() {
            Self::Whitespace
        } else if character.is_ascii_punctuation() && character != '_' {
            Self::Separator
        } else {
            Self::Text
        }
    }
}

pub(super) fn ranges(text: &str) -> impl DoubleEndedIterator<Item = Range<usize>> {
    let mut boundaries = text.split_word_bound_indices().peekable();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut previous_class = None;
    for (offset, grapheme) in text.grapheme_indices(true) {
        while boundaries.peek().is_some_and(|&(start, _)| start < offset) {
            boundaries.next();
        }
        let word_boundary = boundaries.peek().is_some_and(|&(start, _)| start == offset);
        let class = WordClass::for_grapheme(grapheme);
        // Unicode words are subdivided at ASCII punctuation, with adjacent separators grouped.
        // Only whole graphemes are classified, so punctuation with combining marks stays intact.
        if previous_class != Some(class) || (word_boundary && class == WordClass::Text) {
            ranges.push(offset..offset + grapheme.len());
        } else {
            ranges
                .last_mut()
                .expect("a continuing word has a range")
                .end = offset + grapheme.len();
        }
        previous_class = Some(class);
    }
    ranges.into_iter()
}
