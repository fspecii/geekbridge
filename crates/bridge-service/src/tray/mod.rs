// SPDX-License-Identifier: GPL-3.0-or-later
//! A quiet tray / menu-bar presence for the service: an icon that shows connected / disconnected /
//! ringing / update available / transfer in progress, and a small menu (open the app, paste from
//! phone, send a file, answer or decline, pause sync, check for updates, quit).
//!
//! * [`model`]: the pure state-to-icon and menu mapping, unit-tested once for every platform.
//! * [`icon`]: the dot-matrix icons, drawn from code.
//! * `sni` (Linux, StatusNotifierItem + dbusmenu), `windows` (Shell_NotifyIcon), `macos`
//!   (NSStatusItem): the backends, each optional, each silent when its host is missing.
//!
//! It runs inside the service process: it reads the same in-memory state the control API serves
//! and needs no token, no second binary and no second set of sockets. It never polls: the rest of
//! the service calls [`poke`] where something the tray shows changes (a link event, a call that
//! starts or stops ringing, a transfer starting, the update watcher's own tick) and the tray task
//! sleeps until then. See `docs/user-guide.md` for turning it off.
pub mod icon;
#[cfg(target_os = "macos")]
mod macos;
pub mod model;
#[cfg(target_os = "linux")]
pub mod sni;
#[cfg(windows)]
mod windows;

use crate::daemon::Daemon;
use bridge_core::{
    calls_view::CallsView,
    ipc::{CallAction, Feature, LinkPhase, Request, Response},
};
use model::{TrayAction, TrayInput};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Notify, mpsc};

static CHANGED: Notify = Notify::const_new();
static QUIT: Notify = Notify::const_new();

/// Something the tray shows may have changed. Cheap, never blocks, harmless with no tray running.
pub fn poke() {
    CHANGED.notify_one();
}

/// Resolves when "Quit" is chosen in the tray menu.
pub async fn quit_requested() {
    QUIT.notified().await
}

/// Marker file in the state directory: present means "no tray icon".
pub const DISABLED_FILE: &str = "tray-disabled";
/// Device ids whose clipboard sync "Pause sync" switched off, so "Resume sync" (also after a
/// restart) switches exactly those back on.
pub const PAUSED_FILE: &str = "tray-paused";

pub fn enabled(state_dir: &Path) -> bool {
    !state_dir.join(DISABLED_FILE).exists()
}

pub fn set_enabled(state_dir: &Path, on: bool) -> std::io::Result<()> {
    let marker = state_dir.join(DISABLED_FILE);
    if on {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(state_dir)?;
        std::fs::write(marker, b"")
    }
}

/// Edit the debounce only from tests.
const DEBOUNCE: Duration = Duration::from_millis(150);

