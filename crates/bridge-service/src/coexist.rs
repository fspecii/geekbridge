// SPDX-License-Identifier: GPL-3.0-or-later
//! Living next to stock KDE Connect on the same computer (docs/coexistence.md).
//!
//! Both programs may sync the same clipboard to the same phone, which can echo or loop. Nothing is
//! switched off automatically; this only notices that the stock daemon is running so the
//! clipboard settings can say so in one line.

use std::sync::atomic::{AtomicBool, Ordering};

static KDECONNECT_RUNNING: AtomicBool = AtomicBool::new(false);

/// The stock KDE Connect daemon owned `org.kde.kdeconnect` on the session bus at the last check.
pub fn other_sync_app_running() -> bool {
    KDECONNECT_RUNNING.load(Ordering::Relaxed)
}

/// Record a probe result (the watcher below, and tests).
pub fn set_other_sync_app_running(running: bool) {
    KDECONNECT_RUNNING.store(running, Ordering::Relaxed);
}

/// Re-check the session bus every 20 s, for ever. Without a session bus the answer stays "no".
#[cfg(target_os = "linux")]
pub async fn watch() {
    loop {
        set_other_sync_app_running(probe().await);
        // The probe just faulted the D-Bus code back in; let it go again (`bridge_core::codepages`).
        bridge_core::codepages::drop_idle();
        tokio::time::sleep(std::time::Duration::from_secs(20)).await;
    }
}

#[cfg(target_os = "linux")]
async fn probe() -> bool {
    // Connect the socket ourselves: `Connection::session()` would do it on the `blocking` crate's
    // thread pool, whose first thread then wakes twice a second for the life of the process.
    let Ok(stream) = crate::tray::sni::connect(None) else {
        return false;
    };
    let Ok(connection) = zbus::connection::Builder::async_io_unix_stream(stream)
        .build()
        .await
    else {
        return false;
    };
    let Ok(bus) = zbus::fdo::DBusProxy::new(&connection).await else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from("org.kde.kdeconnect") else {
        return false;
    };
    bus.name_has_owner(name).await.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_round_trips() {
        set_other_sync_app_running(true);
        assert!(other_sync_app_running());
        set_other_sync_app_running(false);
        assert!(!other_sync_app_running());
    }
}
