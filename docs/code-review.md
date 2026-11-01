# Correctness review of the new feature code

Scope: smart clipboard, file sharing, call answering, the updater and the Android lifecycle. Correctness only; security was covered in `docs/security-review.md`.
Reviewed 2026-11-01.

## How to read this

- **Fixed (test run)**: a test was added and the Rust suites for `bridge-service`, `bridge-android` and `bridge-update` pass.
- **Fixed (test written, not run)**: the fix is in a Kotlin file and the test is added. This machine has no JDK or Gradle, so the Kotlin tests were **not executed**. Run `./gradlew testDebugUnitTest` before relying on them.
- **Open**: not fixed. The file was being edited by another agent at review time (`ShareActivity.kt` and `HomeActivity.kt` had mtime = now), or the fix is a design call.
- For the Rust fixes, the new tests exercise the new API and would not compile against the old code. For two of them (`Hub::wait` replay, scheduler status) the old behaviour is as described, but I did not run a red/green cycle against the pre-fix code.

## Confirmed bugs

| # | Where | Impact | Status |
|---|---|---|---|
| 1 | `crates/bridge-android/src/transport/engine/calls.rs`, `Hub::wait` / `push` | **A stale or already-handled "answer" request can be replayed.** `wait(since)` kept every event with `seq > since`, and nothing removed events once delivered. If the Kotlin waiter thread restarts it passes `since = 0` and gets the old requests again (up to 8, with no age limit). `CallRules.handle` only checks that the phone is ringing now, so an old "answer" could pick up an unrelated later call. | **Fixed (test run).** Delivered events are never handed out twice, and requests expire after 20 s (`REQUEST_TTL`). Tests: `delivered_requests_are_not_replayed_to_a_restarted_waiter`, `requests_expire_if_nobody_collects_them`. |
| 2 | `crates/bridge-service/src/daemon.rs`, `install_ready` plus `ClipboardLinkGuard` / `CallsLinkGuard`; `clipboard.rs` `link_down`; `call_control.rs` `link_down` | **A reconnect silently disables clipboard sync and call control.** `install_ready` spawns the new session before aborting the old task. The old task's guard then called `link_down(device_id)` and removed the new session's entry. That is a common case (phone Wi-Fi flap, duplicate dial). Afterwards pulls fail with `no_enabled_phone_connected`, nothing is pushed, and no call controls are offered, until the next reconnect. | **Fixed (test run).** Each `link_up` gets a generation, and a guard only removes the entry it created: `link_up_generation` / `link_down_generation` in both hubs. Tests: `clipboard::a_displaced_session_does_not_remove_its_successor`, `call_control::a_displaced_session_does_not_remove_its_successor`. |
| 3 | `crates/bridge-service/src/daemon.rs` `trusted_loop`, `share.rs` | **Queued outgoing files leak transfer slots when the link ends.** `begin_send_file` registers a `Waiting` transfer and queues a job. If the link task exits before running it, the entry stays `Waiting` forever. `Waiting` counts towards `MAX_ACTIVE` (3) and `MAX_ACTIVE_PER_DEVICE` (2), so after a few link failures the device cannot send at all. | **Fixed (test run).** `trusted_loop` now closes the command channel and fails every queued job via `ShareManager::drop_job`. Test: `a_queued_job_dropped_with_its_link_frees_the_slot`. |
| 4 | `crates/bridge-android/src/transport/engine.rs` `run_trusted`, `engine/share.rs` | **Same bug on the phone, with a worse effect.** A queued `Ctl::SendFile` is dropped with the connection and its transfer stays `Waiting`. The Kotlin `ShareCoordinator.endIfDone` then never ends the session and never releases `controller.hold()`. `TransportController.needsLink()` stays true, so the link and foreground service are kept alive indefinitely, which is a battery drain. | **Fixed (test run, partly).** `run_trusted` closes `ctl_rx` and calls `Core::drop_queued`, which fails queued files with `link_lost`. The unit test covers the new `Book::finish` primitive. No integration test drives the drain in `run_trusted`. |
| 5 | `crates/bridge-service/src/share.rs` `MAX_RECEIVED_TEXT` | **Long shared text counts as misbehaviour.** The parser and the phone allow 48 KiB (`ShareLimits.MAX_TEXT_BYTES`), but the desktop stored at most 32 KiB and returned `Rejected` above that. Each such share counted against `MAX_REJECTED_PACKETS`, and enough of them dropped the link. | **Fixed (test run).** The cap is now `kshare::MAX_TEXT_BYTES`. Test: `text_the_phone_may_send_is_not_refused_as_misbehaviour`. |
| 6 | `crates/bridge-update/src/lib.rs` scheduler | **A failed check made the status read "up to date".** After a failed scheduled check the scheduler set `persisted.last_checked_unix` to avoid retrying every tick. `status()` derives `UpToDate` from that field, so a network failure showed UpToDate (with an error string), and the fake timestamp was persisted by the next save. | **Fixed (test run).** The failure time is kept in a separate in-memory `last_attempt_unix`. The scheduler body is now `scheduler_tick` / `check_due`. Test: `a_failed_scheduled_check_does_not_claim_up_to_date`. |
| 7 | `android/.../ClipboardEngine.kt` `onGesture` | **Loop suppression has a gap.** `capture()` ignores text the computer just wrote (`lastApplied`), but a tap that answers a pending pull did not. The phone then sent the computer's own text back as if the user had just copied it. | **Fixed (test written, not run).** Test: `aTapAnsweringAPullDoesNotHandBackTextTheComputerJustSent`. |
| 8 | `android/.../Share.kt` `NameRules.sanitize` | **Long names with emoji can be cut in the middle of a surrogate pair.** `dropLast(1)` and the stem trim both work on UTF-16 units. A lone surrogate is not valid UTF-8 and the core rejects the name, so the send fails. | **Fixed (test written, not run).** Test: `truncationNeverSplitsASurrogatePair`. |
| 9 | `android/.../ShareActivity.kt` `onCreate` / `onDestroy` | **Rotation leaks a `controller.hold()`.** `holding` is a plain field. On a configuration change `onDestroy` skips the release (`!isChangingConfigurations`), and the new instance starts with `holding = false`, so nothing ever releases that hold. The link then stays wanted for the rest of the process, with no owner. | **Open.** The file was modified by another agent at review time. Suggested fix: `override fun onRetainNonConfigurationInstance(): Any? = holding`, and read `lastNonConfigurationInstance as? Boolean` in `onCreate`. |

