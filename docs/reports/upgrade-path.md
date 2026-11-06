# Upgrade path: pairings and settings across versions (2026-11-06)

Dated snapshot, not living documentation. Question: does an update (or a rollback) lose a pairing, a consent or a setting?
Everything ran in sandboxes (private HOME, private loopback ports, scratch state directories, an own emulator and adb
server). The live service, the real state (`target/realdevice-state`), Hyprland, Bluetooth and ports 1716, 17381 and 8000
were not touched.

## Verdict

**No migration bug found, on the computer or on the phone.** No source file needed a fix. What was added: a repeatable
check (`scripts/e2e/upgrade-path.sh`, wired into `check.yml`, 13 s), fixtures written by real old builds
(`tests/fixtures/upgrade/`), and a stand-in phone that pairs with an old service
(`crates/bridge-service/examples/upgrade-phone.rs`). One false alarm in the existing phone harness was fixed (below).

## What was compared

| Role | Build |
| --- | --- |
| Newest | `7b35acff9` (main at the start of this run; release binaries; debug APK `versionCode 20400`) |
| Old A, the first release candidate | tag `v0.1.0-rc.1` (`bb2ff09cc`); service built in a scratch worktree, debug APK `versionCode 10051` built from it |
| Old B, a day-old build | `37af23dba` (round 9, `versionCode 20373`); the binaries and APK of that round were reused. `788543220` was not built separately: it is 15 commits earlier than `37af23dba` and has the same state formats (`respect_dnd`, `call-replies`, `backoff_until` all present) |

rc.1 is the useful one: it predates the `privacy` file (the `sms-notify-content` marker was the only switch), the
`call-replies` file, `respect_dnd`, and the update-state fields `backoff_until`, `last_attempt_unix` and `last_install`,
and it wrote `update-state.json` with mode 0644.

## 1. Computer (service)

