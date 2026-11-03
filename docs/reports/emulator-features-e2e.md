# Emulator end-to-end coverage: battery sharing, notification mirroring, PC dial

Script: `scripts/e2e/features.sh [--stop] [env,pair,battery,mirror,dial]` (follows `e2e-android.sh` / `call-ui.sh`).
Everything private: own AVD clone `gbfeat` (cloned from `gb36`, console 5654, adb server 16090), own desktop service
(state dir under `~/.cache/gb-tmp/feat-e2e`, control 17521, api 17520, discovery 17556, listener 1747), private D-Bus session,
private Xvfb `:93`. Never touches the live service, real config or ports 1716, 17381, 8000. Only the reserved number
5550100 is dialed (simulated modem, no network). Results: `dist/android/logs/e2e-features-results.tsv`.
Phone-side opt-ins are written into the app's own preference files (as `call-ui.sh` does), not by driving the settings UI.

## Result: 28/28 checks PASS on the final full run

- Battery (10): nothing shown with desktop consent but no phone grant; nothing with the phone switch but no per-computer
  allow; nothing with the allow but computer consent off; with both, 42% not charging follows `dumpsys battery set level 42`;
  AC plug shows charging at once; level 9 unplugged shows the low flag; level 77 is followed (rate-limited to once a minute);
  consent off on the computer erases the reading; back on restores it; removing the grant on the phone erases it and it is not re-sent.
- Mirroring (6): `cmd notification allow_listener` grants access; with no app allow-listed a posted notification is not mirrored;
  with `com.android.shell` allow-listed it shows in `geekbridge-ctl notifications`; `notifications dismiss` cancels it on the
  phone and removes it from the desktop; consent off erases the list and nothing more is mirrored.
- Dial (12): no CALL_PHONE and no consent are refused (modem idle); no `--yes` without a terminal and a non-numeric string are
  refused; with everything allowed the phone places the call (modem off-hook, Telecom `CallIntentProcessor` log lines for the
  outgoing CALL intent, saved in `dist/android/logs/e2e-features-dial-logcat.txt`) and shows "Call placed for a computer";
  the 4th call in 10 minutes is refused by the phone ("The phone limits how many calls..."); with "Ask me before placing a call"
  the phone shows "Place this call?", Deny gives "did not confirm that call" and Call places it.

## Real bug found and fixed

Switching Telephony consent on while a phone is linked left the computer with "no call state received" (so `call dial` was
"not available") until the phone's state changed or the link reconnected. The desktop drops call state while the consent is off
and the phone only sends changes. Fix: a new request action `report` (`LinkAction::Report`): the desktop sends it when the consent
goes off to on; the phone core replies with its last state to that connection only (no app code involved, not counted in the dial
limit, no result packet); older phones ignore it. Tests: `bridge-core` wire round trip, `call_control` (asked only on a real
off to on change, takes no pending slot), `bridge-android` loopback (`a_report_request_resends...`), `calls_api` integration
(asks after consent on; control helpers skip it). Documented in `docs/call-control.md`.

## Test-harness findings (not product bugs)

- Android re-binds an allowed notification listener as soon as its process is force-stopped, and it reads the old prefs before a
  test rewrites them; the script takes listener access away while editing the files.
- `cmd notification post` needs the quoting inside one string (adb joins arguments). The screen sleeps after `dumpsys battery
  unplug` (stay-on only applies when plugged), so notification-shade taps need a wake first.
- `ctl call dial` printing `ok` only means the request was queued; the phone's refusal arrives later as the `notice:` line of
  `geekbridge-ctl calls`. A later state report clears that notice, so the script waits for the idle report before the call that
  should be refused. (Possible UX nit: any idle state report clears an older refusal notice.)
- The host load average reached 90+ from other agents; waits are generous and the link is re-dialed through the emulator redirect.

## Not covered

Enabling the features through the phone's Settings UI (toggles) and real notification actions/buttons; phone-side dismissal
propagating to the desktop (`cmd notification` has no cancel).
