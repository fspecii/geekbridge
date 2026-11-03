// SPDX-License-Identifier: GPL-3.0-or-later
//! Two seconds after start the service drops its idle code pages from its resident set
//! (`bridge_core::codepages`: 4.4 of 7.7 MB RSS measured on an idle service). The 20 s session-bus
//! probe (`coexist.rs`) repeats the drop after each run, since it touches the D-Bus code again.
//! Linux only; a no-op elsewhere.
/// Drop the idle code pages once, shortly after start. Call from inside the runtime.
pub fn after_startup() {
    #[cfg(target_os = "linux")]
    {
        // Late enough that the first sockets and the tray are up.
        const DELAY: std::time::Duration = std::time::Duration::from_secs(2);
        tokio::spawn(async {
            tokio::time::sleep(DELAY).await;
            bridge_core::codepages::drop_idle();
        });
    }
}
