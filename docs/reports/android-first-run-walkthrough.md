# Android first-run walkthrough (new user, fresh emulator)

Scope: walk the Android debug app (build 20458, current `origin/main` 84392e594) as someone who has just installed it, on a
fresh API 36 emulator (google_apis x86_64, 1080x2400, 420 dpi, wiped data) with a sandbox desktop service as the
"computer" (private HOME, state dir, ports and Xvfb; never the live service). Every screen was screenshotted and looked at; unlabeled
clickable nodes were checked with `uiautomator dump`. The emulator cannot scan a QR code, so the QR was delivered as the
`geekbridge://pair?...` link, which is what the phone's camera app does with it.

Covered: launch, wizard (9 steps), camera / notification / contacts / phone / SMS prompts (deny, deny again = "blocked",
allow via app settings), battery step (Deny then Allow), notification access, link by link and by pasted text, Home
(linked, link lost, no computer), Diagnostics (update channel, check now), Messages and Calls tiles, Unlink, uninstall and
reinstall, force-stop mid-wizard, dark mode, font scale 1.5, landscape, Back behaviour.

## What works well

- Permission handling is solid: every step has Not allowed yet / Denied (can ask again) / Blocked (open app settings) states
  with plain words; a grant made in Android settings shows as Done on return.
- Linking vocabulary is now consistent ("Link", "Linked", "Unlink") in the wizard, dialogs and Home.
- The link states on Home read clearly: Online, Waiting for the computer (link lost, recovers by itself when the computer
  comes back), No computer linked with a "Link a computer" button.
- Dark mode and font scale 1.5 on Home and the wizard: nothing clipped or overlapping.
- No unlabeled clickable node and no unlabeled field on Home, the overview, all nine steps, the invite page, scan screens and
  Diagnostics.
- The update check works (Dev channel selected, "is the newest dev build"), the chosen channel button is announced selected.

## Findings

| # | Where | Finding | Status |
|---|-------|---------|--------|
| 1 | Back after a Home tile / widget / overview row | Tapping the Messages, Calls, Contacts or Calendar tile (or a row on the overview) opens the wizard on that step, but Back then walked down through every earlier step (nine presses from step 9 before Home). | Fixed: Back from such a page returns to where it came from (Home / the overview); the Back button is shown on a step opened that way. `SetupModel.back` + unit test. |
| 2 | Step "Caller number" | Nine lines of jargon ("smallest permission that does it") before the button; at font scale 1.5 the button is below the fold. | Fixed: shorter copy, same facts. |
| 3 | Step "Contacts" | "Read on this phone; nothing is uploaded" while the same sentence says the contacts fill the computer's Contacts app. | Fixed: "Nothing leaves this phone unless you switch on Share contacts." |
| 4 | Diagnostics (Privacy card, "Paired computers", message intro) and the developer link card | Still say "paired" ("computers you paired", "a paired computer"). | Fixed: "linked" in Diagnostics, the Privacy card and the developer link card (Bluetooth keeps "paired"); Diagnostics goldens regenerated. |
| 5 | Battery step, after "Deny" in Android's dialog | The page is unchanged (To do, same button): no sign that the answer was received. Notifications and the other steps show Denied. | Fixed: after Allow is tapped and the page is still optimised, it says "You chose not to allow this" with Try again and Open app settings (`SetupModel.batteryDenied`). The state lives in the open screen only. |
| 6 | Battery step | The Pixel / Google troubleshooting block (two numbered steps, "Adaptive battery") is shown before the user has tried "Allow unrestricted battery". | Left (copy decision). |
| 7 | Process death mid-wizard | Skip step 1 and 2, reach step 3, force-stop, relaunch: the wizard opens on step 1 again (the first step still not done), not step 3. After a skip the user is asked again on every cold start until Finish. Not a dead end, but skipped steps come back. | Left, by design today (`SetupModel.firstOpen`); decide whether a skipped optional step should count as handled. |
| 8 | Link by pasted text | A pasted non-link says "That is not a GeekBridge QR code." (it is a link/text, not a QR) and the field is cleared. | Fixed: "That is not a GeekBridge link." (and the version / damaged / unreadable lines say "link"). |
| 9 | Link confirm dialog | "This is the computer you scanned" also appears when the link was opened or pasted, not scanned. | Fixed: "This is the computer the link points to". |
| 10 | Link step once a computer is linked | The expander "Don't have GeekBridge on your computer yet?" and the "Looking for computers..." note are still offered; the "nothing is trusted until you confirm a matching code" line contradicts the QR flow, which has no code to compare. | Left. |
| 11 | Share with computer step | Three overlapping intros (body text, bold summary, card text). The "Off" state is appended to the description line ("...Photos are never sent. Off"). Notifications and SMS use the same speech-bubble icon. SMS and notification-access help is dense with Android jargon ("restricted setting", "Allow restricted settings", MMS, RCS, Google Play). | Partly fixed: one intro line instead of three (the long why line, the bold detail and the card intro no longer repeat each other). Icon and jargon decisions left. |
| 12 | Messages tile on Home | Opens "Step 9 of 9 Share with computer" at the top of a long page; the SMS switch is two screens down. | Left. |
| 13 | Phone state step | "Phone state" is Android's term; the system prompt then says "make and manage phone calls", which sounds bigger than "never sees phone numbers". | Left (title could be "Call status"; add a line that the prompt wording is Android's). |
| 14 | Landscape | The wizard keeps the Back/Skip footer, leaving about two lines of scrolling content in landscape. Works, cramped. | Left. |
| 15 | Home | "Connected to AF-Desktop" and "Linked: AF-Desktop" repeat the name on one card. | Left. |
| 16 | Cold start | The very first screenshot taken 4 s after launch was a blank grey frame (window not yet drawn); content was present a moment later. | Noted only. |
| 17 | Desktop (not Android) | After uninstall + reinstall of the app the computer still lists the old phone entry; relinking adds a second entry with the same name (`sdk_gphone64_x86_64`, two device ids). A user who reinstalls sees two identical rows and no hint which is stale. | Fixed on the desktop: a pairing prompt for a name that is already linked says so (reinstall case); the older same-name offline entry is marked "A newer link has the same name" next to Unlink. Never automatic: the new certificate still needs the typed code, the new link starts with every sharing option off, and the old entry stays until the user unlinks it. |
| 18 | Debug build only | Home shows "Experimental: Call audio over Wi-Fi (ADB helper)"; Diagnostics shows the raw core-info JSON. Not in release builds. | Noted. |

## Not found

Dead ends, crashes, clipped text at font scale 1.5 in dark mode, unlabeled controls, the "Pair"/"Paired" wording in the wizard,
Home or dialogs. A back-stack trap was found (finding 1) and fixed. "Clear data" is Android's own (Settings > Apps >
GeekBridge > Storage); the app has no in-app clear-data button, only per-computer Unlink, which also clears the desktop's
entry.

## Verification

`scripts/build-android.sh --skip-host-tests` (assembleDebug, testDebugUnitTest, lintDebug) passes with the changes; the fix was
checked on the emulator (tile, Back, Home; overview row, Back, overview). No golden image covers the changed strings or
pages (no Caller number / Contacts setup golden), so none were regenerated.
