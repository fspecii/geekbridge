# Crash and power-loss safety of the desktop service (2026-11-06)

Dated snapshot, not living documentation. Question: after a `kill -9`, a power cut, a full or read-only disk or a clock
step, does the service start, keep its pairings, leave no debris, and does `doctor` say what to do? Everything ran in
sandboxes (private HOME / XDG / state folder, private loopback ports 20000-32000, a stand-in phone, a private X server for
the call card). The live service, the real state (`target/realdevice-state`), Hyprland, Bluetooth and ports 1716, 17381
and 8000 were not touched.

Repeat it: `scripts/chaos-crash.sh --bin-dir target/release --phone target/debug/examples/chaos-phone --failpoint-bin DIR`
(`DIR` = a debug build made with `--features bridge-kde/failpoints`; 20 rounds per scenario, 80 s on 24 cores). The
installer kill test is a normal unit test (`GB_CHAOS_ROUNDS=30 cargo test -p bridge-update --lib killing_the_installer`).
`scripts/chaos/shim.c` is an `LD_PRELOAD` shim the script builds itself: it steps the wall clock of a running service and
makes `write()` under the state folder fail with `ENOSPC` while a flag file exists.

## Result

Last run, release service of this tree (7 525 008 B, budget 7 560 000 B; `geekbridge-ctl` 1 770 968 B, budget 1 790 000 B):
**1352 checks passed, 0 failed** over 20 rounds of each scenario. After every kill the service started, every pairing
was still listed, every state file parsed (owner-only, no half files), no temporary file of a dead writer was left, and
`doctor` answered.

