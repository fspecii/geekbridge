# Docs audit 4 (2026-11-03)

Snapshot report. Code is the source of truth; nothing in code was changed. Compared the living docs with the tree after the
day's work (doctor, dial, battery, notification mirroring, goodbye packet, listening-port hint, why-disconnected, screen-off
heartbeat, codepages trim, ICF, onboarding card, Windows/macOS firewall and autostart, redial tie-break, clipboard
reconnect on toggle). `docs/reports/*` (except the index), `docs/research` and `docs/private-notes` were not touched.

## Fixed

| File | Was stale | Now |
|---|---|---|
| docs/service-integration.md | listen-inbound "off by default", "no heartbeat", "notifications not integrated", consent list of 5 features, `adapter_integrated` "false for every feature", "no dial request", 14 packet types, no real phone, Windows fsutil "until it lands", GUI buttons "unavailable until adapters" | default 0.0.0.0:1716 walking to 1764, 15 s announce, `--no-announce`/`--tray` flags, 10 consent features, all adapters integrated, 18 in / 19 out packet types, heartbeat 20 s / 60 s screen-off / 70 s silence, new ops (dial, notifications, debug state, rename, agenda refresh, messages) |
| docs/state-formats.md | desktop state dir `$XDG_STATE_HOME/geekbridge` (wrong; code uses `<data dir>/geekbridge/service`), prefs rows | correct per-OS paths, `control-token.lock`, notification/battery prefs, dial-confirm pref |
| README.md, docs/user-guide.md, docs/packaging-linux.md | `--purge-data` deletes `~/.local/state/geekbridge` | pairings are in `~/.local/share/geekbridge` (updater notes in `~/.local/state/geekbridge`), as `install.sh` does |
| docs/performance.md | service budget 5.8 MB (script: 6.0 MB), CI RSS slack 12 500 kB (script: 8000), "no application-level ping", codepages rule, UI size | match `scripts/perf-check.sh`; heartbeat noted; measured sizes 5 846 328 B service, 1 249 056 B ctl, 7 868 672 B messages |
| docs/kdeconnect-interop.md | extension list lacked debug, goodbye, heartbeat marker, dial | added with heartbeat timings |
| docs/platform-status.md | "CI has not run yet", billing note, firewall/autostart rows | CI green, build-binaries artifacts, Defender/socketfilterfw/autostart parity |
| docs/android-transport.md | "not run on ART/phone", "advertises only ping and share", no reconnect, no screen/permits/set_name actions | status update, three actions documented |
| docs/android-contract.md | data flags `contacts/calendar/sms` only | all seven flags |
| docs/call-control.md | dial "never ran on an emulator", no `CALL_PHONE`, no dial protection | emulator e2e, permission row, rate limit/notice/confirmation section |
| docs/android-power.md | heartbeat 20 s only | 60 s screen-off, skipped announcements, goodbye |
| docs/android-app.md | permission list | `CALL_PHONE`, Notification access |
| docs/clipboard-sync.md, docs/pairing.md, docs/omarchy.md, docs/builds.md | missing reconnect on toggle; trust persisted before accept; waybar classes (`why-*`, `charging`, `battery-low`, `battery`); "workflow not pushed or run" | added / corrected |
| docs/user-guide.md | no first-run card, no disconnect behaviour, no CLI overview, tray menu, doctor on Windows/macOS | added |
| SECURITY.md, CHANGELOG.md | consent list, ports; missing entries for onboarding, doctor parity, dial protection, budget, ICF, tie-break, trust order | added |
| docs/README.md | no entry for translations.md and recent feature reports | added |

`scripts/check-doc-links.sh` passes.

## Checked and already right
Heartbeat constants (`bridge-core/src/heartbeat.rs`: 20 s, 60 s, 70 s), goodbye 5 minutes, announce every 15 s, ports
17380/17381/1716-1764, updater timings (15 s start delay, 24 h, 10 min gap, 5 min private-repo cooldown), `geekbridge-ctl`
command names in all docs, `geekbridge-ctl` and service size budgets, release archive of nine binaries.

## Code looks wrong or inconsistent (reported, not changed)
1. `geekbridge-ctl --help` lists 8 consent features; the command accepts all ten (`calendar_edit`, `calendar_alerts` too).
2. `crates/bridge-android/src/transport/mod.rs` doc comment on `parse_permits` says "the six known features"; `PERMIT_FEATURES` has seven.
3. `scripts/build-desktop.sh` packs 7 binaries (no `geekbridge`, `geekbridge-share`) while `release.yml` and `package-linux.sh` ship 9; the docs say nine for the release archive, which is right for releases only.
4. `docs/android-app.md` "Validation" and `docs/android-power.md` size tables are dated snapshots (192 tests, 9.9 MB debug APK); left as dated history.
5. `docs/architecture.md` is a planning document (proposed modules such as `bridge-store`); left as is.
6. `socketfilterfw` output wording and localized Windows `netsh` output are unverified (from the doctor-parity report).
