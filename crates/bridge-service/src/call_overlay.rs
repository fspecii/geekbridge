// SPDX-License-Identifier: GPL-3.0-or-later
//! Starts the incoming-call card (`geekbridge-calls --overlay`) when a call rings, and makes sure
//! it goes away.
//!
//! The card is a separate small process: it polls the service like the Calls window does, shows
//! Answer / Reject in the top-right corner of the screen and exits when the call is over, so the
//! service holds no window code. [`Supervisor`] (driven by the call hub on every change and every
//! tick) keeps the process side honest:
//!
//! * one card at a time; a second ring while a card is up reuses it (the card follows whichever
//!   call rings);
//! * a ring that finds no card (or the previous card closing just as it came) gets one, once: a
//!   card that dies while the same ring goes on is not restarted in a loop;
//! * an exited card is reaped at once (no `<defunct>` process until the next ring);
//! * a card still running [`LINGER`] after the last ring ended is killed (a hung window, a
//!   compositor that never delivered the close).
use bridge_core::call_overlay::Mode;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// What the child needs to find this service again: the service may run on other ports than the
/// defaults (tests, a second instance).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub state_dir: PathBuf,
    pub api_port: u16,
    pub control_port: u16,
}

pub const SETTINGS_FILE: &str = "call-overlay";

/// The mode from `GEEKBRIDGE_CALL_OVERLAY` or `<state dir>/call-overlay`.
pub fn mode(state_dir: &Path) -> Mode {
    let env = std::env::var("GEEKBRIDGE_CALL_OVERLAY").ok();
    let file = std::fs::read_to_string(state_dir.join(SETTINGS_FILE)).ok();
    let mode = Mode::resolve(env.as_deref(), file.as_deref());
    // The Settings choice only narrows what the card shows, and only when no older switch decided.
    let said = |t: &Option<String>| {
        t.as_deref()
            .map(str::trim)
            .is_some_and(|w| !w.is_empty() && Mode::parse(w).is_some())
    };
    if mode == Mode::On
        && !said(&env)
        && !said(&file)
        && crate::privacy::stored_choice(state_dir, bridge_core::privacy::PrivacyKey::CallerNumber)
            == Some(false)
    {
        return Mode::NameOnly;
    }
    mode
}

/// Command-line arguments of the card process.
pub fn args(launch: &Launch, mode: Mode) -> Vec<String> {
    let mut a = vec![
        "--overlay".to_owned(),
        "--api-port".to_owned(),
        launch.api_port.to_string(),
    ];
    if mode == Mode::NameOnly {
        a.push("--name-only".to_owned());
    }
    a
}

/// File in the state dir that holds the result of the last launch attempt ("ok" or "cannot: <code>"),
/// for `geekbridge-ctl doctor`.
pub const STATUS_FILE: &str = "call-overlay-status";

/// Why the card cannot be shown; the code is what the log and `doctor` print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cannot {
    Disabled,
    NotConfigured,
    HelperMissing,
    NoDisplay,
    SpawnFailed(String),
}
impl Cannot {
    pub fn code(&self) -> String {
        match self {
            Self::Disabled => "disabled".into(),
            Self::NotConfigured => "not-configured".into(),
            Self::HelperMissing => "helper-missing".into(),
            Self::NoDisplay => "no-display".into(),
            Self::SpawnFailed(e) => format!("spawn-failed: {e}"),
        }
    }
    pub fn explain(&self) -> &'static str {
        match self {
            Self::Disabled => {
                "switched off (GEEKBRIDGE_CALL_OVERLAY=off or the call-overlay file in the state folder)"
            }
            Self::NotConfigured => "the service has not told the overlay where it listens",
            Self::HelperMissing => {
                "geekbridge-calls is not installed next to geekbridge-service or on PATH"
            }
            Self::NoDisplay => {
                "no Wayland or X11 display found (no WAYLAND_DISPLAY/DISPLAY, no wayland-* socket in XDG_RUNTIME_DIR); only Linux and the BSDs need one"
            }
            Self::SpawnFailed(_) => "starting geekbridge-calls failed",
        }
    }
}

