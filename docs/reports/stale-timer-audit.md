# Stale-timer audit

Bug class: work scheduled for a device id (or any identity that survives a reconnect) that decides what to act on from the
state it finds when it fires, not from what existed when it was scheduled. The known case was `ShareManager::link_lost`
(see `file-d2p-ci.md`). This audit covered every timer, grace period, TTL, retry, delayed closure and cleanup-on-close site in
`bridge-service`, `bridge-android` (and the timers in `bridge-core`, `bridge-kde`, `bridge-apps`, `bridge-calls*`) and the
Android Kotlin app.

## Result

Three more real hits, each fixed with a token captured at schedule time and covered by a test that failed before the fix:

| # | Site | Failure | Token | Test |
|---|------|---------|-------|------|
| 1 | `bridge-service/src/clipboard.rs` `ImageTicket`, `image_received`, `image_failed` | An image transfer started on a link can finish up to 120 s later. Its result was applied by device id: it wrote the OS clipboard after the phone's sharing was switched off or the phone unpaired, and its failure overwrote the "waiting" status of a pull made on the newer link. | `ImageTicket.generation` (the `Peer` generation at announce time), checked against the live peer, which must also still be sharing | `clipboard::tests::an_image_from_a_session_that_ended_cannot_touch_the_newer_one` |
| 2 | `bridge-android/src/transport/engine/redial.rs` `Book`, `redial_loop`, `spawn_redial_now` (engine.rs) | The per-device redial claim was a bool. A redial task asleep in back-off survived unpair; after a re-pair and a new drop a second task started, and the first task's exit cleared the second one's claim, allowing a third (up to three redial loops for one computer). | `Book::begin` returns a token; `owns(device, token)` is checked after the sleep and goodbye wait, `end(device, token)` only releases its own claim | `redial::tests::a_redial_task_that_outlived_an_unpair_cannot_release_or_steer_the_newer_one` |
| 3 | `android/.../LinkService.kt` `restartLink` / `retryStart`, `ServicePolicy.restartStep` | After a network change the 500 ms retry chain decided from the live controller phase; if another component had already brought the link up on the new network, the late retry stopped it. A retry chain also survived a newer network change. | `restartGen` bumped per chain and captured by each retry; `restartStep(attempt, phase)` lets only attempt 0 stop a running link | `RestartChainTest` (Gradle) |

The `link_lost` fix itself (`a_file_offered_on_a_newer_link_survives_the_old_links_grace_period`) is already in.

Not changed, by design, but worth knowing:
- `Daemon::debug_default` (`daemon.rs`) runs `default_debug_on` in `spawn_blocking`; if the phone is unpaired while it runs (task
  aborted, blocking job not), the consent file gets a Debug entry for an unpaired id. Outcome is the same as a fresh re-pair
  (debug on for a debug-build phone), so it is left.
- `ShareManager::link_lost` fixes its candidate set at link end. A link replaced by `install_ready` is aborted and never calls it, so
  its unfetched outgoing files wait out their 120 s accept timeout rather than 10 s. Not this bug class; nothing newer is harmed.

## Sites examined: bridge-service

