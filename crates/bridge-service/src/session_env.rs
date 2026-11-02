// SPDX-License-Identifier: GPL-3.0-or-later
//! Finding the graphical session when the service was started by `systemd --user` before the
//! compositor exported its environment.
//!
//! Under uwsm (Omarchy) and similar session managers the user manager gets `WAYLAND_DISPLAY` only
//! after login, and a unit started earlier has none; wl-clipboard, the Hyprland hotkey binder and
//! `notify-send` then silently do nothing. The service asks the runtime directory instead.
use std::path::Path;

/// `(name, value)` pairs worth setting, given what is already in the environment.
/// `has` tells whether a variable is already set and non-empty.
pub fn missing_session_vars(
    runtime_dir: &Path,
    has: &dyn Fn(&str) -> bool,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if !has("WAYLAND_DISPLAY")
        && let Some(sock) = newest(runtime_dir, |n| {
            n.starts_with("wayland-") && !n.ends_with(".lock")
        })
    {
        out.push(("WAYLAND_DISPLAY", sock));
    }
    if !has("HYPRLAND_INSTANCE_SIGNATURE") {
        // $XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock
        let hypr = runtime_dir.join("hypr");
        if let Some(sig) = newest(&hypr, |n| hypr.join(n).join(".socket.sock").exists()) {
            out.push(("HYPRLAND_INSTANCE_SIGNATURE", sig));
        }
    }
    if !has("DBUS_SESSION_BUS_ADDRESS") && runtime_dir.join("bus").exists() {
        out.push((
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/bus", runtime_dir.display()),
        ));
    }
    out
}

/// Most recently modified entry of `dir` whose file name passes `keep`.
fn newest(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<String> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            keep(&name).then(|| (e.metadata().and_then(|m| m.modified()).ok(), name))
        })
        .max()
        .map(|(_, name)| name)
}

/// Set the variables [`missing_session_vars`] finds. Call before any other thread exists.
#[cfg(unix)]
pub fn adopt_session_env() {
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        return;
    };
    let has = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    for (k, v) in missing_session_vars(Path::new(&dir), &has) {
        // SAFETY: called first thing in `main`, before the runtime or any thread is started.
        unsafe { std::env::set_var(k, v) };
    }
}

#[cfg(not(unix))]
pub fn adopt_session_env() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_wayland_socket_hyprland_signature_and_bus() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("wayland-1"), "").unwrap();
        fs::write(d.path().join("wayland-1.lock"), "").unwrap();
        fs::create_dir_all(d.path().join("hypr/abc_123")).unwrap();
        fs::write(d.path().join("hypr/abc_123/.socket.sock"), "").unwrap();
        fs::create_dir_all(d.path().join("hypr/stale")).unwrap(); // no socket: ignored
        fs::write(d.path().join("bus"), "").unwrap();
        let got = missing_session_vars(d.path(), &|_| false);
        assert_eq!(
            got,
            vec![
                ("WAYLAND_DISPLAY", "wayland-1".to_owned()),
                ("HYPRLAND_INSTANCE_SIGNATURE", "abc_123".to_owned()),
                (
                    "DBUS_SESSION_BUS_ADDRESS",
                    format!("unix:path={}/bus", d.path().display())
                ),
            ]
        );
    }

    #[test]
    fn keeps_what_is_already_set_and_handles_an_empty_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(missing_session_vars(d.path(), &|_| false).is_empty());
        fs::write(d.path().join("wayland-0"), "").unwrap();
        assert!(missing_session_vars(d.path(), &|k| k == "WAYLAND_DISPLAY").is_empty());
    }
}
