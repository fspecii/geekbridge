# Correctness review of the new feature code

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Scope: smart clipboard, file sharing, call answering, the updater and the Android lifecycle. Correctness only; security was covered in `docs/reports/security-review.md`.
Reviewed 2026-11-01.

## How to read this

- **Fixed (test run)**: a test was added and the Rust suites for `bridge-service`, `bridge-android` and `bridge-update` pass.
- **Fixed (test written, not run)**: historical; these Kotlin tests have since been run and pass.
- **Open**: not fixed. The file was being edited by another agent at review time (`ShareActivity.kt` and `HomeActivity.kt` had mtime = now), or the fix is a design call.
- For the Rust fixes, the new tests exercise the new API and would not compile against the old code. For two of them (`Hub::wait` replay, scheduler status) the old behaviour is as described, but I did not run a red/green cycle against the pre-fix code.

## Confirmed bugs

| # | Where | Impact | Status |
|---|---|---|---|
| 1 | `crates/bridge-android/src/transport/engine/calls.rs`, `Hub::wait` / `push` | **A stale or already-handled "answer" request can be replayed.** `wait(since)` kept every event with `seq > since`, and nothing removed events once delivered. If the Kotlin waiter thread restarts it passes `since = 0` and gets the old requests again (up to 8, with no age limit). `CallRules.handle` only checks that the phone is ringing now, so an old "answer" could pick up an unrelated later call. | **Fixed (test run).** Delivered events are never handed out twice, and requests expire after 20 s (`REQUEST_TTL`). Tests: `delivered_requests_are_not_replayed_to_a_restarted_waiter`, `requests_expire_if_nobody_collects_them`. |
| 2 | `crates/bridge-service/src/daemon.rs`, `install_ready` plus `ClipboardLinkGuard` / `CallsLinkGuard`; `clipboard.rs` `link_down`; `call_control.rs` `link_down` | **A reconnect silently disables clipboard sync and call control.** `install_ready` spawns the new session before aborting the old task. The old task's guard then called `link_down(device_id)` and removed the new session's entry. That is a common case (phone Wi-Fi flap, duplicate dial). Afterwards pulls fail with `no_enabled_phone_connected`, nothing is pushed, and no call controls are offered, until the next reconnect. | **Fixed (test run).** Each `link_up` gets a generation, and a guard only removes the entry it created: `link_up_generation` / `link_down_generation` in both hubs. Tests: `clipboard::a_displaced_session_does_not_remove_its_successor`, `call_control::a_displaced_session_does_not_remove_its_successor`. |
| 3 | `crates/bridge-service/src/daemon.rs` `trusted_loop`, `share.rs` | **Queued outgoing files leak transfer slots when the link ends.** `begin_send_file` registers a `Waiting` transfer and queues a job. If the link task exits before running it, the entry stays `Waiting` forever. `Waiting` counts towards `MAX_ACTIVE` (3) and `MAX_ACTIVE_PER_DEVICE` (2), so after a few link failures the device cannot send at all. | **Fixed (test run).** `trusted_loop` now closes the command channel and fails every queued job via `ShareManager::drop_job`. Test: `a_queued_job_dropped_with_its_link_frees_the_slot`. |
| 4 | `crates/bridge-android/src/transport/engine.rs` `run_trusted`, `engine/share.rs` | **Same bug on the phone, with a worse effect.** A queued `Ctl::SendFile` is dropped with the connection and its transfer stays `Waiting`. The Kotlin `ShareCoordinator.endIfDone` then never ends the session and never releases `controller.hold()`. `TransportController.needsLink()` stays true, so the link and foreground service are kept alive indefinitely, which is a battery drain. | **Fixed (test run).** `run_trusted` closes `ctl_rx` and calls `Core::drop_queued`, which fails queued files with `link_lost`. Tests: the `Book::finish` unit test, and `transport::sharing::files_queued_behind_a_closing_link_are_failed_not_left_waiting`, which drives `run_trusted` over a real link (queue three files, close, check that every file the computer never saw is `failed`/`link_lost`). It fails with the drain disabled. |
| 5 | `crates/bridge-service/src/share.rs` `MAX_RECEIVED_TEXT` | **Long shared text counts as misbehaviour.** The parser and the phone allow 48 KiB (`ShareLimits.MAX_TEXT_BYTES`), but the desktop stored at most 32 KiB and returned `Rejected` above that. Each such share counted against `MAX_REJECTED_PACKETS`, and enough of them dropped the link. | **Fixed (test run).** The cap is now `kshare::MAX_TEXT_BYTES`. Test: `text_the_phone_may_send_is_not_refused_as_misbehaviour`. |
| 6 | `crates/bridge-update/src/lib.rs` scheduler | **A failed check made the status read "up to date".** After a failed scheduled check the scheduler set `persisted.last_checked_unix` to avoid retrying every tick. `status()` derives `UpToDate` from that field, so a network failure showed UpToDate (with an error string), and the fake timestamp was persisted by the next save. | **Fixed (test run).** The failure time is kept in a separate in-memory `last_attempt_unix`. The scheduler body is now `scheduler_tick` / `check_due`. Test: `a_failed_scheduled_check_does_not_claim_up_to_date`. |
| 7 | `android/.../ClipboardEngine.kt` `onGesture` | **Loop suppression has a gap.** `capture()` ignores text the computer just wrote (`lastApplied`), but a tap that answers a pending pull did not. The phone then sent the computer's own text back as if the user had just copied it. | **Fixed (Kotlin test run).** Test: `aTapAnsweringAPullDoesNotHandBackTextTheComputerJustSent`. |
| 8 | `android/.../Share.kt` `NameRules.sanitize` | **Long names with emoji can be cut in the middle of a surrogate pair.** `dropLast(1)` and the stem trim both work on UTF-16 units. A lone surrogate is not valid UTF-8 and the core rejects the name, so the send fails. | **Fixed (Kotlin test run).** Test: `truncationNeverSplitsASurrogatePair`. |
| 9 | `android/.../ShareActivity.kt` `onCreate` / `onDestroy` | **Rotation leaks a `controller.hold()`.** `holding` is a plain field. On a configuration change `onDestroy` skips the release (`!isChangingConfigurations`), and the new instance starts with `holding = false`, so nothing ever releases that hold. The link then stays wanted for the rest of the process, with no owner. | **Fixed (Kotlin test run).** The hold is a `ShareHold` token that `ShareActivity` returns from `onRetainNonConfigurationInstance`; the next instance reuses it, and `onDestroy` gives it back unless the activity is only being recreated. Tests: `ShareHoldTest`. |

