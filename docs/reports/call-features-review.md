# Call features: static code review

Scope: keypad DTMF, Recents and the missed-call banner, the call trace, the in-call card, the banner pool,
the call hub, and the Android call control and in-call service.

Method: static read of the files in this worktree. Nothing was compiled or run, and Bash was unavailable
(EDQUOT on /tmp/claude-1000), so there was no `git log -p` or `git diff`.

## Coverage gap: three features were NOT reviewed

This worktree (`hard`) is at 6e61ae7bd. `refs/remotes/origin/main` in the main repository is de262ec23.
The worktree does not contain:

- decline-with-message (the 6e9c971cb range)
- Do Not Disturb (95e96b0d0)
- dialer autocomplete (de262ec23)

There is no DND or decline-message request in `ipc::Request`. `apps/calls.rs` has no suggestion list. There
is no `dnd` module. To review them, rebase or fetch and re-run this review on those diffs.

Checklist for them:

- **Decline-with-message**
  - The text must go through the SMS consent and the `MessagesSend` path (`submission_id`, length limit).
  - The decline must not be marked `declined` in `History` unless it succeeded.
  - Check that the number is never put on a command line.
  - Check that the text is not traced.
- **DND**
  - It must be applied in `announce_new_calls` to the banner, the card and the missed banner.
  - A DND-suppressed ring must still reach Recents.
  - Check whether the state is cleared on service restart or link loss, and whether it is persisted with an
    atomic private write off the async workers.
- **Autocomplete**
  - It must not log queries or send contact data off the control channel.
  - It must cap the number of results and the query length.
  - It must slice only on char boundaries.
  - It must respect the Contacts consent per device.

## Findings, ranked

### HIGH-1. The in-call card closes in the middle of a call when the service is briefly unreachable, and does not come back

Files:

- `crates/bridge-apps/src/call_feed.rs:72-82` (a failed poll sets `status = None`)
- `crates/bridge-apps/src/call_overlay.rs:291-308` and `335-341` (the `None` arm of `Card::update`)
- `crates/bridge-service/src/call_overlay.rs:332-339` (`Supervisor::update`, live branch)

Failure scenario:

1. A call is live and the card shows "IN CALL".
2. One poll fails. The cause can be a service restart, the 8-connection cap in `control.rs` MAX_CONNECTIONS, or
   a 10 s `CLIENT_DEADLINE` timeout while the service is busy (for example the `detect()` stall in MED-4).
3. `FeedState.status` becomes `None`, so `view` is `None` in `Card::update`.
4. That falls into the third arm. `live_since.take()` yields `Some`, so an outcome "Call ended 00:42"
   (`closes = true`) is created.
5. After `ENDED_SHOW` (4 s), `update` returns `Close` and the card process exits.
   - With the 4 s `POLL_DOWN` back-off plus the request time, an outage of about 4 s or more is enough.
   - If the service answers again inside the 4 s, the live arm sees `over = true` and the call timer silently
     restarts at 00:00.
6. The Supervisor only launches a card for a *ring*. For a live call it only writes the trace line "live call
   but no card is running". The call carries on with no card, and Hang up, Mute and Keypad are gone.

Minimal fix:

- In the `None` arm of `Card::update`, if `view.is_none()` (service unreachable) and `live_since.is_some()`,
  return `Keep`. Create no outcome, and give up only after a long grace such as 30 s.
- Optionally, in `Supervisor::update`, launch one card per live call when `live && !running && !launched_for_live`.

### HIGH-2. Recents (numbers and names) outlive consent revocation and unpair

Files:

- `crates/bridge-service/src/call_history.rs:119-122` (`done` is only ever bounded by `KEEP`)
- `crates/bridge-service/src/call_control.rs:646-668` (`set_enabled(false)` clears `report`, `state` and
  `pending`, but not history)

Failure scenario:

1. The user turns off "Telephony" for a phone, or unpairs it, to stop the computer seeing its calls.
2. `set_enabled` documents "Turning it off forgets what the phone reported".
3. `History.done` still holds up to 30 `RecentCall`s with the phone label, number and name. Every authenticated
   `Calls` poll and the Recents tab keep returning them until the service restarts.
4. The "Call back" and "Message" buttons keep working from the revoked device's entries.

Minimal fix:

- Store the source key in `Done` and `Open` (not serialised).
- Add `History::forget_source(prefix)` and call it from `set_enabled(false)`, from unpair, and when a device's
  Telephony consent is revoked in `Daemon::set_consent`.
- Also drop `open` entries of that source.

