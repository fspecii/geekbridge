// SPDX-License-Identifier: GPL-3.0-or-later
//! System-wide binding of the "paste from phone" hotkey.
//!
//! A global shortcut needs a compositor, window-system or portal facility, and there is no portable
//! one. This build binds it where that can be done at runtime without editing the user's files:
//!
//! * **Hyprland** (`hyprctl keyword bind`): bound while at least one phone has clipboard sync on,
//!   removed again afterwards. The binding lives only in the running compositor.
//! * **Windows** (`RegisterHotKey`): a small thread owns the registration and its message loop and
//!   starts `geekbridge-ctl clipboard paste-from-phone` on the hotkey. Another program holding the
//!   same combination makes the binding fail, which the status reports as `not_bound`.
//! * **macOS** has no cheap, dependency-free global hotkey API (Carbon's `RegisterEventHotKey` needs
//!   an application event loop), so it reports `unsupported`; use a Shortcuts / Automator quick
//!   action running the same command.
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

/// Win32 modifier flags (`MOD_ALT`, `MOD_CONTROL`, `MOD_SHIFT`, `MOD_WIN`) for a hotkey.
#[cfg(any(windows, test))]
fn windows_modifiers(key: &Hotkey) -> u32 {
    u32::from(key.alt)
        | u32::from(key.ctrl) << 1
        | u32::from(key.shift) << 2
        | u32::from(key.meta) << 3
}

/// Virtual-key code for the hotkey's key: letters and digits are their ASCII codes, `F1`..`F24`
/// are `0x70..=0x87`.
#[cfg(any(windows, test))]
fn windows_virtual_key(key: &Hotkey) -> Option<u32> {
    let k = key.key.as_str();
    if let [c] = k.as_bytes()
        && (c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return Some(u32::from(*c));
    }
    let n: u32 = k.strip_prefix('F')?.parse().ok()?;
    (1..=24).contains(&n).then_some(0x70 + n - 1)
}

#[cfg(windows)]
mod windows_binder {
    use super::*;
    use std::sync::Mutex;
    use std::sync::mpsc;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        HOT_KEY_MODIFIERS, RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_HOTKEY, WM_QUIT,
    };

    const MOD_NOREPEAT: u32 = 0x4000;
    const HOTKEY_ID: i32 = 0x6B62;

    struct Active {
        thread_id: u32,
        join: std::thread::JoinHandle<()>,
    }

    /// `RegisterHotKey` on a dedicated thread: the registration belongs to the thread that made it,
    /// and `WM_HOTKEY` arrives in that thread's message queue.
    #[derive(Default)]
    pub struct WindowsBinder {
        active: Mutex<Option<Active>>,
    }

    fn spawn_paste() {
        let ctl = sibling_ctl().unwrap_or_else(|| "geekbridge-ctl".to_owned());
        let _ = Command::new(ctl)
            .args(["clipboard", "paste-from-phone"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    impl HotkeyBinder for WindowsBinder {
        fn name(&self) -> &'static str {
            "windows"
        }
        fn bind(&self, key: &Hotkey, _command: &str) -> bool {
            let Some(vk) = windows_virtual_key(key) else {
                return false;
            };
            let mods = windows_modifiers(key) | MOD_NOREPEAT;
            let mut slot = self.active.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_some() {
                return false;
            }
            let (ready_tx, ready_rx) = mpsc::channel::<Option<u32>>();
            let join = std::thread::Builder::new()
                .name("hotkey".into())
                .spawn(move || {
                    // SAFETY: plain Win32 calls on this thread with a null window handle; `msg` is
                    // a local that outlives each call.
                    unsafe {
                        let mut msg = MSG::default();
                        // Force the thread's message queue into existence before announcing it.
                        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                        if RegisterHotKey(None, HOTKEY_ID, HOT_KEY_MODIFIERS(mods), vk).is_err() {
                            let _ = ready_tx.send(None);
                            return;
                        }
                        let _ = ready_tx.send(Some(GetCurrentThreadId()));
                        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                            if msg.message == WM_HOTKEY {
                                spawn_paste();
                            }
                        }
                        let _ = UnregisterHotKey(None, HOTKEY_ID);
                    }
                });
            let Ok(join) = join else {
                return false;
            };
            match ready_rx.recv() {
                Ok(Some(thread_id)) => {
                    *slot = Some(Active { thread_id, join });
                    true
                }
                _ => {
                    let _ = join.join();
                    false
                }
            }
        }
        fn unbind(&self, _key: &Hotkey) {
            let active = self.active.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(a) = active {
                // SAFETY: posts WM_QUIT to a thread this binder started and still owns.
                let _ = unsafe {
                    PostThreadMessageW(a.thread_id, WM_QUIT, Default::default(), Default::default())
                };
                let _ = a.join.join();
            }
        }
    }
}

/// The binder for this session, if any.
pub fn detect() -> Option<std::sync::Arc<dyn HotkeyBinder>> {
    #[cfg(windows)]
    return Some(std::sync::Arc::new(windows_binder::WindowsBinder::default()));
    #[cfg(not(windows))]
    HyprlandBinder::detect().map(|b| std::sync::Arc::new(b) as _)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_codes() {
        let k = Hotkey::parse("Ctrl+Shift+V").unwrap();
        assert_eq!(windows_modifiers(&k), 0x2 | 0x4);
        assert_eq!(windows_virtual_key(&k), Some(0x56));
        let k = Hotkey::parse("Alt+Super+F12").unwrap();
        assert_eq!(windows_modifiers(&k), 0x1 | 0x8);
        assert_eq!(windows_virtual_key(&k), Some(0x7B));
        assert_eq!(
            windows_virtual_key(&Hotkey::parse("Ctrl+7").unwrap()),
            Some(0x37)
        );
    }

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
