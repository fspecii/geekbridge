# Security review 10: restart request, crash-safety sweeps, release tooling, Android notice

Scope: the 26 commits after security-review-9 (4731087cb..0af48d0ae). Read: `RestartIfIdle` (8dec5267f), crash safety
(8560244b2, ee0b38b4c: temp and partial-file sweeps, flush-before-rename, QR token clock, call-overlay status file, doctor
state-folder check), `scripts/chaos/shim.c`, `DebugNetHold`, the upgrade-path fixtures, `release-local.sh` and
`check-release-isolation.sh`, the message body clip, the SMS parts limiter, decline-with-message and the Do Not Disturb
probes, and on Android the manifest exports, `SmsReceiverSet`, the SMS observers and the update notice. Tests ran in
temporary directories only; nothing was run against the live service, a real control port or a device.

## Result in one paragraph

No remote or cross-user weakness was found. One hardening fix: the partial-download sweep in the save folder accepted any
`.gb-*.part` name and then looked through links when removing the empty reserved file. It now requires the exact name the
service writes and never looks through a link. The rest is accepted with the residual risks listed below.

## Checked, no change needed

| Area | Finding |
| --- | --- |
| `RestartIfIdle` authorization | Same gate as every control request: the challenge/response MAC over the control token (private file, mode 0600, fresh nonce per request). Another local user can reach the loopback port but cannot compute the MAC, and a captured exchange cannot be replayed. The request cannot come from the phone link or the HTTP API (`Request` is only parsed by `serve_control`). |
| `RestartIfIdle` effect | Refused while any call row exists, while a phone with Calls allowed connected under 10 s ago without a first report, and (without `--force`) while a file moves. Refused (`Unsupported`) unless `systemctl --user show` says this pid is the `MainPID` of `geekbridge-service.service` with `Restart=on-failure` or `always`: a service started from a shell is never made to exit. The exit is an error status, so systemd's start-rate limit (5 in 10 s by default) bounds a loop. Someone holding the token can restart the idle service as often as they like; they can already read messages and place calls, so no new capability. |
| Stale temp sweep, state folder (`sweep_dead_temps`) | Only plain files named `.<name>.tmp.<digits>[.<x>]` whose pid no longer exists (`kill(pid, 0)` gives `ESRCH`; `EPERM` and the own pid count as alive; pid <= 0 and values over `i32::MAX` are never "gone"). `DirEntry::file_type` does not follow links, `remove_file` removes the link itself, so a planted link is skipped and its target is never touched. The folder is created `0700`; writing a lookalike needs the user's own account. New test: a link and a folder with a dead-pid name survive. |
| Stale temp sweep, `atomic_write_private` | Prefix `.<its own file name>.tmp.` and older than an hour, plain `remove_file`. |
| Save-folder `.part` files | The folder is the user's (may be shared). A paired phone cannot plant a `.gb-` name: `sanitize_filename` rewrites a leading dot. See defect 1 for the local-planting case. |
| Flush-before-rename | `receive_file` and `private_file::replace` write the temp file `O_EXCL` (existing names and links are removed first, never written through), flush it, rename over the final name (rename replaces a link instead of following it) and flush the directory. |
| Call-overlay status file | Now `atomic_write_private` in the `0700` state folder (mode 0600, no temp left); test asserts the mode. |
| QR token clock | Monotonic-guarded: time that passes counts when either clock shows it, so a wall clock set back cannot stretch the two-minute life of a one-time token; a clock stepped forward only expires tokens early. Tests cover both. |
| Doctor state-folder check | Prints the state path to the person running `doctor` (as it already does elsewhere). `diagnose`, the redacted support bundle, does not call it. The probe file is `create_new`, named with the pid, and removed. |
| Update CLI and UI | The new request takes only a boolean; the `--force` flag is a CLI argument, not network input. An old service answers `BadRequest` and ctl falls back to the two-step form (documented race, only against services from before the change). Nothing from the network reaches argv. |
| `DebugNetHold` | Lives under `android/app/src/debug/`; referenced only from the debug receiver and debug diagnostics. `check-release-isolation.sh` now fails a release dex that contains the class name. It only holds a cellular network request. |
| `scripts/chaos/shim.c` | Referenced only by `scripts/chaos-crash.sh`, which sets `LD_PRELOAD` for sandboxed test services. Not in `packaging/`, `Makefile`, the release script or any workflow artifact. It changes the clock and fails writes under one directory, and does nothing without its environment variables. |
| Upgrade-path fixtures | `identity.pem` in both fixture directories is a certificate and key made in a sandbox for the fixtures (subjects `5b7e1342...` and `8d8d2866...`); the live service identity is a different one (`ced7fd06...`), and neither fixture id appears anywhere else in the repository. The paired "phone" in `trusted_peers.json` is the stand-in `upgrade-phone` example. The control token and lock files are not committed. The README says plainly that the key is throwaway. |
| `release-local.sh` | The isolation check runs on both APKs (`die` on failure), `unzip` and `strings` are required up front, and a release note still holding `[OWNER` refuses a real release (a warning in a dry run). Quoting of the new lines is safe. |
| Message body clip | Stored text is cut to 4 KiB at a character boundary with an ellipsis, before the duplicate comparison. This lowers what a message pins in memory and on disk; the full text stays on the phone. `serde` feature `rc` (shared recipient lists): deserializing makes one copy per value, so input size still bounds memory; it adds no code outside serde. |
| SMS parts limiter | 10 texts and 40 parts a minute per phone, checked before the text is queued; one long text is let through on its own (bounded by the intent's length cap). |
| Decline with message, DND probes | The probes run fixed tools with fixed arguments (`makoctl mode`, `dunstctl is-paused`, `swaync-client --get-dnd`, `busctl --user get-property ... Inhibited`, `gsettings get ...`), null stdin, 500 ms per tool, 600 ms for the chain, 4 KiB of output read, off the call path in a background thread, and "not in Do Not Disturb" if nothing answers. No call data, number or text reaches an argument. The tools are found through `PATH` of the user's own session. |
| Android exports | Exported: launcher activities, share targets, `SendToComputerActivity`, `ClipboardTileService` (`BIND_QUICK_SETTINGS_TILE`), `MirrorListenerService` (`BIND_NOTIFICATION_LISTENER_SERVICE`), `SmsReceivedReceiver` (`BROADCAST_SMS`) and `GeekBridgeInCallService` (`BIND_INCALL_SERVICE`). Those permissions are held only by the system, so no other app can bind or broadcast. The in-call service has no screen, `IN_CALL_SERVICE_UI=false`, and is bound only if the user chose GeekBridge as the phone app. |
| `SmsReceiverSet` and SMS status receivers | Per-send actions carry 96 random bits (`SecureRandom`), `Intent.setPackage`, `FLAG_IMMUTABLE`, and `RECEIVER_NOT_EXPORTED` on Android 13 and later. Below 13 the receiver is registered without that flag, so only the secret action name protects it: a forged status would need to guess 96 bits within the life of one send. Receivers are released on the send failing, on the report and on `releaseAll`. |
| SMS and calendar observers | Registered on the provider URIs while the feature is on and unregistered with it; they carry no data, only trigger a read in the engine. |
| Update notice | One conditional `GET https://api.github.com/repos/<repo>/releases/latest` with `If-None-Match`, at most once a day on unmetered networks and once a week on metered ones, 24 h after a success, backoff after a failure. Headers: `User-Agent: geekbridge-android-updater/<version>`, `Accept`, `Accept-Encoding: identity`, `X-GitHub-Api-Version`. No `Authorization`, no cookie, no device or install identifier. GitHub sees the IP address and the app version, as for any download. The notification channel is off until the user switches it on. |

## Defects fixed

| # | Finding | Fix |
| --- | --- | --- |
| 1 | `sweep_partials` removed any file named `.gb-<anything>.part` whose first field read as the pid of a dead process, and then removed the empty regular file named by the rest of the name. A lookalike such as `.gb-ff-.part` or `.gb-zz-x-y-notes.txt.part` planted in a shared save folder could make the next start delete an empty file of the user's (for example a `.gitkeep`), and the reserved-file check looked through a link. | The name must be exactly `.gb-<hex>-<hex>-<hex>-<non-empty name>.part`; the reserved file is checked with `symlink_metadata`, so only an empty regular file goes and a link is never looked through. Tests: lookalikes, a folder and a link with exact names survive; a reserved name that is a link leaves its target (`the_partial_sweep_touches_exact_names_regular_files_and_no_link_targets`). A similar test pins `sweep_dead_temps` (`the_temp_sweep_neither_follows_links_nor_removes_folders`). |

## Dependencies

`cargo audit` and `cargo deny` are not installed on the review machine, and the CI job `supply-chain` was not run
here. Instead every package in `Cargo.lock` (all targets, including Windows-only ones) was compared with the RustSec
advisory database cloned on 2026-11-06 (its newest commit dated 2026-11-05), using each advisory's `patched` and
`unaffected` ranges. Matches: `paste` 1.0.15 (RUSTSEC-2024-0436), `quick-xml` 0.30.0 (RUSTSEC-2026-0194, -0195),
`ttf-parser` 0.25.1 (RUSTSEC-2026-0192), `zbus_names` and `zvariant` and `zvariant_derive` (RUSTSEC-2026-0370, -0371,
-0372). All are in `deny.toml` `ignore` with reasons and a review date of 2027-02-01 where it is a vulnerability. Two
more matches are withdrawn advisories (`ring` RUSTSEC-2025-0007, `xml-rs` RUSTSEC-2022-0048). Nothing new since
security-review-9; yanked releases were not checked offline (CI runs `yanked = "deny"`).

Changes to dependencies since review 9: `Cargo.lock` is unchanged. The only manifest change is the `serde` feature `rc`
(above). `libc` was already a dependency of four crates (Unix targets); the new code adds one `unsafe` call,
`libc::kill(pid, 0)` in `process_is_gone`, with the pid range checked first and no signal delivered. The other new
`unsafe` is a counting allocator in a test.

## Residual risk

- A ring that arrives in the 300 ms between `RestartIfIdle` deciding and the process exiting is not refused (the decision
  and the hub lock end together); the phone link drops and reconnects, the call itself stays on the phone.
- `RestartIfIdle` runs `systemctl --user show` without a time limit; a hung systemd would hold one blocking-pool thread
  per request. Only a holder of the control token can ask.
- A user-owned empty file can still be removed by a sweep if a dead writer's exact-name partial was left for it; that is
  the intended clean-up of the empty reservation.
- Android below 13: the SMS status receivers rely on the secret action name only.
- The DND probe fails open (rings come through) when no known tool answers; chosen so a missed call is never silent.
- `cargo audit`/`cargo deny` were not run; the comparison above is a stand-in and the CI advisory step stays the
  record.