| Site | What | Verdict |
|------|------|---------|
| share.rs `link_lost_after` | grace then fail unfetched outgoing files | Fixed earlier: set fixed at link end; test exists |
| share.rs offer expiry spawn (`offer_ttl + 50ms` then `expire()`) | sweeps all offers by their own `created` time | SAFE: per-transfer timestamps, no identity lookup |
| share.rs `lapse_fetches`, `claim_expected`, `drop_expectations`, `forget_device` | fetch TTL (`until`), claim by device+name | SAFE: pull-model timestamps; `drop_expectations` is called from `DataLinkGuard` only when the generation is current |
| share.rs notifier thread (`NotifySend::offered`) reply closure | desktop notification button decides a transfer | SAFE: keyed by unique transfer id; `decide` rejects finished or expired transfers |
| share.rs `run_file` spawned sender, `receive_file` spawned receiver | stream a payload | SAFE: transfer id, cancel token and payload context captured when spawned |
| share.rs `drop_job` | queued jobs failed when the link task ends | SAFE: jobs are from that link's own queue |
| daemon.rs `finish` / `update` | link end bookkeeping | SAFE: both compare `LiveLink.attempt`; a late close cannot remove a newer entry |
| daemon.rs `install_ready` displacement | replaces the entry, aborts the old task after releasing the lock | SAFE: old entry removed by id before the new insert, old task cannot reach the new entry (every write is attempt-checked) |
| daemon.rs `take_down` | abort and await the current task | SAFE (user-initiated, immediate) |
| daemon.rs `ChallengeApprover` / `ChallengeGuard` / challenge `expires` | pairing prompts | SAFE: bound to attempt and connection id, re-checked at decision |
| daemon.rs `run_session` unpaired idle (`timeout_at`) | idle limit for an unpaired link | SAFE: local to the task |
| daemon.rs heartbeat tick in `trusted_loop` | ping and silence | SAFE: owned by the link task |
| daemon.rs `reconnect_tick`, `dial_back_tick`, `Fallback::lost` after `finish` | redial book and dial-back | SAFE: pull model, they re-read live link state each tick; a late `lost` from an old link is cleared by the next tick (`connected` then `settled`) |
| daemon.rs `disconnect` (`pause_for`/`pause` after `take_down`) | manual-disconnect pause | SAFE: cleared by `settled` on the next connect |
| daemon.rs `say_goodbye` 2 s ack, 100 ms drain | local awaits | SAFE |
| daemon.rs `debug_default` | consent default after link up | Residual, see above |
| daemon.rs clipboard image receive spawn (120 s) | payload socket | Fixed (hit 1) |
| daemon.rs `Data/Calls/Clipboard/DebugLinkGuard` | cleanup on link end | SAFE: all generation-guarded (`link_down(id, generation)`); mirror, photos and share expectations are cleared only if the generation was current |
| daemon.rs `dismiss_notification`, `notification_action` | deferred user actions on mirrored notifications | SAFE: re-resolved through `mirror.get(device, id)`, which is cleared when the session ends |
| clipboard.rs `remove_peer` / `link_down_generation`, `PendingPull` deadline, `expected` settle window, `ChangeStream`, watch loop | clipboard bridge | SAFE: generation guard; pulls pruned by deadline and by device on link down; settle window is global text, not per device |
| clipboard.rs `image_received` / `image_failed` | late image result | Fixed (hit 1) |
| call_control.rs `link_down_generation`, `pending` (20 s TTL) | call link state | SAFE: generation guard, pending list replaced by `link_up` |
| call_control.rs `UNNAMED_GRACE` recheck thread | rescans the whole view after 1.6 s | SAFE: idempotent rescan of live rows; `link_down` prunes `unnamed_since` and `announced` |
| call_control.rs ringing notification reply | Answer/Decline from the notification | SAFE: `act` re-validates the target against the current view |
| call_control.rs host thread (Linux HFP), send_host timeout | global backend | SAFE |
| phone_data.rs `link_down(generation)`, send ledger `connection_lost`, `edits.connection_lost` | marks in-flight SMS sends uncertain on loss | SAFE: only for the current generation, so an old link cannot mark a newer link's sends |
| phone_data.rs `tick_alerts`, notifier threads | calendar alerts | SAFE: periodic pull; threads only run `notify-send` |
| phone_debug.rs `link_up`/`link_down(generation)`, log ring | debug hub | SAFE |
| photos.rs / mirror.rs TTLs (30 min, 3 h), mirror banner ids | timestamps checked on read | SAFE: pull model; banner ids carry a generation (`forget_id`) |
| reconnect.rs `Book`, `Fallback` | backoff books | SAFE: pure data, driven by daemon ticks; cleared by `settled` |
| lib.rs `expire_devices`, discovery TTL, tray, memtrim, coexist, update watcher, pairing_flow announce | global periodic work | SAFE: not keyed by link |
| lib.rs / control.rs accept loops (100 ms back-off), `EXCHANGE_DEADLINE` | per-socket | SAFE |
| macos_host.rs, windows_host.rs, desktop_notify.rs, os_notify.rs, private.rs consent store | host helpers and child processes | SAFE: local deadlines |

