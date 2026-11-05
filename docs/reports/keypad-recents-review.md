# Keypad (DTMF) and Recents/missed-call review

Scope: main checkout working tree (local main 982d12456), read-only. Bash was unusable (EDQUOT on /tmp/claude-1000) and there was no grep, so everything was read with the Read tool. Files read in full or in the relevant part: bridge-service call_control.rs, call_history.rs, call_trace.rs, call_cli.rs, desktop_notify.rs; bridge-core calls.rs (first 420 lines), calls_link.rs, calls_view.rs; bridge-apps call_feed.rs, call_ui.rs (lines 1-1150), apps/calls.rs, prefill.rs; bridge-calls session.rs, guard.rs, pipewire.rs; Android CallControl.kt, CallControlAndroid.kt, AndroidManifest.xml.

Note on paths: the files named `crates/bridge-apps/src/calls.rs` and `android/.../GeekBridgeInCallService.kt` do not exist. The Calls window is `crates/bridge-apps/src/apps/calls.rs` (plus `call_ui.rs`, `prefill.rs`). `GeekBridgeInCallService` is defined inside `CallControlAndroid.kt:128`.

NOT read, so not covered: daemon.rs (the `Request::CallTone`/`CallDial`/Unpair handlers), ipc.rs beyond the request definitions, call_overlay.rs, and any native Rust core that serves `transport_calls_*` on the phone (the place a phone-side tone/dial rate limit would live). Items that depend on those are marked VERIFY.

## What is sound (checked)

- Tone character validation is strict on every path I read:
  - Service `call_control.rs:1102-1111`: exactly one char, `link::valid_tone` (`0-9 * #`, ASCII only, so no Arabic-Indic or full-width digits).
  - Wire `calls_link.rs:354-364` `parse_tone`: same rule; tests cover empty, multi-char, A-D, NUL, non-ASCII digits.
  - Kotlin `CallControl.kt:118` `validTone`, applied in `parseWait` (362), `handle` (207) and `InCallBridge.play`.
  - HFP `guard.rs:46-54` allows 1-32 chars of `0-9 A-D # *`, wider than the service, but the service only ever hands it one validated char, so it is unreachable with anything else.
- Tone with no live call is refused at four layers: UI `tone_request` needs `view.tones` (call_ui.rs:75); service `tone_inner` needs the current view to offer `tones` for that source (call_control.rs:1113-1118, NotFound, nothing sent); Kotlin `handle` needs `OffHook` (CallControl.kt:204); HFP `guard::plan` needs an Active call (guard.rs:205).
- Service rate limit (20 per 5 s, call_control.rs:38-39, 1120-1134) is sliding, global, checked after validation and offer check, and the link send has its own bound of `MAX_TONES` pending tones that cannot starve Answer/Hangup/Dial (1254-1268).
- The tone character is never logged or traced; `LinkRequest`, `CallRequest::toString` and `CallReport::toString` hide it.
- Number masking in the CLI is right: `call_cli.rs:29-36` keeps the last two chars; `calls` prints `name: known`, never the name; recents print masked numbers; `--reveal` is explicit; tests at 531-604. Trace uses `tail()` (last two digits) and per-process salted hashes.
- Call back goes through a confirmation: Recents (apps/calls.rs:399-409) sets `confirming`, which opens the "Call +N?" sheet (confirm_dial, 706); only the sheet's Call button sends `CallDial`. When dialing is not offered it only fills the keypad. The missed-call banner "Call back" only exports `GEEKBRIDGE_DIAL`; `prefill::clean_number` keeps `0-9 + ( ) - .` and space, max 32 chars, so it only pre-fills the field. The number goes in an env var, not argv.
- Missed banner fires once per ring in the normal case: `History::observe` returns a missed call only on the observation where the open entry disappears, it runs under the `announcing` lock (call_control.rs:1397), and the entry is then moved to `done`.
- Recents memory is bounded: `done` capped at `KEEP = 30` (call_history.rs:19, 119-121), `open` only holds calls currently in the view (at most 8 per source) and is dropped when a source disappears. Sort is newest first by finish time (`recent()` iterates `done` in reverse, call_history.rs:164-173); live rows are ringing-first via a stable sort (calls_view.rs:301).
- InCallService declaration (AndroidManifest.xml:164-175) is the standard shape: `exported=true` guarded by `android.permission.BIND_INCALL_SERVICE` (signature/privileged, so only the system can bind), action `android.telecom.InCallService`, `IN_CALL_SERVICE_UI=false`. The app declares no `ACTION_DIAL` activity and no UI in-call service, so it does not meet the DIALER role requirements: it should not appear as a default-phone-app candidate, trigger a ROLE_DIALER prompt, or change default phone behaviour (confirm on a device, see finding 6).

## Findings, ranked

