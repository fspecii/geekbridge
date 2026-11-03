# QR link on a cold start: investigation and fix

## Findings

1. The "empty Pair with QR code page" was **not reproducible** on an idle emulator with the debug APK: `pm clear`,
   grant permissions, `am start -W -a VIEW -d geekbridge://pair?...` showed "Connect to a computer?" correctly (checked
   with 2 and 5 permission grants). The only blank or odd outcomes came while the host was at load average 100-290
   (other agents' builds): the emulator showed "System UI isn't responding" and uiautomator dumps timed out.
2. A real latent defect that matches the report: `QrPairActivity.onCreate` read the link only when
   `savedInstanceState == null`. If the system recreates the activity while the confirmation is showing (theme or
   config change, process death, a permission-grant kill on first launch), the link was dropped and the empty entry
   page appeared. Fixed (below). With the fix, forcing a recreation with `cmd uimode night yes` keeps the confirm page.
3. The `call-ui.sh` failures were three script causes, not app regressions:
   - It waited for "Pair with E2E-Desktop" (the old dialog title); the confirm page reads "Connect to a computer?"
     (what `pair-lifecycle.sh` already waits for). Fixed in `call-ui.sh`, `e2e-qr-pairing.sh`, `e2e-new.sh`.
   - A newly paired computer holds no phone-side grants (`ComputerGrants`), so the phone core reported calls to nobody
     (`queued:0`) and the desktop saw "no calls". `call-ui.sh` wrote only `geekbridge_calls.xml`; it now also writes
     `geekbridge_grants.xml` for the desktop id (as `e2e-new.sh write_grants` does).
   - The new Keypad layout moved the incoming-call popup buttons; the Decline colour sample at x=40 hit the border.
     Now sampled at x=60.

## Code change

- `QrPairActivity`: saves only a flag `link_waiting` (true while the link is being parsed or the confirm page shows);
  on recreation with that flag it reads the launch intent again. The link (one-time token) is never put in saved
  state. `QrReplay.shouldHandle` is the pure rule, unit tested in `QrPairingTest`.
- `ShareActivity`: starting the link and the hold was also gated on `savedInstanceState == null`; it now also runs
  when no hold was handed over (`lastNonConfigurationInstance == null`, i.e. recreation after process death).
- Other entry paths: the manifest has only the `geekbridge://pair` VIEW filter (no https App Link). A scanner app
  returns through `onActivityResult`, which is unaffected. Share and scanner cold starts were checked by code reading
  only, not on the emulator.

## Verification

- `scripts/build-android.sh --skip-lint --skip-host-tests`: exit 0 (includes gradle unit tests).
- `scripts/e2e/call-ui.sh`: 12/12 PASS on fresh desktop state (fresh QR pairing), and again on the paired state.
- `scripts/e2e/pair-background.sh`: prehome, pair, home, swipe, doze PASS.
- No Rust changes, so no fmt or clippy run. No before/after run of the recreate fix against the old APK.
- Emulators started were stopped and the temporary AVD clone removed.