## Problems noticed (second pass)

All Kotlin tests below were run (`gradle --offline :app:testDebugUnitTest`, 215 tests, 0 failures); so were the Rust suites for `bridge-service`, `bridge-android`, `bridge-update` and `bridge-kde`.

| # | Where | Status |
|---|---|---|
| A | `LinkService.kt` `onStartCommand` → `promote()` | **Fixed.** `ServicePolicy.promoteOrStop` catches the refused `startForeground()` and calls `stopSelf()`. Test: `PromoteOrStopTest`. The service wiring itself is not covered (needs Robolectric). |
| B | `ClipboardRuntime.kt` waiter race | **Fixed.** `WaiterSlot`: the waiter gives up its claim in the same locked step in which it decides to leave, so a restart in that window starts a new thread. The claim is also released if the loop throws. Tests: `WaiterSlotTest`. |
| C | `ClipboardEngine.kt` `handlePull` | **Fixed.** A new pull refuses the pending one it replaces, and a pull that lapsed before the tap is refused (`expired`) instead of forgotten. Tests: `aNewPullRefusesTheOneItReplaces...`, `aQuestionThatLapsedBeforeTheTapIsRefused...`. |
| D | `ClipboardEngine.kt` `answer` | **Fixed.** `deliver` returns the real reason. Only reasons the wire knows are sent as `none`; a core that is not running is reported to the user as `not_connected` and no bogus `too_large` goes out. Tests: `aCoreThatIsNotRunning...`, `aCoreSayingTooLargeIsStillAnsweredTooLarge`. |
| E | `AndroidClipboardHost.write` | **Fixed.** `SpoolKeeper` deletes the received image when a text arrives or the engine resets. Tests: `SpoolKeeperTest`, `resetAsksTheHostToDropItsSpooledImage`. Side effect: `reset()` also runs when the link stops, so an image copied from the computer can no longer be pasted after the link drops. |
| F | `ShareCoordinator.kt` sets | **Fixed.** `RecentIds` keeps the newest 512 ids. Tests: `RecentIdsTest`. |
| G | `bridge-kde` `receive_file` leftovers | **Fixed.** `kshare::sweep_partials` removes `.gb-*.part` files idle for 10 minutes and their zero-byte reservation; `ShareManager::new` runs it on the save directory. Test: `sweep_removes_dead_partials_and_their_empty_reservation_only`. |
| H | `offer_file` global `MAX_OFFERED` | **Fixed.** `MAX_OFFERED_PER_DEVICE` = 3. Test: `one_phone_cannot_fill_every_offer_slot` (a phone offering 10 files no longer blocks a second phone). |
| I | `clipboard.rs` `pull` | **Fixed.** Hitting the limit after at least one target was asked returns `Ok`. Test: `hitting_the_pull_limit_part_way_is_not_reported_as_failure`. |
| J | `verified_installer` | **Fixed.** It applies the same "downloaded version equals cached release" rule as `status()`. Test: `an_installer_for_a_superseded_release_is_not_handed_out`. |
| K | `clipboard_wire::parse_item` default `age_ms` | **Not changed: design call.** The phone is the trusted peer and enforces the window. |

