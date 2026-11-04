# Android visual QA: the screens that had no golden

Scope: `docs/reports/visual-qa-3.md` (#21) found that `tests/golden/android` covered only the design gallery, Diagnostics
and the Share pages. Every Android screen added since (scan QR, the "Link to <computer>?" page and dialogs, the invite
page, the setup steps, the Updates card, link health) had never been looked at as pixels. This pass adds goldens for
them, looked at every picture and fixed what was wrong.

How: debug design-gallery pages call the real view code with made-up data (`DesignGalleryActivity`: `scanask`,
`scanblocked`, `scannocamera`, `scanconfirm`, `scanconfirmlink`, `pairdlgqr`, `pairdlgcode`, `updates`, `updatesavail`,
`updatesdev`, `updateswarn`, `diagstate`, `msgson`); the setup steps, the invite page and Diagnostics are the real
activities started from a cleared app. Long screens are shot in pages (`--ei gb_scroll N`, debug-only `DebugScroll`).
Light and dark, font scale 1.0 and 1.5 (the older goldens stay at 1.0 and 2.0). 166 new PNGs (about 7 MB at 50 %).
AVD: own clone `gbandqa` of `gbnothing` (google_apis x86_64 android-36, 1080x2400, 420 dpi), own console port and adb
server port chosen at run time, scratch under `~/.cache`, `TMPDIR` under `~/.cache`; nothing else touched, no live
service or fixed port used. Every image was viewed (a contact sheet per screen and theme, 1.0 and 1.5 side by side, then
the changed ones again after the fixes).

## Defect table

| # | Screen | Defect | Status |
|---|--------|--------|--------|
| 1 | scan confirm (`Connect to Desk?`), link dialogs | Wording mix: page said "Connect to %s?", the dialog "Pair with %s?" with buttons "Decline" / "Pair", the page title "Link to your computer" | Fixed: "Link to Desk?" (page), "Link with Desk?" (dialog), buttons "Decline" / "Link"; body text says "approve the link", "Link only if the code and certificate match" |
| 2 | setup step 1 (pair) | Title "Pair with your computer", detail "No computer paired yet", chip/rows "Paired", button "Pair" while the page above says "Link a phone" | Fixed: "Link to your computer", "No computer linked yet", "Linked", "Link", "Link with %s", "Linked and trusted"; the Diagnostics link card and the Share-with-computer rows ("No computer linked yet") follow |
| 3 | setup step 1 (pair) | Two black primary buttons on one card ("Scan QR code from my computer" and "Look for computers") | Fixed: "Look for computers" is tonal (plain once a computer is linked); the QR scan is the one primary action |
| 4 | scan QR, camera could not start | One red paragraph with no title; the other two states have a bold title | Fixed: title "The camera could not be started" + body |
| 5 | scan QR (3 states), invite (3 cards), call-audio and dial headings, scan confirm | Card titles were plain bold text: TalkBack's heading navigation skipped them (missing accessibility role) | Fixed: `cardTitle()` marks them as headings |
| 6 | Updates card | The chosen channel was the one button that was disabled (grey text) and the other the only enabled one: "Stable" vs "Dev" read as on/off, and a screen reader said nothing about which is chosen | Fixed: the chosen channel is filled like a primary button and announced "Stable, selected"; both stay enabled (`markSelected`) |
| 7 | Diagnostics, Updates card | The golden depended on the network: the check on app open showed "Couldn't reach GitHub: SSLHandshakeException" (the pinned date breaks TLS) | Fixed in the harness: the last check is pinned to one minute before the pinned time, the card shows "Not checked yet." |
| 8 | Diagnostics, Source code / Licence text | At font scale 1.5 the two equal-width buttons had different heights when one label wrapped | Fixed: `buttonRow` makes the buttons the same height |
| 9 | setup, Share with computer (messages) | The hairline above "Open app settings" touched the notes text above it | Fixed: 10 dp of space before the divider |
| 10 | Share with computer, photos | "Tap "Grant"" while the button says "Allow access" | Fixed: "Tap "Allow access"" |
| 11 | link card text (Diagnostics) | "Pairing needs your approval ... Once paired" in the middle of a Link vocabulary | Fixed: "Linking", "Once linked" |
| 12 | gallery page `msgson` (found while building it) | Messages state said "Switched off" with the switch drawn on (the preview read the saved setting) | Fixed in the gallery path (`SmsReadinessAndroid.evaluate(a, switchOn)`); not a runtime bug |
| 13 | Answer calls, Bluetooth pairing helper, dial and confirm switches | Checked at 1.0 and 1.5, light and dark, without and with Bluetooth permission: nothing clipped or overlapping; "Pair phone with your computer for call audio" is the real Bluetooth term | No change |
| 14 | Battery, Notifications, invite, scan blocked / ask, pair dialogs, update warning dialog | Checked: nothing clipped or overlapping at 1.5; dialogs fit with their buttons at 1.5 | No change |
| 15 | Share with computer, every switch on | Five red "No computer linked yet" lines in a row; "Waiting for permission" under a switch and "Android still needs your OK" under it say the same | Left: each line is the state of its own feature; merging them is a copy decision for the whole page |
| 16 | Home, widgets, Share sheet, help texts, error texts | Still say "pair" / "paired" ("Pair a computer first", "Not paired", "Pairing timed out") | Left: about 60 strings and the Home tests; the confirmation flow and the setup wizard are consistent now, Home and the widgets are the next sweep |
| 17 | Diagnostics (debug) | "Prepare: validate with native core" wraps at 1.5; "Paired computers" in the developer link card | Left: debug-only / developer card |
| 18 | scan QR, "denied" camera state | Draws the same card as "ask" (`ScanRules`: a denial that can be asked again is NeedPermission) | Left: by design, "Allow camera" asks again; the "blocked" card has its own picture |

## Not covered by goldens

* The live camera preview (the emulator camera is a moving test pattern); its permission and failure cards are covered.
* States that need a paired computer: the pairing panel with a found computer, per-computer grants.

## Checks

* `gradle testDebugUnitTest lintDebug`: exit 0 (one test string updated: "No computer linked yet").
* Reproducibility: two complete `--update` runs (before and after the last string edits) produced byte-identical PNGs for
  every screen whose text did not change. A final full `--check` pass could not be finished: the host was overloaded (load
  10) and the emulator's System UI stopped responding ("System UI isn't responding" in every capture), so that pass was
  invalid and says nothing about the goldens. Re-run `make golden-android` on a quiet machine.
* `scripts/check-release-isolation.sh`: `DebugScroll` added to the debug-only names the release APK must not contain.
