# Code review 3: first real-phone session fixes

Scope: everything changed since HEAD 4bb7309 (tracked diff plus untracked files), reviewed for correctness,
concurrency, resource and lifecycle bugs. Reviewed 2026-11-02. Nothing was committed. The owner's live service (ports
17380/17381) was not touched; all tests use ephemeral ports.

Status words: **FIXED** (failing test written, fix landed), **OPEN** (reported, not changed: design decision, or needs
the phone), **OK** (checked, no defect).

Honesty note on test-first: the two heartbeat tests were run and failed on the old code before the fix. For the
reconnect flap test, the grace check and the streamer backlog test I wrote test and fix in the same step and only saw
them pass; the old behaviour is argued from the code in each entry.

## Findings

### F1. FIXED: PC pings drift to every 40 s instead of every 20 s
`crates/bridge-core/src/heartbeat.rs` `ping_due`, used by `daemon.rs` `trusted_loop` (~1690).
The interval timer fires on its own schedule, but `sent()` is stamped after the write completes. The next tick
therefore saw 20 s minus a few ms and `>= EVERY` was false, so every second tick was skipped. The phone, which cuts a
heartbeating peer after 70 s of silence, then had one lost packet from a false dead-link.
Test: `review3_ticks_that_land_a_little_early_still_ping_every_interval` (failed with 3 of 6 pings).
Fix: 1 s tick slack in `ping_due`.

### F2. FIXED: one manual ping from a stock KDE peer arms the phone's 70 s silence timeout
`heartbeat.rs` `heard`, used by `crates/bridge-android/src/transport/engine.rs` ~1115/1173.
Any single `kdeconnect.ping` (stock KDE Connect's "Send ping" button) set `peer_pings`. A stock peer sends no periodic
pings, so 70 s later the phone closed the link with `ping_timeout` and redialed. Contradicts the module's own claim
that old or foreign peers are never cut off.
Test: `review3_one_manual_ping_does_not_make_a_stock_peer_a_heartbeating_one` (failed).
Fix: a peer counts as heartbeating only after two pings 10 to 50 s apart. The old test that relied on one ping was
updated to send two.

### F3. FIXED: a phone announcing from alternating addresses resets the redial back-off every time
`crates/bridge-service/src/reconnect.rs` `Book::due`.
"The phone moved" reset `tries` and `next_at = now`. If discovery reports the phone from two addresses alternately
(Wi-Fi plus another interface, a changed IP bouncing), every 2 s tick dialed: about 300 dials in 10 minutes, no
back-off, and each dial is a TLS handshake against a possibly unreachable address.
Test: `review3_a_phone_announcing_from_alternating_addresses_is_not_dialed_in_a_storm` (bound: at most 25 dials in
10 minutes; the old logic dials on every tick).
Fix: a move no longer shortens the wait. After the book gave up, a move gives two more tries, still spaced by the
last wait (60 s). Cost: a phone that really moved while in a long back-off waits up to 60 s.

### F4. FIXED: an extended pairing grace could leave the foreground service (and listener) running forever
`LinkService.kt` `scheduleGraceCheck` / `graceCheck`, `AppState.pairingPromptSeen`.
The check is scheduled once from `onStartCommand`. A second pairing prompt while grace is running extends the grace
without calling `sync`, so the check fires at the old end, sees `wantService()` still true and never looks again. With
0 paired computers the service, notification and TCP listener then stay up with nobody to serve.
Test: `PowerAndServiceTest.review3GraceCheckKeepsLookingWhileAnExtendedGraceRuns`.
Fix: `ServicePolicy.graceCheckDelayMs`; the check reschedules itself while grace remains.

### F5. FIXED: debug log backlog re-sent to the PC on every reconnect
`android/app/src/debug/.../DebugLogStreamer.kt` (`cursors` keyed by connection id).
Each new connection started at `ring.firstIndex()`, so every reconnect (now frequent, with auto-redial) re-sent up to
2000 lines / 256 KB, duplicating them in `phone-logs/<id>.log` and rotating the 5 MB file after about 20 reconnects.
Test: `DebugDiagnosticsTest.review3AComputerThatReconnectsDoesNotGetTheBacklogAgain`.
Fix: a per-computer cursor survives reconnects (`setConnections(ids, deviceOfConnection)`); a computer never seen
before still gets the whole buffer.

### F6. OPEN (design): a manual `disconnect` on the PC is undone by the phone's own redial
`daemon.rs:1023` `disconnect` vs `crates/bridge-android/src/transport/engine.rs:1194` `spawn_redial`.
The PC pauses its own redial for that phone, but the phone sees only a TCP close (`peer_closed`), treats it as an
unexpected loss and redials within 2 s. The PC accepts the inbound link, `reconnect_tick` calls `settled` and clears
the pause. `geekbridge-ctl disconnect` therefore does not stay disconnected while the phone is up. Before this change
nothing redialed, so this is a regression in meaning.
Needs a decision: either the PC refuses inbound links from a paused device (the phone then burns its 12 tries and
gives up, but "hammers" for about 10 minutes), or a wire-level "I disconnected you on purpose" packet is added. Not
changed.

### F7. OPEN (needs a real phone): network-change handling still restarts healthy links
`LinkService.kt:147` (`registerDefaultNetworkCallback`) and `restartLink` (175).
The brief said healthy links are no longer restarted on network change; the code still stops and restarts the
transport on any default-network change. With a LAN-only Wi-Fi (no internet) and mobile data on, Android makes
cellular the default network, so the default flips while the LAN link is perfectly healthy and the link is torn down
on each flip. Suggest watching the Wi-Fi/Ethernet transports with `registerNetworkCallback(TRANSPORT_WIFI|ETHERNET)`
instead, or skipping the restart when a trusted connection exists and the Wi-Fi addresses are unchanged.
Related: `restartLink` retries only 6 x 500 ms while the transport is Stopping. If the native stop takes longer, the
transport stays Stopped with the notification up until something else calls `sync`. And `controller.stop(reason)`
is a silent no-op while a share needs the link, so a network change during a transfer is never re-announced.

### F8. OPEN: `declared` is updated even when the reconnect to re-declare does not happen
`PhoneDataAndroid.kt:689` `resync`. `declared = now` runs before `controller.stop(R.string.pd_reconnecting)`, which is
a no-op while a share/call holds the link (`needsLink()`). The computer then keeps the old capability set and no later
`resync` notices, because `declared` already equals `now`. Fix: set `declared` only when the stop took effect (or
re-check after the link ends). Also, if "Stay connected" is off and the link was started by an activity, nothing
restarts it after the stop (`restartAttempt` only works through the service). Not edited: the SMS agent is working in
this file.

### F9. OPEN (verify on device): `WIFI_MODE_FULL_LOW_LATENCY` may not hold the radio awake with the screen off
`WifiLinkLock.kt:26`. Android documents the low-latency mode as honoured only while the app is in the foreground and
the screen is on; `WIFI_MODE_FULL_HIGH_PERF` is non-functional from API 34. The lock's purpose (no power-save resets of
an idle link, mainly with the screen off) may therefore not be met. Check with a locked, idle phone over 10+ minutes
before relying on it; F1 (the 20 s heartbeat) is the effective mitigation. Lock acquire/release symmetry itself is
fine: every exit (`onDestroy`, loss with linger, recheck, unpair via observer) releases, and the OS drops it with the
process.

