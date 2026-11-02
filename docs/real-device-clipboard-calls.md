# Real-device test: clipboard and calls (Samsung Galaxy Z Fold5 + Hyprland)

A short script for one person with one phone and one PC. It covers only **clipboard sync** and **call control**;
the full release checklist is [real-device-test-plan.md](real-device-test-plan.md). Everything here was checked on
an emulator and in unit tests only. This is the first time it meets One UI, so a failure is information: use the
"if it fails, copy this" box under each section.

Rig: Galaxy Z Fold5 (One UI, Android 14 or 15), PC running Hyprland with `geekbridge-service` and `mako`, both on the
same Wi-Fi, a **second phone** to ring the Fold, and (optional) the PC paired to the Fold over Bluetooth.

```bash
export PKG=dev.geekbridge.android
alias gbctl=geekbridge-ctl
adb logcat -c                       # clear, then keep this running in a spare terminal:
adb logcat -v time -s GB            # GeekBridge's own lines: reasons and counts, never content or numbers
```

## 0. What Android does to a clipboard app (read once)

- Since Android 10 an app can read the clipboard **only while it has window focus** (or is the keyboard). GeekBridge
  never reads it in the background. The phone-to-PC direction therefore always needs one of: GeekBridge open on
  screen, the **Send clipboard** Quick Settings tile, the **tap to send** notification, **Send clipboard** on the app
  Home, or **Send to computer** in the text selection menu.
- Writing the clipboard from the background (PC to phone) is allowed.
- Android 12+ shows its own toast "GeekBridge pasted from your clipboard" each time the app reads a clip. Expected.
  Android 13+ hides the clip text in that preview when the source app flagged it sensitive.
- A clip flagged `EXTRA_IS_SENSITIVE` (most password managers set it) is **refused** by design.
- Samsung's keyboard clipboard history and the Edge panel keep their own copies; they do not change what GeekBridge
  reads, but a clip you "copied" there can be older than you think. If a pull says *expired*, copy again.
- Phone to PC rule: the item must have been copied within the **time window (default 30 s)** and the phone must be
  **unlocked** (screen on, keyguard gone). Locked means refused, whatever else is true.

## 1. Setup

| # | Do | Expect | Prove it |
| --- | --- | --- | --- |
| 1.1 | Install `dist/android/serve/geekbridge-debug.apk`, pair with the PC, finish setup. Allow **Notifications**. | Home says **ONLINE** and the computer's name. | `gbctl status` shows the phone paired and connected. |
| 1.2 | On the PC allow clipboard for this phone: `gbctl consent DEVICE_ID clipboard on`. | `ok`. | `gbctl clipboard status` lists the phone as `enabled, connected`. |
| 1.3 | Phone Home > Settings > **Clipboard sync** on. | The grey line under the Quick actions tiles changes (see 2). | `gbctl clipboard status`: `watching: yes`. |
| 1.4 | Pull down Quick Settings, edit (pencil), drag **Send clipboard** into the panel. (Or tap the Home line "Tap here to add the Send clipboard tile": Android 13+ asks you.) | Tile appears, subtitle "To your computer". | Home line now says **Ready: copy, then tap the Send clipboard tile or use the computer's shortcut.** |
| 1.5 | `gbctl hotkey status` and `gbctl clipboard hotkey`. | A paste-from-phone binding (default Ctrl+Shift+V) is installed. | `hyprctl binds \| grep -i geekbridge` |

## 2. The one-line state on Android Home

Read the small line just under the **Send file / Send clipboard** tiles. Each state, how to produce it, what to do:

| State line | How to see it | What to do |
| --- | --- | --- |
| **Clipboard sync is off.** | Switch the setting off. | Switch it on in Settings. |
| **Waiting for your computer to connect.** | Stop the PC service: `systemctl --user stop geekbridge-service`, then start it again after. | Start the service. |
| **Ready: copy, then tap the Send clipboard tile or use the computer's shortcut.** | Normal, tile added. | Use it. |
| **Ready: ... Tap here to add the Send clipboard tile.** | Remove the tile from Quick Settings. | Tap the line, accept Android's prompt. |
| **Blocked: notifications are off ...** | Settings > Apps > GeekBridge > Notifications off (or just the *Clipboard requests* channel off). | Tap the line, allow notifications. The tile still works meanwhile. |
| **Blocked: the phone was locked ...** | Run `gbctl clipboard paste-from-phone` with the screen locked (the line shows for 2 minutes after). | Unlock, copy again. |
| **Blocked: the last item was marked sensitive ...** | Copy a password from a password manager, then paste-from-phone. | Copy something else. |

