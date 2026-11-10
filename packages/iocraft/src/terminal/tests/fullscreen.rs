use super::super::*;
use crate::prelude::*;

#[test]
fn test_fullscreen_frame_width_shrink_clears_removed_columns() {
    for packed in [false, true] {
        for terminal_width in [10, 14] {
            for top_row in [0, 1] {
                for (width, content) in [(5, "abc"), (5, "abcde"), (0, "")] {
                    let make_canvas = |width, content| {
                        let mut canvas = Canvas::new(width, 1);
                        let mut view = canvas.subview_mut(0, 0, 0, 0, width, 1);
                        view.set_background_color(0, 0, width, 1, Color::DarkBlue);
                        view.set_text(0, 0, content, CanvasTextStyle::default());
                        canvas.clear_damage();
                        canvas
                    };
                    let first = make_canvas(10, "abcdefghij");
                    let next = make_canvas(width, content);
                    let options = TerminalFullscreenCanvasFramePatchOptions {
                        canvas_diff: TerminalFullscreenCanvasDiffOptions {
                            top_row,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let plans = if packed {
                        let mut pools = CanvasPackedCellPools::new();
                        let first = first.pack_with(&mut pools);
                        let next = next.pack_with(&mut pools);
                        let mut state = TerminalFullscreenPackedCanvasFrameState::new();
                        [
                            state.plan_frame(&first, &pools, options.clone()),
                            state.plan_frame(&next, &pools, options.clone()),
                            state.plan_frame(&next, &pools, options),
                        ]
                    } else {
                        let mut state = TerminalFullscreenCanvasFrameState::new();
                        [
                            state.plan_frame(&first, options.clone()),
                            state.plan_frame(&next, options.clone()),
                            state.plan_frame(&next, options),
                        ]
                    };
                    assert!(!plans[0].is_empty());
                    assert!(!plans[1].is_empty());
                    assert!(plans[2].is_empty());

                    let mut vt = avt::Vt::new(terminal_width, 3);
                    let outside = if terminal_width == 14 { "KEEP" } else { "" };
                    vt.feed_str(&format!(
                        "\x1b[{};1H\x1b[42m0123456789{outside}\x1b[0m",
                        top_row + 1
                    ));
                    for (frame, plan) in plans.iter().enumerate() {
                        let ansi = terminal_patches_to_ansi(&plan.patches, true);
                        vt.feed_str(&ansi);
                        let expected = if frame == 0 {
                            format!("abcdefghij{outside}")
                        } else {
                            format!("{content:10}{outside}")
                        };
                        assert_eq!(
                            vt.line(top_row as usize).text(),
                            expected,
                            "packed={packed}, width={width}, frame={frame}, {ansi:?}"
                        );
                        for (col, cell) in vt.line(top_row as usize).cells().iter().enumerate() {
                            let background = if col >= 10 {
                                Some(avt::Color::Indexed(2))
                            } else if frame == 0 || col < width {
                                Some(avt::Color::Indexed(4))
                            } else {
                                None
                            };
                            assert_eq!(cell.pen().background(), background, "column {col}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn test_plan_terminal_fullscreen_diff_patches_matches_cc_alt_screen_anchor_and_park() {
    let mut diff = vec![TerminalPatch::Stdout("body".to_string())];
    let plan = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: !diff.is_empty(),
        erase_before_paint: false,
        terminal_rows: Some(24),
    });

    assert_eq!(
        plan.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[H".to_string()))
    );
    assert_eq!(
        plan.post_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[24;1H".to_string()))
    );
    plan.apply_to(&mut diff);
    assert_eq!(
        terminal_patches_to_ansi(&diff, true),
        "\x1b[Hbody\x1b[24;1H"
    );

    let erase = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: true,
        erase_before_paint: true,
        terminal_rows: Some(10),
    });
    assert_eq!(
        erase.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[2J\x1b[H".to_string()))
    );
    assert_eq!(
        erase.post_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[10;1H".to_string()))
    );

    assert!(
        plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: false,
            has_diff: true,
            erase_before_paint: true,
            terminal_rows: Some(24),
        })
        .is_empty()
    );
    assert!(
        plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
            fullscreen: true,
            has_diff: false,
            erase_before_paint: true,
            terminal_rows: Some(24),
        })
        .is_empty()
    );

    let no_size = plan_terminal_fullscreen_diff_patches(TerminalFullscreenDiffPatchOptions {
        fullscreen: true,
        has_diff: true,
        erase_before_paint: false,
        terminal_rows: None,
    });
    assert_eq!(
        no_size.pre_diff_patch,
        Some(TerminalPatch::Stdout("\x1b[H".to_string()))
    );
    assert_eq!(no_size.post_diff_patch, None);
}

#[test]
fn test_terminal_fullscreen_canvas_diff_patches_use_absolute_rows_and_sparse_prefixes() {
    let mut prev = Canvas::new(6, 2);
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "abcdef", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 1, "stable", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 1);
    next.subview_mut(0, 0, 0, 0, 6, 1)
        .set_text(0, 0, "abXYef", CanvasTextStyle::default());
    next.clear_damage();

    let diff = terminal_fullscreen_canvas_diff_patches(
        Some(&prev),
        &next,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 1,
            force_full_repaint: false,
            terminal_rows: None,
        },
    );
    let ansi = terminal_patches_to_ansi(&diff, true);

    assert!(
        ansi.starts_with("\x1b[2;3HXYef"),
        "changed row should be addressed from first changed column: {ansi:?}"
    );
    assert!(
        ansi.contains("\x1b[3;1H\x1b[2K"),
        "removed rows should be cleared in-place: {ansi:?}"
    );
    assert!(
        !ansi.contains("stable"),
        "unchanged removed content must not be rewritten: {ansi:?}"
    );

    let full = terminal_fullscreen_canvas_diff_patches(
        None,
        &next,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 2,
            force_full_repaint: false,
            terminal_rows: None,
        },
    );
    let full_ansi = terminal_patches_to_ansi(&full, true);
    assert!(
        full_ansi.starts_with("\x1b[3;1HabXYef"),
        "first fullscreen write should address canvas origin absolutely: {full_ansi:?}"
    );
}

#[test]
fn test_terminal_fullscreen_packed_canvas_diff_patches_use_sparse_packed_rows() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 2);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abcdef", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "stable", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 1);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "abXYef", style, None);
    next.clear_damage();

    let mut style_cache = CanvasStyleTransitionCache::new();
    let diff = terminal_fullscreen_packed_canvas_diff_patches(
        Some(&prev),
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 1,
            force_full_repaint: false,
            terminal_rows: None,
        },
    );
    let ansi = terminal_patches_to_ansi(&diff, true);

    assert!(
        ansi.starts_with("\x1b[2;3HXYef"),
        "packed changed row should be addressed from first changed column: {ansi:?}"
    );
    assert!(
        ansi.contains("\x1b[3;1H\x1b[2K"),
        "packed removed rows should be cleared in-place: {ansi:?}"
    );
    assert!(
        !ansi.contains("stable"),
        "unchanged removed packed content must not be rewritten: {ansi:?}"
    );

    let full = terminal_fullscreen_packed_canvas_diff_patches(
        None,
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasDiffOptions {
            top_row: 2,
            force_full_repaint: false,
            terminal_rows: None,
        },
    );
    let full_ansi = terminal_patches_to_ansi(&full, true);
    assert!(
        full_ansi.starts_with("\x1b[3;1HabXYef"),
        "first packed fullscreen write should address origin absolutely: {full_ansi:?}"
    );
}