### MED-1. The stale-phone watchdog turns an unconfirmed ring into a false "Missed call" banner

Files:

- `crates/bridge-service/src/call_control.rs:1344-1354` (the watchdog writes a synthetic Idle report)
- `crates/bridge-service/src/call_control.rs:1375-1377` (sets `STALE_CALL_NOTICE`)
- `crates/bridge-service/src/call_history.rs:108-123` (`source_alive` is still true)

Failure scenario:

1. A ring is reported, and the phone's app is then killed or the phone dozes. The user answers on the phone
   itself.
2. After `ring_quiet` (20 s) plus two unanswered asks (2 x 5 s), the hub applies a synthetic Idle.
3. The link entry still exists, so `live_sources` contains it, and `finish()` yields `Missed` (incoming,
   never answered).
4. The notifier shows "Missed call" with Call back and Message for a call that was probably answered.
5. The module doc promises that a flap does not become a "missed call", but this path does.

Minimal fix: in `tick()`, for each `cleared` device call `inner.history.forget_source(&format!("link:{device}"))`,
the same helper as HIGH-2, so unconfirmed calls are dropped, not recorded.

### MED-2. Keypad tones can overlap on the phone, and the phone has no rate limit of its own

Files:

- `android/app/src/main/java/dev/geekbridge/android/CallControlAndroid.kt:109-124` (`InCallBridge.play`)
- `android/app/src/main/java/dev/geekbridge/android/CallControl.kt:182-227` (`handle`; `RateLimited` at line 90
  is never produced)
- `crates/bridge-service/src/call_control.rs:36-39` (the comment says the phone "applies its own limit as well")

Failure scenario A (garbled digits):

1. The user types "1234#" quickly, or holds a key down.
2. The desktop service accepts 20 tones per 5 s.
3. Each request makes the phone call `playDtmfTone` at once and `stopDtmfTone` 150 ms later on the main
   thread.
4. A second `play` before the first `stop` is overlapped or cut short by the first one's stop. The phone menu
   hears wrong or missing digits.

Failure scenario B (no phone-side limit):

- Nothing in the Kotlin path counts tones. The comment promises a second limit, and `CallFailure.RateLimited`
  is defined but unused. If the Rust core does not enforce one (not verified here), a compromised or buggy
  paired computer is limited only by its own 20/5 s.

Minimal fix:

- Add a single-thread tone queue. `play`, wait `TONE_MS`, `stop`, wait a gap of about 60 ms, then the next.
  Run it on its own executor, not under the `CallControlEngine` monitor.
- Add a sliding-window limiter in `CallRules.handle` for `Tone` that returns `CallFailure.RateLimited`.

### MED-3. Banner text puts the caller on a command line and ignores the "caller number" privacy choice

Files:

- `crates/bridge-service/src/call_control.rs:177-203` (`NotifySendCalls::args`)
- `crates/bridge-service/src/call_control.rs:209-228` (`missed_args`)
- `crates/bridge-service/src/call_control.rs:1519-1527` (the banner gets `caller_label()` unconditionally)

Failure scenario:

- `caller_label()` falls back to the *number*. Banners are built with it regardless of
  `PrivacyKey::CallerNumber` or the `call-overlay` name-only mode.
- With "show caller number: off", the card (`--name-only`) hides the number, but an unnamed caller's number
  still appears in the ring banner and the missed banner.
- Any local user can read it from `/proc/<pid>/cmdline` of `notify-send`. The code comment for the missed
  banner says "never on a command line", which is true only for the callback number, not the label.
- I did not find where the privacy view is applied to the notifier. Verify in the notifier wiring.

Minimal fix:

- When `CallerNumber` is off, use the name only, or "Unknown caller", for banners.
- Pass the text through `notify-send`'s stdin or D-Bus if the command line matters.

### MED-4. `Request::Calls` can block an async worker for seconds

Files:

- `crates/bridge-service/src/daemon.rs:963-970` (called inline, not in `blocking`)
- `crates/bridge-service/src/call_control.rs:833-892` (`sources` -> `bridge_calls::readiness::detect()` at 887-890)
- `crates/bridge-calls/src/readiness.rs:141-181` (spawns `bluetoothctl`: up to 2 s each, 1 + N paired devices,
  with the CACHE mutex held, refreshed every 15 s)

Failure scenario:

1. There are only 2 async workers (`WORKER_THREADS = 2`).
2. On a machine with a Bluetooth adapter and no registered HFP phone, every 15 s one `Calls` poll (each Calls
   window and the card poll once per second) runs `bluetoothctl` synchronously on a worker.