| Scenario | What was done | Result |
|---|---|---|
| trust, consent, privacy, share, update, name files | a loop of `geekbridge-ctl` writes, `SIGKILL` after 5-900 ms (3-6 of 20 kills landed with a temp file on disk) | files intact, writes work after the crash. **Defect D1** (below) |
| failpoint | abort exactly between "temp file written" and `rename`, for consent, privacy, share, update (two kinds) and device name | the old files are byte-identical, the change works after the restart; the temp file is now removed at start |
| file transfer | 64 MiB from the stand-in phone, `SIGKILL` 0-700 ms after the `.part` file appears (18 of 20 killed mid-file, 2 finished) | no `.part` and no empty file with the real name after the restart; the 2 finished files are byte-exact (SHA-256). **Defect D2** |
| message sync | 3000 messages pushed, `SIGKILL` at 0-400 ms | restart fine, conversation list answers (messages are memory only: nothing on disk to damage) |
| self-update staging | the installer in its own process, `SIGKILL` at a random moment of 30 installs (23 landed before it finished; first install converting `install.sh` files, and update of an already versioned prefix) | every `bin/<name>` still runs, `recover()` leaves no debris, running the update again finishes. No defect |
| ring | real call card on a private X server; ringing and answered calls; `SIGKILL` of the service | restart shows no call and no history, no card within 15 s (10 of 10 ringing, 10 of 10 answered). **Defect D3** |
| disk full | `write()` under the state folder fails with `ENOSPC` while the service runs | the service keeps running, the change is refused ("the name could not be saved"), no file truncated or changed, no temp file left, works again when space returns. **Gap D4** |
| read-only state folder | `chmod 500` while running, and when starting | same as above; a read-only folder at start is accepted (reads work). **Gap D4** |
| clock | wall clock of a running service stepped by -10 y, -1 d, -1 h, -1 min, +1 min, +1 h, +1 d, +10 y, +60 y, twice per round | service alive, state writes, `update status`, `pair-qr` work. A new phone pairs while the clocks differ by at most 30 min and is refused with "clocks out of sync" beyond that (KDE Connect's own rule). **Defect D5** (unit-tested) |

## Defects found and fixed

| # | Defect | Fix and test |
|---|---|---|
| D1 | A writer killed between "create temp file" and `rename` left `.<name>.tmp.<pid>[.<rand>]` in the state folder for good (6 of 20 kills on the old build). The sweep that existed ran only after a later successful write of the same file and only for files older than an hour; files written through `PlainIo` (update state) were never swept. | `fsutil::sweep_dead_temps`, called when the service opens its state folder: removes such a file when the pid in its name no longer exists. Test `temp_files_of_dead_writers_are_swept_and_nothing_else`. 0 left in 20 of 20 after the fix. |
| D2 | A service killed during a download left the `.gb-*.part` file (23-47 MB in the runs, up to a whole file) and the empty file with the real name in the user's save folder; the start-up sweep only took files idle for 10 minutes (12 of 12 on the old build). Also the finished file was renamed into place without `fsync`, so a power cut could leave a long file of zeros under the final name. | `sweep_partials` also takes a partial whose writer pid (in the name) is gone, at once; `receive_file` flushes the file and the folder around the rename. Test `a_partial_whose_writer_is_gone_is_swept_at_once_a_live_ones_is_not`. |
| D3 | A card for an answered call stayed on screen forever when the service was gone for good (killed, crashed and not restarted): it deliberately keeps the last state while the service is unreachable, with no limit. A ringing card closes by itself in 0-3 s. | The in-call card closes after 180 s without an answer from the service (a restart or update is far shorter). Test `a_card_for_a_call_in_progress_closes_when_the_service_stays_gone_for_good`. A service restarted on the same ports ends the card normally (10 of 10). |
| D4 | `doctor` said nothing about a full or read-only state folder; the refusal text is only "the choice could not be saved". | `doctor::state_folder_check` (probe file write): "State folder: ... is full / is not writable ... (the old ones are untouched)", with the fix. Test `state_folder_check_is_silent_when_writable_and_names_a_read_only_folder`; the script asserts the line for both cases. |
| D5 | The pairing QR token's deadline used the wall clock: a clock set back (NTP step, hand-set date, a board waking with a wrong date) kept a "2 minute" single-use token valid for as long as the step was long. | `qrpair::now_ms` never goes back, and counts time the monotonic or the wall clock shows (a forward step expires tokens early, which only asks for a new scan). Tests `the_token_clock_ignores_a_wall_clock_set_back_and_counts_a_jump_forward`, `a_token_does_not_outlive_its_ttl_when_the_wall_clock_goes_back`. |
| D6 | Smaller write-ordering gaps found reading every writer: `call-overlay-status` was written with plain `fs::write` (world-readable mode, not atomic); the call-audio choice, remembered phone and restore files were renamed without a directory `fsync` and the call-audio files used a non-exclusive temp name; the WirePlumber fragment and the Hyprland config rewrite (`hotkey install --apply`, the user's own file) were renamed without flushing the temp file first, so a power cut could leave an empty file under the real name. | `call-overlay-status` through `atomic_write_private`; `private_file::replace` (temp, `fsync`, rename, directory `fsync`) for the call-audio files; `sync_all` before the rename for the fragment and the config. Tests `replace_swaps_the_file_leaves_no_temp_and_keeps_the_old_one_on_failure`, `the_last_outcome_is_kept_for_doctor_and_only_rewritten_on_change` (now also checks mode and no leftover). |

## Not defects (checked)

* Every state file written by the service goes through temp file, `fsync`, `rename`, directory `fsync`, `O_EXCL` for the
  private store (`atomic_write_private`, `write_atomic_plain`); `.bak` is refreshed first. Call replies, privacy, share
  settings, consents, trust, identity, update state share these two paths; `call-replies` has no `geekbridge-ctl`
  writer, so it is covered by the shared path and `a_refused_list_leaves_the_old_one`, not by the kill loop.
* Slow disk (soak 5: 35 s control stall, fsyncs on the service's two async workers). Moved off the workers: the
  clipboard settings file, the device name, update choices (`skip`, enable, channel), the tray's pause list and the
  closing fsync/rename of a received file. `update-state.json` is no longer written under the updater's status lock
  (numbered snapshots, the newest wins). Durability did not change; only where the wait happens. `unpair` finishes even
  when the client gave up after 8 s, waits 20 s (was 2 s) for the link that owns the trust record instead of starting a
  second delete beside it, and a new `Connect` to that phone is refused as busy until it is done (a late delete would
  remove the fresh record). A pairing write that times out on the file lock is retried twice before the accept.
  Tests: `crates/bridge-service/tests/io_stall.rs`, `a_slow_save_does_not_hold_the_status_lock`,
  `receive_file_keeps_the_runtime_alive_while_the_disk_is_slow`, `a_lock_timeout_on_the_pairing_write_is_retried_before_the_accept`.
* `call-trace.log` is emptied at every start by design (call history empty after a restart). Its entries carry
  millisecond wall-clock stamps used for display only; a clock step reorders nothing (entries are appended).
* Update backoff and "last checked" in the future are bounded (`a_last_check_time_in_the_future_does_not_freeze_or_hammer_checks`,
  `backoff_until` capped at `MAX_BACKOFF_SECS`, already unit-tested); the live clock steps above ran with the check disabled
  (no network in the sandbox).
* Transfers are not resumed: the KDE Connect share protocol has no offset, the phone offers the file again. A killed
  receive therefore leaves nothing, and a retry starts from zero.
* A real power cut was not simulated (no way to drop the page cache here); the ordering above (data, then name, then
  directory) is what protects it, and each rename in the writers was checked for it.
* Disk full is simulated at `write()` on the state folder only; a full save folder is covered by the existing
  `disk_full_on_the_receiver_fails_cleanly` chaos test (see `chaos-findings.md` C-9).

## Tools added

`scripts/chaos-crash.sh`, `scripts/chaos/shim.c`, `crates/bridge-service/examples/chaos-phone.rs` (a scriptable stand-in
phone: pairs, then `file`, `messages`, `ring`, `talk`, `hangup` on stdin), the installer kill test in
`crates/bridge-update/src/install/tests.rs`.
