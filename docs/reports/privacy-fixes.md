# Privacy fixes after the privacy review

Follow-up to [docs/privacy.md](../privacy.md) "Findings". Each change was written test first (the new tests were run
against the old code and failed).

## 1. Debug logs removed with the pairing
* `PhoneDebug::erase` (`crates/bridge-service/src/phone_debug.rs`) deletes `phone-logs/<id>.log` and `.log.1` to `.log.3`
  of that phone only, and drops a waiting state request. A missing file is fine.
* Called from `Daemon::forget_consent`, which every unpair path goes through (control API and `geekbridge-ctl unpair`,
  unpair with no live link, phone-initiated unpair), and from `set_consent` when `debug` is switched off.
* `phone-state` dumps were never written to disk (printed by `geekbridge-ctl`, held in memory only); the doc now says so.
* Tests: `unpair_in_every_direction_deletes_the_phones_debug_logs`,
  `withdrawing_debug_consent_deletes_the_log_but_keeps_the_pairing` (`crates/bridge-service/tests/control_api.rs`),
  `erase_removes_live_and_rotated_logs_of_that_phone_only` (unit). The first two failed before the change.

## 2. `geekbridge.*` capability names no longer broadcast
* Finding: every consumer of a peer's capabilities (`is_geekbridge_peer` for the heartbeat in `daemon.rs` and the Android
  engine, `collision.rs`, `gate.rs` negotiation, the Android `peer_debug`/`peer_accepts`) reads `link.info().peer`, which
  is the identity exchanged inside TLS (`link.rs`, secure identity replaces the plaintext claim; only id and protocol
  version are cross-checked). Stock KDE Connect protocol 8 does the same. So the UDP list is not needed for them.
* The only user of the broadcast list was the desktop `advertises_feature` hint (`bridge-apps/src/state.rs`); it is
  displayed only for Messages, which looks for `kdeconnect.sms`, still broadcast. The Agenda prefix
  (`geekbridge.calendar`) is not shown anywhere, so nothing is lost.
* `Identity::for_broadcast` and `addressed_to` (plaintext TCP line) now drop `geekbridge.*` capabilities and the port
  hint. Used by `Daemon::announcement` and the Android `encode_announcement`. The TLS identity is unchanged.
* Remaining exposure: stock `kdeconnect.*` capability names (needed for stock interop) and the device name/type.
* Tests: `plaintext_identities_carry_no_geekbridge_capabilities_but_the_secure_one_does` (kde),
  `the_broadcast_names_no_geekbridge_feature` (android discovery); `announce_follows_a_ready_listener_and_is_kde_shaped`
  updated to the new contract. Negotiation, stock-peer, heartbeat and collision tests pass unchanged.

## 3. Messages read marks
Code read: marks are `read_through_ms` / `unread_at_mark` in the service's in-memory per-thread state
(`phone_data.rs`, `mark_read`). The Messages app only calls the service and writes no file. They are lost on a service
restart and dropped on consent-off or unpair. `docs/privacy.md` states this. Not verified by running a restart.

## 4. Docs
`docs/privacy.md` updated (SMS, identity, debug, erase table, file table, threat notes, findings 1, 4, 5); the
`privacy_doc` guard test passes. One Security line added under `[0.1.0]` in `CHANGELOG.md`.

## Checks (clean worktree of origin/main 796047f16)
`cargo fmt --all --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo test --workspace
--no-fail-fast` all pass; `scripts/build-android.sh` (gradle unit tests, lintDebug, debug APK) 0. The Kotlin code did not
change.
