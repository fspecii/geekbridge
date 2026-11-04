# Persisted state: formats and the upgrade policy

The auto-updater replaces the program, never the user's data. Pairings, trust, settings and caches
must survive an update, a rollback to the previous version, a crash in the middle of a write and a
half-written or zero-byte file. This page lists every persisted format, the rules every store
follows, and the tests that keep them true.

## Policy (all stores)

1. **Every JSON document has an integer `version`.** A file without one is version 1 (that is
   what the first releases wrote). Version 1 is the baseline: `tests/fixtures/state-v1/` holds
   golden files of it, and CI loads them with the current code. A change that breaks a v1 fixture
   is a format break.
2. **Additive changes keep the version.** Adding an optional field needs no bump; older builds
   ignore it. *Unknown fields are preserved*: stores rewrite only the fields they own into the
   freshly read document, so a newer build's extra data survives a round trip through an older one.
3. **Anything else bumps the version** and adds one step to the store's `Spec::migrations`
   (`migrations[i]` turns version `i + 1` into `i + 2`). Steps run in order on load, in memory;
   the upgraded document is written on the next save, after the old file was copied to `.bak`.
   There is a single entry point per store: `StateFile::load` (read) / `StateFile::update` (write).
4. **A newer version than the build knows is never overwritten.** The file is read as far as it is
   understood, the store runs read-only/degraded (writes return a clear error such as
   `state_newer`, "written by a newer version … left untouched"), and a diagnostics line is
   printed. Updating again resumes normal operation; nothing was lost.
5. **Writes are atomic and serialised.** Temp file in the same directory (`O_EXCL`, 0600), `fsync`,
   `rename`, `fsync` of the directory, all under an exclusive cross-process lock
   (`<file>.lock`, `flock`/`LockFileEx`, released if the holder dies). Before replacing a good
   file it is copied to `<file>.bak` (one generation, same permissions). Temp files left by a
   killed writer are swept after an hour.
6. **Damage never bricks the app.** Empty, truncated, zero-byte, non-object, wrongly typed,
   unreadable (EACCES) or failing-migration files are *damaged*. Order of fallback: `.bak` → the
   store's policy (`Corrupt::Reset`: start empty and say so; `Corrupt::Fail`: report an error and
   leave the file). A damaged file is copied to `<file>.corrupt` before the next save replaces it
   and never pushes the good `.bak` out. Loose permissions or a symlink are *not* damage: they are
   a security refusal and stay an error (`state_insecure`).
7. **Identity and trust are protected harder than settings.** The identity key is never regenerated
   while it is intact. Certificate dates are not enforced anywhere (peers pin the certificate byte for byte),
   so a wrong clock can neither stop the service nor be a reason to rotate; new identities are valid from 1970 to 2100.
   If `identity.pem` is missing or its content is damaged it is restored from `identity.pem.bak`
   (same device id, so every pairing stays valid). Only with no usable backup is a new identity
   generated, loudly: the damaged file is kept as `identity.pem.corrupt` and a line says all devices
   must be paired again. An unreadable file is an error, not a reason to replace the key.
8. **Diagnostics.** Anything out of the ordinary (migrated, recovered, reset, newer) becomes one
   line via `bridge_core::statefile::note`, printed to stderr (the service log / journal) and
   kept in `statefile::notes()`.

Implementation: `crates/bridge-core/src/statefile.rs` (generic, backend-agnostic; `PlainIo` for
non-secret single-writer files) and `crates/bridge-kde/src/fsutil.rs` (`PrivateIo`: owner-only
files, symlink/permission checks, locks).

## Desktop state directory

`$XDG_DATA_HOME/geekbridge/service` (default `~/.local/share/geekbridge/service`) on Linux,
`~/Library/Application Support/geekbridge/service` on macOS, `%LOCALAPPDATA%\geekbridge\service` on Windows;
overridden by `--state-dir` / `$GEEKBRIDGE_STATE_DIR`; owner-only (mode 0700 on Unix, a protected ACL on Windows).

| File | Content | Version | Damaged, no backup | Notes |
|---|---|---|---|---|
| `identity.pem` (+`.bak`) | own certificate + P-256 key, PEM | PEM (no JSON); dates never enforced | restored from `.bak`; else new identity (loud, see rule 7) | `.bak` is written at first creation and refreshed whenever a good file is loaded |
| `trusted_peers.json` | paired devices, pinned certificate per device | `version` 1 | trust nobody (fail closed), file kept as `.corrupt` | each entry is validated (fingerprint, CN = device id); one bad entry marks the file damaged |
| `consent.json` | per-device feature opt-ins (`"devices": {id: [feature,…]}`) | `version` 1 | everything off (opt-in) | unknown feature names are kept on rewrite |
| `share.json` | per-device receive / auto-accept | `version` 1 | nothing allowed | per-device unknown fields kept |
| `clipboard-settings.json` | window, hotkey, max image size | no field before; now `version` 1 | defaults | hotkey/tray hotkey settings live here |
| `update-state.json` | updater switch, ETag, last answer, skipped version | no field before; now `version` 1 | defaults (checks on) | written by `bridge-update` through `PlainIo` |
| `whats-new.json` | the version whose "What's new" card was last shown/dismissed (`{"version":1,"last_seen":"X.Y.Z"}`) | `version` 1 | no card, rewritten with the running version | written by Home via `PlainIo`; missing + no paired phone = fresh install (recorded silently), downgrade is silent |
| `device-name` | UTF-8 text, the computer's chosen name | plain text, unversioned | configured name is kept | validated again on read; atomic write |
| `ipc-token` | 64 hex chars, local control token | unversioned | replaced with a new token (clients re-read it), line logged | holds no pairing data |
| `privacy` | `message_text=on|off`, `caller_number=…`, `phone_notification_text=…` lines: what banners and the call card may show | unversioned text | defaults (text hidden, caller number shown); unknown keys and bad lines skipped | atomic write; `sms-notify-content` and the old environment variables still force a value |
| `tray-paused` | JSON list of device ids paused from the tray | unversioned list | treated as empty | atomic write |
| `tray-disabled`, `sms-notify-content` | empty marker files | n/a | n/a | existence is the value |
| `control-token.lock`, `*.lock` | lock files | n/a | n/a | never deleted |

