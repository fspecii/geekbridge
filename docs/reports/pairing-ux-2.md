# Pairing UX 2: link a phone like WhatsApp Web

A dated snapshot (2026-11-04), not living documentation. Living text: [pairing.md](../pairing.md),
[user-guide.md](../user-guide.md), [real-device-quickstart.md](../real-device-quickstart.md).

Goal: the owner tests on a real phone; pairing should need one scan, say what is happening on both screens, and not
require a computer app to be installed first for the phone to be useful as a starting point.

## What changed

### Computer (Pair view, first-run card)

- **Link a phone** is the title and the primary action of the Pair view (the sidebar entry and the first-run step use
  the same words). The QR is inline (no modal window), up to 300 px, always dark on white.
- With **no phone paired** and the service listening, the code is already on screen the first time the view is drawn
  (`pair.auto_opened`). With phones paired, the button shows it. After **Stop showing the code** it stays stopped.
- **Self-renewing:** the existing renew-before-expiry logic (10 s before the 2 min token dies) is kept, there is no
  refresh button. The code is **not replaced while a scan is being confirmed**.
- **Live line** under the code: *Making a one-time code…* / *Waiting for phone…* / *Phone scanned — confirm on phone*
  (service event `qr_accepted`; cleared by `paired`, `qr_refused`, `pair_failed`, Stop, or after 90 s) / then the green
  card *Paired ✓ with <name>* with the existing "what to do next" buttons. The lines are exposed to the accessibility
  tree; the QR has the name "Pairing QR code. Scan it with the GeekBridge app on your phone."
- **Token hygiene:** the code is only kept alive while the Pair view is drawn. If the view was not drawn for 4 s, the
  window drops it and sends `PairQrCancel` (revokes every outstanding token), so a hidden view never keeps issuing
  tokens. Unchanged from before and still tested in `crates/bridge-service/tests/qr_pairing.rs`: single use, 120 s TTL,
  bound to the computer certificate fingerprint (`fp`), never logged.
- **Stock camera app:** unchanged and still supported: the payload is `geekbridge://pair?...`, the manifest intent
  filter opens `QrPairActivity`. "Can't scan? Use the link" has Copy link. No https fallback (not needed).
- **Unlink:** "Forget…" / "Forget phone?" became **Unlink…** / **Unlink phone?** with a sentence saying what happens
  ("will no longer be trusted by this computer. To use it again, link it with a new QR code").
