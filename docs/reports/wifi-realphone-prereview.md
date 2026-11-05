# Pre-review of the Wi-Fi call audio real-phone procedure (Fold5, Android 16, Arch/Omarchy PC)

Read-only static review of main 982d12456 (working tree). Bash was unavailable in this session (EDQUOT on
/tmp/claude-1000, so the user's /tmp quota is full), so NOTHING below was executed: no `pw-cat --help`, no `adb`,
no `pipewire --version`. Items marked [VERIFY] need one command on the PC; [GUESS] are reasoned, not measured.

Files read: docs/reports/call-audio-wifi-realphone.md, security-review-8.md, scripts/callaudio-enable.sh,
crates/bridge-service/src/callwifi.rs, callwifi_setup.rs, crates/bridge-callwifi/src/{lib,jitter,codec,tone,helper,selftest}.rs,
crates/bridge-calls/src/audio/endpoint.rs, android/.../callaudio/{CallAudioHelper,HelperSession,CallWifiRuntime,UplinkTest,CallWifiReceiver}.kt.
Not read (names unknown, no directory listing): the other files in scripts/callaudio-helper/ and scripts/callaudio-*.sh,
gate.rs, wire.rs, the phone hub. Findings do not cover them.

## Ranked findings

### HIGH (will break the run, or leaves the phone in a bad state, or can be loud)

1. **Step K cannot show what it claims.** It says: turn Wireless debugging off, then `callwifi setup --status` to see
   whether the helper survived. With Wireless debugging off adb cannot reach the phone, so `--status` finds no device and
   `choose_phone` (callwifi_setup.rs, round 0 with an empty device list) drops into the interactive pairing walk and asks for
   a pairing address and code. It never reports survival. Also `setup --stop` is run after that, with the same problem, so
   a helper that DID survive is left running with no adb path to kill it (reboot, or phone switch off, see 9).
   Correction: see "Corrected K" below (observe survival on the phone's own screen, or toggle Wireless debugging back on
   and reconnect, then `--status`).

2. **adb is not on PATH here; the guided setup finds the repo copy only in one layout.** `find_adb` checks `--adb`, `$ADB`,
   PATH, `ANDROID_HOME`, `~/Android/Sdk`, then `<exe>/../../..` + `.tools/android-sdk/platform-tools/adb`, which is the repo
   root only when `geekbridge-ctl` is run from `target/<profile>/`. If `eval "$(scripts/run-desktop.sh --env)"` puts a copy or
   symlink of the ctl somewhere else (for example `~/.local/bin`), step C fails with "adb not found" ([VERIFY] where the
   ctl on PATH lives: `readlink -f "$(command -v geekbridge-ctl)"`). The procedure's install line uses a relative
   `.tools/...` path, which only works with the repo root as cwd, and `scripts/callaudio-enable.sh` is fine (same
   fallback via $ROOT). Correction: export `ADB` and PATH once, and pass `--adb "$ADB"`.

3. **Installing the debug APK may fail or wipe the app.** The debug build uses the same application id
   (`dev.geekbridge.android`) as the release build. If the Fold5 has a release-signed or CI-signed GeekBridge, `adb install -r`
   fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`; the only fix is `adb uninstall`, which deletes the app's pairing/trust
   data (the PC must pair again, and the Wi-Fi call-audio consent is per paired computer). A lower versionCode fails too.
   Samsung Auto Blocker (One UI 6.1+) and Play Protect can additionally put a prompt on the phone ("scan app" / "blocked")
   that stalls `adb install` until tapped. Correction: before step A, run `$ADB shell dumpsys package dev.geekbridge.android | grep -E 'versionName|versionCode|DEBUGGABLE'`
   and decide on purpose; keep the phone unlocked and watch the screen during install; Auto Blocker off for the test.

4. **Notification permission is not granted by `adb install`, and the failure happens AFTER capture started.** Android 13+
   POST_NOTIFICATIONS is a runtime permission. `CallWifiRuntime.showNotification` ends the session if
   `areNotificationsEnabled()` is false, but the order is: Rust "start" event -> `startCapture` (helper opens the call source and
   streams PCM) -> "indicator" event -> notification check -> `endSession`. So with notifications off, Listen (step I)
   fails with "Notifications for GeekBridge are off" after a brief capture window and the user sees a confusing stop.
   Correction: add `$ADB shell pm grant dev.geekbridge.android android.permission.POST_NOTIFICATIONS` to step A and check
   that channel "Call audio sharing" is not silenced to nothing. [VERIFY] whether the app already asks for it on first
   start (if it does, accept the prompt instead). Code suggestion: check notifications before opening capture.

5. **Test tones are loud for the far party, and two of the three routes may play on the PHONE's own output.**
   - Beeps (`UplinkTest`) and the probe's in-call 1 kHz loop test (`ToneCapture(1000.0)`) are 0.3 of full scale = -10 dBFS,
     hard-gated with no ramp (clicks). On a carrier call that is plenty loud in someone's ear. Suggest 0.1 (-20 dBFS) and
     a 5 ms ramp.
   - `CallAudioHelper.preferredTx`: `if (!t.setPreferredDevice(dev)) lastError = "preferred device refused"` and then it
     returns the `TrackInjector` anyway. If Samsung refuses the preferred device, the tx_voice / tx_music tracks play out
     of the phone's normal route: earpiece or speaker, or Bluetooth/USB headphones attached to the PHONE, at the media
     volume (USAGE_MEDIA for route 3). That is the one path in this design that can put loud sound into headphones, and
     it also reports `outcome: played`, so the result looks like success. It can also produce a false positive: the
     iPhone user hears the beeps acoustically through the Fold5 microphone from the speaker.
     Corrections to the procedure (step H): take every headphone/earbud off the Fold5 and disconnect them; set the call
     volume and media volume to about 30 %; phone NOT on speaker; ask the far person to hold their phone slightly away from
     the ear; read the `uplinkTest` field and look for `refused` / `preferred device refused` before trusting a "2" or "3".
     Code fix: return null when `setPreferredDevice` is false.

6. **No limiter or gain control anywhere on the PC output path.** Chain: phone PCM -> Opus -> jitter -> `Decoder::render` ->
   `pw-cat -p` at the default stream volume 1.0 -> the sink at whatever level it is set to (the user's music level).
   Nothing clamps or fades in. Realistic loud inputs: a full-scale DTMF or carrier announcement, a ring-back burst, a
   far party who is shouting, a HAL that applies AGC to VOICE_DOWNLINK. Decoder garbage is not a worry: a rejected or lost
   frame becomes PLC, then silence (`Out::Idle` -> zeros, `render` fills zeros on decoder failure), and nothing adds DC.
   Corrections to the procedure (step I): lower the headset level first, for example
   `wpctl set-volume @DEFAULT_AUDIO_SINK@ 25%` (or the chosen sink's id), start the first Listen with the headset on the desk,
   then raise it. Code suggestion: `--volume 0.5` on the pw-cat playback and a 100 ms fade-in at stream start.

7. **Aborted run leaves the phone in a bad state, and the doc has no single cleanup.** After any failure between C and K:
   Wireless debugging stays on (on Android 16 it also keeps a long-lived trust: adb authorisation lasts until the
   authorisation timeout, default 7 days), a helper may still be running, and the ongoing, non-dismissible
   notification "Call audio is being shared" can stay behind if the app process was killed while sharing (install -r, crash,
   freezer): notifications survive the process, only force-stop clears them, and the Stop button then starts a fresh
   process. See "Cleanup block" below.

### MEDIUM

8. **Echo guard failure modes** (callwifi.rs).
   a. *Playback backlog defeats the guard timing.* `PwPlayback` writes 20 ms frames into a non-blocking pipe (64 KiB =
      about 2 s of 16 kHz s16). `pw-cat` only drains it at real time once its stream is linked. A sink that has to wake up
      (WirePlumber suspends idle sinks; USB DACs and Bluetooth take 100 ms to over 1 s) lets the worker queue the whole wake-up
      time into the pipe, and because production equals consumption that backlog never shrinks. Result: a constant extra delay,
      and the guard (which keys on the write time) opens the microphone while the speaker is still playing old far-party audio,
      so echo gets through. The delay estimate in step J would show this as "well over 0.3 s on the first call, normal on the
      second". Mitigation in the procedure: wake the sink first (play any sound for 2 s) before Listen. Code fix: drop frames when
      the pipe holds more than about 100 ms (`FIONREAD`), or prime pw-cat with silence and wait until it reads.
   b. *Over-eager guard.* Threshold RMS 330 (about -40 dBFS) plus 300 ms hangover on the decoded output. Line noise, comfort
      noise, music on hold or PLC after loss can sit above that, which holds the microphone closed for the whole call: the
      symptom is "they never hear me unless they stop talking", and `echo_held_pct` near 100. It is half duplex by design
      (no barge-in): in step I expect that the far party cannot interrupt the user on speakers, and do not count it as a bug.
      `callwifi echo off` is the test for it, with a headset on.
   c. *Wrong "headset" guess.* `auto_echo_guard` matches `bluez_output`, `buds`, `hfp`, `headset`, `headphone`. A Bluetooth
      SPEAKER (a `bluez_output.*` node) or a laptop whose analogue node description contains "Headphones" while the speaker port is
      active is treated as a headset: guard off, full echo. The decision is made once at Speak start and not refreshed if the output
      changes. Check the `echo guard` line of `geekbridge-ctl callwifi` says what you expect.
   d. *Fail-open when forced off* is the user's explicit choice (documented), fine.

9. **Helper lifetime and the adb-less kill.** The helper exits when its loopback connection closes
   (`HelperSession.run` ends on IOException, `main` then calls `System.exit(0)`), so switching the Experimental switch off on
   the phone (or the STOP broadcast) ends it without adb. Use that in cleanup. Whether the helper survives Wireless debugging
   being turned off is genuinely unknown: if USB debugging is off, turning Wireless debugging off can stop adbd, and init kills the
   service's process group (cgroup) even for `setsid` children [GUESS]. If the helper dies, the phone shows "Helper lost / not
   connected" and the app's `helperUp` goes false.

10. **Bluetooth headset as the chosen PC device** (not for the agent to touch; for the user's run). Opening a
    `pw-cat -r --media-role Communication` capture on a Bluetooth headset triggers WirePlumber's bluetooth autoswitch from A2DP to
    HFP, which removes the A2DP sink node that the playback `pw-cat --target bluez_output...` is attached to; the playback dies and
    the session ends with `output_closed`. Quality drops to 16 kHz mSBC/8 kHz CVSD mid-call, with a loud pop on some headsets.
    For the first run use a wired/USB headset or the built-in speakers with the guard on.

11. **pw-cat flags vs the installed PipeWire** [VERIFY, cannot run here]. The command is
    `pw-cat {-p|-r} --raw --rate 16000 --channels 1 --format s16 --latency 40ms --media-role Communication --target NODE -`.
    All these options exist in 1.x pw-cat as far as I know (`--raw` is the "RAW mode" switch that makes `-` raw PCM instead of a
    sndfile stream). Run:
    `pipewire --version; pw-cat --version; pw-cat --help | grep -E -- '--(raw|target|latency|media-role|volume|format)'`.
    Risks even if they parse: `--latency 40ms` crackles on some USB/Bluetooth sinks (try 80ms); `--target` with an unknown name
    falls back to the default device on newer PipeWire instead of failing (harmless here, because `endpoint()` already resolves a
    present device); mono into stereo is upmixed by the adapter, if only one ear plays, report it.

12. **Pairing and connection flow on Android 16 / One UI 8.**
    - The pairing dialog needs the phone unlocked and in front; folding or unfolding the Fold5, screen-off, or leaving the
      screen dismisses it and invalidates the code. Pick unfolded, keep it awake (Developer options > Stay awake).
    - The pairing port (dialog) and the connect port (main Wireless debugging screen) differ, and the connect port changes every time
      Wireless debugging is toggled. `connect` failing with "connection refused" almost always means a stale port.
    - First connect per network shows "Allow wireless debugging on this network?" on the phone; tick "Always allow".
    - `adb mdns services` may find nothing on this PC (multicast filtered or the host firewall; Omarchy ships ufw with default
      deny incoming): the walk then asks for typed addresses, so have the numbers from the phone ready. Outbound `pair`/`connect`
      themselves are not affected by ufw. Same subnet, no AP/client isolation.
    - After pairing, `adb devices` can list the same phone twice (an `adb-...-._adb-tls-connect._tcp` entry and `IP:port`).
      `callwifi setup` then asks "Which one (number)?" (pick either); `scripts/callaudio-enable.sh` instead fails with "several
      devices are connected" unless `--serial` is given.
    - A different `adb` version than a running adb server makes the client kill and restart the server ("server version doesn't
      match"), which can drop the user's other adb sessions. Isolate: `export ANDROID_ADB_SERVER_PORT=5038` for this test (keep
      away from 1716/17381/8000; 5037 is the default and 5038 is free by default).

13. **The app process can be frozen or killed in the background.** The OPEN broadcast starts the app (possibly cold) and leaves a
    90 s loopback listener plus a loop thread in a process with no foreground UI. Samsung's "Put unused apps to sleep" /
    Battery > Restricted, and the Android cached-app freezer, can freeze it; the helper then connects at TCP level but no
    HELLO is read ("The helper did not connect in time") or the session stalls mid-call [GUESS]. Mitigation: GeekBridge battery
    "Unrestricted", keep GeekBridge open on the Experimental screen from step B until K, never swipe it away.

14. **`--probe` side effects** (CallAudioHelper.probe). It always runs the voip mix self test (registers a system-wide AudioPolicy
    INJECTOR mix for MIC and VOICE_COMMUNICATION recorders for about 1.5 s, not gated on call state) and, in a call, the 1 kHz
    loop test through the uplink injection track (0.9 s, to the far party). It opens 7 sources of 0.6 s each, so the phone's
    microphone indicator shows during D, and the whole run takes about 8 to 12 s. Do not Ctrl-C it mid-run (the tone can keep
    going until the next write fails). Make sure no Listen/Speak session is live while probing (two captures of a call source
    silence each other). The `adb shell` form in D/G uses single quotes, which is correct: `$(pm path ...)` is evaluated on the phone.

15. **Beep counting is ambiguous if a route is refused.** If route 1 is refused, the far person hears 2 beeps then 3 beeps and may
    answer "2" or "3 and 2". The summary on the phone ("Played: 2 beep(s), 3 beep(s)") must be read to the far person first:
    "you will hear groups; tell me the size of each group".

### LOW

16. **Jitter buffer and CPU** (jitter.rs, codec.rs). Bounds are sound: min 2 / max 6 frames (40 to 120 ms), hard ceiling sheds the
    oldest, 5 empty pops rebuffer, a jump of 48 frames (about 1 s) restarts, depth grows only with measured jitter. After a Wi-Fi
    stall TCP delivers a burst; the buffer keeps the newest 6 frames (an audible skip, not noise). The worker re-syncs its clock
    after a 200 ms stall instead of bursting. CPU is low: pure-Rust Opus at complexity 5, 50 frames/s, one 150 kB decoder per
    session. Possible spikes: `Decoder::new` at session start; building host binaries without `--release` (step A uses
    `--release`, keep it) would make opus-pure much slower. On the phone each frame is JSON + Base64 through JNI (about 100
    calls/s with the uplink poll at 20 ms); a few percent CPU, some warmth, nothing alarming. The uplink adds poll jitter up to
    20 ms plus the hub's 100 ms queue plus the 200 ms track buffer; underruns give silence, not garbage.
17. **Procedure step A:** `git pull` in the main checkout (which has an uncommitted user edit) can refuse or conflict; prefer
    `git pull --ff-only`, and know that `scripts/run-desktop.sh --restart` restarts the live service and drops all links. Do it
    before the call, not during.
18. `pm list packages -U dev.geekbridge.android` is a prefix match; a second package with that prefix (a companion or test app) can
    return the wrong uid. Only affects `--app-uid`, which the `--connect` path does not use.
19. Hard-edged beeps and 1 kHz tone have no fade (clicks). Cosmetic.
20. The STATUS broadcast field `uplinkTest` is clipped to 600 characters (`clean(..., 600)`), enough for three routes
    (about 400). If a refusal message is long, the third route can be cut; read the on-phone summary as well.

## Corrected procedure (replace the matching steps)

**Before A (once per shell).** In the repo root of the checkout you run from:
```
export ADB="$PWD/.tools/android-sdk/platform-tools/adb"
export PATH="$(dirname "$ADB"):$PATH"
export ANDROID_ADB_SERVER_PORT=5038          # keeps this test away from any other adb server
pipewire --version; pw-cat --help | grep -E -- '--(raw|target|latency|media-role|volume|format)'
readlink -f "$(command -v geekbridge-ctl)"   # should be <repo>/target/<profile>/geekbridge-ctl, else always pass --adb "$ADB"
$ADB devices                                 # starts the isolated server
```

**A.** Use `git pull --ff-only` (or work in a clean worktree). Do the restart of the service now, not later.
After the APK is built: check what is on the phone (`$ADB shell dumpsys package dev.geekbridge.android | grep -E 'versionName|versionCode|DEBUGGABLE'`
once connected, see C) and expect `INSTALL_FAILED_UPDATE_INCOMPATIBLE` if it is a release build.

**B.** Also: GeekBridge battery "Unrestricted"; Stay awake on; unfold the Fold5 and keep it unfolded; Auto Blocker off if the install
prompts. Keep the GeekBridge Experimental screen in front.

**C.** Always `geekbridge-ctl callwifi setup --adb "$ADB"`. If the pairing address is not found automatically, type it. After
`Using the phone ...`:
```
$ADB install -r dist/android/geekbridge-debug.apk      # watch the phone for a prompt
$ADB shell pm grant dev.geekbridge.android android.permission.POST_NOTIFICATIONS
geekbridge-ctl callwifi setup --adb "$ADB"
```
(If the installer reports a signature mismatch, stop and decide whether losing the app's pairing is acceptable.)

**D / G.** As written, plus: no Listen active; the microphone indicator will flash; run `--probe` to completion (8 to 12 s).
In G the far person also hears a 1 kHz tone for about a second, and the voip-mix test runs for about 1.5 s.

**H.** Before pressing: remove any headphones from the Fold5; phone not on speaker; call volume about 30 %; far person holds their
phone a little away from the ear. Tell them: "you will hear groups of beeps with a pause between; tell me the size of each group".
Read the on-phone summary and the `uplinkTest` field; ignore any "played" route that mentions `preferred device refused`.

**I.** Before Listen: lower the headset level (`wpctl set-volume @DEFAULT_AUDIO_SINK@ 25%`, or the sink you chose), play any sound for
two seconds so the sink is awake, headset on a desk for the first seconds, then raise. Prefer a wired/USB headset for the first run
(finding 10). If you are on speakers, expect half-duplex (finding 8b). Report `callwifi` output including the `echo guard` line.

**K (replacement).**
1. `geekbridge-ctl callwifi stop`, hang up.
2. While Wireless debugging is still ON: `geekbridge-ctl callwifi setup --adb "$ADB" --status` (note pid).
3. Turn Wireless debugging OFF on the phone. Look at GeekBridge > Settings > Experimental on the phone: "Helper connected." still
   shown = the helper survived (it is only connected through the phone's loopback, so the phone screen is the observer).
   Optionally start a new call and try Listen: if it plays, it survived.
4. To confirm by `ps`: turn Wireless debugging ON again (new connect port), `$ADB connect IP:NEWPORT`, then `--status`.
5. Cleanup block below.

## Cleanup block (run at the end, and after ANY aborted run)

```
geekbridge-ctl callwifi stop                                   # PC side: stops listen/speak
geekbridge-ctl callwifi setup --adb "$ADB" --stop              # kills the helper, sends the STOP broadcast
$ADB shell am force-stop dev.geekbridge.android                # also clears a stuck "being shared" notification
$ADB shell settings put global adb_wifi_enabled 0              # switches Wireless debugging off from the PC (last adb command)
$ADB disconnect; $ADB kill-server
```
With no adb at all: Settings > Experimental on the phone, switch "Call audio over Wi-Fi" off (this ends the helper), tap Stop on
the notification (or Settings > Apps > GeekBridge > Force stop), turn Wireless debugging off, revoke USB debugging authorisations
if you want the trust gone, and reboot if a helper is still listed.

## Suggested code follow-ups (not done, read-only review)

- `CallAudioHelper.preferredTx`: return null when `setPreferredDevice` is false (finding 5).
- Lower beep and loop-test amplitude to 0.1 with a short ramp (5).
- Show the notification before opening capture (4).
- Playback: cap the pipe backlog (about 100 ms), and add a startup fade-in plus `--volume` (6, 8a).
- `callwifi setup --status/--stop` with no device: say "no phone connected" instead of starting the pairing walk (1).
- `auto_echo_guard`: do not treat `bluez_output` alone as a headset (8c).