Not persisted on the desktop: calendar write ledger (`calendar_edit.rs`), phone-data caches
(contacts, messages, calendar), call state. They are in memory and rebuilt from the phone after a
restart, so there is nothing to migrate. The Hyprland/Waybar/autostart files are user config that
`geekbridge-ctl hotkey` edits with timestamped backups; they are not state of this program.

## Android

Pairing lives in the transport state directory `filesDir/kde/` (`identity.pem`, `trusted_peers.json`,
written by the same Rust code, so every rule above applies). Settings live in SharedPreferences,
governed by `StateMigrations.kt`: one entry point, called first in `GeekBridgeApp.onCreate`,
keeps `schema_version` (int, prefs file `gb_state`; absent means 1) and runs ordered steps,
saving the version after each (idempotent, crash-resumable). A stored version newer than the build
knows runs no step, sets `readOnly`, rewrites nothing and adds a line to the support report.
JSON blobs go through `BackedBlob`: one `<key>.bak` generation, restored when the main value is
damaged, else empty, never throwing. Only blob writers honour read-only; scalar prefs only gain
keys additively and rely on defaults.

| Store | Location | Policy |
|---|---|---|
| `gb_state` | prefs: `schema_version` | the version marker |
| `geekbridge_state` | prefs, scalars | paired keys are re-derived from the trust store |
| `geekbridge_clipboard` | prefs: `enabled`, `window_ms`, `max_image` | scalar |
| `geekbridge_calls` | prefs: `enabled` (and the on-phone dial confirmation switch) | scalar |
| `geekbridge_phonedata` | prefs: scalars (the sharing switches including `battery` and `notifications`, and the `notification_apps` string set of apps allowed to be mirrored, empty by default), blobs `calendar_ops` (JSON, backed), `sms_requests` (newline list, unbacked) | `calendar_ops` has `.bak` |
| `geekbridge_grants` | prefs: `grants` JSON `{migrated, grants:{id:[features]}}` (per-computer opt-ins migrated from the global ones) | backed; unknown features dropped on read |
| `share_policy` | prefs: `auto_accept:<id>` | scalar |
| `geekbridge_device_name` | prefs: `name` | scalar |
| `updates` | prefs: check state, ETag, cached body | cache, safe to lose |
| `geekbridge_whatsnew` | prefs: `state_version` (1), `last_seen_code` (versionCode of the last card shown) | scalar; damaged or `<= 0` is repaired silently, a newer `state_version` is never rewritten |
| `filesDir/kde/` | identity, trust store | Rust side, rules above |
| `last-crash.txt`, `cacheDir/updates`, spool dir | files | diagnostic / disposable |

Widgets keep no state of their own (the system holds the appwidget ids). Android's own handling
of a corrupt prefs XML file is not covered by tests.

Android tests: `StateMigrationsTest.kt` with golden fixtures in
`android/app/src/test/resources/state-v1/` (fixtures, pairing and per-computer opt-ins, fake v2
step, crash-resume, newer-version no-clobber, corrupt blobs) and
`scripts/e2e/update-install.sh android` (APK upgrade with clipboard sync on, renamed device,
pinned widget, pairing and opt-ins; see `docs/reports/e2e-findings.md`).

## Tests that enforce this

| Layer | Test |
|---|---|
| Generic machinery | `bridge-core` `statefile::tests` (migration order, backup, newer, damage, unknown fields) |
| Golden v1 files | `bridge-kde/tests/state_compat.rs`, `bridge-service/tests/state_compat.rs`, `bridge-update/tests/state_compat.rs` (fixtures in `tests/fixtures/state-v1/`; regenerate only for a deliberate new baseline: `GB_REGEN_FIXTURES=1 cargo test -p bridge-service --test state_compat regenerate -- --ignored`) |
| Fake v2 migration + downgrade | `bridge-kde/tests/state_compat.rs::a_fake_v2_upgrade_then_a_v1_downgrade_loses_nothing` |
| Crash between temp write and rename | `bridge-kde/tests/state_compat.rs` (child process aborted at the `before_rename` failpoint; the `failpoints` cargo feature compiles the hook in, it is absent from release builds) |
| Two writer processes | `bridge-kde/tests/state_compat.rs::two_processes_incrementing_one_file_lose_no_update`, `bridge-kde/tests/persistence.rs` |
| Update flow | `bridge-service/tests/state_compat.rs::pairing_and_settings_survive_an_upgrade_with_a_live_phone`: starts the service on a v1 state dir, reconnects the fixture phone, expects trust, consent and device name kept and no pairing challenge |
| Newer / damaged state, whole service | `files_from_a_newer_version_are_read_but_never_overwritten`, `damaged_state_files_do_not_brick_the_service`, `a_damaged_identity_or_trust_store_is_restored_from_its_backup` |

## Rollback and downgrade

*Downgrade by installing the previous version:* newer-format files are read-only to it (rule 4);
the identity and pairings keep working because those formats have not changed. Updating again
continues where the newer build stopped. For a manual rollback to the state before an upgrade
copy `<file>.bak` over `<file>` while the service is stopped.