3. With 3 paired devices that is up to about 8 s, during which half of the workers cannot serve the control
   API or the link tasks.
4. This feeds HIGH-1 (client timeouts).

Minimal fix:

- Wrap `self.calls.status()` in `blocking(...)`.
- Better, refresh the readiness cache from the host thread and make `detect()` non-blocking (return the last
  value).

### MED-5. Hub work and blocking I/O run inline on the async link task

Files:

- `crates/bridge-service/src/daemon.rs:2483-2490` (`d.calls.on_packet(...)` inside `trusted_loop`)
- `crates/bridge-service/src/call_control.rs:583-594`, `625-643` and `804` (`link_down`, `link_down_generation`
  and `on_packet` all call `announce_new_calls`)
- `crates/bridge-service/src/call_control.rs:559` (the trace record happens under `inner`) and `503-505`
  (under the host `shared` lock)
- `crates/bridge-service/src/call_trace.rs:87-93` (a file write, and rewrite-by-truncate, under the global
  trace mutex)

Failure scenario:

- A burst of state packets, or a slow or full state-dir disk (NFS or encrypted home), blocks the link's
  async task (and the other tasks sharing the 2 workers) on the trace `write_all`.
- `announce_new_calls` also does `card.kill()` -> `Child::wait()` under the `overlay` mutex.
- None of this uses `spawn_blocking`.

Minimal fix:

- Make `call_trace::record` push to the in-memory ring only, and let one small writer thread flush it (or
  flush on `snapshot`).
- Run `on_packet` and `link_down` through `block_in_place` or `spawn_blocking` for the announce part.

### MED-6. "Declined" is recorded before the phone confirms, so a refused decline suppresses the missed banner

File: `crates/bridge-service/src/call_control.rs:1058-1071` (`History::declined` is called when the request
is only queued, `result.is_ok()` from `send_link`)

Failure scenario:

1. The user clicks Decline.
2. The phone answers `permission_missing` or `failed`, so the call keeps ringing and is then missed.
3. History says `Declined`, and no missed banner appears for a call the user did not actually decline.
4. The converse race also exists: `declined()` only marks an `open` entry that `observe` has already created.
   A Decline click within the same 250 ms host poll as the first sighting is lost, so a declined call is
   later reported as Missed.

Minimal fix:

- Record the intent in the pending list (`pending` has the action).
- Mark `declined` in the RESULT handler on `Ok(())` for `Reject`, or on the next report in which the ring
  disappears while a Reject was pending.

### LOW

1. **`send_link` ignores `enabled`** (`call_control.rs:1233-1286`).
   - A request that passed the view check races `set_enabled(false)` and still reaches the phone.
   - Add `if !phone.enabled { return Err(NotConnected) }`.
2. **`tone_count` is keyed by the row key, which is the constant `link:<dev>|link-current`** (`call_control.rs:1150-1156`).
   - The trace's "tones sent=N" carries over from one call to the next on the same phone.
   - Clear it in `announce_new_calls` when `view.live()` is `None`.
3. **Trace line injection** (`call_trace.rs:28-30` and `64-86`).
   - `detail` is truncated on a char boundary (good) but control characters are not stripped.
   - `outcome()` (`call_control.rs:86-91`) puts host `e.message` text into the trace. That text can contain
     BlueZ object paths with the phone's MAC address (`/org/bluez/hci0/dev_AA_BB_...`) and newlines or tabs
     from a backend error.
   - Map `c.is_control()` to ' ' in `push`, and strip `dev_XX_XX...` patterns.
   - Numbers appear as the last two digits only, by design; names and full numbers do not appear.
4. **The `calls --trace` response can approach the response cap.**
   - `call_trace.rs:24` allows 128 KiB of "line bytes", but the JSON adds roughly 30 to 40 bytes per event.
   - With many short events (`mute=Some(true)`, `link down ...`) the response nears `ipc::MAX_RESPONSE_BYTES`
     (256 KiB, `ipc.rs:46`).
   - Return only the newest N events that fit, or lower `MEMORY_CAP` to 96 KiB.
5. **Banner pool eviction** (`banner_pool.rs:86-94`).
   - `MAX_IN_FLIGHT = 4` is shared by ring and missed banners, and the oldest is evicted.
   - Four missed banners (a flapping caller list) kill a still-visible *ring* banner.
   - Reserve one slot for ring banners, or evict a missed banner first.
   - The key `missed:{caller}` also replaces the earlier missed banner from the same label, for example two
     different "Unknown caller" misses.