- **Failure text:** timeout advice now ends "Still stuck? Run geekbridge-ctl doctor on the computer." (desktop
  `explain_failure` and the phone's `help_no_answer_a`); the expired/used-code advice says to scan the new code.
- Wording kept consistent in the first-run card (steps 1 and 2), the sidebar, Clipboard and Share hints.

### Phone

- **Step 1 of the wizard** (and the Pair step everywhere) leads with **Scan QR code from my computer** (primary).
  Below it, **Don't have GeekBridge on your computer yet?**. Computers already paired get an **Unlink** button on
  their row (confirmation names the computer).
- **In-app scanner, decision** (full table in pairing.md): Camera2 for the picture, the native core for the decoding.
  New core action `pairing_decode_qr` (base64 grey frame <= 960 x 720 in, `{text}` or `qr_not_found` out) uses `rqrr`
  (pure Rust, `default-features = false`, no `image` crate). No CameraX, ML Kit, zxing or Play services: no new Kotlin
  dependency, offline, a few tens of KB of native code. New permission `CAMERA` (+ `uses-feature camera` /
  `camera.autofocus` `required=false`), asked on the scan screen on first use, after an explanation. Denied: the screen
  says why and offers **Open app settings** (blocked) or **Allow camera**, plus **Use another way** (camera app, scanner
  app, paste). Frames are analysed about 5 times a second, in memory; nothing is saved or sent; the decoded text (it
  holds the one-time token) is returned only to `QrPairActivity`, and never logged. A QR code that is not ours shows a
  hint instead of ending the scan.
- After a scan: **Connect to <computer name>?** (name marked unverified, address, certificate fingerprint start) >
  Continue > the existing **Pair with X?** approval > **Home with a "Paired ✓ with X" banner** (live region for
  TalkBack, **Got it** to dismiss). The old in-activity "done" page and file picker were removed.
- **Send me the download link** (`ComputerLinkActivity`, `ComputerInvite`): `ACTION_SEND text/plain` through a chooser
  (subject "GeekBridge for your computer"; body: intro, `https://github.com/fspecii/geekbridge/releases/latest`, three
  numbered install lines), **Email it to myself** (`mailto:` with the same subject and body, no recipient), and a
  short address `github.com/fspecii/geekbridge/releases/latest` with **Copy link**. Our code needs no network or
  account. Unit tests assert the text has no `geekbridge://` link, no token, no address, no 32-hex id, and that the only
  link is the public releases page.
- **Unlink** wording replaces Unpair on the phone (Diagnostics and the pair step), with a message that says what
  changes on this phone and to also unlink on the computer.

## Tests and checks (all in a clean worktree of origin/main under ~/.cache/gb-pairux2)

| Check | Result |
| --- | --- |
| `cargo fmt --all --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo test -p bridge-android` | pass (new: a rendered pairing QR decodes through `pairing_decode_qr`; no code / wrong size / bad base64 / unknown field are refused; a frame far over the 64 KiB request cap is accepted) |
| `cargo test -p bridge-apps -p bridge-core` | all pass except `calendar_tests::stepping_and_jumping_move_the_shown_day_in_every_mode` (see Known issues); new desktop tests: first-run auto-open and stop, no surprise code when paired, live status transitions from events, hidden view revokes the code, scanned code is not replaced; i18n allowlist lowered |
| Gradle `:app:testDebugUnitTest` | exit 0 (new: `QrScanTest`, `ComputerInviteTest`) |
| Gradle `:app:lintDebug` | exit 0 |
| Desktop goldens (separate worktree, serial; a full --check run hit its time limit on the unrelated calendar screens under load, the 8 changed screens matched after the update) | 8 screens x 4 variants regenerated after a visual review of the diffs (`home`, `home-first-run`, `home-first-run-firewall`, `home-service-off`, `settings`, `diagnostics`, `pair`, `update-banner`: sidebar "Link a phone", first-run step text, the new Pair card); the rest unchanged |
| Doc links | 0 broken |

The golden run needs one Xvfb per worker and was unreliable at the default parallelism on this busy machine ("Failed to
open connection to X server", mock server not up in 5 s); updating screen by screen with one worker was stable.
There is no golden of the live QR itself: its token is random, so every run would differ.

## E2E scripts (written, not run)

I was told not to build or serve an APK, so none of these ran against an emulator; they are syntax-checked
(`bash -n`) only. **Run them before trusting them.**

- `scripts/e2e-qr-pairing.sh`: `pair` now waits for Home's "Paired ✓ with"; new scenarios `wizard` (scan button first),
  `camera` (granted: the scan screen explains itself; Back; denied: explanation and a way out), `invite` (download page,
  short address, share sheet), `unlink` (pair, then Unlink from the wizard's pair step). First taps use `tap_to`.
- Other scripts that waited for "What to do next" after a QR pairing or tapped "Unpair" on the phone were updated to
  "Paired ✓ with" / "Unlink": `e2e-new.sh`, `e2e/features.sh`, `e2e/pair-lifecycle.sh`, `e2e/pair-rename.sh`,
  `android-perf.sh`. `ui wait "Pair with"` (the old screen title) became "Connect to a computer".
- The emulator cannot point its camera at the desktop window, so the picture-to-link path is covered by the Rust test
  that renders a real pairing QR and decodes it. Camera2 preview orientation (the TextureView transform) is the part
  most likely to need a fix on first real use; the decode does not depend on orientation.

## Not done / known issues

- **Android goldens** (`scripts/golden-android.sh`) not run (needs the debug APK; another agent builds). The captured
  screens are Home, status, controls, dial and share; the unpaired Home is unchanged, so no change is expected, but the
  Home banner, scan screen and invite page have no golden.
- **Real device untested:** Camera2 on the Fold5, the permission dialog flow, the chooser, `mailto:`.
- The phone's wizard step is still titled "Pair with your computer" (kept so existing scripts and docs match).
- Pre-existing, unrelated: `bridge-apps` `calendar_tests::stepping_and_jumping_move_the_shown_day_in_every_mode` fails
  today (the month step is 27 days at the date this ran; it depends on the real date). Not touched.
- `announcements_stay_paused_while_linked_...` in `bridge-android` failed once under load and passed on rerun (timing).
