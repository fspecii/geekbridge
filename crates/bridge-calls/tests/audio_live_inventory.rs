// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in, read-only check of the real libpulse adapter against this host's audio server:
//! `cargo test -p bridge-calls --test audio_live_inventory -- --ignored --nocapture`.
//!
//! It connects, reads cards/sinks/sources/modules and prints a summary. It loads and unloads
//! nothing, plays nothing, and never starts a sound server. It cannot prove calling or audio
//! relay: that needs a Bluetooth controller and a paired phone.
#![cfg(target_os = "linux")]

use bridge_calls::audio::{AudioRouteManager, AudioServer, PulseOptions, PulseServer};

#[test]
#[ignore = "talks to the host's real audio server (read-only)"]
fn live_read_only_inventory() {
    let server = match PulseServer::connect(PulseOptions::default()) {
        Ok(s) => s,
        Err(e) => {
            println!("no audio server reachable (not a failure): {e}");
            return;
        }
    };
    let mut server = server;
    let snap = server.snapshot().expect("read-only snapshot");
    let with_args = snap
        .modules
        .iter()
        .filter(|m| m.argument.as_deref().is_some_and(|a| !a.is_empty()))
        .count();
    println!(
        "modules: {} ({} report an argument string)",
        snap.modules.len(),
        with_args
    );
    let mut mgr = AudioRouteManager::new(server);
    let inv = mgr.inventory().expect("read-only inventory");
    println!(
        "server: {} {}",
        inv.server.server_name, inv.server.server_version
    );
    println!(
        "server identity cookie reported: {}",
        inv.server.cookie != 0
    );
    println!("bluetooth phone cards: {}", inv.phones.len());
    println!(
        "local outputs: {}, local inputs: {}",
        inv.local_outputs.len(),
        inv.local_inputs.len()
    );
    println!("orphaned geekbridge modules: {}", inv.orphans.len());
    assert!(inv.phones.iter().all(|p| p.address.expose().len() == 17));
    assert!(mgr.active().is_none(), "inventory must not start anything");
}