### F10. OPEN (low): `O_NOFOLLOW` constant is wrong on Linux arm/aarch64
`crates/bridge-service/src/phone_debug.rs:330`. `0o400_000` is `O_NOFOLLOW` on x86 only; on arm/aarch64 it is
`O_LARGEFILE`. The symlink is still refused by the `symlink_metadata` check just before, so the exposure is a TOCTOU
window inside the 0700 private directory. Use `libc::O_NOFOLLOW` (or `rustix`) instead of a hand-written value.

### F11. OPEN (low): `phone-log -f` rotation detection
`geekbridge-ctl.rs:1029`. Rotation is detected by `len < pos`, and the file handle is not re-opened by inode, so a
rotation followed by a new file that grows past the old position within 400 ms (not realistic at 5 MB) is missed.
`from_utf8_lossy` on chunk boundaries can also show a replacement character for a split multibyte char. Cosmetic.

### F12. OPEN (debug builds only): `DebugControlReceiver` is exported
`android/app/src/debug/AndroidManifest.xml`. Any app on the phone can flip the SMS/contacts/calendar/calls switches and
per-computer grants of a debug build (`am broadcast`). Release builds do not contain it. Fine for the emulator and the
owner's phone; do not install a debug build on a phone with untrusted apps, or add a signature-level permission to the
receiver.

### F13. OPEN (low): permission checks in Home render
`HomeActivity.kt:132/233` call `SmsReadinessAndroid.evaluate` (3x `checkSelfPermission` + 3x
`shouldShowRequestPermissionRationale` + `getPermissionInfo`, all binder calls) on every render, twice per render, and
renders run on every controller observer notification. A few ms on the main thread per update; not a defect, but cache
per render if Home ever feels slow.

## Checked, no defect

* **Redial trust and limits.** `reconnect_tick` dials only ids in the pairing store (`store.get` is `Ok(Some)`), only
  from fresh (< 20 s) announcements, never when any link exists (`!linked`), and `connect` itself rejects duplicates
  (`Busy`), a non-LAN endpoint and `MAX_LINKS`. A `TooMany` still burns a try, so a full link table lets a phone give
  up after 12 cycles; harmless. Two paired phones have independent entries. Shutdown aborts the redial task; an
  unpair of an announcing phone is skipped by the store check and its entry is dropped when it goes silent. Backoff
  2, 4, ... 60 s, capped at 12 tries.