## Problems noticed and not fixed

| # | Where | Impact | Notes |
|---|---|---|---|
| A | `LinkService.kt` `onStartCommand` → `promote()` | On Android 12+ a START_STICKY restart from the background can throw `ForegroundServiceStartNotAllowedException` from `startForeground()`. It is not caught here (the `sync()` caller does catch it), so the process can crash on restart. | Wrap `promote()` in try/catch and `stopSelf()`. Not unit-testable without Robolectric. `LinkService.kt` was quiet, so this could be done next. |
| B | `ClipboardRuntime.kt` `onLinkChanged` / `ensureWaiting` | The `waiter?.isAlive` check races with a waiter that is returning (`!settings.enabled` or `stopped`). Turning the setting off and on quickly can leave no waiter, and clipboard events are never read until the next link change. | Low. Fix: let the waiter hold a generation, or join before restart. |
| C | `ClipboardEngine.kt` `handlePull` | A new pull replaces `pending` without refusing the older one. An unanswered `pending` is never refused when it lapses either. The computer's 90 s timeout is the only cleanup. | Cosmetic. |
| D | `ClipboardEngine.kt` `answer` | Any `deliver()` failure is reported to the computer as `too_large`, including "core not running". | Low. |
| E | `AndroidClipboardHost.write` | The spooled clipboard image stays on disk until the next image arrives. A later text write or `reset()` does not delete it. | Privacy hygiene. One file, so no unbounded growth. |
| F | `ShareCoordinator.kt` | `answered`, `finished` and `handledText` sets grow for the life of the process. | Negligible (a few longs per transfer). |
| G | `crates/bridge-kde/src/share.rs` `receive_file` | If the process dies mid-transfer, the empty reserved file and the `.gb-*.part` file stay in the destination. Nothing sweeps them at startup. | A zero-byte plausible-looking file can remain. A startup sweep of `.gb-*.part` would fix it. |
| H | `crates/bridge-service/src/share.rs` `offer_file` | `MAX_OFFERED` (8) is global, so one paired phone can fill all offer slots and block the others for 120 s. | Low. Make it per device. |
| I | `crates/bridge-service/src/clipboard.rs` `pull` | If `MAX_PENDING_PULLS` is hit part-way through the target loop, it returns `Err("too_many_requests")` even though earlier targets were already sent. | Cosmetic. |
| J | `bridge-update` `verified_installer` | After a newer release is cached, an older downloaded installer is still returned. It is hidden from `status()` but not from the open action. | Low. |
| K | `clipboard_wire::parse_item` | A missing `age_ms` defaults to 0, so the freshness window is enforced by the phone only. | By design, since the phone is the trusted peer. Noted. |

## Checked and found correct

- **Foreground service rules.** `connectedDevice` is allowed from BOOT_COMPLETED and MY_PACKAGE_REPLACED on Android 14 to 16 (only dataSync, camera, microphone, mediaPlayback and similar are blocked). The manifest declares `FOREGROUND_SERVICE_CONNECTED_DEVICE` and `CHANGE_WIFI_MULTICAST_STATE`, one of the prerequisite permissions. No timeout applies to this type.
- **Lifecycle plumbing.** `ClipboardFloatingActivity` takes focus before reading the clipboard. The tile uses the `PendingIntent` overload on API 34+. The `askForTap` notification carries no content. `ClipImageProvider` is not exported and grants through the clip.
- **Native call ordering.** `TransportController` runs native calls on a single executor, so the accept-then-Done ordering in `ShareCoordinator` is safe.
- **File receive on the desktop.** The temp file and exclusive reservation are cleaned up on every error path, including disk-full and cancellation. Concurrent same-name transfers do not collide.
- **Version compare.** The Rust and Kotlin semver parsers agree on prefix, build metadata, leading zeros, empty identifiers and pre-release ordering. ETag use is guarded: `If-None-Match` is only sent when a release is cached. No-checksum offers are not pinned behind the ETag.
- **Call answering.** The permission and API-level gates (26 for answer, 28 for end) and the current-state check in `CallRules.handle` are correct once bug 1 is fixed.

## Tests

- Rust: `cargo test -p bridge-service -p bridge-android -p bridge-update` passes in full after these changes. Other crates were not re-run.
- Kotlin: three tests were added (`ClipboardTest.kt`, `ShareTest.kt`) but could not be run here; there is no JDK or Gradle.
- Missing coverage that would have caught more of the above:
  - An integration test for link replacement (bug 2) through `Daemon::install_ready`.
  - An integration test that a closed connection fails its queued files, on the desktop and the phone.
  - Robolectric or instrumented tests for the Android lifecycle (bugs 9 and A).
