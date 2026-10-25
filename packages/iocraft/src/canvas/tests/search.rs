#![allow(unused_imports)]
use super::super::*;
use crate::prelude::*;
use crossterm::csi;

#[test]
fn test_search_folds_unicode_and_maps_expansions_to_terminal_cells() {
    let mut canvas = Canvas::new(24, 1);
    canvas.subview_mut(0, 0, 0, 0, 24, 1).set_text(
        0,
        0,
        "ΟΣ οσ ος 中Straße İ",
        CanvasTextStyle::default(),
    );
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let mut packed = CanvasPackedScreen::new(24, 1);
    packed.write_line_with_cache(
        &mut pools,
        &mut line_cache,
        0,
        0,
        "ΟΣ οσ ος 中Straße İ",
        CanvasResolvedStyle::default(),
        None,
    );
    for query in ["ΟΣ", "οσ", "ος"] {
        assert_eq!(
            canvas.scan_text_positions(query),
            vec![
                TextMatchPosition {
                    row: 0,
                    col: 0,
                    len: 2
                },
                TextMatchPosition {
                    row: 0,
                    col: 3,
                    len: 2
                },
                TextMatchPosition {
                    row: 0,
                    col: 6,
                    len: 2
                },
            ],
            "{query}"
        );
        assert_eq!(
            packed.scan_text_positions(&pools, query),
            canvas.scan_text_positions(query)
        );
        assert!(canvas.apply_search_highlight(query, StyleOverlay::inverse()));
        assert!(canvas.resolved_text_style(1, 0).unwrap().invert);
    }
    for (query, col, len) in [("中STRASSE", 9, 8), ("sse", 15, 2), ("i\u{307}", 18, 1)] {
        assert_eq!(
            packed.scan_text_positions(&pools, query),
            canvas.scan_text_positions(query)
        );
        assert_eq!(
            canvas.scan_text_positions(query),
            vec![TextMatchPosition { row: 0, col, len }]
        );
    }
}

#[test]
fn test_scan_text_positions_is_case_insensitive_and_wide_aware() {
    let mut canvas = Canvas::new(12, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "ab中c AB", CanvasTextStyle::default());

    assert_eq!(
        canvas.scan_text_positions("中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "match spans should be measured in terminal cells, including wide tails"
    );
    assert_eq!(
        canvas.scan_text_positions("ab"),
        vec![
            TextMatchPosition {
                row: 0,
                col: 0,
                len: 2,
            },
            TextMatchPosition {
                row: 0,
                col: 6,
                len: 2,
            },
        ],
        "search should be case-insensitive and non-overlapping"
    );
}

#[test]
fn test_scan_text_positions_region_returns_subtree_relative_positions() {
    let mut canvas = Canvas::new(16, 3);
    canvas
        .subview_mut(0, 0, 0, 0, 16, 3)
        .set_text(2, 1, "xx lazy 中c", CanvasTextStyle::default());
    canvas.mark_no_select_region(5, 1, 4, 1);

    assert_eq!(
        canvas.scan_text_positions_region(5, 1, 10, 1, "lazy"),
        Vec::<TextMatchPosition>::new(),
        "region scanning should respect noSelect metadata"
    );
    assert_eq!(
        canvas.scan_text_positions_region(8, 1, 8, 1, "中c"),
        vec![TextMatchPosition {
            row: 0,
            col: 2,
            len: 3,
        }],
        "positions should be relative to the scanned region and wide-aware"
    );
}

#[test]
fn test_apply_search_highlight_skips_no_select_and_marks_damage() {
    let mut canvas = Canvas::new(12, 1);
    canvas
        .subview_mut(0, 0, 0, 0, 12, 1)
        .set_text(0, 0, "foo foo", CanvasTextStyle::default());
    canvas.mark_no_select_region(0, 0, 3, 1);

    let applied = canvas.apply_search_highlight(
        "foo",
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    );

    assert!(applied);
    for col in 0..3 {
        assert!(!canvas.resolved_text_style(col, 0).unwrap().invert);
    }
    for col in 4..7 {
        assert!(canvas.resolved_text_style(col, 0).unwrap().invert);
    }
    assert_eq!(
        canvas.damage_region(),
        Some(DamageRegion {
            x: 4,
            y: 0,
            width: 3,
            height: 1,
        }),
        "search highlight should damage only highlighted selectable cells"
    );
}

#[test]
fn test_apply_positioned_highlight_translates_and_clips_row_offset() {
    let mut canvas = Canvas::new(8, 2);
    canvas
        .subview_mut(0, 0, 0, 0, 8, 2)
        .set_text(0, 1, "target", CanvasTextStyle::default());
    let positions = vec![TextMatchPosition {
        row: 0,
        col: 0,
        len: 6,
    }];

    assert!(!canvas.apply_positioned_highlight(
        &positions,
        -1,
        0,
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    ));
    assert!(canvas.apply_positioned_highlight(
        &positions,
        1,
        0,
        StyleOverlay {
            invert: Some(true),
            ..Default::default()
        },
    ));
    assert!(canvas.resolved_text_style(0, 1).unwrap().invert);
    assert!(canvas.resolved_text_style(5, 1).unwrap().invert);
}

// ----- P1-5: wide character / grapheme cluster tests -----
