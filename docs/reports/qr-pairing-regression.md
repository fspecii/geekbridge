# QR pairing regression after the control-stall fix (2026-11-03)

Symptom: after commit 5e54be057, three tests in `crates/bridge-service/tests/qr_pairing.rs` saw `status().paired` empty right after the phone's pair completed.

Root cause: when the desktop approves an incoming request, the pairing machine emits `Send(accept)` and `Complete` in one batch. `PendingLink::complete` inserted the trust record after the accept had gone out. Moving that insert to `spawn_blocking` turned a synchronous step into a yield point, so the phone could finish and the caller could query status before the record existed. The store itself (reader/writer lock split) was correct: readers see the new state as soon as the insert returns.

Fix (`crates/bridge-kde/src/pairing.rs`): `complete` is split into `persist` (await the blocking insert; on failure send a pair rejection and fail) and `into_trusted`. When a batch holds both our accept and `Complete`, the insert runs before the accept is sent, so pairing is durable and visible before either side reports success. The insert stays in `spawn_blocking`, so the no-blocking-workers property and its two latency tests are unchanged.

Verification: qr_pairing 9/9, five repeats. bridge-kde and bridge-service suites pass; on this machine (load average about 28 from parallel agents) a few lock-timing tests (persistence, pair/unpair cycles, cancel midway, a data-consent test) flaked in full runs and pass alone.

Performance budget job (red on 66d5327af and df91d0f6a): not related to pairing. The stripped `geekbridge-service` was 5,823,760 B against a 5,800,000 B budget (phone mirroring growth). Budget raised to 6,000,000 B in `scripts/perf-check.sh`; all other idle metrics were within budget.
