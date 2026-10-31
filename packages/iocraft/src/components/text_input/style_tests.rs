use super::*;
use crate::prelude::*;

#[test]
fn bidi_regrouping_preserves_input_styles() {
    const CHILD: &str = "COMETIXTUI_TEST_SOFTWARE_BIDI";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "components::text_input::style_tests::bidi_regrouping_preserves_input_styles",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("TERM_PROGRAM", "vscode")
            .output()
            .expect("software bidi test process");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    assert!(crate::bidi::needs_software_bidi());
    let value = "א\u{202e}\u{1f1e6}\u{1f1e7}\u{1f1e8}\u{202c}";
    let render = |styled| {
        element! {
        View(width: 30, height: 1) {
            TextInput(value, style_ranges: if styled {
                vec![TextInputStyleRange {
                    range: 0..value.len(),
                    style: StyleOverlay { color: Some(Some(Color::Cyan)), ..Default::default() },
                }]
            } else { Vec::new() })
        }
    }.render(None)
    };
    let plain = render(false);
    let styled = render(true);
    assert_eq!(styled.to_string(), plain.to_string());
    assert_eq!(
        styled
            .resolved_text_style(0, 0)
            .expect("first grapheme")
            .color,
        Some(Color::Cyan)
    );
    let mixed = element! {
        View(width: 30, height: 1) {
            TextInput(value, style_ranges: vec![
                TextInputStyleRange { range: 5..13, style: StyleOverlay { color: Some(Some(Color::Cyan)), ..Default::default() } },
                TextInputStyleRange { range: 13..17, style: StyleOverlay { color: Some(Some(Color::Magenta)), ..Default::default() } },
            ])
        }
    }.render(None);
    for (text, color) in [
        ("\u{1f1e8}\u{1f1e6}", Color::Magenta),
        ("\u{1f1e7}", Color::Cyan),
    ] {
        let column = (0..30)
            .find(|&column| {
                mixed
                    .cell(column, 0)
                    .is_some_and(|cell| cell.text() == Some(text))
            })
            .expect("regrouped regional indicators");
        assert_eq!(
            mixed
                .resolved_text_style(column, 0)
                .expect("regrouped style")
                .color,
            Some(color)
        );
    }
    let repeated = element! {
        View(width: 10, height: 1) {
            TextInput(value: "אא", style_ranges: vec![TextInputStyleRange {
                range: 0..2, style: StyleOverlay { color: Some(Some(Color::Cyan)), ..Default::default() },
            }])
        }
    }.render(None);
    assert_eq!(
        repeated
            .resolved_text_style(0, 0)
            .expect("second source letter")
            .color,
        None
    );
    assert_eq!(
        repeated
            .resolved_text_style(1, 0)
            .expect("first source letter")
            .color,
        Some(Color::Cyan)
    );
}

#[test]
fn input_styles_are_covered_by_later_components() {
    let canvas = element! {
        View(width: 3, height: 1) {
            TextInput(value: "abc", style_ranges: vec![TextInputStyleRange {
                range: 0..3,
                style: StyleOverlay { color: Some(Some(Color::Cyan)), hidden: Some(true), underline: Some(true), ..Default::default() },
            }])
            View(position: Position::Absolute, top: 0, left: 0, width: 3, height: 1, background_color: Color::Red) {
                Text(content: "XYZ", color: Color::Green)
            }
        }
    }.render(None);
    assert_eq!(canvas.to_string(), "XYZ\n");
    assert_eq!(
        canvas
            .resolved_text_style(0, 0)
            .expect("covering text")
            .color,
        Some(Color::Green)
    );
    let style = canvas.resolved_text_style(0, 0).expect("covering style");
    assert!(!style.hidden);
    assert!(!style.underline);
}
