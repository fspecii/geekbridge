// SPDX-License-Identifier: GPL-3.0-or-later
//! Fuzzing for GeekBridge's network-facing parsers. See `docs/fuzzing.md`.
//!
//! * [`targets`]: one `fn(&[u8])` per parser. Each asserts its own oracle (no panic, bounded
//!   output, sanitiser outputs free of control characters and path separators) and is shared by
//!   the libFuzzer crate in `fuzz/` and the deterministic mutation fuzzer below.
//! * [`mutate`]: structure-aware generation and mutation (grammar-built JSON packets, field-level
//!   mutations, byte-level mutation of recorded real packets, encoding tricks).
//! * [`harness`]: counting allocator, per-input time limit with a hang watchdog, crash reports.
//!
//! Tooling code: panics are the failure signal, so the workspace's no-unwrap lints do not apply.
#![allow(clippy::unwrap_used, clippy::expect_used)]

pub mod harness;
pub mod mutate;
pub mod rng;
pub mod targets;

pub use harness::CountingAlloc;
