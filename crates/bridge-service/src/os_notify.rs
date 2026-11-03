// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain banner notifications for Windows and macOS, so an incoming call, a file offer and a new
//! text are not Linux-only (Linux uses `notify-send` with buttons, see `desktop_notify`).
//!
//! A banner has no buttons here: the call, offer and message are answered in the GeekBridge
//! windows, the banner only says that something is waiting. Text that came from a phone goes
//! through [`crate::notify_text::plain`] and reaches the helper as an argument or an environment
//! variable, never inside script source, so it cannot become code. Best effort: a failing helper
//! means no banner, and every window and the phone itself keep working.
//!
//! The command lines are built by pure functions for both systems so they are unit-tested on any
//! host; actually showing a banner is only verified on a real Windows or Mac (docs/platform-status.md).
use crate::call_control::{CallNotifier, RingChoice};
use crate::notify_text::plain;
use crate::share::Notifier;
use std::path::Path;

/// Which banner helper to build a command for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
}

/// A helper process to run: program, arguments and extra environment variables.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub program: &'static str,
    pub args: Vec<String>,
    pub env: Vec<(&'static str, String)>,
}

/// Static toast script. Title and body arrive in `GB_TITLE` / `GB_BODY` and are XML-escaped before
/// they are placed in the toast. The app id is Windows PowerShell's own, which is always registered
/// (an unregistered id makes Windows drop the toast silently).
const TOAST_SCRIPT: &str = "$ErrorActionPreference='Stop';\
[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime]|Out-Null;\
[Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime]|Out-Null;\
$t=[Security.SecurityElement]::Escape($env:GB_TITLE);$b=[Security.SecurityElement]::Escape($env:GB_BODY);\
$x=New-Object Windows.Data.Xml.Dom.XmlDocument;\
$x.LoadXml(\"<toast><visual><binding template='ToastGeneric'><text>$t</text><text>$b</text></binding></visual></toast>\");\
$n=[Windows.UI.Notifications.ToastNotification]::new($x);\
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($n)";

/// The helper command that shows `title` / `body` as a banner on `os`.
pub fn banner(os: Os, title: &str, body: &str) -> Banner {
    let title = plain(title, 80);
    let body = plain(body, 160);
    match os {
        Os::Windows => Banner {
            program: "powershell.exe",
            args: [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                TOAST_SCRIPT,
            ]
            .map(String::from)
            .to_vec(),
            env: vec![("GB_TITLE", title), ("GB_BODY", body)],
        },
        // `on run argv`: the texts are AppleScript values, never part of the script source.
        Os::MacOs => Banner {
            program: "osascript",
            args: vec![
                "-e".into(),
                "on run argv".into(),
                "-e".into(),
                "display notification (item 2 of argv) with title \"GeekBridge\" subtitle (item 1 of argv)"
                    .into(),
                "-e".into(),
                "end run".into(),
                "--".into(),
                title,
                body,
            ],
            env: Vec::new(),
        },
    }
}

/// Show a banner without blocking the caller. No-op on Linux.
pub fn show(title: &str, body: &str) {
    #[cfg(windows)]
    let os = Os::Windows;
    #[cfg(target_os = "macos")]
    let os = Os::MacOs;
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (title, body);
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let b = banner(os, title, body);
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(b.program);
            cmd.args(&b.args)
                .envs(b.env.iter().map(|(k, v)| (*k, v.as_str())))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            let _ = cmd.status();
        });
    }
}

/// Banners for calls, file offers, texts and calendar reminders on Windows and macOS.
pub struct BannerNotifier;

impl CallNotifier for BannerNotifier {
    fn ringing(
        &self,
        caller: &str,
        _can_answer: bool,
        _can_decline: bool,
        _reply: Box<dyn FnOnce(RingChoice) + Send>,
    ) {
        show(
            "Incoming call",
            &format!("{caller}: open GeekBridge Calls to answer"),
        );
    }
}

impl Notifier for BannerNotifier {
    fn offered(
        &self,
        device: &str,
        file: &str,
        _size: Option<u64>,
        _reply: Box<dyn FnOnce(bool) + Send>,
    ) {
        show(
            &format!("{device} wants to send a file"),
            &format!("{file}: open GeekBridge Share to accept"),
        );
    }
    fn received_text(&self, device: &str, is_url: bool) {
        let what = if is_url { "a link" } else { "text" };
        show(&format!("Received {what}"), &format!("From {device}"));
    }
    fn received_file(&self, path: &Path) {
        let name = path
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        show("File received", &name);
    }
}

impl crate::mirror::MirrorNotifier for BannerNotifier {
    fn show(
        &self,
        b: &crate::mirror::MirrorBanner,
        _reply: Box<dyn FnOnce(crate::mirror::Choice) + Send>,
    ) {
        // No buttons in a plain banner: dismiss and buttons are `geekbridge-ctl notifications`.
        let body = match &b.text {
            Some(t) if !t.is_empty() => format!("{}: {t}", b.title),
            _ => format!("On {}", b.device_name),
        };
        show(&b.app, &body);
    }
}

impl crate::phone_data::SmsNotifier for BannerNotifier {
    fn received(&self, device: &str, sender: &str) {
        show(
            &format!("New message from {sender}"),
            &format!("On {device}"),
        );
    }
    fn event_soon(&self, device: &str, title: &str, minutes: u32) {
        let when = if minutes == 0 {
            "Now".to_owned()
        } else {
            format!("In {minutes} min")
        };
        show(
            &format!("{when}: {title}"),
            &format!("Calendar on {device}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_text_is_an_argument_not_script_source() {
        let b = banner(Os::MacOs, "x\" & do shell script \"id", "<b>hi</b>\n");
        assert_eq!(b.program, "osascript");
        let script: Vec<_> = b.args.iter().take(6).collect();
        assert!(script.iter().all(|a| !a.contains("do shell script")));
        let dashes = b.args.iter().position(|a| a == "--").unwrap();
        assert_eq!(b.args.len(), dashes + 3);
        assert!(!b.args[dashes + 2].contains('<'));
        assert!(!b.args[dashes + 2].contains('\n'));
    }

    #[test]
    fn windows_text_travels_in_the_environment_and_the_script_is_static() {
        let b = banner(Os::Windows, "Ada'; Remove-Item x #", "a & b");
        assert_eq!(b.program, "powershell.exe");
        assert_eq!(b.args.last().map(String::as_str), Some(TOAST_SCRIPT));
        assert!(b.args.iter().all(|a| !a.contains("Remove-Item")));
        assert_eq!(b.env[0].0, "GB_TITLE");
        assert!(b.env[0].1.contains("Remove-Item")); // as inert text
        assert!(!b.env[1].1.contains('&'));
    }

    #[test]
    fn long_text_is_cut() {
        let b = banner(Os::MacOs, &"a".repeat(500), &"b".repeat(500));
        assert!(b.args[b.args.len() - 2].chars().count() <= 80);
        assert!(b.args[b.args.len() - 1].chars().count() <= 160);
    }
}