/// Run the tray until the service ends. Returns at once when the tray is off or has no host.
pub async fn run(daemon: Arc<Daemon>, state_dir: PathBuf) {
    let (tx, mut rx) = mpsc::unbounded_channel::<TrayAction>();
    let on_action: Arc<dyn Fn(TrayAction) + Send + Sync> = Arc::new(move |a| {
        let _ = tx.send(a);
    });
    let input = collect(&daemon, &state_dir).await;
    let Some(backend) = backend::start(&input, on_action).await else {
        return;
    };
    loop {
        tokio::select! {
            () = CHANGED.notified() => {
                // Several events usually arrive together (a link coming up emits a few).
                tokio::time::sleep(DEBOUNCE).await;
                backend.update(&collect(&daemon, &state_dir).await).await;
            }
            action = rx.recv() => {
                let Some(action) = action else { return };
                perform(&daemon, &state_dir, action).await;
                poke();
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod backend {
    use super::{TrayAction, TrayInput, icon::Style, sni::Sni};
    use std::sync::Arc;

    pub struct Backend(Sni);
    pub async fn start(
        input: &TrayInput,
        on_action: Arc<dyn Fn(TrayAction) + Send + Sync>,
    ) -> Option<Backend> {
        Sni::start(None, Style::Light, input, on_action)
            .await
            .ok()
            .map(Backend)
    }
    impl Backend {
        pub async fn update(&self, input: &TrayInput) {
            self.0.update(input).await
        }
    }
}

#[cfg(windows)]
mod backend {
    pub use super::windows::start;
}

#[cfg(target_os = "macos")]
mod backend {
    pub use super::macos::start;
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
mod backend {
    use super::{TrayAction, TrayInput};
    use std::sync::Arc;
    pub struct Backend;
    pub async fn start(_: &TrayInput, _: Arc<dyn Fn(TrayAction) + Send + Sync>) -> Option<Backend> {
        None
    }
    impl Backend {
        pub async fn update(&self, _: &TrayInput) {}
    }
}

fn paused_ids(state_dir: &Path) -> Vec<String> {
    std::fs::read(state_dir.join(PAUSED_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Read the service's own state into the tray's input. Only counts and flags leave this function.
pub async fn collect(daemon: &Arc<Daemon>, state_dir: &Path) -> TrayInput {
    let mut input = TrayInput {
        paused: !paused_ids(state_dir).is_empty(),
        ..TrayInput::default()
    };
    if let Response::Status(s) = daemon.handle(Request::Status).await {
        input.paired = s.paired.len();
        input.connected = s
            .links
            .iter()
            .filter(|l| l.phase == LinkPhase::Connected)
            .map(|l| l.name.clone())
            .collect();
        input.battery = s
            .links
            .iter()
            .filter(|l| l.phase == LinkPhase::Connected)
            .find_map(|l| l.battery);
        if input.connected.is_empty() {
            input.why = s.disconnected.first().cloned();
        }
    }
    if let Response::Calls(c) = daemon.handle(Request::Calls).await
        && let Some(row) = c.view.ringing()
    {
        input.ringing = true;
        input.can_answer = row.answer.is_some();
        input.can_decline = row.reject.is_some();
    }
    if let Response::Share(s) = daemon.handle(Request::ShareStatus).await {
        use bridge_core::share::TransferState::{Active, Waiting};
        input.transfers_live = s
            .transfers
            .iter()
            .filter(|t| matches!(t.state, Waiting | Active))
            .count();
    }
    if let Response::Update(u) = daemon.handle(Request::UpdateStatus).await
        && u.enabled
        && u.newer
        && !u.skipped
    {
        input.update = u.latest.map(|l| l.version);
    }
    if let Response::ClipboardStatus(c) = daemon.handle(Request::ClipboardStatus).await {
        input.can_paste = c.available
            && c.devices
                .iter()
                .any(|d| d.enabled && d.connected && d.negotiated);
    }
    input
}

async fn perform(daemon: &Arc<Daemon>, state_dir: &Path, action: TrayAction) {
    match action {
        TrayAction::Open => launch("geekbridge"),
        TrayAction::SendFile => launch("geekbridge-share"),
        TrayAction::PasteFromPhone => {
            let _ = daemon
                .handle(Request::ClipboardPull { device_id: None })
                .await;
        }
        TrayAction::Answer | TrayAction::Decline => {
            let wanted = if action == TrayAction::Answer {
                CallAction::Answer
            } else {
                CallAction::Reject
            };
            if let Response::Calls(c) = daemon.handle(Request::Calls).await
                && let Some(target) = target_for(&c.view, wanted)
            {
                let _ = daemon
                    .handle(Request::CallAction {
                        target,
                        action: wanted,
                    })
                    .await;
            }
        }
        TrayAction::TogglePause => toggle_pause(daemon, state_dir).await,
        TrayAction::CheckUpdates => {
            let _ = daemon.handle(Request::UpdateCheck).await;
        }
        TrayAction::Quit => QUIT.notify_one(),
    }
}

/// The control the latest call view offers for the ringing call, never one made up here.
fn target_for(
    view: &CallsView,
    action: CallAction,
) -> Option<bridge_core::calls_view::ActionTarget> {
    let row = view.ringing()?;
    match action {
        CallAction::Answer => row.answer.clone(),
        CallAction::Reject => row.reject.clone(),
        CallAction::Hangup => row.hangup.clone(),
    }
}

fn launch(app: &str) {
    crate::desktop_notify::open_app(&format!("{app}{}", std::env::consts::EXE_SUFFIX));
}

/// "Pause sync" switches the clipboard feature off for every paired phone that has it on and
/// remembers which; "Resume sync" switches exactly those back on. Nothing else is touched, and
/// the record survives a restart so a paused session is never silently half-restored.
async fn toggle_pause(daemon: &Arc<Daemon>, state_dir: &Path) {
    let paused = paused_ids(state_dir);
    if !paused.is_empty() {
        for device_id in &paused {
            let _ = daemon
                .handle(Request::SetConsent {
                    device_id: device_id.clone(),
                    feature: Feature::Clipboard,
                    enabled: true,
                })
                .await;
        }
        let _ = std::fs::remove_file(state_dir.join(PAUSED_FILE));
        return;
    }
    let Response::Status(status) = daemon.handle(Request::Status).await else {
        return;
    };
    let on: Vec<String> = status
        .paired
        .iter()
        .filter(|d| {
            d.consents
                .iter()
                .any(|c| c.feature == Feature::Clipboard && c.enabled)
        })
        .map(|d| d.device_id.clone())
        .collect();
    if on.is_empty() {
        return;
    }
    // Record first: if the write fails nothing has been switched off.
    let Ok(json) = serde_json::to_vec(&on) else {
        return;
    };
    // Atomic: a crash mid-write must not leave a torn list that "forgets" which devices to resume.
    if bridge_core::statefile::write_atomic_plain(&state_dir.join(PAUSED_FILE), &json).is_err() {
        return;
    }
    for device_id in on {
        let _ = daemon
            .handle(Request::SetConsent {
                device_id,
                feature: Feature::Clipboard,
                enabled: false,
            })
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disable_setting_round_trips_and_is_enabled_by_default() {
        let dir = tempfile::tempdir().unwrap();
        assert!(enabled(dir.path()));
        set_enabled(dir.path(), false).unwrap();
        assert!(!enabled(dir.path()));
        set_enabled(dir.path(), false).unwrap();
        set_enabled(dir.path(), true).unwrap();
        assert!(enabled(dir.path()));
        set_enabled(dir.path(), true).unwrap();
    }

    #[test]
    fn a_missing_or_garbled_paused_record_means_not_paused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(paused_ids(dir.path()).is_empty());
        std::fs::write(dir.path().join(PAUSED_FILE), b"not json").unwrap();
        assert!(paused_ids(dir.path()).is_empty());
        std::fs::write(dir.path().join(PAUSED_FILE), br#"["abc"]"#).unwrap();
        assert_eq!(paused_ids(dir.path()), vec!["abc".to_owned()]);
    }
}