* **Cross-dial.** Both sides redial; the phone's `begin_attempt` refuses while a connection or attempt exists and the
  PC's `connect` refuses while a link exists, and the existing `prefer_new` rule picks one winner. No duplicate links
  found in the code paths; the phone's loop also stops when a connection to that device exists.
* **Debug packets on the PC.** Only trusted, negotiated links reach `phone_debug`; consent `debug` gates every
  packet (dropped unread when off); packets with a payload are `Invalid`; token bucket 5/s (burst 10), 200 lines,
  64 KB per packet, 1024 chars per line, strictly increasing `seq` per link; files 0600 in a 0700 dir, rotated at
  5 MB keeping 3, ids restricted to `[A-Za-z0-9_-]` and `DeviceId::parse` in the CLI (no path traversal);
  symlinks refused; control characters (ESC, C1 incl. CSI, newline, NUL) become `?` before the file or terminal.
  Unpaired or unconsented senders cannot reach it. Worst case per phone is about 20 MB of disk. A paired phone that
  declares the debug packets gets consent switched on once automatically; that is documented behaviour.
* **Personal data in the debug stream.** Grep of every `Log.*`, `GbLog.*`, `println`, `printStackTrace` in
  `android/app/src`: app logs carry reasons, counts and phases only (`GbLog` forbids names, numbers, content;
  `caught` logs the class only). The redactor adds email, ip, MAC/IPv6, hex, uuid, number-like and storage-path
  rules and cuts the rest of a line after a content-bearing key. Residual risk is framework or library lines from the
  app's own pid (logcat `--pid` also includes the Android framework's output for this process) and exception
  messages that echo input (`NumberFormatException: For input string: "..."` is not caught by any key). Nothing in the
  repo logs such input today. `ScaleSeedReceiver` logs `$e` but only seeds made-up data.
* **DebugLogStreamer lifecycle.** Ring bounded at 2000 lines / 256 KB, one line capped at 512; the logcat thread is a
  daemon thread and restarts with backoff; `stop()` is never called, so the "cannot restart after stop" shape of
  `OwnLogReader` is unreachable. A RuntimeException thrown by the line callback would end that thread uncaught (debug
  builds only); wrap `onLine` in a try if it ever starts doing more.
* **Foreground-service start failures.** `ForegroundServiceStartNotAllowedException` and the Android 14 type
  `SecurityException` are both `RuntimeException`s and are caught in `LinkService.sync` and `promoteOrStop`;
  `startForeground` is always reached right after `startForegroundService` (stop path included);
  `CHANGE_NETWORK_STATE` and `FOREGROUND_SERVICE_CONNECTED_DEVICE` are declared; the notification channel is
  recreated idempotently. `onTaskRemoved` restarts only a Stopped controller when the service is wanted.
* **Multicast lock.** Held by `PairPanel`/`LinkSection` only while discovery listens and released in `detach`/`onStop`;
  the service holds none.
* **SMS readiness.** `SmsReadinessLogic` is a pure decision table and consistent. `resync` compares the declared set
  and updates it before reconnecting, so a flapping *input* cannot loop (each distinct change causes at most one
  reconnect); the only gap is F8.
* **Clipboard and call changes.** `ClipboardFloatingActivity` gives up after 4 s and removes its callback in
  `onDestroy`; the tile records add/remove; `ClipboardState` is pure; `CallControlEngine` keeps an early number for
  3 s and clears it on idle/reset. No defect found.
* **Keepalive vs stock KDE peers.** The PC never cuts a phone off for silence, and with F2 the phone no longer cuts
  a stock peer that was pinged by hand. Our own ping to a stock peer is only sent when that peer declared
  `kdeconnect.ping`, and stock peers ignore it.

## Verification

* `cargo fmt --all` (ran on the whole workspace; no file outside this review's scope was knowingly changed),
  `cargo clippy -p bridge-core -p bridge-service -p bridge-android --all-targets -- -D warnings`: clean.
* `cargo test -p bridge-core -p bridge-service -p bridge-android`: all pass, including the 3 new Rust tests.
* `./gradlew --offline :app:testDebugUnitTest :app:lintDebug`: BUILD SUCCESSFUL, including the 2 new Kotlin tests.

## Files changed by this review

`crates/bridge-core/src/heartbeat.rs`, `crates/bridge-service/src/reconnect.rs`,
`android/app/src/main/java/dev/geekbridge/android/{ServicePolicy,LinkService}.kt`,
`android/app/src/debug/java/dev/geekbridge/android/{DebugLogStreamer,DebugDiagnostics}.kt`,
`android/app/src/test/.../PowerAndServiceTest.kt`, `android/app/src/testDebug/.../DebugDiagnosticsTest.kt`.
