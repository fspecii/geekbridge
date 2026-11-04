// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in, read-only check of the real libpulse adapter against this host's audio server:
//! `cargo test -p bridge-calls --test audio_live_inventory -- --ignored --nocapture`.
//!
//! It connects, reads cards, devices and streams and prints a summary. It moves, mutes and
//! switches nothing, plays nothing, and never starts a sound server.
#![cfg(target_os = "linux")]

use bridge_calls::audio::model::ProfileKind;
use bridge_calls::audio::{AudioServer, PulseOptions, PulseServer, route};

#[test]
#[ignore = "talks to the host's real audio server (read-only)"]
fn live_read_only_inventory() {
    let mut server = match PulseServer::connect(PulseOptions::default()) {
        Ok(s) => s,
        Err(e) => {
            println!("no audio server reachable (not a failure): {e}");
            return;
        }
    };
    let snap = server.snapshot().expect("read-only snapshot");
    println!(
        "server: {} {} (PipeWire: {})",
        snap.identity.server_name,
        snap.identity.server_version,
        snap.identity.is_pipewire()
    );
    let phones = snap
        .cards
        .iter()
        .filter(|c| c.gateway_profile().is_some())
        .count();
    println!("bluetooth phone cards: {phones}");
    for c in &snap.cards {
        if c.address().is_some() {
            println!(
                "  bluetooth card active profile: {:?} ({:?})",
                c.active_profile,
                c.active_kind().unwrap_or(ProfileKind::Other)
            );
        }
    }
    let l = route::devices(&snap);
    println!(
        "local outputs: {}, local inputs: {}, playback streams: {}, capture streams: {}",
        l.outputs.len(),
        l.inputs.len(),
        snap.sink_inputs.len(),
        snap.source_outputs.len()
    );
}