### 1. (Medium) Link-path keypad is not told when an outgoing/answered call becomes ACTIVE, so `canTones` can stay false for the whole call
- `CallControlAndroid.kt:77-125, 444`; `CallControl.kt:564-573`.
- `InCallBridge.onChange` fires only in `onCallAdded`/`onCallRemoved`/`clear`. `canPlayTones()` is `active() != null`, i.e. a `Call` in `STATE_ACTIVE`. For a call placed from the computer (the main "press 1 for..." case) the `Call` is added in `STATE_CONNECTING`/`DIALING`, `OFFHOOK` is reported by the phone-state callback at dial time, the report goes out with `canTones=false`, and nothing re-reports when the call reaches ACTIVE. `send()` suppresses an unchanged report. The same race exists for answered incoming calls (OFFHOOK vs ACTIVE ordering).
- Scenario: computer dials a bank over the link, bank answers, the computer shows no Keypad button for the entire call.
- Minimal fix: register a `Call.Callback` in `InCallBridge.added` (`onStateChanged` -> `onChange()`), unregister in `removed`/`clear`.

### 2. (Medium) `geekbridge-ctl call tone` with more than about 20 keys fails halfway through
- `call_cli.rs:429-442` vs `call_control.rs:38-39`.
- The CLI paces at 150 ms (about 6.7 keys/s) but the service allows 20 per 5 s (4/s). A 21-32 key string (allowed, limit 32) hits `Busy` around key 21 and the CLI aborts with the error, leaving a PIN or menu path half entered on a live call.
- Minimal fix: pace at 260 ms or more, and on `Busy` wait and retry the same key instead of aborting (or reject more than 20 keys up front).

### 3. (Medium) Timed-out HFP tone commands still run later
- `call_control.rs:1282-1305` (`send_host`, 8 s wait) with `pipewire.rs` method timeout 30 s (`PipeWireConfig::default`, line 79) and the host loop at `call_control.rs:475-482`.
- `HostCommand`s are queued on an unbounded `std_mpsc` channel and executed one at a time on the host thread. If one `SendTones` blocks on the phone's AT reply (up to 30 s), later tones queue behind it; the caller gives up after 8 s with "did not answer in time", but the queued commands still execute afterwards, possibly into a different call, and `poll()` (call state) is stalled meanwhile.
- Minimal fix: add a deadline (`Instant`) to `HostCommand` and drop it in the host loop if expired; for tones, also re-check the offered `tones` view on the host thread before executing.

### 4. (Medium-low) Recents keeps numbers after the phone's Telephony consent is turned off or the phone is unpaired; entries cannot be forgotten per source
- `call_history.rs:32-35, 129-152`; `call_control.rs:640-662` (`set_enabled`), `906-923` (`status`).
- `Done` stores only the rendered `RecentCall` (phone label, number, name) and no source key. `status()` returns `history.recent()` unconditionally. `set_enabled(false)` clears the phone's report/state but not its history; unpair via `link_down` does not either. Recents therefore still shows numbers and names for a phone the user withdrew consent from or removed.
- The other agent is fixing `forget_source`; this is the data-model point that fix needs: store `source: String` (the `link:`/`hfp:` key from `Open.parts`) in `Done`, add `History::forget_source(&str)`, call it from `set_enabled(false)` as well as unpair, and filter `recent()` by sources still enabled. Matching by the `phone` label is wrong (labels can repeat and HFP uses "Bluetooth phone").
- Also leaking after unpair/consent-off but small: `tone_count` (call_control.rs:355) holds the last call row key until the next tone; reset it in `link_down`/`set_enabled(false)`.

### 5. (Medium-low) A stale-cleared ring is recorded as Missed, then can be recorded and bannered a second time
- `call_control.rs:1333-1349, 1365-1372, 1404-1408`; `call_history.rs:103-124`.
- The watchdog turns a silent ringing phone into Idle after about 30 s (`ring_quiet` + 2 probes). The source is still alive, so `observe` finalises the call as Missed and fires a "Missed call" banner. `suspend()` remembers the ring so no second ringing banner appears if the phone later reports Ringing again, but history has no such memory: the later Ringing opens a new entry, and when it ends a second Missed entry and banner appear for the same ring.
- Minimal fix: when `tick` clears a stale call, tell history to hold the finished entry (e.g. `history.defer(parts)`) for `flap_memory`, or reopen the entry if the same parts reappear within `flap_memory`. Alternatively, do not emit a missed banner for stale-cleared calls (`STALE_CALL_NOTICE` already tells the user).