/// Environment variables the card needs and the service may lack (started from systemd before the
/// session imported its variables, or from a bare terminal).
pub fn display_env(
    get: &dyn Fn(&str) -> Option<String>,
    runtime_dir_entries: &dyn Fn(&Path) -> Vec<String>,
    x11_sockets: &dyn Fn() -> Vec<String>,
    default_runtime_dir: Option<PathBuf>,
) -> Result<Vec<(String, String)>, Cannot> {
    let has = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    if has("WAYLAND_DISPLAY") || has("DISPLAY") {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let runtime = get("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or(default_runtime_dir);
    if let Some(dir) = &runtime {
        let mut socks: Vec<String> = runtime_dir_entries(dir)
            .into_iter()
            .filter(|n| n.starts_with("wayland-") && !n.ends_with(".lock"))
            .collect();
        socks.sort();
        if let Some(first) = socks.first() {
            out.push(("WAYLAND_DISPLAY".to_owned(), first.clone()));
            if !has("XDG_RUNTIME_DIR") {
                out.push(("XDG_RUNTIME_DIR".to_owned(), dir.display().to_string()));
            }
            return Ok(out);
        }
    }
    let mut x: Vec<String> = x11_sockets();
    x.sort();
    match x.first() {
        Some(n) => Ok(vec![(
            "DISPLAY".to_owned(),
            format!(":{}", n.trim_start_matches('X')),
        )]),
        None => Err(Cannot::NoDisplay),
    }
}

/// Linux and the BSDs find their windows through Wayland/X11 variables the service may lack.
/// Windows and macOS have one native window system that every process of the user reaches.
pub fn needs_display_variables(os: &str) -> bool {
    !matches!(os, "windows" | "macos")
}

fn real_display_env() -> Result<Vec<(String, String)>, Cannot> {
    if !needs_display_variables(std::env::consts::OS) {
        return Ok(Vec::new());
    }
    display_env(
        &|k| std::env::var(k).ok(),
        &|dir| {
            std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect()
                })
                .unwrap_or_default()
        },
        &|| {
            std::fs::read_dir("/tmp/.X11-unix")
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .filter(|n| n.starts_with('X'))
                        .collect()
                })
                .unwrap_or_default()
        },
        default_runtime_dir(),
    )
}

#[cfg(unix)]
fn default_runtime_dir() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    Some(PathBuf::from(format!("/run/user/{uid}")))
}
#[cfg(not(unix))]
fn default_runtime_dir() -> Option<PathBuf> {
    None
}