## Sites examined: bridge-android (Rust), bridge-core, bridge-kde, bridge-apps, bridge-calls*

| Site | Verdict |
|------|---------|
| engine.rs `redial_loop`, `spawn_redial_now`, `Book::active` | Fixed (hit 2) |
| engine.rs `redial_loop` poll after `begin_attempt` | SAFE: exits when a link or attempt for the device exists |
| engine.rs `run_trusted` exit, `remove_conn`, `drop_connection`, `with_conn`, `clear_prompts` | SAFE: keyed by unique connection id; a superseded connection finds no entry and does not redial |
| engine.rs `admit` | SAFE: replaced connection closed and dropped by id |
| engine.rs `peer_goodbye`, `redial::suppress`, `disconnect` | SAFE: device read from the live connection id; a new link or a manual connect clears the window |
| engine.rs `redial_on_nudge` | SAFE: checks live state at the nudge and goes through `begin`/`begin_attempt` |
| engine.rs prompts and deadlines, `run_pending` / `PARK_TTL`, `attempt_failed`, heartbeat, `accept_loop`, handshake timeouts | SAFE: keyed by request, connection or attempt id; local to a task |
| engine/share.rs, clipboard.rs, calls.rs, data.rs, permits.rs | SAFE: connection id or transfer id captured when scheduled; the dial limiter is meant to span reconnects |
| discovery.rs, transport/mod.rs | SAFE: no per-device deferred state |
| bridge-kde keepalive, payload, link, store; bridge-core heartbeat, ipc, report; bridge-update | SAFE: per-socket, per-link or global |
| bridge-apps pollers and workers, bridge-calls, bridge-calls-windows, bridge-calls-macos | SAFE: re-poll live state or hold local deadlines |

## Sites examined: Android Kotlin

| Site | Verdict |
|------|---------|
| LinkService `restartLink`/`retryStart` | Fixed (hit 3) |
| LinkService `scheduleRestart` (debounce), `graceCheck`, `wifiRecheck`, `onDestroy` | SAFE: `removeCallbacks` before re-post; decisions come from current app state, not a link |
| ClipboardRuntime / PhoneDataAndroid / DebugDiagnostics `reconnectToDeclare`, `restartAttempt` | SAFE: only start a link, and only when stopped |
| PhoneDataAndroid SMS receiver cleanup (15 min), battery `later`, calendar retry (`calToken`), incoming-text retries, waiter back-off | SAFE: closure owns its own receivers, or re-checks consent and session flags at fire time and `reset()` clears the token |
| ShareCoordinator `ticker`, `sessionConn` | SAFE: connection id captured at session start; a dead connection fails rather than sending to a new one |
| WidgetUpdater debounce and alarm | SAFE: not keyed by device |
| CallControlAndroid prompts and monitor | SAFE: no delay; ids from the request |
| TransportController callbacks | SAFE: capture an `epoch` at schedule time |
| BackgroundWork / WorkRunner, ClipReader | SAFE: cancel token captured at submit |
| ClipboardFloatingActivity, LinkSection, HomeActivity, PairPanel, QrPairActivity, UpdateSection, ComputerGrants, DebugLogStreamer, ScaleSeedReceiver, GeekBridgeApp, other one-off posts | SAFE: UI-only, lifecycle-bound, no identity |

## Checks

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and
`:app:testDebugUnitTest` exit codes are recorded in the commit/CI run. Each new test was run against the unfixed code and failed.
