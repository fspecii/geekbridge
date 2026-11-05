// SPDX-License-Identifier: GPL-3.0-or-later
//! Is the desktop in Do Not Disturb? Asked only when the person switched on "Respect Do Not Disturb" and
//! a call rings. Nothing is stored or sent; the answer is one yes/no, cached for a few seconds.
//!
//! Linux, in this order (the first tool that answers decides, so the daemon actually running wins):
//! `makoctl mode` (a mode named `do-not-disturb` or `dnd`, the Omarchy and sway convention),
//! `dunstctl is-paused`, `swaync-client --get-dnd`, then the freedesktop `Inhibited` property of the
//! notification service (KDE Plasma) and the GNOME `show-banners` setting.
//!
//! Windows (Focus Assist) and macOS (Focus) have no documented, cheap, permission-free way to read the
//! state, so there this answers "no" and the setting says it is not supported on this system.
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Whether this build can tell at all.
pub const SUPPORTED: bool = cfg!(target_os = "linux");

const CACHE: Duration = Duration::from_secs(3);

/// `makoctl mode` prints the active modes one per line.
pub fn mako_modes_are_dnd(out: &str) -> bool {
    out.lines()
        .map(|l| l.trim().to_ascii_lowercase())
        .any(|m| m == "do-not-disturb" || m == "dnd")
}

/// `true`/`false` on its own (dunstctl, swaync-client) or `b true` (busctl).
pub fn word_is_true(out: &str) -> Option<bool> {
    match out.trim().trim_start_matches("b ").trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// GNOME: banners hidden is the Do Not Disturb switch.
pub fn gnome_banners_hidden(out: &str) -> Option<bool> {
    word_is_true(out).map(|shown| !shown)
}

/// One tool's answer: `Some(on)` when it ran and said something we understand.
type Probe = (
    &'static str,
    &'static [&'static str],
    fn(&str) -> Option<bool>,
);

const PROBES: [Probe; 5] = [
    ("makoctl", &["mode"], |o| Some(mako_modes_are_dnd(o))),
    ("dunstctl", &["is-paused"], word_is_true),
    ("swaync-client", &["--get-dnd"], word_is_true),
    (
        "busctl",
        &[
            "--user",
            "get-property",
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "Inhibited",
        ],
        word_is_true,
    ),
    (
        "gsettings",
        &["get", "org.gnome.desktop.notifications", "show-banners"],
        gnome_banners_hidden,
    ),
];

/// Run `tool`, wait at most half a second, return stdout of a successful run.
fn run(tool: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let end = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => break,
            Some(_) => return None,
            None if Instant::now() >= end => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(4096)
        .read_to_string(&mut out)
        .ok()?;
    Some(out)
}

fn probe_now() -> bool {
    if !SUPPORTED {
        return false;
    }
    PROBES
        .iter()
        .find_map(|(tool, args, parse)| run(tool, args).and_then(|o| parse(&o)))
        .unwrap_or(false)
}

/// Cached answer for the call hub.
pub struct Detector {
    last: Mutex<Option<(Instant, bool)>>,
    probe: fn() -> bool,
}

impl Default for Detector {
    fn default() -> Self {
        Self::with_probe(probe_now)
    }
}

impl Detector {
    pub fn with_probe(probe: fn() -> bool) -> Self {
        Self {
            last: Mutex::new(None),
            probe,
        }
    }

    pub fn active(&self) -> bool {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, on)) = *last
            && at.elapsed() < CACHE
        {
            return on;
        }
        let on = (self.probe)();
        *last = Some((Instant::now(), on));
        on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mako_modes_are_read_line_by_line() {
        assert!(mako_modes_are_dnd("default\ndo-not-disturb\n"));
        assert!(mako_modes_are_dnd("DND"));
        assert!(!mako_modes_are_dnd("default\n"));
        assert!(!mako_modes_are_dnd("not-do-not-disturb-really"));
        assert!(!mako_modes_are_dnd(""));
    }

    #[test]
    fn booleans_from_dunst_swaync_busctl_and_gnome() {
        assert_eq!(word_is_true("true\n"), Some(true));
        assert_eq!(word_is_true("b false\n"), Some(false));
        assert_eq!(word_is_true("b true"), Some(true));
        assert_eq!(word_is_true("maybe"), None);
        assert_eq!(
            gnome_banners_hidden("false\n"),
            Some(true),
            "banners hidden"
        );
        assert_eq!(gnome_banners_hidden("true\n"), Some(false));
    }

    #[test]
    fn the_answer_is_cached_for_a_few_seconds() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CALLS: AtomicU32 = AtomicU32::new(0);
        let d = Detector::with_probe(|| {
            CALLS.fetch_add(1, Ordering::SeqCst);
            true
        });
        assert!(d.active() && d.active() && d.active());
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_missing_tool_is_not_dnd() {
        assert!(run("geekbridge-no-such-tool-xyz", &[]).is_none());
    }
}
