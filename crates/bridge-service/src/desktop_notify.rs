// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop notifications with action buttons through `notify-send --wait` (libnotify >= 0.8):
//! "file received" (Open / Show in folder) and "update available" (Update), plus the helpers the
//! call and share notifications use for a plain click on the body.
//!
//! Daemons differ: mako shows no buttons (the actions are reached with `makoctl menu`, and a click
//! on the body invokes the `default` action), dunst/swaync/GNOME/KDE show buttons. With `--wait`,
//! `notify-send` prints the chosen action's name when the user picks one and nothing when the
//! notification just expires, so a daemon without action support degrades to a plain notification.
//! Text that came from a phone goes through [`crate::notify_text::plain`] and after `--`.
use crate::notify_text::plain;
use bridge_i18n::{tr, tr_args};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Run `notify-send` with `args`, and call `on_action` with the action name the user chose.
/// Never blocks the caller; a missing `notify-send` is silently nothing.
pub fn spawn(args: Vec<String>, on_action: impl FnOnce(&str) + Send + 'static) {
    std::thread::spawn(move || {
        let out = Command::new("notify-send")
            .args(&args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if let Ok(out) = out {
            let choice = String::from_utf8_lossy(&out.stdout);
            let choice = choice.trim();
            if !choice.is_empty() {
                on_action(choice);
            }
        }
    });
}

/// Start a GeekBridge window (`geekbridge-share`, `geekbridge`, ...) next to this binary if it is
/// there, else from `PATH`. Detached; the child is reaped on a thread.
pub fn open_app(name: &str) {
    detach(Command::new(app_path(name)));
}

/// Like [`open_app`], with one environment variable set for the new window (a value that must not
/// appear on a command line).
pub fn open_app_with_env(name: &str, var: &str, value: &str) {
    let mut cmd = Command::new(app_path(name));
    cmd.env(var, value);
    detach(cmd);
}

fn app_path(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn detach(mut cmd: Command) {
    if let Ok(mut child) = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// `file://` URI with everything but unreserved characters and `/` percent-encoded.
pub fn file_uri(path: &Path) -> String {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Arguments of the "file received" notification. `name` is the saved file's name.
pub fn received_args(name: &str) -> Vec<String> {
    vec![
        "--app-name=GeekBridge".into(),
        "--wait".into(),
        "--category=transfer.complete".into(),
        "--expire-time=20000".into(),
        format!("--action=open={}", tr("banner.open")),
        format!("--action=folder={}", tr("banner.show_in_folder")),
        // A click on the body (mako: no buttons) reveals the file; opening it needs a deliberate choice.
        format!("--action=default={}", tr("banner.show_in_folder")),
        "--".into(),
        tr("banner.file_received").into(),
        plain(name, 80),
    ]
}

/// Tell the user a file arrived, with Open / Show in folder.
pub fn file_received(path: &Path) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = path.to_owned();
    spawn(received_args(&name), move |action| match action {
        "open" => {
            let mut c = Command::new("xdg-open");
            c.arg(&path);
            detach(c);
        }
        "folder" | "default" => show_in_folder(&path),
        _ => {}
    });
}

/// Highlight `path` in the file manager (FileManager1 over D-Bus), or open its folder.
pub fn show_in_folder(path: &Path) {
    let uri = file_uri(path);
    let ok = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest=org.freedesktop.FileManager1",
            "--object-path=/org/freedesktop/FileManager1",
            "--method=org.freedesktop.FileManager1.ShowItems",
            &format!("['{uri}']"),
            "",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok && let Some(dir) = path.parent() {
        let mut c = Command::new("xdg-open");
        c.arg(dir);
        detach(c);
    }
}

/// Arguments of the "update available" notification.
pub fn update_args(version: &str) -> Vec<String> {
    vec![
        "--app-name=GeekBridge".into(),
        "--wait".into(),
        "--category=update".into(),
        "--expire-time=30000".into(),
        format!("--action=update={}", tr("banner.update")),
        format!("--action=default={}", tr("banner.update")),
        "--".into(),
        tr("banner.update_available").into(),
        tr_args("banner.update_body", &[("version", &plain(version, 24))]),
    ]
}

/// Notify once per newly seen release; "Update" opens the GeekBridge window, whose update panel
/// downloads and verifies the installer (nothing is installed automatically).
pub fn spawn_update_watcher(updates: std::sync::Arc<bridge_update::Updater>) {
    tokio::spawn(async move {
        let mut told = String::new();
        loop {
            crate::tray::poke();
            let status = updates.status();
            if status.enabled
                && status.newer
                && !status.skipped
                && let Some(latest) = status.latest
                && latest.version != told
            {
                spawn(update_args(&latest.version), |a| {
                    if matches!(a, "update" | "default") {
                        open_app("geekbridge");
                    }
                });
                told = latest.version;
            }
            tokio::time::sleep(std::time::Duration::from_secs(600)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_encodes_spaces_unicode_and_quotes() {
        assert_eq!(
            file_uri(Path::new("/home/a/Downloads/it's é.txt")),
            "file:///home/a/Downloads/it%27s%20%C3%A9.txt"
        );
    }

    #[test]
    fn received_notification_has_the_documented_actions_and_safe_text() {
        let a = received_args("<b>x</b>\n.txt");
        for want in [
            "--action=open=Open",
            "--action=folder=Show in folder",
            "--wait",
        ] {
            assert!(a.contains(&want.to_owned()), "{want}");
        }
        let dd = a.iter().position(|s| s == "--").unwrap();
        assert!(a[dd + 1..].iter().all(|s| !s.contains(['<', '>', '\n'])));
        assert!(a[..dd].iter().all(|s| !s.contains("b>")));
    }

    #[test]
    fn update_notification_text() {
        let a = update_args("0.2.0\u{202E}");
        assert!(a.contains(&"--action=update=Update".to_owned()));
        assert!(a.last().unwrap().contains("0.2.0") && !a.last().unwrap().contains('\u{202E}'));
    }
}
