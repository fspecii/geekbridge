# First-run audit (brand-new user, both sides)

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Date 2026-11-02. Emulator (Pixel 6 image, Android 36, own adb server port 16090, `pm clear` before every fresh run) against
the desktop from `target/release` (fresh private state dir, headless Xvfb `:197`, loopback listener with
`GEEKBRIDGE_QR_ADDRESSES=10.0.2.2:PORT`), following [real-device-quickstart.md](../real-device-quickstart.md) literally.
Screenshots: `dist/screenshots/android/firstrun-*.png` and `dist/screenshots/desktop/firstrun-*.png`.

**What the emulator cannot show:** UDP discovery (no broadcasts reach it, so "Look for computers", pairing by name and
automatic reconnect after a PC restart were not exercised), a real camera/QR scan (the link was delivered with
`am start -d geekbridge://pair?...`, and separately typed into the paste field), Bluetooth/calls, OEM battery screens.
These stay on the real-phone list.

## Step log

| Step | Matched the quickstart? | Time | Notes |
| --- | --- | --- | --- |
| Desktop first start (identity key) | Partly | 10-15 s on a loaded host | `run-desktop.sh` waited only 10 s for the service: first run could end in "service did not come up". Fixed (waits 30 s). While waiting, the window says "Looking for the service". |
| Desktop Home, fresh | No: quickstart said the QR is in the window | - | Home has no QR. Tile said "Pair one to get started" and did not say it is clickable. Fixed. |
| Desktop Pair a phone > Pair with QR code | Yes | 1 click | QR dialog is clear, shows a countdown, renews itself, has "Can't scan? Use the link". |
| Phone first launch | Yes | instant | Wizard step 1 of 9 is "Pair with your computer" (Scan QR code / Look for computers / **Skip**). |
| Scan QR code on the phone | Mostly | - | Opens "Pair with QR code": **Scan with a scanner app**, **Open the camera app**, paste field + **Paste from clipboard** + **Continue**. Without a scanner app it says so in red and points to the camera / paste. Good fallback, no camera permission. |
| Confirm dialog | No: said "Continue" | - | Dialog is "Pair with GeekBridge-desk?" with **Pair** / **Decline** and the full fingerprint. A link opened from another app first shows "Connect to a computer?" with **Continue**. Quickstart fixed. |
| Paired | Yes | about 3 s after Pair | "Paired with ..." + "What to do next" on both sides. |
| What-to-do-next card | No, two wrong instructions | - | Phone said "Ctrl+Shift+V on your computer" (only works inside the Clipboard window unless the hotkey is installed) and "turn on Clipboard sync on the Home screen" (it is in Home > Settings, and the PC side also needs a tick). Both fixed, both sides. |
| Wizard Notifications .. Battery | Yes | about 20 s | Order: Pair, Notifications, Contacts, Calendar, Phone state, Answer calls, Caller number, Share with computer, Battery, Overview. Only Pair and Notifications ask the system immediately; the rest are explicit buttons. System dialogs: notifications (Allow / Don't allow), battery "Let app always run in background?" (Allow / Deny). |
| Skip everything | Yes | - | Pairing still works: Home shows "Unpaired / Pair a computer" and a Notifications warning above it. |
| Clipboard PC to phone / phone to PC | Partly | - | Needs the per-phone tick on the PC, which the quickstart did not say; the refusal was the raw code `no_enabled_phone_connected`. Fixed (message and Off label). |
| File PC to phone | Yes | 5 s | "GeekBridge-desk wants to send a file" with Accept / Decline (app open). 200000 bytes arrived in `Downloads/GeekBridge`. With notifications skipped there was no success moment: fixed (toast). |
| File phone to PC | No | - | Phone sat on "Sending 1 of 1 · 0%" forever: the PC silently rejects phones that are not allowed in Share files. Copy fixed on both sides and in the quickstart. |
| Wrong network | Yes | 25 s to the error | Error was "The connection closed before pairing finished", only **Test connection** gave "The computer seems to be on a different network" with a checklist. Added a pointer to Test connection. |
| Forget on the PC, re-pair | Yes | - | Phone still thought it was paired; a fresh QR re-paired directly. |
| Unpair on the phone, re-pair | **No** | - | First re-pair attempt: "Pairing timed out before it was confirmed" (PC logs `peer_requested_re_pair`, `peer_unpaired`); second attempt works. **Fixed; re-run on the emulator 2026-11-02, first attempt works (QR and code), see e2e-findings E2E-P1/P2**: the phone's unpair never reached the PC (no live link), so the PC's trusted link ignored the new request. A trusted link that receives a pair request now becomes a normal incoming approval on the same connection (PC and phone). Covered by `lifecycle`, `qr_pairing`, `loopback` and `transport` tests. |
| App restart (phone) | Yes | - | Reopen shows state; service restarts. PC restart + reconnect not testable here. |
| Rename | - | - | Added: Settings > "This phone's name" (phone), "This computer's name" (desktop), `geekbridge-ctl rename`. Applies to the next connection and discovery announcement (the protocol has no identity update); checked on the emulator 2026-11-02 (E2E-P3, P4). |

## Ranked defect list

| # | Severity | Defect | Status |
| --- | --- | --- | --- |
| 1 | High | A phone that was never allowed to send files makes the phone hang at "Sending 0%" forever, and the next-steps cards told the user to send a file without saying to allow it on the PC. | Fixed: cards (both sides) and phone transfer line explain it; quickstart step 6. The silent drop itself (no PC-side hint when an unallowed phone sends) is open. |
| 2 | High | Next-steps cards gave instructions that do not work on a fresh setup (Ctrl+Shift+V global, Clipboard sync location, PC-side clipboard tick). | Fixed both sides. |
| 3 | High | Re-pairing after unpairing on one side only fails the first time (timeout), the PC then drops the stale record. | Fixed and re-checked on the emulator 2026-11-02 (`scripts/e2e/pair-lifecycle.sh`: phone unpairs, computer forgets, both forget; QR and code paths; 3/3 first-attempt each; see [e2e-findings.md](e2e-findings.md) E2E-P1..P4). The re-check also found and fixed a false "Paired" when the computer had forgotten the phone (E2E-P1). The by-name path for "phone unpairs" needs UDP discovery and is not emulator-testable (QR covers the same phone-initiated request). |
| 4 | Medium | Desktop Home shows no QR and the empty-state tile did not say it opens pairing. | Fixed: tile says "Click to pair a phone"; quickstart now names the click path. A QR on Home is a design call, not done. |
| 5 | Medium | `run-desktop.sh` could time out on the first start while the identity key is created. | Fixed (30 s). |
| 6 | Medium | Clipboard refusal showed `no_enabled_phone_connected`, and the Off label did not say how to turn it on. | Fixed (daemon message with the two fixes; "Off · tick the box..."). |
| 7 | Medium | With notifications skipped, a finished or failed transfer was silent; the first send then triggers the notification permission prompt out of context. | Toast fallback added. The out-of-context prompt is open (it appears on first Send file). |
| 8 | Low | Required Notifications step said "Optional: GeekBridge works without it". | Fixed ("Recommended. You can pair without it, but alerts ... will not show."). |
| 9 | Low | Phone Pair step kept a stale "Approved on this phone. Waiting for the other device" under "Paired". | Fixed (shown only while a pairing is in progress). |
| 10 | Low | Phone has no visible Unpair outside Diagnostics. | Hint added on the Pair step: Home > Diagnostics & about > Unpair. |
| 11 | Low | After a QR "Pair", the status line still says "You will be asked to confirm" after confirming. | Open (cosmetic). |
| 12 | Low | The PC's "Pair only after comparing the code" line sat under QR pairing. | Fixed (says it is for pairing by name). |
| 13 | Low | Generic connect failure did not point to Test connection. | Fixed. |
| 14 | Low | Desktop "service not running" says "Start the GeekBridge service" without how. | Open. |
| 15 | Low | No rename for either device; phone name is its model (the emulator showed `sdk_gphone64_x86_64`). | Fixed and re-checked on the emulator 2026-11-02 (rename on both sides, hostile names, notifications, Diagnostics report; E2E-P3, P4). Desktop names now also lose direction marks. |
| 16 | Info | "Scan with a scanner app" stays the primary button when none is installed. | Open. |

## Changes made

- Android: `gb_pairing.xml` (next-steps copy), `gb_ui.xml` (`setup_required_note`, `pair_manage_hint`), `SetupActivity.kt`
  (required vs optional note), `PairPanel.kt` (stale note, manage hint), `PairingHelp.kt`, `Share.kt` (waiting copy),
  `ShareNotifications.kt` (toast when notifications are off).
- Desktop: `pair_ui.rs` (next-steps card), `home_app.rs` (empty-state tile), `connection.rs` (pairing line),
  `apps/clipboard.rs` (Off label), `bridge-service/src/daemon.rs` (clipboard error text), `scripts/run-desktop.sh`.
- Docs: [real-device-quickstart.md](../real-device-quickstart.md) steps 0.4, 1, 2, 5, 6 and a new unpair/re-pair/rename section.

## Verification

Android `scripts/build-android.sh` (unit tests and lint) passes; `cargo fmt --check`, `cargo clippy --all-targets -D warnings`
and `cargo test` on `bridge-apps` and `bridge-service` (see the end of this file for the final run).
Nothing was committed.

Final run: fmt clean, clippy clean, all `bridge-apps` and `bridge-service` tests pass; Android unit tests and lint pass.
The new Android copy and the stale-note/hint fixes were checked on the emulator with the final APK; the toast fallback
(`ShareNotifications.kt`) is compiled and linted but was not seen firing at runtime.
