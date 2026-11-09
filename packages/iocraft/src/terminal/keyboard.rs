use super::*;
use std::sync::atomic::{AtomicU8, Ordering};

const KITTY_ACTIVE: u8 = 1;
const MODIFY_OTHER_KEYS_ACTIVE: u8 = 2;

struct ActiveKeyboardModes {
    modes: Weak<AtomicU8>,
    fullscreen: bool,
}

static ACTIVE_MODES: Mutex<Vec<ActiveKeyboardModes>> = Mutex::new(Vec::new());

#[derive(Clone, Copy)]
pub(super) struct KeyboardPolicy {
    report_event_types: bool,
    modify_other_keys: bool,
}

impl KeyboardPolicy {
    pub(super) fn detect() -> Self {
        let tmux = env_present("TMUX") || env_present("TMUX_PANE");
        let format = tmux.then(tmux_extended_keys_format).flatten();
        Self::from_environment(
            |key| std::env::var(key).ok(),
            xtversion_name().as_deref(),
            format.as_deref(),
        )
    }

    pub(super) fn from_environment(
        mut lookup: impl FnMut(&str) -> Option<String>,
        version: Option<&str>,
        tmux_format: Option<&str>,
    ) -> Self {
        let tmux = lookup("TMUX").is_some() || lookup("TMUX_PANE").is_some();
        let screen = lookup("STY").is_some();
        let shortcut_releases = ["TERM", "TERM_PROGRAM", "LC_TERMINAL"]
            .into_iter()
            .filter_map(&mut lookup)
            .chain(version.map(str::to_owned))
            .any(|name| {
                let name = name.to_ascii_lowercase();
                name.contains("ghostty") || name.contains("iterm")
            });
        // Unknown tmux formats use the xterm fallback: event types can erase Shift+Enter.
        Self {
            report_event_types: !shortcut_releases
                && !screen
                && (!tmux || tmux_format == Some("csi-u")),
            modify_other_keys: tmux && tmux_format == Some("csi-u"),
        }
    }

    fn flags(self, requested: KeyboardEnhancementFlags) -> KeyboardEnhancementFlags {
        if self.report_event_types {
            requested
        } else {
            requested - KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        }
    }
}

fn env_present(key: &str) -> bool {
    std::env::var_os(key).is_some()
}

fn tmux_extended_keys_format() -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("tmux")
        .args(["show-options", "-s", "-v", "extended-keys-format"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let output = child.wait_with_output().ok()?;
                return String::from_utf8(output.stdout)
                    .ok()
                    .map(|value| value.trim().to_owned());
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub(super) struct KeyboardModes {
    pub(super) supported: bool,
    pub(super) flags: KeyboardEnhancementFlags,
    policy: KeyboardPolicy,
    active: Arc<AtomicU8>,
}

impl KeyboardModes {
    pub(super) fn new(supported: bool, policy: KeyboardPolicy) -> Self {
        Self {
            supported,
            flags: policy.flags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES),
            policy,
            active: Arc::new(AtomicU8::new(0)),
        }
    }

    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst) != 0
    }

    pub(super) fn enable(
        &mut self,
        output: &mut TerminalWriter<'_>,
        fullscreen: bool,
    ) -> io::Result<()> {
        if self.is_active() || (!self.supported && !self.policy.modify_other_keys) {
            return Ok(());
        }
        let mut active = ACTIVE_MODES
            .lock()
            .expect("keyboard mode registry is not poisoned");
        active.retain(|entry| {
            entry
                .modes
                .upgrade()
                .is_some_and(|modes| modes.load(Ordering::SeqCst) != 0)
        });
        active.push(ActiveKeyboardModes {
            modes: Arc::downgrade(&self.active),
            fullscreen,
        });
        drop(active);
        if self.supported {
            // A command may reach the terminal before its flush reports failure.
            self.active.fetch_or(KITTY_ACTIVE, Ordering::SeqCst);
            output.execute(event::PushKeyboardEnhancementFlags(self.flags))?;
        }
        if self.policy.modify_other_keys {
            self.active
                .fetch_or(MODIFY_OTHER_KEYS_ACTIVE, Ordering::SeqCst);
            output.write_all(TERMINAL_MODIFY_OTHER_KEYS_ENABLE.as_bytes())?;
            output.flush()?;
        }
        Ok(())
    }

    pub(super) fn disable(&mut self, output: &mut TerminalWriter<'_>) -> io::Result<()> {
        release_modes(&self.active, &mut **output)
    }

    pub(super) fn reassert(
        &mut self,
        output: &mut TerminalWriter<'_>,
        fullscreen: bool,
    ) -> io::Result<()> {
        self.disable(output)?;
        self.enable(output, fullscreen)
    }

    pub(super) fn set_flags(
        &mut self,
        requested: KeyboardEnhancementFlags,
        output: &mut TerminalWriter<'_>,
        fullscreen: bool,
    ) -> io::Result<()> {
        let flags = self.policy.flags(requested);
        if self.flags != flags {
            self.flags = flags;
            if self.is_active() {
                self.reassert(output, fullscreen)?;
            }
        }
        Ok(())
    }
}

fn release_modes(active: &AtomicU8, output: &mut (impl Write + ?Sized)) -> io::Result<()> {
    let mut result = Ok(());
    for (mode, sequence) in [
        (KITTY_ACTIVE, "\x1b[<1u"),
        (MODIFY_OTHER_KEYS_ACTIVE, TERMINAL_MODIFY_OTHER_KEYS_DISABLE),
    ] {
        if active.load(Ordering::SeqCst) & mode != 0 {
            let released = output
                .write_all(sequence.as_bytes())
                .and_then(|()| output.flush());
            if released.is_ok() {
                active.fetch_and(!mode, Ordering::SeqCst);
            }
            result = result.and(released);
        }
    }
    result
}

pub(super) fn restore_keyboard_for_panic(output: &mut impl Write, fullscreen: bool) {
    let modes = ACTIVE_MODES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for entry in modes
        .iter()
        .rev()
        .filter(|entry| entry.fullscreen == fullscreen)
    {
        if let Some(active) = entry.modes.upgrade() {
            let _ = release_modes(&active, output);
        }
    }
}