#[test]
fn test_plan_terminal_fullscreen_packed_canvas_frame_patches_orders_scroll_diff_and_cursor() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 2);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "old", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 2);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "new", style, None);
    next.clear_damage();

    let mut style_cache = CanvasStyleTransitionCache::new();
    let plan = plan_terminal_fullscreen_packed_canvas_frame_patches(
        Some(&prev),
        &next,
        &pools,
        &mut style_cache,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
                terminal_rows: None,
            },
            scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    let ansi = terminal_patches_to_ansi(&plan.patches, true);

    assert!(plan.had_scroll_patch);
    assert_eq!(plan.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
        "cursor anchor must precede DECSTBM and packed row repairs: {ansi:?}"
    );
    assert!(
        ansi.ends_with("\x1b[24;1H"),
        "packed fullscreen patch plan should park the cursor after content diff: {ansi:?}"
    );
}

#[test]
fn test_plan_terminal_fullscreen_canvas_frame_patches_orders_scroll_diff_and_cursor_like_cc() {
    let mut prev = Canvas::new(6, 2);
    prev.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "old", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 2);
    next.subview_mut(0, 0, 0, 0, 6, 2)
        .set_text(0, 0, "new", CanvasTextStyle::default());
    next.clear_damage();

    let plan = plan_terminal_fullscreen_canvas_frame_patches(
        Some(&prev),
        &next,
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
                terminal_rows: None,
            },
            scroll_patch_ansi: Some("\x1b[1;2r\x1b[1S\x1b[r\x1b[H".to_string()),
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    );
    let ansi = terminal_patches_to_ansi(&plan.patches, true);

    assert!(plan.had_scroll_patch);
    assert_eq!(plan.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;2r\x1b[1S\x1b[r\x1b[H\x1b[1;1Hnew"),
        "cursor anchor must precede DECSTBM and row repairs: {ansi:?}"
    );
    assert!(
        ansi.ends_with("\x1b[24;1H"),
        "fullscreen patch plan should park the cursor after content diff: {ansi:?}"
    );

    let empty = plan_terminal_fullscreen_canvas_frame_patches(
        Some(&next),
        &next,
        TerminalFullscreenCanvasFramePatchOptions::default(),
    );
    assert!(empty.is_empty());
    assert_eq!(empty.content_patch_count, 0);
}

#[test]
fn test_terminal_fullscreen_canvas_frame_state_tracks_previous_and_scroll_hint() {
    let mut state = TerminalFullscreenCanvasFrameState::new();
    let mut first = Canvas::new(6, 3);
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    first
        .subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());
    first.clear_damage();

    let initial = state.plan_frame(
        &first,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(!initial.is_empty());
    assert_eq!(state.previous().unwrap().get_text(0, 0, 6, 1), "aaa");

    let identical = state.plan_frame(
        &first,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(
        identical.is_empty(),
        "stateful identical frame should be a no-op"
    );

    let mut next = Canvas::new(6, 3);
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "bbb", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "ccc", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ddd", CanvasTextStyle::default());
    next.clear_damage();

    let scroll = state
        .plan_scroll_frame(
            &next,
            Some(crate::canvas::ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            }),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        )
        .unwrap();
    let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
    assert!(scroll.had_scroll_patch());
    assert!(
        ansi.contains("ddd"),
        "incoming edge row should be repainted: {ansi:?}"
    );
    assert_eq!(state.previous().unwrap().get_text(0, 2, 6, 1), "ddd");

    state.reset();
    let first_again = state
        .plan_scroll_frame(
            &next,
            Some(crate::canvas::ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            }),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions::default(),
        )
        .unwrap();
    assert_eq!(first_again.scroll_hint_plan, None);
    assert!(!first_again.had_scroll_patch());
}

#[test]
fn test_terminal_fullscreen_packed_canvas_frame_state_tracks_previous_and_scroll_hint() {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut state = TerminalFullscreenPackedCanvasFrameState::new();
    let mut first = CanvasPackedScreen::new(6, 3);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
    first.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
    first.clear_damage();

    let initial = state.plan_frame(
        &first,
        &pools,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(!initial.is_empty());
    assert_eq!(
        state.previous().unwrap().char_in_cell(&pools, 0, 0),
        Some("a")
    );

    let identical = state.plan_frame(
        &first,
        &pools,
        TerminalFullscreenCanvasFramePatchOptions {
            terminal_rows: Some(24),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    );
    assert!(identical.is_empty());

    let mut next = CanvasPackedScreen::new(6, 3);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
    next.clear_damage();

    let scroll = state
        .plan_scroll_frame(
            &next,
            &pools,
            Some(crate::canvas::ScrollHint {
                top: 0,
                bottom: 2,
                delta: 1,
            }),
            TerminalScrollHintPatchOptions::fullscreen_synchronized(),
            TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: Some(24),
                ..TerminalFullscreenCanvasFramePatchOptions::default()
            },
        )
        .unwrap();
    let ansi = terminal_patches_to_ansi(&scroll.frame.patches, true);
    assert!(scroll.had_scroll_patch());
    assert!(
        ansi.contains("ddd"),
        "packed incoming edge should repaint: {ansi:?}"
    );
    assert_eq!(
        state.previous().unwrap().char_in_cell(&pools, 0, 2),
        Some("d")
    );
}

#[test]
fn test_plan_terminal_fullscreen_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm() {
    let mut prev = Canvas::new(6, 3);
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "aaa", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "bbb", CanvasTextStyle::default());
    prev.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ccc", CanvasTextStyle::default());
    prev.clear_damage();

    let mut next = Canvas::new(6, 3);
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 0, "bbb", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 1, "ccc", CanvasTextStyle::default());
    next.subview_mut(0, 0, 0, 0, 6, 3)
        .set_text(0, 2, "ddd", CanvasTextStyle::default());
    next.clear_damage();

    let hint = crate::canvas::ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    };
    let plan = plan_terminal_fullscreen_canvas_scroll_frame_patches(
        &prev,
        &next,
        Some(hint),
        TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
                terminal_rows: None,
            },
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    )
    .unwrap();
    let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

    assert_eq!(
        plan.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Emit(
            "\x1b[1;3r\x1b[1S\x1b[r\x1b[H".to_string()
        ))
    );
    assert!(plan.had_scroll_patch());
    assert_eq!(plan.frame.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
        "scroll frame should shift previous baseline and repaint only incoming edge rows: {ansi:?}"
    );
    assert!(
        !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
        "shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
    );

    let skipped = plan_terminal_fullscreen_canvas_scroll_frame_patches(
        &prev,
        &next,
        Some(hint),
        TerminalScrollHintPatchOptions {
            fullscreen: false,
            synchronized_output: true,
        },
        TerminalFullscreenCanvasFramePatchOptions {
            scroll_patch_ansi: Some("unsafe-scroll-patch".to_string()),
            ..TerminalFullscreenCanvasFramePatchOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        skipped.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Skip(
            TerminalScrollHintPatchSkipReason::NotFullscreen
        ))
    );
    assert!(!skipped.had_scroll_patch());
    assert!(
        !terminal_patches_to_ansi(&skipped.frame.patches, true).contains("unsafe-scroll-patch"),
        "safety-gated helper must drop caller-provided scroll prefixes when a supplied hint is skipped"
    );
}

