//! System-wide binding of the "paste from phone" hotkey.
//!
//! A global shortcut needs a compositor, window-system or portal facility, and there is no portable
//! one. This build binds it where that can be done at runtime without editing the user's files:
//!
//! * **Hyprland** (`hyprctl keyword bind`): bound while at least one phone has clipboard sync on,
//!   removed again afterwards. The binding lives only in the running compositor.
//! * Everywhere else the status says `unsupported` and the user binds
//!   `geekbridge-ctl clipboard paste-from-phone` in their desktop's shortcut settings
//!   (`docs/clipboard-sync.md` lists GNOME, KDE, Sway, X11/xbindkeys, Windows and macOS options).
use bridge_core::hotkey::Hotkey;
use std::process::{Command, Stdio};

/// Something that can register a system-wide shortcut running `command`.
pub trait HotkeyBinder: Send + Sync {
    fn name(&self) -> &'static str;
    fn bind(&self, key: &Hotkey, command: &str) -> bool;
    fn unbind(&self, key: &Hotkey);
}

pub struct HyprlandBinder;

impl HyprlandBinder {
    pub fn detect() -> Option<Self> {
        let in_hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
        let has_ctl = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("hyprctl").is_file()));
        (in_hyprland && has_ctl).then_some(Self)
    }

    fn run(args: &[&str]) -> bool {
        Command::new("hyprctl")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// The value of Hyprland's `bind` keyword: `MODS,key,exec,command`.
pub fn hyprland_bind_arg(key: &Hotkey, command: &str) -> String {
    format!("{},exec,{command}", key.hyprland_spec())
}

/// Command line the shortcut runs. `ctl` is only used when it is a plain path without characters
/// that the compositor's parser could split on.
pub fn paste_command(ctl: Option<&str>) -> String {
    let plain = ctl.filter(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    });
    format!(
        "{} clipboard paste-from-phone",
        plain.unwrap_or("geekbridge-ctl")
    )
}

/// `geekbridge-ctl` next to the running service binary, if it exists.
pub fn sibling_ctl() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let ctl = exe.parent()?.join("geekbridge-ctl");
    ctl.is_file().then(|| ctl.to_string_lossy().into_owned())
}

impl HotkeyBinder for HyprlandBinder {
    fn name(&self) -> &'static str {
        "hyprland"
    }
    fn bind(&self, key: &Hotkey, command: &str) -> bool {
        Self::run(&["keyword", "bind", &hyprland_bind_arg(key, command)])
    }
    fn unbind(&self, key: &Hotkey) {
        Self::run(&["keyword", "unbind", &key.hyprland_spec()]);
    }
}

/// The binder for this session, if any.
pub fn detect() -> Option<std::sync::Arc<dyn HotkeyBinder>> {
    HyprlandBinder::detect().map(|b| std::sync::Arc::new(b) as _)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyprland_argument_and_command() {
        let k = Hotkey::parse("Ctrl+Shift+V").unwrap();
        assert_eq!(
            hyprland_bind_arg(&k, &paste_command(None)),
            "CTRL SHIFT,V,exec,geekbridge-ctl clipboard paste-from-phone"
        );
        assert_eq!(
            paste_command(Some("/opt/geek bridge/geekbridge-ctl")),
            "geekbridge-ctl clipboard paste-from-phone",
            "paths with spaces are not trusted to survive the compositor's parser"
        );
        assert_eq!(
            paste_command(Some("/usr/local/bin/geekbridge-ctl")),
            "/usr/local/bin/geekbridge-ctl clipboard paste-from-phone"
        );
    }
}