6. **Card "Keypad" button spawns a new Calls window per click, and children are not reaped.**
   - `bridge-apps/src/call_overlay.rs:927-934` calls `prefill::launch_with` (`prefill.rs:17-27`).
   - It runs `Command::spawn` on the UI thread, drops the `Child` unreaped (a zombie until the card exits),
     and has no "window already open" check.
   - The banner "Open" actions (`call_control.rs:256` and `282`) behave the same way.
   - Debounce, and use a detached reaper like `desktop_notify::detach`.
7. **Prefill variables are inherited by grandchildren** (`prefill.rs:11-14`).
   - `GEEKBRIDGE_DIAL`, `GEEKBRIDGE_MESSAGE_TO` and `GEEKBRIDGE_KEYPAD` stay in the Calls process environment.
   - Any sibling it launches, for example Messages from Recents, inherits a stale number.
   - Use `Command::env_remove` for the other prefill variables in `launch_with`.
8. **Direction and waiting calls from the link.**
   - `calls_link.rs:243-248`: OffHook is reported as `Active` with `Direction::Unknown`.
   - A call placed from this computer therefore appears in Recents as "Connected", never "Outgoing".
   - There is one `link-current` call per phone, so a *waiting* call that rings over a live one merges into it
     and is never recorded as missed.
9. **The card never launches for calls with no ring.** `Supervisor::update` (service `call_overlay.rs:332-339`)
   starts a card only for rings, so outgoing calls and calls started on the phone have no in-call card.
10. **`call_overlay.rs:239` (service) `std::fs::write` of the status file** is non-atomic and not 0600. It is
    harmless text ("ok" or "cannot: ..."), but `SpawnFailed(e.to_string())` can carry the exe path into the
    trace and the file.
11. **Android `InCallBridge.onChange`** is a no-op until `CallControlRuntime.init` has run
    (`CallControlAndroid.kt:83` and `444`). The first `onCallAdded` of a cold-started process is lost until
    the next state change.
12. **Android tones need the app to be the default phone app** (`AndroidManifest.xml` InCallService).
    - On stock setups `canPlayTones()` is always false, so the link keypad is effectively dead code there.
      This is documented in the code.
    - Only the Bluetooth hands-free path carries tones.
13. **`status()` holds `inner` while calling `audio_note` -> `detect()`** (`call_control.rs:920-926`).
    - Today this is safe because `sources()` has just refreshed the 15 s cache.
    - It becomes a stall if the order or the cache lifetime changes. Compute `audio_note` before taking `inner`.

## Checked and found fine

- No `unwrap`/`expect` on user or phone text in these paths.
  - The only `expect`s are `call_ui.rs:569` and `call_overlay.rs:271`, guarded by the caller.
  - Slicing uses `is_char_boundary` (`call_trace::push`), `find` offsets (`plain_line`), and
    `digits[n.saturating_sub(2)..]` (`call_trace::tail`).
- Integer arithmetic: counters are `u64` with `+= 1`, durations use `saturating_duration_since`, and
  `pending.len() - tones` cannot underflow.
- Keypad tones:
  - They are validated at three layers (`valid_tone` in the UI, in the service, and on the phone).
  - Only `0-9*#` is accepted, one character per request.
  - The tone is never in `LinkRequest`'s Debug, the trace, or the Android `toString`; the trace has only counts.
  - The service rate limit (20 per 5 s) is global, checked against the *current* view, and tones are sent via
    `spawn_blocking`.
  - Pending slots for tones and for Answer/Hang up/Dial are separate.
- Dial-back from Recents, the missed banner and Contacts:
  - The number goes by environment variable, never argv.
  - It is cleaned (`clean_number`, `callback_number`, at most 20 digits).
  - It only fills the keypad or the compose box.
  - The same confirmation sheet gates the dial.
- Link loss: `link_down*` suspends the rings (flap memory) and drops the link, `History` forgets calls whose
  source vanished, and the Android engine `reset()`s on link-down and on consent off.
- Trace file: created 0600 on unix, truncated on start, bounded, and numbers are reduced to two digits with
  per-process salted hashes.
- Windows and macOS gating:
  - `notify-send` failing to spawn is harmless.
  - `NoDisplay` logic is skipped there via `needs_display_variables`.
  - `readiness::detect` is Linux-only (`cfg(all(linux, not(test)))`).
  - `open_private` has a `#[cfg(unix)]` mode branch.
  - `bridge-apps` has Windows and macOS card-window dependencies.
  - I did not find the notifier construction site, so the gating of `NotifySendCalls` itself is unverified.
