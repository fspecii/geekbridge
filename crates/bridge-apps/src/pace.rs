// SPDX-License-Identifier: GPL-3.0-or-later
//! Poll pacing for the background workers. The service offers no pushed events (IPC is request /
//! response), so idle windows poll less often: unfocused or minimised windows and windows with no
//! phone paired back off. Interactive commands still wake a worker at once through its channel.
use crate::state::DiscoveryModel;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

static ACTIVE: AtomicBool = AtomicBool::new(true);
static PAIRED: AtomicBool = AtomicBool::new(true);

/// Backoff multiplier for an unfocused / minimised window, and for one with nothing paired.
const UNFOCUSED: u32 = 4;
const UNPAIRED: u32 = 2;

/// Record the window state; call once per frame.
pub fn observe(ctx: &eframe::egui::Context, model: &DiscoveryModel) {
    let active = ctx.input(|i| i.focused && !i.viewport().minimized.unwrap_or(false));
    ACTIVE.store(active, Ordering::Relaxed);
    crate::memtrim::on_frame(active);
    // Only a reachable service with an empty device list counts as "nothing paired".
    let paired = !model.service_reachable() || !model.devices().is_empty();
    PAIRED.store(paired, Ordering::Relaxed);
}

/// Window state without a discovery model (single-purpose windows).
pub fn observe_focus(ctx: &eframe::egui::Context) {
    let active = ctx.input(|i| i.focused && !i.viewport().minimized.unwrap_or(false));
    ACTIVE.store(active, Ordering::Relaxed);
    crate::memtrim::on_frame(active);
}

/// `base` stretched by the current backoff.
pub fn scaled(base: Duration) -> Duration {
    let mut factor = 1;
    if !ACTIVE.load(Ordering::Relaxed) {
        factor *= UNFOCUSED;
    }
    if !PAIRED.load(Ordering::Relaxed) {
        factor *= UNPAIRED;
    }
    base * factor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backs_off_when_inactive_or_unpaired() {
        // Statics are process-wide: keep every assertion in one test.
        let base = Duration::from_secs(1);
        ACTIVE.store(true, Ordering::Relaxed);
        PAIRED.store(true, Ordering::Relaxed);
        assert_eq!(scaled(base), base);
        ACTIVE.store(false, Ordering::Relaxed);
        assert_eq!(scaled(base), base * 4);
        PAIRED.store(false, Ordering::Relaxed);
        assert_eq!(scaled(base), base * 8);
        ACTIVE.store(true, Ordering::Relaxed);
        assert_eq!(scaled(base), base * 2);
        PAIRED.store(true, Ordering::Relaxed);
    }
}
