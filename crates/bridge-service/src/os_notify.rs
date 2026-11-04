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
use crate::notify_text::{banner_safe, isolate};
use crate::share::Notifier;
use bridge_i18n::{tr, tr_args};
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
    // Phone-chosen parts arrive already isolated (see `isolate`); anything else loses its direction controls.
    let title = banner_safe(title, 80);
    let body = banner_safe(body, 160);
    match os {
        Os::Windows => Banner {
            program: "powershell.exe",
            args: [
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass", // i18n-ok: PowerShell policy name
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
                "on run argv".into(), // i18n-ok: script
                "-e".into(),
                "display notification (item 2 of argv) with title \"GeekBridge\" subtitle (item 1 of argv)" // i18n-ok: script
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
        crate::call_overlay::ring();
        show(
            tr("banner.incoming_call"),
            &tr_args("banner.call_body", &[("caller", &isolate(caller, 80))]),
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
            &tr_args(
                "banner.file_offer_title",
                &[("device", &isolate(device, 48))],
            ),
            &tr_args("banner.file_offer_body", &[("file", &isolate(file, 80))]),
        );
    }
    fn received_text(&self, device: &str, is_url: bool) {
        let title = tr(if is_url {
            "banner.received_link"
        } else {
            "banner.received_text"
        });
        show(
            title,
            &tr_args("banner.received_from", &[("device", &isolate(device, 48))]),
        );
    }
    fn received_file(&self, path: &Path) {
        let name = path
            .file_name()
            .map_or(String::new(), |n| n.to_string_lossy().into_owned());
        show(tr("banner.file_received"), &name);
    }
}

/// The banner body of a mirrored phone notification: `title: text` with each phone-chosen piece isolated.
fn mirror_body(b: &crate::mirror::MirrorBanner) -> String {
    match &b.text {
        Some(t) if !t.is_empty() => tr_args(
            "banner.mirror_body",
            &[
                ("title", &isolate(&b.title, 80)),
                ("text", &isolate(t, 160)),
            ],
        ),
        _ => tr_args(
            "banner.on_device",
            &[("device", &isolate(&b.device_name, 48))],
        ),
    }
}

impl crate::mirror::MirrorNotifier for BannerNotifier {
    fn show(
        &self,
        b: &crate::mirror::MirrorBanner,
        _reply: Box<dyn FnOnce(crate::mirror::Choice) + Send>,
    ) {
        // No buttons in a plain banner: dismiss and buttons are `geekbridge-ctl notifications`.
        let body = mirror_body(b);
        show(&isolate(&b.app, 48), &body);
    }
}

/// Title and body of the banner for a new text: the sender, and the phone's name, or the text itself only
/// when the user chose to show message text in notifications (`IncomingText::body` is `None` otherwise).
fn text_banner(n: &crate::phone_data::IncomingText) -> (String, String) {
    let body = match &n.body {
        Some(t) => isolate(&t.replace('\n', " "), 160),
        None => tr_args(
            "banner.on_device",
            &[("device", &isolate(&n.device_name, 48))],
        ),
    };
    (
        tr_args("banner.new_message", &[("sender", &isolate(&n.sender, 60))]),
        body,
    )
}

impl crate::phone_data::SmsNotifier for BannerNotifier {
    fn received(&self, device: &str, sender: &str) {
        show(
            &tr_args("banner.new_message", &[("sender", &isolate(sender, 60))]),
            &tr_args("banner.on_device", &[("device", &isolate(device, 48))]),
        );
    }
    fn incoming(&self, n: &crate::phone_data::IncomingText) {
        let (title, body) = text_banner(n);
        show(&title, &body);
    }
    fn event_soon(&self, device: &str, title: &str, minutes: u32) {
        let title = isolate(title, 70);
        let head = if minutes == 0 {
            tr_args("banner.event_now", &[("title", &title)])
        } else {
            tr_args(
                "banner.event_in",
                &[("minutes", &minutes), ("title", &title)],
            )
        };
        show(
            &head,
            &tr_args("banner.calendar_on", &[("device", &isolate(device, 48))]),
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
    fn mirrored_body_keeps_title_and_text_in_order_with_each_isolated() {
        let arabic = "\u{645}\u{643}\u{62a}\u{628}";
        let b = crate::mirror::MirrorBanner {
            device_name: "Pixel".into(),
            app: "Chat".into(),
            title: arabic.into(),
            text: Some("\u{202e}5 min".into()),
            clearable: false,
            actions: vec![],
            slot: "1".into(),
        };
        let body = banner_safe(&mirror_body(&b), 300);
        assert_eq!(
            body,
            format!("\u{2068}{arabic}\u{2069}: \u{2068}5 min\u{2069}")
        );
        // Without content the body names the phone, isolated, never the title.
        let hidden = crate::mirror::MirrorBanner { text: None, ..b };
        assert_eq!(mirror_body(&hidden), "On \u{2068}Pixel\u{2069}");
    }

    #[test]
    fn a_text_banner_shows_the_message_only_when_allowed() {
        let mut n = crate::phone_data::IncomingText {
            device_name: "Pixel".into(),
            sender: "Ann".into(),
            number: "+1555".into(),
            body: None,
        };
        let (title, body) = text_banner(&n);
        assert!(title.contains("Ann"));
        assert!(body.contains("Pixel") && !body.contains("pin"));
        n.body = Some("my pin\nis 1234".into());
        let (_, body) = text_banner(&n);
        assert!(body.contains("my pin is 1234") && !body.contains("Pixel"));
    }

    #[test]
    fn long_text_is_cut() {
        let b = banner(Os::MacOs, &"a".repeat(500), &"b".repeat(500));
        assert!(b.args[b.args.len() - 2].chars().count() <= 80);
        assert!(b.args[b.args.len() - 1].chars().count() <= 160);
    }
}