/// Whether `geekbridge-calls` can be found: next to this executable, else on `PATH`.
fn helper_present() -> bool {
    let name = if cfg!(windows) {
        "geekbridge-calls.exe"
    } else {
        "geekbridge-calls"
    };
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .is_some_and(|p| p.is_file());
    beside
        || std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

/// Everything a launch needs, or the reason there is none. Cheap; `doctor` calls it too.
pub fn preflight(state_dir: &Path) -> Result<(Mode, Vec<(String, String)>), Cannot> {
    let mode = mode(state_dir);
    if mode == Mode::Off {
        return Err(Cannot::Disabled);
    }
    if !helper_present() {
        return Err(Cannot::HelperMissing);
    }
    Ok((mode, real_display_env()?))
}

fn record(state_dir: &Path, result: &Result<(), Cannot>) {
    let line = match result {
        Ok(()) => "ok".to_owned(),
        Err(c) => format!("cannot: {}", c.code()),
    };
    // One line in the service log (journal) whenever the outcome changes.
    let path = state_dir.join(STATUS_FILE);
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    if before.trim() != line {
        if result.is_err() {
            eprintln!("incoming-call overlay: {line}");
        }
        let _ = std::fs::write(&path, format!("{line}\n"));
    }
}

fn spawn(launch: &Launch, args: Vec<String>, env: Vec<(String, String)>) -> Result<Child, Cannot> {
    let mut cmd = Command::new(crate::desktop_notify::app_path("geekbridge-calls"));
    cmd.args(args)
        .env(bridge_core::ipc::ENV_STATE_DIR, &launch.state_dir)
        .env(
            bridge_core::ipc::ENV_CONTROL_PORT,
            launch.control_port.to_string(),
        )
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().map_err(|e| Cannot::SpawnFailed(e.to_string()))
}

/// Starts card processes. The real one is [`ProcessLauncher`]; tests count launches and kills.
pub trait Launcher: Send + Sync {
    fn launch(&self) -> Result<Box<dyn Card>, Cannot>;
}

/// A started card process.
pub trait Card: Send {
    /// Still running. Reaps the process once it exited.
    fn running(&mut self) -> bool;
    /// Stop it and reap it.
    fn kill(&mut self);
}

/// How long a card may outlive the last ring: the card itself closes 1.5 s after the ring ends, or
/// 2.5 s after showing an outcome ("Answered on phone"); this leaves room for both.
pub const LINGER: Duration = Duration::from_secs(6);

/// One card for the rings the hub reports. Not thread-safe by itself; the hub holds it in a mutex.
pub struct Supervisor {
    launcher: std::sync::Arc<dyn Launcher>,
    card: Option<Box<dyn Card>>,
    /// Rings (hub ring numbers) a card was started for, while they ring.
    launched_for: BTreeSet<u64>,
    /// When a ring was last seen.
    last_ring: Option<Instant>,
    linger: Duration,
}

impl Supervisor {
    pub fn new(launcher: std::sync::Arc<dyn Launcher>) -> Self {
        Self::with_linger(launcher, LINGER)
    }

    pub fn with_linger(launcher: std::sync::Arc<dyn Launcher>, linger: Duration) -> Self {
        Self {
            launcher,
            card: None,
            launched_for: BTreeSet::new(),
            last_ring: None,
            linger,
        }
    }

    /// `ringing`: the numbers of the rings going on right now (empty when nothing rings).
    pub fn update(&mut self, ringing: &BTreeSet<u64>, now: Instant) {
        let running = self.card.as_mut().is_some_and(|c| c.running());
        if !running {
            self.card = None; // an exited card was reaped by running()
        }
        self.launched_for.retain(|r| ringing.contains(r));
        if !ringing.is_empty() {
            self.last_ring = Some(now);
            let uncovered = ringing.iter().any(|r| !self.launched_for.contains(r));
            if !running && uncovered {
                // Every ring now going on counts as covered, whether or not the card could start:
                // a missing display is not retried four times a second, and a card that dies during
                // its ring is not restarted in a loop.
                self.launched_for.extend(ringing.iter().copied());
                if let Ok(card) = self.launcher.launch() {
                    self.card = Some(card);
                }
            }
            return;
        }
        if running
            && self
                .last_ring
                .is_none_or(|t| now.saturating_duration_since(t) >= self.linger)
            && let Some(mut card) = self.card.take()
        {
            card.kill();
        }
    }

    /// Whether a card process is up (diagnostics and tests).
    pub fn card_running(&mut self) -> bool {
        self.card.as_mut().is_some_and(|c| c.running())
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if let Some(mut card) = self.card.take() {
            card.kill();
        }
    }
}

/// The real card: `geekbridge-calls --overlay` next to this program, with the display found by
/// [`preflight`]. Each failure is logged once and kept in `call-overlay-status` (`doctor` shows
/// it); the banner and the Calls window still work.
pub struct ProcessLauncher {
    launch: Launch,
}

impl ProcessLauncher {
    pub fn new(launch: Launch) -> Self {
        Self { launch }
    }
}

struct ProcessCard(Child);

impl Card for ProcessCard {
    fn running(&mut self) -> bool {
        matches!(self.0.try_wait(), Ok(None))
    }
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Launcher for ProcessLauncher {
    fn launch(&self) -> Result<Box<dyn Card>, Cannot> {
        let launch = &self.launch;
        let result = preflight(&launch.state_dir)
            .and_then(|(mode, env)| spawn(launch, args(launch, mode), env));
        record(
            &launch.state_dir,
            &result.as_ref().map(|_| ()).map_err(Clone::clone),
        );
        result.map(|child| Box::new(ProcessCard(child)) as Box<dyn Card>)
    }
}

/// `geekbridge-ctl call-overlay-test`: the card with a fake call for 10 seconds, no phone needed.
/// Runs in the ctl process, so it uses that session's display and waits for the card to close.
pub fn test(state_dir: &Path, api_port: u16, control_port: u16) -> Result<(), Cannot> {
    let launch = Launch {
        state_dir: state_dir.to_path_buf(),
        api_port,
        control_port,
    };
    let (_, env) = preflight(state_dir)?;
    let mut args = args(&launch, Mode::On);
    args.push("--test".into());
    let mut child = spawn(&launch, args, env)?;
    let _ = child.wait();
    Ok(())
}

/// The doctor line after a ring that could not show the card; the fix depends on the system.
pub fn last_ring_failed(os: &str, why: &str) -> String {
    if needs_display_variables(os) {
        format!(
            "the last ring could not start it ({why}); the service's session may lack the display variables. Try: systemctl --user import-environment WAYLAND_DISPLAY DISPLAY XDG_RUNTIME_DIR && systemctl --user restart geekbridge-service"
        )
    } else {
        format!(
            "the last ring could not start it ({why}); check that geekbridge-calls is installed next to geekbridge-service, then restart GeekBridge from its tray icon or the Start menu / Applications folder. The call banner still shows."
        )
    }
}

/// The `doctor` line: what the last real launch said, and whether one would work from here.
pub fn doctor_line(state_dir: &Path) -> (bool, String) {
    let last = std::fs::read_to_string(state_dir.join(STATUS_FILE)).unwrap_or_default();
    let last = last.trim();
    match preflight(state_dir) {
        Err(c) => (false, format!("cannot: {} ({})", c.code(), c.explain())),
        Ok(_) if last.starts_with("cannot") => (
            false,
            last_ring_failed(std::env::consts::OS, last.trim_start_matches("cannot: ")),
        ),
        Ok(_) => (
            true,
            "can launch (geekbridge-ctl call-overlay-test shows it)".to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            state_dir: PathBuf::from("/tmp/x"),
            api_port: 17380,
            control_port: 17381,
        }
    }

    #[test]
    fn arguments_carry_the_port_and_the_privacy_mode() {
        assert_eq!(
            args(&launch(), Mode::On),
            ["--overlay", "--api-port", "17380"]
        );
        assert_eq!(
            args(&launch(), Mode::NameOnly),
            ["--overlay", "--api-port", "17380", "--name-only"]
        );
    }

    #[test]
    fn the_privacy_switch_hides_the_number_unless_an_older_switch_decided() {
        if std::env::var_os("GEEKBRIDGE_CALL_OVERLAY").is_some() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(crate::privacy::FILE), "caller_number=off\n").unwrap();
        assert_eq!(mode(d.path()), Mode::NameOnly);
        std::fs::write(d.path().join(crate::privacy::FILE), "caller_number=on\n").unwrap();
        assert_eq!(mode(d.path()), Mode::On);
        // The call-overlay file still decides first: `on` there shows the number, `off` disables the card.
        std::fs::write(d.path().join(crate::privacy::FILE), "caller_number=off\n").unwrap();
        std::fs::write(d.path().join(SETTINGS_FILE), "off\n").unwrap();
        assert_eq!(mode(d.path()), Mode::Off);
        std::fs::write(d.path().join(SETTINGS_FILE), "on\n").unwrap();
        assert_eq!(
            mode(d.path()),
            Mode::On,
            "an explicit `on` forces the number"
        );
        std::fs::write(d.path().join(SETTINGS_FILE), "\n").unwrap();
        assert_eq!(
            mode(d.path()),
            Mode::NameOnly,
            "an empty file forces nothing"
        );
    }

    #[test]
    fn the_settings_file_is_read_when_the_variable_is_unset() {
        // The variable is process-global; only the file path is exercised here.
        if std::env::var_os("GEEKBRIDGE_CALL_OVERLAY").is_some() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        assert_eq!(mode(d.path()), Mode::On);
        std::fs::write(d.path().join(SETTINGS_FILE), "off\n").unwrap();
        assert_eq!(mode(d.path()), Mode::Off);
        std::fs::write(d.path().join(SETTINGS_FILE), "name\n").unwrap();
        assert_eq!(mode(d.path()), Mode::NameOnly);
    }

    fn no_entries(_: &Path) -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn a_service_with_a_display_adds_nothing() {
        let env = |k: &str| (k == "WAYLAND_DISPLAY").then(|| "wayland-1".to_owned());
        assert_eq!(display_env(&env, &no_entries, &Vec::new, None), Ok(vec![]));
    }

    #[test]
    fn a_bare_service_finds_the_wayland_socket_in_the_runtime_dir() {
        let env = |k: &str| (k == "XDG_RUNTIME_DIR").then(|| "/run/user/1000".to_owned());
        let entries = |_: &Path| {
            vec![
                "bus".to_owned(),
                "wayland-1.lock".to_owned(),
                "wayland-1".to_owned(),
                "hypr".to_owned(),
            ]
        };
        assert_eq!(
            display_env(&env, &entries, &Vec::new, None),
            Ok(vec![("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned())])
        );
        // Without XDG_RUNTIME_DIR the default location is used and exported.
        let none = |_: &str| None;
        assert_eq!(
            display_env(
                &none,
                &entries,
                &Vec::new,
                Some(PathBuf::from("/run/user/1000"))
            ),
            Ok(vec![
                ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned()),
                ("XDG_RUNTIME_DIR".to_owned(), "/run/user/1000".to_owned())
            ])
        );
    }

    #[test]
    fn windows_and_macos_need_no_display_variables_but_linux_does() {
        assert!(!needs_display_variables("windows"));
        assert!(!needs_display_variables("macos"));
        assert!(needs_display_variables("linux"));
        assert!(needs_display_variables("freebsd"));
    }

    #[test]
    fn the_failed_ring_hint_names_no_linux_tool_off_linux() {
        for os in ["windows", "macos"] {
            let t = last_ring_failed(os, "spawn-failed: x");
            assert!(!t.contains("systemctl") && !t.contains("WAYLAND"), "{t}");
            assert!(t.contains("spawn-failed: x"));
        }
        assert!(last_ring_failed("linux", "x").contains("systemctl --user import-environment"));
    }

    #[test]
    fn x11_is_the_last_resort_and_nothing_at_all_is_a_reason() {
        let none = |_: &str| None;
        assert_eq!(
            display_env(&none, &no_entries, &|| vec!["X0".to_owned()], None),
            Ok(vec![("DISPLAY".to_owned(), ":0".to_owned())])
        );
        let err = display_env(&none, &no_entries, &Vec::new, None).unwrap_err();
        assert_eq!(err.code(), "no-display");
        assert!(
            Cannot::SpawnFailed("x".into())
                .code()
                .starts_with("spawn-failed")
        );
    }

    #[test]
    fn the_last_outcome_is_kept_for_doctor_and_only_rewritten_on_change() {
        let d = tempfile::tempdir().unwrap();
        record(d.path(), &Err(Cannot::NoDisplay));
        let text = std::fs::read_to_string(d.path().join(STATUS_FILE)).unwrap();
        assert_eq!(text, "cannot: no-display\n");
        record(d.path(), &Ok(()));
        assert_eq!(
            std::fs::read_to_string(d.path().join(STATUS_FILE)).unwrap(),
            "ok\n"
        );
    }

    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fake {
        launches: AtomicUsize,
        kills: Arc<AtomicUsize>,
        fail: AtomicBool,
        /// The card of the last launch is "running" while this is true.
        alive: Arc<AtomicBool>,
    }
    struct FakeCard {
        alive: Arc<AtomicBool>,
        kills: Arc<AtomicUsize>,
    }
    impl Card for FakeCard {
        fn running(&mut self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        fn kill(&mut self) {
            self.kills.fetch_add(1, Ordering::SeqCst);
            self.alive.store(false, Ordering::SeqCst);
        }
    }
    impl Launcher for Fake {
        fn launch(&self) -> Result<Box<dyn Card>, Cannot> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(Cannot::NoDisplay);
            }
            self.alive.store(true, Ordering::SeqCst);
            Ok(Box::new(FakeCard {
                alive: self.alive.clone(),
                kills: self.kills.clone(),
            }))
        }
    }
    fn rings(r: &[u64]) -> BTreeSet<u64> {
        r.iter().copied().collect()
    }

    #[test]
    fn one_card_per_ring_and_a_hung_card_is_killed_after_the_linger() {
        let fake = Arc::new(Fake::default());
        let mut s = Supervisor::with_linger(fake.clone(), Duration::from_secs(6));
        let t = Instant::now();
        for i in 0..10 {
            s.update(&rings(&[1]), t + Duration::from_millis(250 * i));
        }
        assert_eq!(fake.launches.load(Ordering::SeqCst), 1);
        // The ring ended; the card is allowed its own close first.
        s.update(&rings(&[]), t + Duration::from_secs(4));
        assert_eq!(fake.kills.load(Ordering::SeqCst), 0);
        s.update(&rings(&[]), t + Duration::from_secs(9));
        assert_eq!(fake.kills.load(Ordering::SeqCst), 1);
        assert!(!s.card_running());
    }

    #[test]
    fn a_card_closing_just_as_a_new_ring_comes_is_replaced_but_a_crashing_one_is_not_looped() {
        let fake = Arc::new(Fake::default());
        let mut s = Supervisor::new(fake.clone());
        let t = Instant::now();
        s.update(&rings(&[1]), t);
        // Ring 2 arrives while card 1 is still up (closing): no second card yet.
        s.update(&rings(&[2]), t + Duration::from_millis(100));
        assert_eq!(fake.launches.load(Ordering::SeqCst), 1);
        // Card 1 exits a moment later: ring 2 gets its own card.
        fake.alive.store(false, Ordering::SeqCst);
        s.update(&rings(&[2]), t + Duration::from_millis(300));
        assert_eq!(fake.launches.load(Ordering::SeqCst), 2);
        // That card crashes while ring 2 goes on: not restarted.
        fake.alive.store(false, Ordering::SeqCst);
        for i in 0..8 {
            s.update(&rings(&[2]), t + Duration::from_millis(400 + 250 * i));
        }
        assert_eq!(fake.launches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_launch_that_cannot_start_is_tried_once_per_ring() {
        let fake = Arc::new(Fake::default());
        fake.fail.store(true, Ordering::SeqCst);
        let mut s = Supervisor::new(fake.clone());
        let t = Instant::now();
        for i in 0..20 {
            s.update(&rings(&[7]), t + Duration::from_millis(250 * i));
        }
        assert_eq!(fake.launches.load(Ordering::SeqCst), 1);
        s.update(&rings(&[]), t + Duration::from_secs(6));
        s.update(&rings(&[8]), t + Duration::from_secs(7));
        assert_eq!(fake.launches.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn dropping_the_supervisor_kills_its_card() {
        let fake = Arc::new(Fake::default());
        let mut s = Supervisor::new(fake.clone());
        s.update(&rings(&[1]), Instant::now());
        drop(s);
        assert_eq!(fake.kills.load(Ordering::SeqCst), 1);
    }
}