Each old build was started on an empty state directory in a sandbox. A stand-in phone (`upgrade-phone`, real
TLS/pairing protocol over loopback through the service's control port) paired, three consents were given
(sms, clipboard, contacts) and six text messages were pushed; then `telephony` consent, `share allow` and `auto-accept`,
`rename`, `tray disable`, `update skip 9.9.9` and `update disable`, and where the build had them the privacy switches
(`message_text on`, `caller_number off`, `respect_dnd on`) and a two-line `call-replies`. The old service was stopped and
the **newest** service started on the same directory.

| Check | rc.1 | 37af23dba |
| --- | --- | --- |
| New service starts, no migration, recovery or damage note in its log, no `.corrupt` file | pass | pass |
| Pairings: `status` lists exactly the devices in `trusted_peers.json` | pass | pass |
| Consents: nothing lost, nothing added | pass | pass |
| Privacy choices read back as stored (`privacy show`) | n/a (marker file only, still honoured: "forced by a file") | pass |
| `update` shows the stored switch; `doctor`, `diagnose`, `status` answer | pass | pass |
| `trusted_peers.json`, `identity.pem`, `consent.json`, `share.json`, `device-name`, `privacy`, `call-replies`, marker files: byte-identical after the new service ran | pass | pass |
| `update-state.json` keeps every stored field (the new build only adds fields) | pass | pass |
| Writes through the new commands (`consent photos on`, `privacy set`, `update enable/disable`) | pass | pass |
| New files (`privacy`, `call-trace.log`, rewritten `update-state.json`) are owner-only, directory 0700 | pass | pass |
| **Rollback**: the old service starts on the state the new one wrote, lists the pairing, logs no damage note | pass | pass |
| Rollback keeps a consent the old build does not know (`photos`), keeps `backoff_until`, `last_attempt_unix`, `last_install` and an invented `future_field` when it rewrites `update-state.json` | pass | pass |
| New service again after the rollback: same pairings | pass | pass |

Observed, harmless: rc.1's `update-state.json` (0644) is rewritten as 0600 by the new build; the new build starts
writing `call-trace.log`; `doctor` on a sandbox reports warnings about clipboard tools and Bluetooth, which is the machine,
not the state.

### Single-file variants (the new service on a copy of the 37af23dba state with one file changed)

`update-state.json`: old shape without any of the newer fields; with `version`, `backoff_until` (year 2100: ignored as a
clock that went forward), `last_attempt_unix`, `checked_for`, `last_install` and an unknown field; `version: 99`
(read-only, left untouched, writes refused or ignored, a clear line in the log); truncated JSON with no backup (started
empty, the damaged file kept); zero bytes with a backup (restored); missing.
`privacy`: without `respect_dnd`; with it and all four keys; CRLF line endings with an unknown key, a bare word and
`message_text=maybe` (bad lines skipped); 300 random bytes (defaults, file left alone); missing.
`call-replies`: three good lines; five lines; one 100-character line; random bytes (built-in replies, file left alone).
`trusted_peers.json` and `consent.json` with `version: 99`: the service starts, lists the pairing, leaves the file untouched.
All 17 variants: service starts, pairing listed, `update` and `privacy show` answer, and the changed file is not rewritten
by merely starting.

### Rolled-up numbers

`scripts/e2e/upgrade-path.sh` with both old builds: **182 checks, 0 failed** (run twice, each old build as the rollback
target; 24 to 39 s with the release build). With committed fixtures only, as CI runs it: 113 checks, 12.9 s with the debug build.

## 2. Phone (Android)

Own AVD (`gbupg`, a copy of `gbnothing`, writable; console 5724, adb server 16124, service ports 17580/17581/17516/1830),
driven by `scripts/e2e/upgrade-preflight.sh phone`: install the old APK, finish the setup wizard, pair for real with a
sandbox service, give consents on both sides, set per-computer opt-ins, auto-accept and the device name (all in
`shared_prefs`), then `adb install -r` the newest APK (what the package installer does).

| Old APK | Result |
| --- | --- |
| `37af23dba` debug (20373) to newest (20400) | 23 pass, 0 fail on the third run. The first run never got past the setup wizard on the freshly cloned emulator (no app involved); the second hit the harness defect below |
| `v0.1.0-rc.1` debug (10051) to newest (20400) | 23 pass, 0 fail |

Checked in both: install succeeds with the same signer (`4d711ab4...`); the app restarts by itself within 8 s
(`MY_PACKAGE_REPLACED`); no `FATAL EXCEPTION`; private files unchanged apart from bookkeeping; `files/kde/trusted_peers.json`
present; `geekbridge_grants` (per-computer opt-ins), `share_policy` (auto-accept) and `geekbridge_device_name` kept;
the computer lists the phone, **reconnects without a code**, consents on the computer are the same, its pin of the phone's
certificate is byte-identical, ping works; reinstalling the same APK works.
**Downgrade is refused cleanly** in both: `INSTALL_FAILED_VERSION_DOWNGRADE: Update version code 20373 (10051) is older than
current 20400`, and an APK signed with another key is refused with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`; after both refusals
the link is still connected with no code and the data is intact.

Preference files, by source diff `v0.1.0-rc.1` to HEAD: the files are the same (`geekbridge_state`, `geekbridge_grants`,
`geekbridge_calls`, `geekbridge_clipboard`, `geekbridge_phonedata`, `geekbridge_whatsnew`, `geekbridge_device_name`,
`share_policy`, `updates`, `gb_state`); **no key read or written by the old code is gone**. New keys are additive and default
off or on as before (`bind_wifi`, `battery`, `notifications`, `photos`, `notification_apps`, `confirm_dial`,
`auto_check`, `last_check_ms`, `last_attempt_ms`, `handed_off`). New grant features (`battery`, `notifications`, `photos`,
`callaudio`) are off for every existing pairing until the person allows them on the phone.

The one file that does change on an upgrade is `geekbridge_whatsnew.xml`: it holds the version the "What's new" card last
showed, and moving it is the point.

## 3. Defect found in the harness, fixed

`upgrade-preflight.sh phone` failed `phone-private-files-intact` once for `geekbridge_whatsnew.xml`: it listed the file as
changed because the restarted app was rewriting it (temp file and rename) at the moment of the second listing, and
because the file is meant to move. The check now ignores that file, looks a second time after 6 s before judging and records
the bookkeeping files that changed as an INFO line. No app change.

## 4. What the check does not cover

- A real phone and real package installer (Samsung screens, Play Protect): see `preflight-upgrade.md`.
- Stores that live only in memory (phone-data caches, messages): nothing persisted, nothing to migrate.
- Windows and macOS state directories (same code, different paths; the script runs on Linux in CI).
- Release (R8) APKs: `run-as` is refused on them; the earlier snapshot in `preflight-upgrade.md` covers rc.1 release by behaviour.
- The Home window's own `whats-new.json` and the hotkey/Waybar user config files.

## Reproduce

    cargo build --release -p bridge-service --bins --example upgrade-phone     # the new build
    # an old build from a scratch worktree: git worktree add --detach DIR v0.1.0-rc.1 && cargo build --release -p bridge-service -p bridge-apps
    scripts/e2e/upgrade-path.sh --bin-dir target/release --phone-example target/release/examples/upgrade-phone \
        --old-bin rc1=DIR/target/release --old-bin r9=OTHER/target/release
    scripts/e2e/upgrade-path.sh                       # CI: fixtures only, against target/debug
    scripts/e2e/upgrade-preflight.sh phone --old OLD.apk --new NEW.apk   # emulator, about 4 minutes