## Checked and found correct

- **Foreground service rules.** `connectedDevice` is allowed from BOOT_COMPLETED and MY_PACKAGE_REPLACED on Android 14 to 16 (only dataSync, camera, microphone, mediaPlayback and similar are blocked). The manifest declares `FOREGROUND_SERVICE_CONNECTED_DEVICE` and `CHANGE_WIFI_MULTICAST_STATE`, one of the prerequisite permissions. No timeout applies to this type.
- **Lifecycle plumbing.** `ClipboardFloatingActivity` takes focus before reading the clipboard. The tile uses the `PendingIntent` overload on API 34+. The `askForTap` notification carries no content. `ClipImageProvider` is not exported and grants through the clip.
- **Native call ordering.** `TransportController` runs native calls on a single executor, so the accept-then-Done ordering in `ShareCoordinator` is safe.
- **File receive on the desktop.** The temp file and exclusive reservation are cleaned up on every error path, including disk-full and cancellation. Concurrent same-name transfers do not collide.
- **Version compare.** The Rust and Kotlin semver parsers agree on prefix, build metadata, leading zeros, empty identifiers and pre-release ordering. ETag use is guarded: `If-None-Match` is only sent when a release is cached. No-checksum offers are not pinned behind the ETag.
- **Call answering.** The permission and API-level gates (26 for answer, 28 for end) and the current-state check in `CallRules.handle` are correct once bug 1 is fixed.

## Tests

- Rust: `cargo test -p bridge-service -p bridge-android -p bridge-update -p bridge-kde --locked` passes.
- Kotlin: `source scripts/android-env.sh && cd android && gradle --offline :app:testDebugUnitTest` passes (215 tests). This includes the tests for bugs 7 and 8, which were written earlier without being run.
- Still missing: an integration test for link replacement (bug 2) through `Daemon::install_ready`, and Robolectric or instrumented tests for the activity and service wiring (bug 9's `onRetainNonConfigurationInstance` call, problem A's `onStartCommand`). The logic they rely on is unit-tested, the Android glue is not.
