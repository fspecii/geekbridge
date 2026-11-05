# Call audio over Wi-Fi: hear AND speak, honest routes and the real-phone procedure (Fold5, Android 16)

Date: 2026-11-05. EXPERIMENTAL, off by default, debug phone builds only; the release APK carries none of it
(`scripts/check-release-isolation.sh`). Bluetooth hands-free stays the supported way to use the PC as a headset.
Background: [design](../design/call-audio-over-wifi.md), [feasibility](call-audio-over-wifi-feasibility.md),
[prototype](call-audio-wifi-prototype.md), [OSS survey](oss-call-audio-over-wifi.md), [integration](call-audio-wifi-integration.md).

Tags: **[EMU]** measured on an Android emulator (its voice path is synthetic: it proves plumbing and permissions,
never that a person hears anything); **[HOST]** tested on this PC; **[SOURCE]** read in AOSP or a primary page;
**[GUESS]** reasoned, to be settled on the Fold5 with the procedure below.

## 1. Bottom line

| Direction | What is built | Proven | Open on the Fold5 |
| --- | --- | --- | --- |
| Hear the far party on the PC (downlink) | shell-uid helper reads `VOICE_DOWNLINK` or the call-audio-interception record, Opus over the paired TLS link, jitter buffer, `pw-cat` to the speakers/headset chosen for calls | whole chain [EMU] with a tone standing in for the caller | whether Samsung hands the shell user real call audio. Reports for S22/S23/S25 say silent (survey); the self-test answers it in a minute |
| Speak into the call from the PC mic (uplink) | PC mic (`pw-cat -r` on the chosen microphone) → echo guard → Opus → TLS → phone hub → helper → an uplink route into the call | PC mic → helper → frames written into the platform's call-uplink injection track [EMU] | whether the far party hears it. Nothing on the phone can know; the "Test speaking" beeps and the other person's answer decide |
| Setup | `geekbridge-ctl callwifi setup` (pairing with the code, consent, debug-build check, helper start, waits for ready); phone Settings > Experimental guides the rest | scripted stand-in phone [HOST]; real adb against the emulator [EMU] | Wireless debugging behaviour on One UI 8 (does the helper survive turning it off) |

So: **two-way over Wi-Fi without Bluetooth is possible only if the Fold5's audio HAL declares the telephony TX/RX
devices and lets the shell user use them.** That is a property of the phone, not something software here can work
around without root. The procedure below finds out in about fifteen minutes and needs a second phone to call.

## 2. Uplink routes, one by one

How a carrier call's uplink works: the modem/DSP takes the phone's microphone straight from the audio HAL; Android
apps never sit in that path. The only framework-level door is the HAL's `TELEPHONY_TX` output device ("in-call
music", Qualcomm `incall_music_uplink`), which AOSP exposes through two APIs. Everything else either records, routes
between real devices, or feeds apps that record.