#[test]
fn test_plan_terminal_fullscreen_packed_canvas_scroll_frame_patches_shifts_previous_like_cc_decstbm()
 {
    let mut pools = CanvasPackedCellPools::new();
    let mut line_cache = CanvasPackedLineCache::new();
    let style = CanvasResolvedStyle::default();
    let mut prev = CanvasPackedScreen::new(6, 3);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "aaa", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "bbb", style, None);
    prev.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ccc", style, None);
    prev.clear_damage();

    let mut next = CanvasPackedScreen::new(6, 3);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 0, "bbb", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 1, "ccc", style, None);
    next.write_line_with_cache(&mut pools, &mut line_cache, 0, 2, "ddd", style, None);
    next.clear_damage();

    let hint = crate::canvas::ScrollHint {
        top: 0,
        bottom: 2,
        delta: 1,
    };
    let mut style_cache = CanvasStyleTransitionCache::new();
    let plan = plan_terminal_fullscreen_packed_canvas_scroll_frame_patches(
        &prev,
        &next,
        &pools,
        &mut style_cache,
        Some(hint),
        TerminalScrollHintPatchOptions::fullscreen_synchronized(),
        TerminalFullscreenCanvasFramePatchOptions {
            canvas_diff: TerminalFullscreenCanvasDiffOptions {
                top_row: 0,
                force_full_repaint: false,
                terminal_rows: None,
            },
            scroll_patch_ansi: None,
            erase_before_paint: false,
            terminal_rows: Some(24),
            optimize: true,
        },
    )
    .unwrap();
    let ansi = terminal_patches_to_ansi(&plan.frame.patches, true);

    assert!(matches!(
        plan.scroll_hint_plan,
        Some(TerminalScrollHintPatchPlan::Emit(ref patch))
            if patch == "\x1b[1;3r\x1b[1S\x1b[r\x1b[H"
    ));
    assert!(plan.had_scroll_patch());
    assert_eq!(plan.frame.content_patch_count, 3);
    assert!(
        ansi.starts_with("\x1b[H\x1b[1;3r\x1b[1S\x1b[r\x1b[H\x1b[3;1Hddd"),
        "packed scroll frame should shift previous baseline before row diff: {ansi:?}"
    );
    assert!(
        !ansi.contains("\x1b[1;1Hbbb") && !ansi.contains("\x1b[2;1Hccc"),
        "packed shifted baseline should avoid rewriting stable scrolled rows: {ansi:?}"
    );
}