### 6. (Low, VERIFY on device) The manifest comment claims Android never starts the InCallService for non-default apps
- `AndroidManifest.xml:164-166`; `CallControl.kt:112-113, 241-246`.
- With `IN_CALL_SERVICE_UI=false` the service is a non-UI in-call service. I could not confirm from the sources whether Telecom binds non-UI services of non-default apps on every Android version; if it does, `canTones` becomes true on stock phones and the comments ("tones only for the default phone app", "never started") are wrong; if it does not, the link tone path is dead code except on a rooted/default-dialer setup and the keypad is HFP-only. Either way no dialer-candidate risk found.
- Check with `adb shell dumpsys telecom | grep -i -A3 incall` during a call, and `adb shell cmd role get-role-holders android.app.role.DIALER` (should stay the system dialer, and GeekBridge must not be listed under `cmd role` candidates). Fix the comment to match what is observed.

### 7. (Low) Phone-side tone limit promised in comments is not in the Kotlin I read
- `call_control.rs:36-37` ("The phone applies its own limit as well"), `calls_link.rs:270-271` ("so the phone can rate-limit by count"); `CallControl.kt:182-227` (`handle`) has no count/time limit for `Tone`, and `onRequests` (482-494) answers every request. `LinkFailure::RateLimited` exists on the wire.
- VERIFY whether the native core (`transport_calls_*`) limits tones. If not, a paired computer with Telephony consent that ignores the service limit can send unbounded DTMF; each tone also posts a `main.postDelayed` holding the `Call`.
- Minimal fix: in `CallControlEngine.onRequests`, keep a deque of tone timestamps and answer `CallFailure.RateLimited` above, say, 30 per 5 s.

### 8. (Low) Overlapping tones cut each other short
- `CallControlAndroid.kt:109-123`. Each `play` posts `stopDtmfTone` 150 ms later without cancelling the previous stop; a tone played less than 150 ms after the previous one is stopped early by the previous tone's timer.
- Fix: keep one `Runnable`, `removeCallbacks` before posting the next, or stop the previous tone first.

### 9. (Low) UI sends every typed keypad character with no per-frame or queue cap
- `call_ui.rs:293-306` and `call_feed.rs:62, 124` (unbounded `mpsc::channel`, worker handles one request at a time).
- Held-down key repeat or an injected burst (xdotool, IME) produces one `CallTone` per char; beyond 20 per 5 s the service answers Busy and the notice shows "too many keypad tones", but the queue is drained serially (each HFP send can take up to 8 s, finding 3) and the poll waits behind it. A tone queued for a call that has ended is still sent and can reach the next call on the same source because `CallTone` carries only `source`, not the call.
- Minimal fix: cap queued tones in the feed (drop tone requests if more than ~8 are pending) and clear them when `view.tones` goes away.

### 10. (Low) Wrong notice text for a tone refused by the phone for lack of an in-call session
- `call_control.rs:773-785`. Tone `PermissionMissing` falls to `reason.message()` -> "The phone has not allowed GeekBridge to answer or place calls." Add a `(Tone, PermissionMissing)` arm: "The phone cannot play keypad tones now."

### 11. (Low) Dial confirmation in the CLI shows only the masked number
- `call_cli.rs:161`: "Place a call to *******42 with Pixel? [y/N]". The user cannot verify the number they are approving (the GUI sheet shows the full number). Print the full number in the prompt (it is on the user's own terminal and the argument is already on the command line), or require `--yes` for scripts only.

### 12. (Low) Recents classification for link-only outgoing calls
- `call_history.rs:89-99`; `calls_link.rs:243-248`. The link reports an outgoing call as `OffHook` with `CallDirection::Unknown` and phase Active from the moment dialing starts, so it is recorded as `Connected` with a talk time that includes ringing, never `Outgoing`/`OutgoingUnanswered`. Minimal fix: when `CallHub::dial` succeeds, remember "next new call from this source is Outgoing" and apply it as the direction in `observe`.

### 13. (Low, VERIFY) Missed-call banner and the caller-number privacy setting
- `call_control.rs:167-169` (default `missed` is a no-op), `231-258`, `1513-1515`. `NotifySendCalls::missed_args` prints `caller_label()` (name or the full number). The `privacy caller_number` setting (geekbridge-ctl usage text) is applied somewhere I did not read. Check that every `CallNotifier` wrapper overrides `missed` and applies the same redaction as `ringing`. A wrapper that only implements `ringing` silently drops missed banners (trait default), or shows the number when privacy says not to.

### 14. (Info) Trace keeps the keypad length
- `call_control.rs:1089-1097`: `keypad tones sent=N` is written to the trace (memory plus 0600 `call-trace.log`). That reveals PIN length but no digits; it is documented. `tone_count` also accumulates across consecutive calls on the same link phone because the link row key is always `link:<dev>|link-current` (call_control.rs:1119, 1146-1148).

## Suggested order
1, 2, 3 (user-visible keypad failures), then 5 and 4 (coordinate 4 with the `forget_source` work), then 7-9, 13; the rest as cleanups.
