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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
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

/// The whole chain gives up after this, whatever each tool does.
const CHAIN_DEADLINE: Duration = Duration::from_millis(600);
/// One tool gets at most this long.
const TOOL_LIMIT: Duration = Duration::from_millis(500);

/// Run `tool`, wait at most `limit`, return stdout of a successful run.
fn run(tool: &str, args: &[&str], limit: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let end = Instant::now() + limit;
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

/// The first tool that answers decides; the whole chain stops at `deadline`.
fn probe_chain(probes: &[Probe], deadline: Instant) -> bool {
    for (tool, args, parse) in probes {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        if let Some(on) = run(tool, args, left.min(TOOL_LIMIT)).and_then(|o| parse(&o)) {
            return on;
        }
    }
    false
}

fn probe_now() -> bool {
    SUPPORTED && probe_chain(&PROBES, Instant::now() + CHAIN_DEADLINE)
}

#[derive(Default)]
struct Shared {
    last: Mutex<Option<(Instant, bool)>>,
    refreshing: AtomicBool,
}

/// Cached answer for the call hub. Asking never runs a tool on the caller's thread and never waits
/// for one: a stale answer is returned at once and a short-lived thread refreshes it.
pub struct Detector {
    shared: Arc<Shared>,
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
            shared: Arc::default(),
            probe,
        }
    }

    /// The last known answer (not in Do Not Disturb until one has been learned). Starts a refresh in
    /// the background when it is older than a few seconds.
    pub fn active(&self) -> bool {
        let known = *self.shared.last.lock().unwrap_or_else(|e| e.into_inner());
        match known {
            Some((at, on)) if at.elapsed() < CACHE => on,
            _ => {
                self.refresh_in_background();
                known.is_some_and(|(_, on)| on)
            }
        }
    }

    /// Learn the current state off the caller's thread. One refresh at a time; no lock is held
    /// while a tool runs.
    pub fn refresh_in_background(&self) {
        if self.shared.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let (shared, probe) = (self.shared.clone(), self.probe);
        let spawned = std::thread::Builder::new()
            .name("gb-dnd-probe".into())
            .spawn(move || {
                let on = probe();
                *shared.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), on));
                shared.refreshing.store(false, Ordering::Release);
            });
        if spawned.is_err() {
            self.shared.refreshing.store(false, Ordering::Release);
        }
    }
}

/// Poked when the "Respect Do Not Disturb" choice is saved, so the keep-warm thread need not poll the file.
static CHOICE_SAVED: (Mutex<u64>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());

/// Tell the keep-warm thread that the choice changed.
pub fn choice_saved() {
    *CHOICE_SAVED.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    CHOICE_SAVED.1.notify_all();
}

