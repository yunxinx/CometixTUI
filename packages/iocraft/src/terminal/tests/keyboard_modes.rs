use super::*;

#[test]
fn negotiated_keyboard_modes_follow_terminal_and_transport_capabilities() {
    let requested = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
    for (environment, version, format, supported, expected, modify_other_keys) in [
        (vec![], None, None, true, Some(3), false),
        (
            vec![("TERM_PROGRAM", "unknown")],
            None,
            None,
            true,
            Some(3),
            false,
        ),
        (
            vec![("TERM", "xterm-kitty")],
            None,
            None,
            false,
            None,
            false,
        ),
        (
            vec![("TERM", "xterm-ghostty")],
            None,
            None,
            true,
            Some(1),
            false,
        ),
        (
            vec![("TERM_PROGRAM", "iTerm.app")],
            None,
            None,
            true,
            Some(1),
            false,
        ),
        (vec![], Some("libghostty 1.4.0"), None, true, Some(1), false),
        (
            vec![("TMUX", "/tmp/socket")],
            None,
            Some("csi-u"),
            true,
            Some(3),
            true,
        ),
        (
            vec![("TMUX", "/tmp/socket"), ("LC_TERMINAL", "iTerm2")],
            None,
            Some("csi-u"),
            true,
            Some(1),
            true,
        ),
        (
            vec![("TMUX_PANE", "%1")],
            None,
            Some("xterm"),
            true,
            Some(1),
            false,
        ),
        (
            vec![("TMUX", "/tmp/socket")],
            None,
            None,
            true,
            Some(1),
            false,
        ),
        (
            vec![("TMUX", "/tmp/socket")],
            None,
            Some("unknown"),
            true,
            Some(1),
            false,
        ),
        (
            vec![("TMUX", "/tmp/socket")],
            None,
            Some("csi-u"),
            false,
            None,
            true,
        ),
    ] {
        let policy = KeyboardPolicy::from_environment(
            |key| {
                environment
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.to_string())
            },
            version,
            format,
        );
        let (output, bytes) = new_test_writer();
        let mut terminal = new_inline_term(output, 0);
        terminal.keyboard = KeyboardModes::new(supported, policy);
        terminal.set_keyboard_enhancement_flags(requested).unwrap();
        terminal
            .apply_raw_mode_enabled_with(true, |_| Ok(()))
            .unwrap();
        let enabled = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        if let Some(flags) = expected {
            assert!(enabled.contains(&format!("\x1b[>{flags}u")), "{enabled:?}");
        } else {
            assert!(!enabled.contains("u"), "{enabled:?}");
        }
        assert_eq!(enabled.contains("\x1b[>4;2m"), modify_other_keys);
        bytes.lock().unwrap().clear();
        terminal
            .apply_raw_mode_enabled_with(false, |_| Ok(()))
            .unwrap();
        let disabled = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(disabled.contains("\x1b[<1u"), expected.is_some());
        assert_eq!(disabled.contains("\x1b[>4m"), modify_other_keys);
    }
}

#[test]
fn keyboard_modes_transfer_between_screens_and_survive_handoff_without_extra_pops() {
    let (output, bytes) = new_test_writer();
    let mut terminal = new_inline_term(output, 0);
    terminal.input_is_terminal = true;
    terminal.keyboard.supported = true;
    terminal
        .set_keyboard_enhancement_flags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        .unwrap();
    terminal
        .apply_raw_mode_enabled_with(true, |_| Ok(()))
        .unwrap();
    bytes.lock().unwrap().clear();

    let enter = terminal.plan_screen_transition(Some(crate::context::AlternateScreenRequest {
        mouse_tracking: false,
    }));
    terminal.apply_screen_transition(enter).unwrap();
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(output.find("\x1b[<1u").unwrap() < output.find("\x1b[?1049h").unwrap());
    assert!(output.find("\x1b[?1049h").unwrap() < output.find("\x1b[>1u").unwrap());

    for fullscreen in [true, false] {
        assert_eq!(terminal.fullscreen, fullscreen);
        bytes.lock().unwrap().clear();
        terminal.release_terminal_modes_with(|| Ok(())).unwrap();
        assert!(!terminal.keyboard.is_active());
        let released = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert_eq!(released.matches("\x1b[<1u").count(), 1);
        bytes.lock().unwrap().clear();
        terminal.reinitialize_after_resume_with(|| Ok(())).unwrap();
        let resumed = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(!resumed.contains("\x1b[<1u"), "{resumed:?}");
        assert_eq!(resumed.matches("\x1b[>1u").count(), 1);
        assert!(terminal.keyboard.is_active());
        bytes.lock().unwrap().clear();
        if fullscreen {
            let leave = terminal.plan_screen_transition(None);
            terminal.apply_screen_transition(leave).unwrap();
            let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
            assert!(output.find("\x1b[<1u").unwrap() < output.find("\x1b[?1049l").unwrap());
            assert!(output.find("\x1b[?1049l").unwrap() < output.find("\x1b[>1u").unwrap());
        }
    }
    terminal
        .apply_raw_mode_enabled_with(false, |_| Ok(()))
        .unwrap();
    assert_eq!(
        String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .matches("\x1b[<1u")
            .count(),
        1
    );
}

#[test]
fn panic_releases_the_owned_keyboard_entry_once() {
    const CHILD: &str = "IOCRAFT_TEST_KEYBOARD_PANIC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "terminal::tests::keyboard_modes::panic_releases_the_owned_keyboard_entry_once",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .matches("\x1b[<1u")
                .count(),
            1
        );
        return;
    }
    let (output, bytes) = new_test_writer();
    let mut terminal = new_inline_term(output, 0);
    register_terminal_for_panic_restore(false);
    terminal.keyboard.supported = true;
    terminal
        .apply_raw_mode_enabled_with(true, |_| Ok(()))
        .unwrap();
    bytes.lock().unwrap().clear();
    assert!(std::panic::catch_unwind(|| panic!("keyboard cleanup")).is_err());
    assert!(!terminal.keyboard.is_active());
    terminal
        .apply_raw_mode_enabled_with(false, |_| Ok(()))
        .unwrap();
    assert!(
        !String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .contains("\x1b[<1u")
    );
}
