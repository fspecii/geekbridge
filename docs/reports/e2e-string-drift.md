# E2E string drift: partial findings (Bash unavailable)

Bash failed 8 of 8 attempts with EDQUOT on /tmp/claude-1000, so nothing was run or reproduced, and no worktree, AVD, commit, push or workflow dispatch was done. This is a read-only comparison.

## What the scripts grep (phone side)
- scripts/e2e/call-ui.sh, features.sh and share.sh all wait for the phone texts `Link to a computer?`, `Computer identity` and `Paired ✓ with`, and tap `Continue`, `Link`, `Back to home` or `Done`.
- scripts/e2e/call-ui.sh, `stage_pair`: the failure "calls not negotiated" comes from `calls_negotiated`, which reads `status.links[0].negotiated` and wants `geekbridge.calls`. The script looks for it only 30 s after `ctl consent ... telephony on`.

## What was checked on the desktop side
- crates/bridge-i18n/i18n/en.properties: `pair.link.*` and `pair.paired = Paired ✓ with {name}` match the `Paired ✓ with` grep. Desktop wording is "Link a phone".
- overlay.* matches the overlay stage: `overlay.answered.link = Answered on phone` against grep "Answered".
- `overlay.reject` is "Decline", but call-ui.sh clicks by coordinates, not by text, so it is not affected.
- Banner and tray: `banner.incoming_call` / `tray.incoming_call` = "Incoming call". The notify stage greps "Incoming call", so it matches.

## What could not be checked
- android/app/src/main/res/values/strings.xml holds only 9 strings (share and grant wording). The real Android UI strings are in some other file, and I could not list the directory (no ls or grep), so I could not confirm whether "Link to a computer?", "Computer identity", "Paired ✓ with", "Place this call?" and the share status words still exist as written.
- I did not read the rest of features.sh (from line 400: the "Place this call?" Deny/Allow taps) or the rest of share.sh (share_survives_no_service, share_grant_revoked).

## Hypotheses for the reds
1. features feat_dial_confirm_deny/allow ("Place this call?"): the ~100 pair->link rename very likely changed or reworded this dialog title or its buttons. Compare `ui tap`/`ui wait` strings in features.sh lines 400 onwards against the Android string resource that holds "Place this call?".
2. callui_pair "calls not negotiated" and callui_overlay_ring: the script writes `geekbridge_grants.xml` and `geekbridge_calls.xml` directly. If the rename also changed the pref file or key names (grants JSON shape, `enabled`, `confirm_dial`), the phone never offers geekbridge.calls, so no overlay ring follows. Check the Kotlin that reads those prefs (ComputerGrants, CallsSettings) against the XML written in `write_grants_calls` / `write_calls_prefs` / `write_grants`.
3. share_survives_no_service and share_grant_revoked: the share.sh steps grep phone status words, and ShareActivity strings (grant_*, send_confirm_*) may have been reworded. Compare the `screen_has` / `status_lines` greps in share.sh with the Android share strings.

## Next steps once Bash works
Run `grep -rn "Place this call\|Link to a computer\|Computer identity\|Paired ✓" android/app/src/main scripts/`, fix whichever side drifted (prefer keeping the scripts and the UI text in one place, with a test that fails on drift), then reproduce on a private AVD clone as instructed.