fn fullscreen_height_test_canvas(rows: &[&str]) -> Canvas {
    let mut canvas = Canvas::new(10, rows.len());
    for (y, text) in rows.iter().enumerate() {
        canvas.subview_mut(0, 0, 0, 0, 10, rows.len()).set_text(
            0,
            y as isize,
            text,
            CanvasTextStyle::default(),
        );
    }
    canvas.clear_damage();
    canvas
}

fn fullscreen_height_test_vt() -> avt::Vt {
    let seed = fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "old bottom"]);
    let mut bytes = Vec::new();
    seed.write_ansi_without_final_newline(&mut bytes).unwrap();
    let mut vt = avt::Vt::new(10, 4);
    vt.feed_str(&String::from_utf8(bytes).unwrap());
    vt
}

#[test]
fn test_fullscreen_shrink_preserves_bottom_border_and_clears_visible_removed_rows() {
    let previous =
        fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "old bottom", "tail4", "tail5"]);
    for next_rows in [
        &["TITLE", "body", "FOOTER", "╰────────╯"][..],
        &["TITLE", "body"][..],
    ] {
        let next = fullscreen_height_test_canvas(next_rows);
        let (writer, bytes) = super::new_test_writer();
        let mut terminal = super::new_fullscreen_term(writer, 0, 6);
        terminal.size = Some((10, 4));
        terminal.prev_size_on_write = terminal.size;
        let mut vt = fullscreen_height_test_vt();
        assert!(terminal.write_canvas_would_write(Some(&previous), &next));
        terminal
            .write_canvas(Some(&previous), &next, OutputContext::default())
            .unwrap();
        vt.feed_str(&String::from_utf8(std::mem::take(&mut *bytes.lock().unwrap())).unwrap());
        for y in 0..4 {
            assert_eq!(
                vt.line(y).text(),
                format!("{:10}", next_rows.get(y).unwrap_or(&""))
            );
        }
        assert!(!terminal.write_canvas_would_write(Some(&next), &next));
        terminal
            .write_canvas(Some(&next), &next, OutputContext::default())
            .unwrap();
        assert!(bytes.lock().unwrap().is_empty());
    }
}

