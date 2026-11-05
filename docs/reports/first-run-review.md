# First-run review (non-technical user, Android phone + Linux desktop)

Method: read the setup wizard (`SetupActivity`, `SetupModel`, `QrPairActivity`, `PairPanel`, `PairingHelp`), the desktop
Home first-run guide (`onboarding.rs`, `pair_ui.rs`, `connection.rs`) and every string behind them; looked at the
Android goldens (`tests/golden/android/setup-*`, `invite-*`, `home-*`, light/dark, font scale 1.0 and 1.5) and the desktop
goldens (`home-first-run*`, `home-service-off`, `pair`, `home-no-bluetooth`). No real phone, BlueZ or live service used.

## Taps to a first working link (as built)

Desktop: open GeekBridge, "Link a phone" (1 tap, the QR shows; it renews by itself).
Phone, from a cold start: the wizard opens on its own, "Scan QR code from my computer" (1), camera "Allow camera" (1) +
system dialog (1), scan (0), "Continue" on "Link to <computer>?" (1), "Link" in the dialog (1; the QR already proves the
identity), then "Linked" and "What to do next". About 5 to 6 taps, no typing. No dead end found on the happy path.
Failure paths all end on a card with a headline, one thing to do, "Test connection" and "Try again":
QR expired or already used ("The computer did not accept the QR code ... scan the one on the screen now"), different
network ("The computer is on a different network", step-by-step Test connection), clock skew, VPN, firewall.
Camera denied: card with "Allow camera"; denied for good: card with the app settings route; no camera: use the phone's
camera app or paste the link. Notification / battery denial: "Ask again" plus "Open app settings"; blocked goes straight to
settings. Desktop with no Bluetooth: banner "no Bluetooth: you can see and answer calls here, talking needs Bluetooth".

## Fixed in this pass

1. One word. About 100 user-facing strings said pair/paired/pairing next to "Link a phone": Android Home, widgets, Share,
   QR errors, troubleshooting help, link events and notices, setup copy; desktop Home, tray, Link a phone page, the legacy
   "Pair" buttons, doctor/why-disconnected fixes, `pairing_help`, daemon error texts, Messages/Contacts/Clipboard hints.
   "Pair" remains only for Bluetooth (call audio) and in the debug developer card. Guards: `LinkVocabularyTest` (Android,
   scans all resources) and `english_says_link_not_pair_outside_bluetooth` (desktop catalogue).
2. Battery step moved from 9th to 3rd. It is required and decides whether the link survives, but "Skip" on six optional
   pages could reach the end without ever asking. Test: `stepOrderIsStable`, `firstOpenSkipsDoneAndInfo`.
3. Desktop hint for "Can't scan? Use the link" named a screen that does not exist ("Scan QR code -> Enter link"); now
   "Link a computer", "Paste from clipboard".
4. Autostart hint said "systemd user unit"; now "To start it at every login, run this once in a terminal".
5. Desktop goldens regenerated only for the screens whose text changed (`home-first-run`, `home-first-run-firewall`,
   `home-service-off`, `pair`; a `messages-dark` difference was a capture flake and was not committed).
6. NOT done: Android goldens. Every `setup-*` page (step order and dots), `home-*` ("No computer linked") and the pages
   with the linked wording (`msgson`, `shareon`, `setup-answer*`, `setup-sms*`) are stale. Three emulator runs on this host
   died (adb "device offline" while the machine was at load 40 to 65 and the tool temp volume hit its disk quota), so no
   Android image was updated rather than committing unchecked pixels. Run `make golden-android`, review, then
   `scripts/golden-android.sh --update` on a quiet machine. The debug APK with these changes builds.

## Remaining, ranked

1. (High) "Answer calls" is one page with six sections (permission, switch, grants, dial, confirm, Bluetooth audio) and
   about 600 words at 1.5x. A first-timer should see only the permission and the switch; dial and call audio belong in a
   later "More about calls" page or Settings.
2. (High) Step 1 offers three equal ways to link (scan, "Look for computers", camera app) plus a pasted link and a
   "KDE Connect" mention. Scan should be the only visible path; the rest behind "Another way".
3. (Medium) Back on the Android QR page while "Connecting..." closes the page without cancelling the attempt
   (`QrPairActivity` has no back handler); Back should behave like "Cancel".
4. (Medium) Desktop first-run: the firewall card sits in the middle of step 2 and is long; show it only after the phone
   has failed to appear for a while, or as step 2b.
5. (Medium) Desktop doctor hint tells the user to run `geekbridge-ctl doctor` in a terminal; a "Check for problems" button
   that shows the same output in the window would remove the terminal for non-technical users.
6. (Medium) The Android "Share with computer" page repeats "No computer linked yet" under every switch (also in
   `android-visual-qa.md` #15).
7. (Low) Android setup calls note says "paired with this phone over Bluetooth" (kept: it is Bluetooth pairing).
8. (Low) The permission-rationale text is shown only before the first ask; after a denial the page gives the action but
   not why again.
9. (Low) Internal/debug texts in `gb_link.xml` (`dev_*`, `link_*`) and desktop `callaudio.*` still use pair; intended.
10. (Low) Not verified on a real phone: OEM permission dialogs, Samsung "Restricted settings" step wording, real camera.