Screenshot each state you see (volume-down + power) and keep them with the results.

## 3. Phone to PC (the 30 s + unlocked rule)

Use text you can recognise, for example `gb-test-<time>`.

| # | Do | Expect on phone | Expect on PC / prove it |
| --- | --- | --- | --- |
| 3.1 | Unlocked phone. Copy text in any app (Samsung Notes, Chrome). Within 30 s press the PC hotkey, or run `gbctl clipboard paste-from-phone --notify`. | Either a brief toast (Android's "pasted from clipboard") or a heads-up notification **Your computer wants your clipboard**. | If the notification appeared: tap it. PC prints `Pasted from phone (text).`; `wl-paste` returns your text. `gbctl clipboard status` shows `last pull: Pasted from phone`. |
| 3.2 | Same, but tap the **Send clipboard tile** instead of the notification. | The shade closes, a toast "Sent to your computer." | Same as 3.1. |
| 3.3 | Copy, wait **40 s**, then pull. Tap the notification. | Toast "That was copied too long ago. Copy it again." | `Nothing recent on the phone: copy again, then paste within the time window.` |
| 3.4 | Copy, lock the phone (power button), pull. | Nothing visible (no notification: locked). | `Phone is locked: unlock it and copy again.` |
| 3.5 | Copy a URL, then a long text (>64 KiB), then an image (Samsung Gallery > share > copy, or Chrome > long-press image > copy image). Pull each and tap. | Toast "Sent". The >64 KiB text: "too large". | URL arrives as a URL; image arrives on the PC clipboard (`wl-paste -t image/png \| file -`). The big text: `That item is too large to send.` |
| 3.6 | Copy a password from a password manager (sensitive). Pull. | Toast "marked sensitive, so it isn't sent". | `The phone marked that item as sensitive; it was not sent.` Home line shows **Blocked: ... sensitive**. |
| 3.7 | Empty clipboard (copy a single space, or reboot the phone), pull and tap. | Toast "The clipboard is empty." | `The phone's clipboard is empty.` |
| 3.8 | **Notifications blocked.** Turn off GeekBridge notifications, copy, pull, wait. | No notification. Home line: **Blocked: notifications are off**. | After about a minute `The phone did not answer in time: ... notifications are blocked ...`. Then tap the tile, then pull again within 10 s: it should work (the tile read is trusted for 10 s). |
| 3.9 | **Phone in the background, GeekBridge not on screen**, screen on. Copy in Chrome, pull. | Heads-up notification appears over Chrome. | Tap it: the invisible activity flashes, toast, item arrives. |
| 3.10 | **Select text** in Chrome, tap **⋮ (More)** in the selection toolbar > **Send to computer**. (On One UI third-party items hide under the three dots.) | A confirmation dialog shows the text; tap **Send**. | `wl-paste` returns it. |
| 3.11 | Share sheet: select text > Share > GeekBridge. | Goes to the computer (see sharing docs). | Receive notice on PC. |

**Samsung things to watch:** the Edge panel > Clipboard may pop up when you copy; that is Samsung's own. If the tile
tap shows a toast "Android didn't let GeekBridge read the clipboard" after 4 seconds, the invisible window never got
focus (a Samsung overlay, Secure Folder, or a multi-window pane has it): close the overlay and retry. Also check
Settings > Apps > GeekBridge > Battery is **Unrestricted** so the link survives the screen being off.

## 4. PC to phone

| # | Do | Expect | Prove it |
| --- | --- | --- | --- |
| 4.1 | PC: `echo hello-from-pc \| wl-copy`, then `gbctl clipboard push-to-phone`. **Phone locked, screen off.** | Nothing on screen. | PC prints `Sent this computer's clipboard to the phone ...`. Unlock the phone, long-press a text field > Paste: `hello-from-pc`. |
| 4.2 | Same with the phone unlocked and GeekBridge in the background. | Android may show "Copied" in the keyboard clipboard history. | Paste works. |
| 4.3 | Copy an image on the PC (`wl-copy -t image/png < pic.png`), push. | Nothing. | Paste in Samsung Notes / Gallery: the image appears (served by GeekBridge's `ClipImageProvider`). Android 14 "selected photos" access does not matter: it is not a media-library read. |
| 4.4 | Clipboard consent off (`gbctl consent DEVICE_ID clipboard off`), push. | Nothing. | An error saying no connected phone has Clipboard switched on. |
| 4.5 | Stop the phone's link (airplane mode), push. | - | Error naming the same cause (not connected). |

## 5. Calls: permissions and what the PC can show

GeekBridge asks for these (all runtime permissions, each on its own setup step):

| Permission | Why | Needed for |
| --- | --- | --- |
| Phone state (`READ_PHONE_STATE`) | Idle / ringing / in call | Everything below |
| Answer calls (`ANSWER_PHONE_CALLS`) | `TelecomManager.acceptRingingCall()` and `endCall()` | Answer, decline, hang up from the PC. On Android 14/15 this permission is what lets a non-default dialer answer. |
| Call logs (`READ_CALL_LOG`) | The only way Android gives an app the caller's number (via the phone-state broadcast, Android 9+). This is the smallest permission that does it; Contacts alone cannot. The call log itself is never read. | Showing the number on the PC |
| Contacts (`READ_CONTACTS`) | Looks the number up **on the phone**, sends only the name | Showing the name |

What the PC popup can truthfully say:

- Call logs allowed: the number (and the name when Contacts is also allowed).
- Call logs not allowed, or a hidden/private number: **"Unknown caller"** with Answer/Decline. The PC cannot tell the
  two cases apart. Android Home shows which one it is: a grey line under the Answer row, "Your computer can't show who
  is calling ...", with a tap to fix.
- Wi-Fi calling and VoLTE use the same state and number broadcast as a circuit call. **Samsung call screening / Bixby
  text call / Smart call** can answer or reject by itself before GeekBridge sees it; note if that happens.

## 6. Calls: script

Enable first: setup wizard > **Phone state**, **Answer calls** (turn on "Share call status with my computer"), **Caller
number**; PC: `gbctl consent DEVICE_ID telephony on`. Ring the Fold from the second phone each time.

| # | Do | Expect on PC | Prove it |
| --- | --- | --- | --- |
| 6.1 | Ring the Fold, all permissions on, caller in your contacts. | A popup and a `mako` notification **Incoming call** with the contact's name, buttons **Answer** / **Decline**. | `gbctl calls --reveal` shows `ringing` with name and number. |
| 6.2 | Same, caller **not** in contacts. | Number only. | `gbctl calls --reveal` |
| 6.3 | Revoke **Call logs** (Android Settings > Apps > GeekBridge > Permissions), ring again. | **Unknown caller** with Answer/Decline. Home shows the grey "can't show who is calling" line. | `gbctl calls --reveal` has no number. |
| 6.4 | Press **Answer** on the PC (or `gbctl call answer`). | The Fold answers; the PC shows **Answered on the phone** if there is no Bluetooth audio. Talk to the caller on the phone. | `gbctl calls` shows `in call`, audio on the phone. |
| 6.5 | With the PC Bluetooth-paired as hands-free: answer from the PC. | Audio comes to the PC speakers/mic. | `gbctl call audio`; `gbctl calls`. |
| 6.6 | Ring again, press **Decline**. | The call is rejected, popup disappears. | `gbctl call reject` is equivalent; caller hears busy/voicemail. |
| 6.7 | Ring, ignore it until the caller hangs up (**missed call**). | Popup goes away when ringing stops. | `gbctl calls` is empty. The missed call is in the phone's log; GeekBridge does not show a history. |
| 6.8 | Answer on the phone itself, then the **caller hangs up** (ended remotely). | Popup / live call row disappears within a second or two. | `gbctl calls` empty. |
| 6.9 | **Call waiting.** Be on a call (6.4). Ring the Fold from a third number. | A new ringing row for the second caller, with **no** number carried over from the first call. Answer/Decline act on the *ringing* call (Decline rejects the waiting call; the first call continues). | `gbctl calls --reveal`. After the waiting call ends, the live row may show no number: it is deliberately left blank rather than guessed. |
| 6.10 | **Wi-Fi calling**: turn on Wi-Fi calling and airplane mode + Wi-Fi, ring the Fold. Repeat on VoLTE. | Same as 6.1. | `gbctl calls --reveal` |
| 6.11 | Samsung call screening / "Bixby Text Call" on, ring the Fold. | Note what happens; GeekBridge may not see a call that Samsung rejects. | Note it, do not count as a GeekBridge failure. |
| 6.12 | Lock the phone, ring. | Popup and notification still appear; Answer works on the locked phone. | `gbctl call answer` |
| 6.13 | Revoke **Answer calls**, ring, press Answer. | Popup shows no Answer button (or says the phone cannot answer). | `gbctl calls` shows no answer control for the ringing call and says why. |

`call` and `calls` need the telephony consent from 6; `gbctl calls` with no phone prints why.

## 7. If it fails: what to copy

Collect, in one folder, then attach to a report:

```bash
gbctl diagnose > diagnose.txt                 # redacted: versions, capabilities, counters
gbctl clipboard status > clip-status.txt
gbctl calls --reveal > calls.txt              # contains numbers: look before sharing
gbctl events 0 > events.txt
journalctl --user -u geekbridge-service -b --no-pager > service.log
adb logcat -d -v time -s GB > phone-gb.log    # the GB tag only: no content, numbers or names
adb shell dumpsys package dev.geekbridge.android | grep -E "versionName|permission.*(CALL|PHONE|CONTACTS|NOTIF)" > perms.txt
adb shell dumpsys notification --noredact | grep -A6 "pkg=$PKG" > notifications.txt   # only the clipboard request lines
```

Plus: the phone app's **Diagnostics > Copy report**, the Home screenshots from section 2, the Android and One UI
version (Settings > About phone > Software information), and one sentence: which step number, what you expected,
what happened. Do **not** paste clipboard content or phone numbers into a public issue.

Quick triage table:

| Symptom | Likely cause | Next check |
| --- | --- | --- |
| Pull says *Phone did not answer in time* and no notification | Notifications blocked, link asleep | Home line; `gbctl status` link phase; battery set to Unrestricted |
| Tile tap: "Android didn't let GeekBridge read the clipboard" | The invisible window got no focus | Close overlays/Edge panel/Secure Folder, retry |
| Always *locked* with the phone in your hand | Screen-off or Samsung Always-On Display counts as locked by design | Wake and unlock, then copy and pull again |
| Always *expired* | Window shorter than your gap, or Samsung clipboard history showing an older copy | `gbctl clipboard window 60`; copy again |
| Popup shows no number although Call logs is allowed | Hidden/private number, or the broadcast was not delivered | `adb logcat -s GB`; try another caller; note the carrier |
| Answer does nothing, no error | `ANSWER_PHONE_CALLS` revoked, Samsung dialer rejected `acceptRingingCall` | Home "Answer from computer" row; `adb shell dumpsys telecom \| head -60` |

## 8. Emulator rehearsal (no real phone)

`adb emu gsm call 5551234` rings the emulator, `adb emu gsm accept 5551234` answers, `adb emu gsm cancel 5551234`
ends it. The emulator console is only reachable through your own adb server port:

```bash
export ANDROID_ADB_SERVER_PORT=16037
adb -s emulator-5584 shell pm grant $PKG android.permission.READ_PHONE_STATE
adb -s emulator-5584 shell pm grant $PKG android.permission.READ_CALL_LOG
adb -s emulator-5584 shell pm grant $PKG android.permission.ANSWER_PHONE_CALLS
adb -s emulator-5584 emu gsm call 5551234
```

There is no `cmd clipboard` in the emulator image: exercise the clipboard paths from the app (tile, notification,
Send to computer) instead, as in sections 3 and 4.
