// SPDX-License-Identifier: GPL-3.0-or-later
//! Starts the incoming-call card (`geekbridge-calls --overlay`) when a call rings.
//!
//! The card is a separate small process: it polls the service like the Calls window does, shows
//! Answer / Reject in the top-right corner of the screen and exits when the call is over, so the
//! service holds no window code. One card at a time; a second ring while the first card is still
//! up reuses it (the card follows whichever call rings).
use bridge_core::call_overlay::Mode;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// What the child needs to find this service again: the service may run on other ports than the
/// defaults (tests, a second instance).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub state_dir: PathBuf,
    pub api_port: u16,
    pub control_port: u16,
}

static LAUNCH: OnceLock<Launch> = OnceLock::new();
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Remember how this service was started. Called once at start-up; without it no card is started.
pub fn configure(launch: Launch) {
    let _ = LAUNCH.set(launch);
}

pub const SETTINGS_FILE: &str = "call-overlay";

/// The mode from `GEEKBRIDGE_CALL_OVERLAY` or `<state dir>/call-overlay`.
pub fn mode(state_dir: &Path) -> Mode {
    let env = std::env::var("GEEKBRIDGE_CALL_OVERLAY").ok();
    let file = std::fs::read_to_string(state_dir.join(SETTINGS_FILE)).ok();
    Mode::resolve(env.as_deref(), file.as_deref())
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

/// Show the card for a ringing call, unless it is switched off or already up. Best effort: when
/// it cannot start, the reason is logged once and kept in `call-overlay-status` (`doctor` shows
/// it); the notification and the Calls window still work.
pub fn ring() {
    let Some(launch) = LAUNCH.get() else { return };
    let mut slot = CHILD.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(child) = slot.as_mut() {
        match child.try_wait() {
            Ok(None) => return, // still up
            _ => *slot = None,
        }
    }
    let result =
        preflight(&launch.state_dir).and_then(|(mode, env)| spawn(launch, args(launch, mode), env));
    match result {
        Ok(child) => {
            record(&launch.state_dir, &Ok(()));
            *slot = Some(child);
        }
        Err(c) => record(&launch.state_dir, &Err(c)),
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

    #[test]
    fn ring_without_configuration_does_nothing() {
        // No LAUNCH in this test binary unless another test set it; either way this must not panic.
        ring();
    }
}
