# Desktop to phone file transfer failing with `link_lost` (emulator CI)

## Verdict

A real bug in the desktop service, not a CI artefact. `ShareManager::link_lost` (crates/bridge-service/src/share.rs) chose the
transfers to give up *after* its 10 s grace period, so the end of an old link to a phone failed an outgoing file that was
offered over a newer link to the same phone. Any user who disconnected (or lost the link), redialled and sent a file within
10 s of the old link ending, then took a few seconds to press Accept, saw "Failed (link_lost)" with 0 bytes.

## Evidence

CI runs 41659816195, 41666540220, 41680837634 and 41686018738 all failed `file_d2p` with
`Failed 0/300000 bytes (link_lost)`. The phone log showed it creating and then deleting the pending download, so the phone had
started receiving. The saved desktop service log was empty, so the artifacts alone could not name the cause.

Local reproduction (fresh AVD clone, newest origin/main 3c809f95e, `--only env,battery,pair,file_d2p`): 5 of 5 runs failed the
same way. The desktop's event list (`geekbridge-ctl events`) showed the timeline:

```
link_lost        peer closed the connection     t = 600.0 s   (the script's `ctl disconnect`)
connecting/connected                            t = 606.2 s   (redial)
share_finished   ok=false                       t = 610.05 s  (= link_lost + 10.0 s, no new link_lost event)
```

The transfer failure came exactly one grace period after the previous link ended while the new link stayed up. The e2e
script's `ensure_session` always disconnects and redials, then the offer dialog and the Accept tap take a few seconds; on the
owner's machine the stages before `file_d2p` leave enough time between redial and send, on a slow runner (and on a fresh pair) they do not.
None of the other suspects was involved: the link stayed up, the phone reached the point of opening the file, ports and
addresses were fine, and the goodbye packet, tie-break, announce pause, ICF and idle trim changes are not on this path.

## Fix

`link_lost` now fixes the set of waiting outgoing transfers at the moment the link ends and only gives those up after the grace
period (still skipping any that have started streaming). A transfer begun later belongs to a newer link and is left alone.
Regression test: `share::tests::a_file_offered_on_a_newer_link_survives_the_old_links_grace_period` (the old code fails the
newer transfer).

After the fix: `--only env,battery,pair,file_d2p` 5 of 5 PASS locally with the same setup that failed 5 of 5. Fmt, clippy
(`-D warnings`) and the `bridge-service` tests exit 0 (no Kotlin changed, so no Gradle run). A bisect was not needed: the logic is old
(the grace timer predates the recent changes).

## pair-background `prehome`

In the saved artifact (run 41680837634) the phone logged `pairing_failed conn_aborted` about 10 s after the pairing prompt,
i.e. as the 8 s background window ended; the stage then paired by QR. Locally it passed 5 of 7 and failed twice with "no link
to the phone" when the emulator was reused from a previous run: the phone still trusted the desktop after `ctl unpair`, so the
redial produced a trusted link rather than a pairing one. That second failure only happens on a dirty phone state, not on a fresh CI emulator.

The first (CI) failure is not root-caused; the desktop log was not saved. Changes: up to three attempts (re-dialling up to three
times per attempt), the 8 s window measured by the clock, the PASS line names the attempt, and every failed attempt saves
desktop status and events plus the phone's `GB` log (`pb-prehome-attempt-N*.txt` in the artifact). The `e2e-file_d2p-status.txt` evidence now also holds the desktop events.
Both jobs stay non-blocking until nightly runs confirm them.