| Route | Who can use it | Evidence | Verdict for a Fold5 on Android 16 |
| --- | --- | --- | --- |
| `AudioManager.getCallUplinkInjectionAudioTrack` (SystemApi, API 33: `USAGE_CALL_ASSISTANT` + call-redirection flag → `TELEPHONY_TX`) | holders of `CALL_AUDIO_INTERCEPTION` (+ `MODIFY_PHONE_STATE` for the TX output). The AOSP Shell manifest lists both (comment: needed for CTS `CallAudioInterceptionTest`), android-13 to 16 and main [SOURCE] | Only in `MODE_IN_CALL` (or call screening/redirect); throws unless `isPstnCallAudioInterceptable()`, which is true only if the HAL declares `TELEPHONY_TX` **and** `TELEPHONY_RX` [SOURCE AudioService/AudioManager]. CTS treats it as optional, so Samsung passes CTS either way. Emulator: true, track handed out, frames written [EMU] | **Best candidate. Unknown.** Route 1 of the beep test. If `pstnInterceptable` is false on the Fold5 this route is closed |
| AudioTrack routed explicitly to the `TELEPHONY_TX` device, voice or media usage | callers with `MODIFY_PHONE_STATE` (shell has it), during a call. AudioPolicyManager then gives the track the `INCALL_MUSIC` output [SOURCE] | Same HAL dependency. gsm2sip/Android-sip-gateway do the same with root via ALSA [SECONDARY] | **Unknown.** Routes 2 (voice) and 3 (media) of the beep test, in case Samsung's policy treats them differently |
| `AudioRecord` with `VOICE_UPLINK`/`VOICE_CALL` | shell (capture permission) | These are capture sources: they read the uplink, they cannot write to it. Useful only as an on-phone hint in the beep test ("loop" ratio) | **Not an injection route.** On many Samsung builds also silent [SECONDARY] |
| AudioPolicy dynamic mix, INJECTOR role (scrcpy #3880/#6439/#7004) | `MODIFY_AUDIO_ROUTING` (shell) | Replaces what *recording apps* hear. scrcpy testers: reached the phone, not the other caller; a Samsung call app kept its own mic [SECONDARY]. Emulator: reaches a `VOICE_COMMUNICATION` recorder [EMU] | **Works for VoIP apps (WhatsApp, Signal, Teams), not for a carrier call.** Kept as `voip_mix`, not used for calls |
| `setCommunicationDevice` / virtual or USB audio device | any app | Picks among existing devices; Android has no API to create a virtual call device. A USB audio class headset does carry calls, but that needs the PC to *be* a USB audio gadget, which x86 PCs cannot (no device controller) | **No** (a Raspberry Pi-class gadget could; out of scope) |
| Bluetooth SCO loopback inside the phone | nobody | There is no in-phone Bluetooth loopback; SCO goes to a real radio peer | **No** (real Bluetooth is the supported path) |
| `InCallService` (default dialer), `CallRedirectionService`, self-managed `ConnectionService` | dialer/redirection roles | Routing, mute and signalling only; a self-managed ConnectionService is for the app's *own* VoIP calls and cannot adopt a carrier call [SOURCE InCallService] | **No audio path.** (A future GeekBridge-to-GeekBridge VoIP call could use it) |
| Samsung Live Translate / Bixby Text Call | Samsung system apps | They put audio into the far party's ear, so Samsung's HAL has *some* injection path; which one is not public [SOURCE Samsung pages] | Encouraging but proves nothing for the shell user |
| Phone Link / Link to Windows, Intel Unison, Sefirah | their apps | Calls need Bluetooth Hands-Free [SOURCE Microsoft support] | Bluetooth, not Wi-Fi |
| KDE Connect, termux-api, sndcpy | apps | No call audio; termux-api records the mic only; sndcpy is playback capture | **No** |
| Speakerphone + PC microphone (acoustic) | anyone | The phone on speaker plays the caller into the room; the PC mic hears it but cannot put your voice into the call (only the phone's own mic can, i.e. you talk to the phone) | Last resort for *hearing* only; not a headset |

Downlink routes are covered in the [feasibility report](call-audio-over-wifi-feasibility.md); in short: shell can open
`VOICE_DOWNLINK` and the interception record, Samsung may hand it silence.

## 3. What was built (this change)

* **Speak end to end.** The phone app stores the uplink route the beep test confirmed (`tx_interception`,
  `tx_voice`, `tx_music`, or `telephony_tx` = first that opens) and the helper opens exactly that route when the PC
  asks to speak. The helper writes 60 ms before starting the track (a cushion against Wi-Fi jitter), then the
  track's own 200 ms buffer absorbs bursts; the phone hub keeps at most 100 ms queued and drops older frames.
* **Receiver report for the uplink**: once a second while speaking the phone sends got / missing / dropped / queued;
  `geekbridge-ctl callwifi` shows them next to the computer's own numbers (frames sent, far-party jitter, late,
  concealed, buffer depth).
* **Echo guard** on the computer (`geekbridge-ctl callwifi echo auto|on|off`): with speakers and no echo
  cancellation the caller hears themselves; the guard sends silence while the far party is heard (RMS above about
  -40 dBFS, 300 ms hangover), i.e. half duplex like a speakerphone. `auto` (default) leaves it off for a headset
  (names like headset/headphone/buds/bluez) or an echo-cancelling microphone (PipeWire's `echo-cancel` source),
  on otherwise; status says which and why. A headset is the recommendation: full duplex, no echo.
* **Test speaking** on the phone (Settings > Experimental, step 3): 1 beep through route 1, 2 through route 2,
  3 through route 3; the person on the other end says how many they heard, the answer picks the route.
* **In-call card**: Speak / Mic off next to Stop while listening (only when the phone offers a route); the Calls
  window card already had Listen, Speak and Stop.
* **`geekbridge-ctl callwifi setup`**: the guided one-time start (below).

## 4. Emulator and sandbox results

See section 7. One real bug was found that way: the app framed the PC's microphone frames with a flag the helper
rejects, so the helper's session ended the moment Speak started (no earlier test had driven Speak through the
Kotlin helper). Fixed, with a JVM regression test over the app's own client.

## 5. Real-phone procedure (do it now, about 15 minutes)

You need: the Fold5 on the same Wi-Fi as the PC, a second phone to call it (the iPhone), and a person on that phone
who agrees to hear a few beeps and your voice (**warn them first: they will hear beeps and a 1 kHz tone, tell them to
hold the phone a little away from the ear**). Use a wired or USB headset on the PC for the first run (a Bluetooth headset
switches profile mid-call and can drop the playback). Before anything else, lower the PC output: 
`wpctl set-volume @DEFAULT_AUDIO_SINK@ 25%` (or the sink you chose), and raise it only once you hear the call.
On the Fold5 take every headphone off, keep the call and media volume near 30 %, phone not on speaker.

**Once per shell (PC, repository root).** Absolute paths, one isolated adb server:

```
export ADB="$PWD/.tools/android-sdk/platform-tools/adb"
export PATH="$(dirname "$ADB"):$PATH"
export ANDROID_ADB_SERVER_PORT=5038          # keeps this test away from any other adb server
pw-cat --help | grep -E -- '--(raw|target|latency|media-role|volume|format)'   # all six must be listed
$ADB devices                                 # starts the isolated server
```

Every `geekbridge-ctl callwifi setup` below is given `--adb "$ADB"`.

**A. Build (PC terminal, repository root).** Do the service restart now, not during the call (it drops all links).

```
git pull --ff-only
cargo build --release --locked -p bridge-apps -p bridge-service
scripts/run-desktop.sh --restart && eval "$(scripts/run-desktop.sh --env)"
scripts/build-android.sh --skip-host-tests            # debug APK with the helper
```

Install caveats (step C does the install): the debug build has the **same application id** as the release build, so
`adb install -r` over a release-signed or CI-signed GeekBridge fails with `INSTALL_FAILED_UPDATE_INCOMPATIBLE` (so does a
lower versionCode). The only fix is `adb uninstall`, which deletes the app's pairing: the PC must pair again and the Wi-Fi
call-audio consent is per paired computer. Decide on purpose; check first with
`$ADB shell dumpsys package dev.geekbridge.android | grep -E 'versionName|versionCode|DEBUGGABLE'`.
Samsung **Auto Blocker** (One UI 6.1+) and Play Protect can put a prompt on the phone that stalls the install: keep the
phone unlocked, watch its screen, and switch Auto Blocker off for the test.

**B. Phone switch.** GeekBridge > Settings > Experimental > *Call audio over Wi-Fi (ADB helper)*: switch on, *Turn on*.
In "3. Which computers" tick this PC. Also: GeekBridge battery "Unrestricted", Developer options > Stay awake on, the Fold5
unfolded and kept unfolded, and the GeekBridge Experimental screen kept in front until K.

**C. Pair and start the helper (PC).** On the phone: Developer options > *Wireless debugging* on, tap *Pair device
with pairing code* and leave the dialog open. Then:

```
geekbridge-ctl callwifi setup --adb "$ADB"
```

It asks to continue, to allow the phone on this PC, then for the pairing address (it usually finds it itself; press
Enter; if not, type the numbers from the phone) and the 6-digit code. Keep the pairing dialog in front: folding the
phone or letting the screen turn off invalidates the code. The pairing port and the connect port differ, and the
connect port changes each time Wireless debugging is toggled ("connection refused" = stale port). Tick "Always allow" on
the first-connect prompt. Once it prints `Using the phone ...` the phone is connected over Wi-Fi; install this build's
APK over that connection (pairings are kept), grant the notification permission (Android 13+ does not grant it on
install; without it Listen fails after capture has already started), then run the setup once more so the helper of the
new APK starts:

```
$ADB install -r "$PWD/dist/android/geekbridge-debug.apk"      # watch the phone for a prompt
$ADB shell pm grant dev.geekbridge.android android.permission.POST_NOTIFICATIONS
geekbridge-ctl callwifi setup --adb "$ADB"
```

If the installer reports a signature mismatch, stop and decide whether losing the app's pairing is acceptable. If
`adb devices` lists the phone twice, `setup` asks "Which one (number)?" (either).

**Look for:** `Helper started` and `[4/4] The phone reports its helper ready (self-test: ...)`; on the phone
"Helper connected." **Report:** the last 6 lines it printed.

**D. Raw facts, no call (PC).** One JSON line, about 10 s, nothing is heard by anyone:

```
$ADB shell 'CLASSPATH=$(pm path dev.geekbridge.android | sed s/^package://) app_process /system/bin dev.geekbridge.android.callaudio.CallAudioHelper --probe'
```

No Listen may be active; the phone's microphone indicator flashes; let it run to the end (8 to 12 s), no Ctrl-C.

**Report the whole line.** The decisive fields: `pstnInterceptable` (true = the HAL declares the call TX/RX devices:
speaking is possible in principle; false = the interception route is closed), `telephonyTxDevice`, and each source's
`outcome`/`rms` (with no call they should be `refused`, `silent` or near 0).

**E. Self-test step 1, no call (phone).** Settings > Experimental > *Step 1: check with no call*. **Look for:**
"Good: with no call the call sources are silent or refused." Since commit 915e4ebb2 a later *Works*
counts only if this capture opened and was quiet; if the phone says it "still produced sound ... with no call",
step 2 ends *Unsure* (= not working). **Report:** the sentence.

**F. Self-test step 2, in a call.** Press *Step 2: test during a call*, call the Fold5 from the iPhone, answer, ask
the other person to talk for ten seconds. **Report:** the result sentence (*Works* / *Silent* / *Refused* / *Unsure*).

**G. Raw facts in a call (PC, call still up).** Run the `--probe` line from D again. **Warning:** during a call it
plays a quiet 1 kHz tone for about one second into the call (its loop test) and runs the voip-mix test for about 1.5 s;
tell the other person. **Report:** the line,
and whether they heard a beep.

**H. Test speaking (phone, call still up).** First: no headphones on the Fold5, phone not on speaker, call volume about
30 %. Tell the other person: "you will hear groups of beeps with a pause between; tell me the size of each group".
Step 3 on the same screen: *Play the beeps now*. Ask the other person to count: 1 beep (route 1, interception), a pause, 2 beeps (route 2, in-call voice), a pause, 3 beeps (route 3, in-call
media). Tap what they heard (*They heard 1/2/3 beep(s)* or *They heard nothing*). **Report:** what they heard, and
`$ADB shell am broadcast -n dev.geekbridge.android/.callaudio.CallWifiReceiver -a dev.geekbridge.callwifi.STATUS`
(its `uplinkTest` field lists each route as `played` or `refused` with the reason, `written` frames and `loop`).
A route whose preferred telephony device is refused is now reported `refused` and plays nothing (earlier builds played
it out of the phone's own route); read the on-phone summary to the other person first, since a refused route changes
what the groups mean. The field is clipped to 600 characters, so read the on-phone summary too.

**I. Listen and speak from the PC (call still up).**

Before Listen: the PC output is at about 25 % (see the top), play any sound for two seconds so the sink is awake, headset
on the desk for the first seconds, then raise it. The PC plays the far party at 60 % stream volume with a 100 ms fade-in.

```
geekbridge-ctl call-audio use --output NAME --input NAME   # optional: pick headset (no names = defaults); see `geekbridge-ctl call audio-devices`
geekbridge-ctl callwifi listen
geekbridge-ctl callwifi speak on      # on the phone first: Settings > Experimental > Speak (experimental) on
geekbridge-ctl callwifi               # numbers, run it a few times
```

Or use **Listen over Wi-Fi**, then **Speak**, on the in-call card. Talk normally; ask the other person whether they
hear you, how clear, how late. Then try `geekbridge-ctl callwifi echo off` / `on` once each if you are on speakers.
Try once with the dialer's **Mute** pressed on the Fold5 (does your PC voice still get through?).
**Look for** in `geekbridge-ctl callwifi`: `LIVE with ...: N frames` rising about 50 per second; `far party: jitter
J ms, L late, C concealed`; `your microphone: S frames sent; the phone got G, M missing, D dropped, Q ms queued;
echo guard ...`. **Report:** two `callwifi` outputs about 20 s apart, and on the phone
`$ADB shell am broadcast -n dev.geekbridge.android/.callaudio.CallWifiReceiver -a dev.geekbridge.callwifi.STATUS`
(the `inject` field: `route R: N frames from the computer, W written, level L`). Answers: heard them (yes/no,
delay in your estimate), they heard you (yes/no, which route, with Mute on/off), echo (yes/no).

**J. Delay estimate.** While listening, tap the iPhone's microphone (or say "one") and note when you hear it on the
PC; the other way, say "one" into the PC mic and ask them. Below about 0.3 s is fine for a conversation.

**K. Stop, and does the helper survive Wireless debugging off?** With Wireless debugging off adb cannot reach the phone,
so `--status` cannot answer then (it now says "phone not reachable over adb" and never starts pairing). Do it in this order:

1. `geekbridge-ctl callwifi stop`, hang up.
2. While Wireless debugging is still ON: `geekbridge-ctl callwifi setup --adb "$ADB" --status` (note the pid).
3. Turn Wireless debugging OFF on the phone. Look at GeekBridge > Settings > Experimental: "Helper connected." still
   shown = the helper survived (it talks only over the phone's loopback). Optionally start a new call and try Listen.
4. To confirm with `ps`: turn Wireless debugging ON again (new connect port), `$ADB connect IP:NEWPORT`, then
   `geekbridge-ctl callwifi setup --adb "$ADB" --status`. **Report** the result.
5. Run the cleanup block below.

**Cleanup (at the end, and after ANY aborted run).**

```
geekbridge-ctl callwifi stop                                   # PC side: stops listen/speak
geekbridge-ctl callwifi setup --adb "$ADB" --stop              # kills the helper, sends the STOP broadcast
$ADB shell am force-stop dev.geekbridge.android                # also clears a stuck "being shared" notification
$ADB shell settings put global adb_wifi_enabled 0              # Wireless debugging off, from the PC (last device command)
$ADB disconnect; $ADB kill-server
```

With no adb at all: Settings > Experimental on the phone, switch "Call audio over Wi-Fi" off (this ends the helper), Stop on
the notification (or Settings > Apps > GeekBridge > Force stop), Wireless debugging off, and reboot if a helper is still
listed.

What the answers mean:

| You saw | Meaning |
| --- | --- |
| `pstnInterceptable:false` and nobody heard beeps | Speaking from the PC is impossible on this phone without root. Use Bluetooth for two-way |
| `pstnInterceptable:true`, they heard 1, 2 or 3 beeps | Two-way over Wi-Fi works on this phone through that route; Speak uses it |
| Self-test *Works* | Hearing works; *Silent/Refused/Unsure*: Samsung withholds the call audio from the shell user; Listen is not possible either |
| They hear you twice or with an echo | the phone's own microphone is still in the call (the injected audio is mixed with it) or the PC speakers leak; use a headset and try Mute on the phone |

## 6. What is impossible, and why

* **A normal app hearing or speaking into a carrier call**: `CAPTURE_AUDIO_OUTPUT` and `CALL_AUDIO_INTERCEPTION`
  are system/role permissions; `pm grant` refuses them ("managed by role", measured [EMU]). Hence the shell helper.
* **Speaking when the HAL has no `TELEPHONY_TX`**: no framework API reaches the modem's uplink then; only root/ALSA
  mixer control (gsm2sip style) would, which this project does not do.
* **Without Wireless debugging**: nothing else starts a shell-uid process on a stock phone; the helper ends on reboot.
* **The PC as a USB headset**: x86 PCs cannot act as USB devices.

## 7. Emulator and sandbox evidence

Command: `GB_CW_SLOT=1 GB_CW_SKIP_LISTENER=1 scripts/e2e-callwifi.sh` (own AVD clone `gbcw1` of an API 36
`google_apis` image, own adb server and ports, a private PipeWire with WirePlumber, a null sink as the "headset" and a
`pw-loopback` virtual source as the "microphone"; never the user's session, Bluetooth or ports 1716/17381/8000).
All 34 checks PASS (the listener checks were skipped in that run; they passed in the run before, including the one
90 s deadline). The emulator's call sources make sound even with no call, so everything here proves plumbing and
permissions only [EMU].

| Check | Result |
| --- | --- |
| Helper started by `geekbridge-ctl callwifi setup --yes` (real adb, the emulator) | PASS, phone reports "Helper connected" |
| Listen: the helper's 440 Hz tone (standing in for the caller) in the private headset | PASS, 440 Hz, RMS 6405; 480 frames in 9 s, 18 lost at start-up, buffer 20 ms |
| Speak: 660 Hz into the virtual microphone → pw-cat → echo guard (off) → Opus → TLS → phone hub → helper | PASS: route `tx_interception` (the platform's call-uplink injection track, handed out in `MODE_IN_CALL`): 389 frames in, 345 written, level 8428 |
| Uplink numbers | computer sent 415; the phone reports got 396, missing 0, dropped 2 (helper not fast enough at the start), queue 0 ms; far party jitter 2 ms, 22 late, 22 concealed |
| Echo guard on, while the far party (the tone) talks all the time | PASS: level into the route 0 (34 % of the session's frames silenced) |
| Echo `auto` with a null sink as output | guard on: "speakers: the microphone closes while the other person talks" |
| Speak off | PASS on both sides; the phone stops injecting |
| Test speaking (beeps) | PASS: all three routes opened and played (1, 2, 3 beeps: written 40, 70, 100 frames); VOICE_UPLINK tap ratio 2.5 / 1.1 / 1.2 (the emulator's tap is synthetic, so this means nothing) |
| HEARD 2 → Speak uses `tx_voice`; HEARD 0 → the PC no longer offers Speak | PASS |
| Link loss, blocked notifications | PASS (as before) |

Host tests: hub (echo guard silences while the far party talks, off when asked; the phone's report reaches the view),
gate (uplink report only for the live session), setup walk with a scripted stand-in phone (pairing, consent, release
build refused, wrong code never reaches adb, status/stop), phone hub (`callwifi_transport`), JVM `UplinkTestTest`
(each route its own number of beeps, refused route says why) and `HelperProtocolTest` (the app's INJECT frames reach
the route; a route that throws does not end the helper).

Seen on the side: on this run's build (before 915e4ebb2) the emulator's self-test said **works** although its no-call
control was loud; main now has the control fix (Works only if the control opened, delivered samples and was quiet),
which makes this *unsure*.

Not proven anywhere: that a person on the far end hears the PC microphone, latency on a real phone's capture path
(the emulator adds none), Samsung's behaviour. Expected mouth-to-ear delay on the Fold5 from the parts: phone capture
40-80 ms + Wi-Fi 5-30 ms + jitter buffer 40-120 ms + PipeWire 40 ms ≈ 130-270 ms per direction [GUESS until J].