#[test]
fn test_fullscreen_initial_resize_and_forced_paints_clip_tall_canvas() {
    let next =
        fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "╰────────╯", "tail4", "tail5"]);
    for mode in 0..3 {
        let (writer, bytes) = super::new_test_writer();
        let mut terminal = super::new_fullscreen_term(writer, 0, 6);
        terminal.size = Some((10, 4));
        terminal.prev_size_on_write = Some((10, if mode == 1 { 6 } else { 4 }));
        let mut forced = next.clone();
        forced.force_full_repaint();
        let (previous, canvas) = match mode {
            0 => (None, &next),
            1 => (Some(&next), &next),
            _ => (Some(&next), &forced),
        };
        assert!(terminal.write_canvas_would_write(previous, canvas));
        terminal
            .write_canvas(previous, canvas, OutputContext::default())
            .unwrap();
        let ansi = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(!ansi.contains("tail"), "{ansi:?}");
        let mut vt = avt::Vt::new(10, 4);
        vt.feed_str(&ansi);
        assert_eq!(vt.line(0).text(), "TITLE     ");
        assert_eq!(vt.line(3).text(), "╰────────╯");
    }
}

#[test]
fn test_fullscreen_hidden_rows_are_noop_for_writer_and_prediction() {
    let previous = fullscreen_height_test_canvas(&["same", "same", "same", "same", "old"]);
    let mut next = previous.clone();
    next.subview_mut(0, 0, 0, 0, 10, 5)
        .set_text(0, 4, "new", CanvasTextStyle::default());
    next.clear_damage();
    for (rows, top_row, force, initial) in [
        (4, 0, false, false),
        (4, 1, false, false),
        (4, 4, true, false),
        (4, u16::MAX, true, false),
        (0, 0, true, false),
        (0, 0, true, true),
    ] {
        let (writer, bytes) = super::new_test_writer();
        let mut terminal = super::new_fullscreen_term(writer, top_row, 5);
        terminal.size = Some((10, rows));
        terminal.prev_size_on_write = terminal.size;
        let mut canvas = next.clone();
        if force {
            canvas.force_full_repaint();
        }
        let previous = (!initial).then_some(&previous);
        assert!(!terminal.write_canvas_would_write(previous, &canvas));
        terminal
            .write_canvas(previous, &canvas, OutputContext::default())
            .unwrap();
        assert!(bytes.lock().unwrap().is_empty());
    }
}

