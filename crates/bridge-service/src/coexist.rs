// SPDX-License-Identifier: GPL-3.0-or-later
//! Living next to stock KDE Connect on the same computer (docs/coexistence.md).
//!
//! Both programs may sync the same clipboard to the same phone, which can echo or loop. Nothing is
//! switched off automatically; this only notices that the stock daemon is running so the
//! clipboard settings can say so in one line.

use std::sync::atomic::{AtomicBool, Ordering};

static KDECONNECT_RUNNING: AtomicBool = AtomicBool::new(false);

/// The stock KDE Connect daemon owned `org.kde.kdeconnect` on the session bus at the last check. The answer
/// comes back at once; when it is older than [`STALE`] a re-check runs in the background (only someone asking,
/// like the clipboard settings, causes one, so an idle service never wakes for it).
pub fn other_sync_app_running() -> bool {
    #[cfg(target_os = "linux")]
    let _ = recheck_if_stale();
    KDECONNECT_RUNNING.load(Ordering::Relaxed)
}

/// Record a probe result (the watcher below, and tests).
pub fn set_other_sync_app_running(running: bool) {
    KDECONNECT_RUNNING.store(running, Ordering::Relaxed);
}

/// Age after which a question starts a new check, in seconds.
#[cfg(target_os = "linux")]
const STALE_SECS: u64 = 20;

#[cfg(target_os = "linux")]
static ASKED: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Unix seconds of the last check; `u64::MAX` while one runs.
#[cfg(target_os = "linux")]
static CHECKED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(u64::MAX);

/// Rings when a check has finished, for askers that wait for a fresh answer ([`refresh_if_stale`]).
#[cfg(target_os = "linux")]
static DONE: tokio::sync::Notify = tokio::sync::Notify::const_new();

#[cfg(target_os = "linux")]
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Check at start, then again whenever a question finds the answer stale. Without a session bus the
/// answer stays "no". Runs for the life of the service; between checks it waits on a notification, no timer.
#[cfg(target_os = "linux")]
pub async fn watch() {
    loop {
        set_other_sync_app_running(probe().await);
        CHECKED.store(unix_secs(), Ordering::Relaxed);
        DONE.notify_waiters();
        // The probe just faulted the D-Bus code back in; let it go again (`bridge_core::codepages`).
        bridge_core::codepages::drop_idle();
        ASKED.notified().await;
    }
}

/// Starts a check when the answer is stale. Returns whether one is now running (started here or already).
#[cfg(target_os = "linux")]
fn recheck_if_stale() -> bool {
    let at = CHECKED.load(Ordering::Relaxed);
    if at == u64::MAX {
        return true;
    }
    if unix_secs().saturating_sub(at) >= STALE_SECS
        && CHECKED
            .compare_exchange(at, u64::MAX, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        ASKED.notify_one(); // one asker wins, so a burst of questions starts one check
        return true;
    }
    false
}

/// Before an answer that is shown to a person (the clipboard status): when the last check is stale, run one
/// and wait for it, at most half a second, so a one-shot `geekbridge-ctl clipboard` after hours of idle does
/// not print the answer from hours ago. Fresh answers cost nothing.
pub async fn refresh_if_stale() {
    #[cfg(target_os = "linux")]
    {
        let done = DONE.notified();
        tokio::pin!(done);
        done.as_mut().enable(); // registered before the check can finish
        if recheck_if_stale() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(500), done).await;
        }
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

    /// A stale answer is re-checked and waited for, so a one-shot status is not hours old.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_stale_answer_is_rechecked_before_it_is_shown() {
        let watcher = tokio::spawn(watch());
        while CHECKED.load(Ordering::Relaxed) == u64::MAX {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        CHECKED.store(1, Ordering::Relaxed); // hours old
        let t = std::time::Instant::now();
        refresh_if_stale().await;
        assert!(CHECKED.load(Ordering::Relaxed) > 1, "a new check ran");
        assert!(t.elapsed() < std::time::Duration::from_millis(450));
        let t = std::time::Instant::now();
        refresh_if_stale().await; // fresh now: no wait, no check
        assert!(t.elapsed() < std::time::Duration::from_millis(50));
        watcher.abort();
    }
}
