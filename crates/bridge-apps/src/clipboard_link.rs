//! Clipboard sync state for the clipboard app: a small background worker that asks the service for
//! the clipboard bridge status once a second and forwards the user's actions. The view-model text
//! lives here, in pure functions, so what the window promises is tested.
use crate::{connection::Config, state::display_text};
use bridge_core::{
    clipboard_wire::NoneReason,
    ipc::{ClipboardPullStatus, ClipboardStatus, Feature, Request, Response},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClipState {
    pub status: Option<ClipboardStatus>,
    pub error: Option<String>,
    pub notice: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Action {
    Pull,
    Push,
    SetEnabled { device_id: String, enabled: bool },
    SetWindowSeconds(u64),
    SetHotkey(String),
    SetImageLimitMib(u64),
}

impl Action {
    fn request(&self) -> Request {
        match self {
            Self::Pull => Request::ClipboardPull { device_id: None },
            Self::Push => Request::ClipboardPush { device_id: None },
            Self::SetEnabled { device_id, enabled } => Request::SetConsent {
                device_id: device_id.clone(),
                feature: Feature::Clipboard,
                enabled: *enabled,
            },
            Self::SetWindowSeconds(s) => Request::ClipboardSettings {
                window_ms: Some(s.saturating_mul(1000)),
                hotkey: None,
                max_image_bytes: None,
            },
            Self::SetHotkey(h) => Request::ClipboardSettings {
                window_ms: None,
                hotkey: Some(h.clone()),
                max_image_bytes: None,
            },
            Self::SetImageLimitMib(m) => Request::ClipboardSettings {
                window_ms: None,
                hotkey: None,
                max_image_bytes: Some(m.saturating_mul(1024 * 1024)),
            },
        }
    }
}

pub struct ClipLink {
    commands: mpsc::SyncSender<Action>,
    latest: Arc<Mutex<Option<ClipState>>>,
    stop: Arc<AtomicBool>,
    pub state: ClipState,
}

impl ClipLink {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let config = Config::from_environment();
        let (commands, rx) = mpsc::sync_channel::<Action>(4);
        let latest = Arc::new(Mutex::new(None));
        let output = latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = stop.clone();
        std::thread::Builder::new()
            .name("geekbridge-clipboard-link".into())
            .spawn(move || {
                let mut state = ClipState::default();
                let mut previous: Option<ClipState> = None;
                while !quitting.load(Ordering::Acquire) {
                    match config.request(&Request::ClipboardStatus) {
                        Ok(Response::ClipboardStatus(s)) => {
                            state.status = Some(*s);
                            state.error = None;
                        }
                        Ok(_) => state.error = Some("The service answered unexpectedly.".into()),
                        Err(e) => {
                            state.status = None;
                            state.error = Some(e);
                        }
                    }
                    if previous.as_ref() != Some(&state) {
                        previous = Some(state.clone());
                        *output.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
                        wake();
                    }
                    match rx.recv_timeout(crate::pace::scaled(Duration::from_secs(1))) {
                        Ok(action) if !quitting.load(Ordering::Acquire) => {
                            state.notice = Some(match config.request(&action.request()) {
                                Ok(_) => action_notice(&action).into(),
                                Err(e) => e,
                            });
                        }
                        Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn clipboard link worker");
        Self {
            commands,
            latest,
            stop,
            state: ClipState::default(),
        }
    }

    pub fn update(&mut self) {
        if let Some(s) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.state = s;
        }
    }

    pub fn send(&mut self, action: Action) {
        if self.commands.try_send(action).is_err() {
            self.state.notice = Some("Another request is in progress. Try again shortly.".into());
        }
    }
}

impl Drop for ClipLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn action_notice(a: &Action) -> &'static str {
    match a {
        Action::Pull => "Asking your phone…",
        Action::Push => "Sent this computer's clipboard to your phone.",
        Action::SetEnabled { enabled: true, .. } => "Clipboard sharing is on for this phone.",
        Action::SetEnabled { .. } => "Clipboard sharing is off for this phone.",
        Action::SetWindowSeconds(_) => "Time window saved.",
        Action::SetHotkey(_) => "Shortcut saved.",
        Action::SetImageLimitMib(_) => "Image size limit saved.",
    }
}

// ------------------------------------------------------------------ words

/// What happened to the last "paste from phone", in a sentence.
pub fn pull_line(p: &ClipboardPullStatus) -> String {
    match (p.state.as_str(), p.reason.as_deref()) {
        ("delivered", _) => format!(
            "Pasted from your phone ({}). It's on this computer's clipboard.",
            p.kind.as_deref().unwrap_or("item")
        ),
        ("waiting", _) => {
            "Waiting for your phone. If nothing happens, tap the GeekBridge notification on it."
                .into()
        }
        (_, Some(r)) => match NoneReason::parse(r) {
            Some(NoneReason::Locked) => "Your phone is locked. Unlock it and copy again.".into(),
            Some(NoneReason::Expired) => {
                "Nothing recent on your phone. Copy again, then paste within the time window."
                    .into()
            }
            Some(NoneReason::Empty) => "Your phone's clipboard is empty.".into(),
            Some(NoneReason::NeedsGesture) => {
                "Android needs a tap: open the GeekBridge notification on your phone.".into()
            }
            Some(NoneReason::Disabled) => "Clipboard sharing is off in the phone app.".into(),
            Some(NoneReason::TooLarge) => "That item is too large to send.".into(),
            Some(NoneReason::Sensitive) => {
                "The phone marked that item as sensitive, so it wasn't sent.".into()
            }
            Some(NoneReason::Unsupported) => "The phone can't send that kind of item.".into(),
            None if r == "timeout" => "Your phone didn't answer in time.".into(),
            None => "Couldn't get an item from your phone.".into(),
        },
        _ => "Couldn't get an item from your phone.".into(),
    }
}

/// Why this device last skipped something, for a person.
pub fn skip_line(reason: &str) -> &'static str {
    match reason {
        "sensitive" => "Skipped a password-manager item",
        "too_large" => "Skipped something too large",
        "phone_not_accepting" => "This phone's app doesn't accept clipboard items",
        "link_busy" => "Phone link was busy",
        "invalid_text" => "Skipped text it couldn't send",
        _ => "Skipped an item",
    }
}

pub fn backend_line(s: &ClipboardStatus) -> String {
    if !s.available {
        return match s.problem.as_deref() {
            Some("no_clipboard_tool") => {
                "No clipboard access. Install wl-clipboard (Wayland) or xclip (X11).".into()
            }
            _ => "Clipboard sync is turned off on this computer.".into(),
        };
    }
    let how = match s.observe.as_str() {
        "polling" => "checked about twice a second",
        "event" => "instant",
        _ => "not watched",
    };
    let state = if s.watching {
        "watching"
    } else {
        "idle until you turn a phone on"
    };
    let problem = match s.problem.as_deref() {
        Some("clipboard_io_failed") => " · last read failed",
        Some("clipboard_unavailable") => " · tool missing",
        _ => "",
    };
    format!("Clipboard access: {} ({how}, {state}){problem}", s.backend)
}

pub fn hotkey_line(s: &ClipboardStatus) -> String {
    match s.hotkey_binding.as_str() {
        "bound" => format!("{} pastes from your phone, anywhere.", s.hotkey),
        "not_bound" => format!(
            "{} is set, but this desktop wouldn't register it. Bind the command below to a key.",
            s.hotkey
        ),
        _ => format!(
            "{} works while this window is focused. For a system-wide shortcut, bind the command below in your desktop's keyboard settings (Wayland doesn't let apps grab keys themselves).",
            s.hotkey
        ),
    }
}

pub const PASTE_COMMAND: &str = "geekbridge-ctl clipboard paste-from-phone --notify";

pub fn window_line(window_ms: u64) -> String {
    let s = window_ms / 1000;
    format!(
        "Your phone only offers something you copied in the last {} and only while it's unlocked.",
        if s >= 60 && s.is_multiple_of(60) {
            format!("{} min", s / 60)
        } else {
            format!("{s} s")
        }
    )
}

pub fn device_name(name: &str) -> String {
    display_text(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::ipc::ClipboardStatus;

    fn status() -> ClipboardStatus {
        ClipboardStatus {
            backend: "wl-clipboard".into(),
            available: true,
            observe: "polling".into(),
            watching: false,
            problem: None,
            max_text_bytes: 65536,
            max_image_bytes: 8 << 20,
            window_ms: 30_000,
            hotkey: "Ctrl+Shift+V".into(),
            hotkey_binding: "unsupported".into(),
            last_pull: None,
            devices: vec![],
        }
    }

    fn pull(state: &str, reason: Option<&str>) -> ClipboardPullStatus {
        ClipboardPullStatus {
            device_id: "d".into(),
            state: state.into(),
            reason: reason.map(str::to_owned),
            kind: Some("text".into()),
            at_unix_ms: 0,
        }
    }

    #[test]
    fn every_reason_the_phone_can_give_has_a_human_sentence() {
        for r in [
            "locked",
            "expired",
            "empty",
            "needs_gesture",
            "disabled",
            "too_large",
            "unsupported",
            "sensitive",
            "timeout",
            "failed",
        ] {
            let line = pull_line(&pull("none", Some(r)));
            assert!(!line.contains('_'), "{r}: {line}");
            assert!(line.ends_with('.'), "{r}: {line}");
        }
        assert!(pull_line(&pull("delivered", None)).contains("clipboard"));
        assert!(pull_line(&pull("waiting", None)).contains("notification"));
    }

    #[test]
    fn honest_about_android_and_wayland() {
        let s = status();
        assert!(hotkey_line(&s).contains("Wayland doesn't let apps grab keys"));
        assert!(pull_line(&pull("none", Some("needs_gesture"))).contains("Android needs a tap"));
        let mut bound = s.clone();
        bound.hotkey_binding = "bound".into();
        assert!(hotkey_line(&bound).contains("anywhere"));
        let mut missing = s;
        missing.available = false;
        missing.problem = Some("no_clipboard_tool".into());
        assert!(backend_line(&missing).contains("wl-clipboard"));
    }

    #[test]
    fn window_sentence_states_the_limits() {
        assert!(window_line(30_000).contains("30 s") && window_line(30_000).contains("unlocked"));
        assert!(window_line(120_000).contains("2 min"));
    }

    #[test]
    fn actions_map_to_service_requests() {
        assert!(matches!(
            Action::SetWindowSeconds(45).request(),
            Request::ClipboardSettings {
                window_ms: Some(45_000),
                ..
            }
        ));
        assert!(matches!(
            Action::SetEnabled {
                device_id: "x".into(),
                enabled: true
            }
            .request(),
            Request::SetConsent {
                feature: Feature::Clipboard,
                enabled: true,
                ..
            }
        ));
        // Huge numbers saturate instead of wrapping.
        assert!(matches!(
            Action::SetImageLimitMib(u64::MAX).request(),
            Request::ClipboardSettings {
                max_image_bytes: Some(u64::MAX),
                ..
            }
        ));
    }

    #[test]
    fn skip_lines_never_leak_codes() {
        for r in ["sensitive", "too_large", "phone_not_accepting", "weird"] {
            assert!(!skip_line(r).contains('_'));
        }
    }
}