fn fullscreen_height_test_diff(
    previous: Option<&Canvas>,
    next: &Canvas,
    options: TerminalFullscreenCanvasDiffOptions,
    packed: bool,
) -> Vec<TerminalPatch> {
    if packed {
        let mut pools = CanvasPackedCellPools::new();
        let previous = previous.map(|canvas| canvas.pack_with(&mut pools));
        let next = next.pack_with(&mut pools);
        terminal_fullscreen_packed_canvas_diff_patches(
            previous.as_ref(),
            &next,
            &pools,
            &mut CanvasStyleTransitionCache::new(),
            options,
        )
    } else {
        terminal_fullscreen_canvas_diff_patches(previous, next, options)
    }
}

#[test]
fn test_fullscreen_canvas_and_packed_helpers_clip_writes_and_clears_from_origin() {
    let previous =
        fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "old bottom", "tail4", "tail5"]);
    let next = fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "╰────────╯"]);
    for packed in [false, true] {
        for top_row in [0, 1] {
            for force_full_repaint in [false, true] {
                let options = TerminalFullscreenCanvasDiffOptions {
                    top_row,
                    terminal_rows: Some(4),
                    force_full_repaint,
                };
                let patches = fullscreen_height_test_diff(Some(&previous), &next, options, packed);
                let ansi = terminal_patches_to_ansi(&patches, true);
                let mut vt = fullscreen_height_test_vt();
                vt.feed_str(&ansi);
                if top_row == 0 {
                    assert_eq!(vt.line(3).text(), "╰────────╯");
                } else {
                    // The changed border is below the visible origin-relative extent.
                    assert!(!ansi.contains("╰"), "{ansi:?}");
                    if !force_full_repaint {
                        assert!(patches.is_empty());
                    }
                }
            }
        }
        for (terminal_rows, top_row) in [(0, 0), (4, 4), (4, u16::MAX)] {
            let options = TerminalFullscreenCanvasDiffOptions {
                top_row,
                terminal_rows: Some(terminal_rows),
                force_full_repaint: true,
            };
            assert!(
                fullscreen_height_test_diff(Some(&previous), &next, options, packed).is_empty()
            );
            assert!(fullscreen_height_test_diff(None, &next, options, packed).is_empty());
        }
        let options = TerminalFullscreenCanvasDiffOptions::default();
        let ansi = terminal_patches_to_ansi(
            &fullscreen_height_test_diff(Some(&previous), &next, options, packed),
            true,
        );
        assert!(
            ansi.contains("\x1b[6;1H"),
            "unknown height keeps the unconstrained convention: {ansi:?}"
        );
    }
}

#[test]
fn test_fullscreen_frame_row_count_overrides_or_falls_back_to_diff_options() {
    let previous =
        fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "old bottom", "tail4", "tail5"]);
    let next = fullscreen_height_test_canvas(&["TITLE", "body", "FOOTER", "╰────────╯"]);
    for packed in [false, true] {
        for (frame_rows, diff_rows, expected_rows) in [
            (Some(4), Some(6), 4),
            (None, Some(4), 4),
            (Some(0), Some(4), 0),
        ] {
            let options = TerminalFullscreenCanvasFramePatchOptions {
                terminal_rows: frame_rows,
                canvas_diff: TerminalFullscreenCanvasDiffOptions {
                    terminal_rows: diff_rows,
                    ..Default::default()
                },
                ..Default::default()
            };
            let plan = if packed {
                let mut pools = CanvasPackedCellPools::new();
                let previous = previous.pack_with(&mut pools);
                let next = next.pack_with(&mut pools);
                plan_terminal_fullscreen_packed_canvas_frame_patches(
                    Some(&previous),
                    &next,
                    &pools,
                    &mut CanvasStyleTransitionCache::new(),
                    options,
                )
            } else {
                plan_terminal_fullscreen_canvas_frame_patches(Some(&previous), &next, options)
            };
            if expected_rows == 0 {
                assert!(plan.is_empty());
            } else {
                let ansi = terminal_patches_to_ansi(&plan.patches, true);
                let mut vt = fullscreen_height_test_vt();
                vt.feed_str(&ansi);
                assert_eq!(vt.line(3).text(), "╰────────╯");
                assert_eq!(
                    plan.cursor_plan.post_diff_patch,
                    Some(TerminalPatch::Stdout("\x1b[4;1H".into()))
                );
            }
        }
    }
}