/// Keeps `detector` fresh every `every`, but only while `enabled()` says the setting is on. While it is
/// off the thread sleeps for `30 * every` (it is woken at once by [`choice_saved`]); it ends by itself
/// once the detector is dropped.
pub fn keep_warm(
    detector: Weak<Detector>,
    enabled: impl Fn() -> bool + Send + 'static,
    every: Duration,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("gb-dnd-warm".into())
        .spawn(move || {
            let mut seen = *CHOICE_SAVED.0.lock().unwrap_or_else(|e| e.into_inner());
            while let Some(d) = detector.upgrade() {
                let on = enabled();
                if on {
                    d.refresh_in_background();
                }
                drop(d);
                let wait = if on { every } else { every * 30 };
                let g = CHOICE_SAVED.0.lock().unwrap_or_else(|e| e.into_inner());
                // Saved since the last look: no wait. The guard must not be held while the count is read again
                // (locking twice on one thread is a deadlock that also blocks every later save).
                let g = if *g == seen {
                    CHOICE_SAVED
                        .1
                        .wait_timeout(g, wait)
                        .map_or_else(|e| e.into_inner().0, |(g, _)| g)
                } else {
                    g
                };
                // Saved meanwhile or woken: look again (a stray wake-up costs one look).
                seen = *g;
            }
        })
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
        let _ = d.active(); // learns in the background
        wait_until(|| d.shared.last.lock().unwrap().is_some());
        assert!(d.active() && d.active() && d.active());
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_reader_and_the_setting_agree_on_which_systems_are_supported() {
        use bridge_core::privacy::PrivacyKey;
        assert_eq!(SUPPORTED, PrivacyKey::RespectDnd.supported());
    }

    #[test]
    fn a_missing_tool_is_not_dnd() {
        assert!(run("geekbridge-no-such-tool-xyz", &[], TOOL_LIMIT).is_none());
    }

    fn wait_until(mut cond: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !cond() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(cond(), "timed out");
    }

    #[test]
    fn a_slow_probe_never_blocks_the_caller() {
        let d = Detector::with_probe(|| {
            std::thread::sleep(Duration::from_millis(400));
            true
        });
        let t = Instant::now();
        assert!(!d.active(), "nothing learned yet: not Do Not Disturb");
        assert!(!d.active(), "still probing");
        assert!(
            t.elapsed() < Duration::from_millis(200),
            "{:?}",
            t.elapsed()
        );
        wait_until(|| d.shared.last.lock().unwrap().is_some());
        assert!(d.active());
    }

    #[test]
    fn a_stale_answer_is_returned_at_once_while_it_refreshes() {
        let d = Detector::with_probe(|| {
            std::thread::sleep(Duration::from_millis(300));
            false
        });
        *d.shared.last.lock().unwrap() = Some((Instant::now() - CACHE * 2, true));
        let t = Instant::now();
        assert!(d.active(), "the stale yes");
        assert!(t.elapsed() < Duration::from_millis(150));
        // The lock is free while the probe runs.
        assert!(d.shared.last.try_lock().is_ok());
        wait_until(|| !d.active());
    }

    #[test]
    fn only_one_refresh_runs_at_a_time() {
        use std::sync::atomic::AtomicU32;
        static STARTS: AtomicU32 = AtomicU32::new(0);
        let d = Detector::with_probe(|| {
            STARTS.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            false
        });
        for _ in 0..20 {
            d.active();
        }
        wait_until(|| d.shared.last.lock().unwrap().is_some());
        assert_eq!(STARTS.load(Ordering::SeqCst), 1);
    }

    #[cfg(unix)]
    #[test]
    fn the_chain_stops_at_its_deadline() {
        fn never(_: &str) -> Option<bool> {
            None
        }
        let probes: [Probe; 5] = [("sleep", &["5"], never); 5];
        let t = Instant::now();
        assert!(!probe_chain(
            &probes,
            Instant::now() + Duration::from_millis(250)
        ));
        assert!(
            t.elapsed() < Duration::from_millis(600),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn keep_warm_probes_only_while_the_option_is_on_and_ends_with_the_detector() {
        use std::sync::atomic::AtomicU32;
        static PROBES: AtomicU32 = AtomicU32::new(0);
        let d = Arc::new(Detector::with_probe(|| {
            PROBES.fetch_add(1, Ordering::SeqCst);
            false
        }));
        let on = Arc::new(AtomicBool::new(false));
        let flag = on.clone();
        let thread = keep_warm(
            Arc::downgrade(&d),
            move || flag.load(Ordering::SeqCst),
            Duration::from_millis(5),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(PROBES.load(Ordering::SeqCst), 0, "option off: no tool runs");
        on.store(true, Ordering::SeqCst);
        wait_until(|| PROBES.load(Ordering::SeqCst) >= 1);
        on.store(false, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100)); // let an in-flight refresh finish
        let settled = PROBES.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            PROBES.load(Ordering::SeqCst),
            settled,
            "off again: it stops"
        );
        drop(d);
        thread.join().unwrap();
    }

    #[test]
    fn keep_warm_sleeps_long_while_off_and_wakes_when_the_choice_is_saved() {
        use std::sync::atomic::AtomicU32;
        static PROBES: AtomicU32 = AtomicU32::new(0);
        let d = Arc::new(Detector::with_probe(|| {
            PROBES.fetch_add(1, Ordering::SeqCst);
            false
        }));
        let on = Arc::new(AtomicBool::new(false));
        let flag = on.clone();
        // 10 s while on, so 300 s while off: only the save can wake it in time.
        let thread = keep_warm(
            Arc::downgrade(&d),
            move || flag.load(Ordering::SeqCst),
            Duration::from_secs(10),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        on.store(true, Ordering::SeqCst);
        choice_saved();
        wait_until(|| PROBES.load(Ordering::SeqCst) >= 1);
        drop(d);
        choice_saved();
        thread.join().unwrap(); // the detector is gone: it ends at the wake, not 10 s later
    }

    #[test]
    fn a_save_during_the_look_does_not_deadlock_the_keep_warm_thread() {
        // The choice is saved while the thread is between reading the counter and waiting (here: inside `enabled`,
        // which runs exactly there). It used to lock the counter twice and hang itself and every later save.
        let d = Arc::new(Detector::with_probe(|| false));
        let first = Arc::new(AtomicBool::new(true));
        let f = first.clone();
        let thread = keep_warm(
            Arc::downgrade(&d),
            move || {
                if f.swap(false, Ordering::SeqCst) {
                    choice_saved();
                }
                false
            },
            Duration::from_secs(10),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(d);
            choice_saved(); // blocks for ever when the thread holds the counter
            thread.join().unwrap();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the keep-warm thread ends at the wake and later saves go through");
    }
}
