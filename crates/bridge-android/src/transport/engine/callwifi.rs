// SPDX-License-Identifier: GPL-3.0-or-later
//! EXPERIMENTAL call audio over Wi-Fi, phone side (`geekbridge.callaudio.*`, docs/design/call-audio-over-wifi.md).
//!
//! Compiled only with the `callwifi` cargo feature (debug builds); without it [`Hub`] is an inert stub, so a release
//! build has no Opus, no gate and does not even know the packet names.
//!
//! Rust owns the wire, the consent gate and the codec; Kotlin owns the shell-uid helper, the self-test and the
//! notification. Kotlin tells the hub the switch, the call state and the self-test result, pushes 20 ms PCM frames
//! of the far party, and collects events (`start`, `stop`, `speak`, `indicator`). Audio is never stored or logged.
#[cfg(feature = "callwifi")]
mod real;
#[cfg(feature = "callwifi")]
pub(in crate::transport) use real::*;

#[cfg(not(feature = "callwifi"))]
mod stub {
    use super::super::Core;
    use bridge_kde::Packet;

    #[derive(Default)]
    pub(in crate::transport) struct Hub(());

    impl Hub {
        pub(in crate::transport) fn shutdown(&self) {}
        pub(in crate::transport) fn link_lost(&self, _connection: u64) {}
    }

    pub(in crate::transport) fn incoming_types() -> Vec<String> {
        Vec::new()
    }
    pub(in crate::transport) fn outgoing_types() -> Vec<String> {
        Vec::new()
    }
    pub(in crate::transport) fn is_kind(_kind: &str) -> bool {
        false
    }
    impl Core {
        pub(in crate::transport) fn incoming_callwifi(&self, _id: u64, _p: &Packet) -> bool {
            false
        }
        pub(in crate::transport) fn callwifi_revalidate(&self) {}
        pub(in crate::transport) fn callwifi_connected(&self, _id: u64) {}
    }
}
#[cfg(not(feature = "callwifi"))]
pub(in crate::transport) use stub::*;
