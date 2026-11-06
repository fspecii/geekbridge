# Emulator end-to-end status (main, 2026-11-05)

Runs: 42094171376 (d66e59908, all green), 42099651533 (04c2c8765, 2 red), 42086734785 (65855a070, update-install and photos red).

| Job | Gate | Last green | Last red | Cause of the red | Owner |
| --- | --- | --- | --- | --- | --- |
| features | blocking | 42099651533 | earlier runs | none open | - |
| call-ui | blocking | 42094171376 | 42099651533 | script drift: `adb: device offline` right after boot, the single `adb install` failed, the rest of the suite ran without the app. Fixed by `adb_install_retry` (steady transport + package manager, 3 tries) | this commit |
| battery-optimisation | blocking | 42099651533 | earlier runs | none open | - |
| update-install | blocking | 42099651533 | 42086734785 | same `device offline` drift; fixed in 04c2c8765, green since | done |
| sms | experimental | 42099651533 | - | none open | - |
| contacts-calendar-cases | experimental | 42099651533 | - | none open | - |
| photos | experimental | 42099651533 | 42086734785 | list not delivered after configuring (link drop); retry added in d66e59908, green since | done |
| share | experimental | 42094171376 | 42099651533 | link drop: `share_cancel_desktop` failed at 20:33:28 after `link_closed conn_aborted` at 20:32:43; logcat-full has 928 `Destroyed .* sockets` (emulator mobile network) | link-drop suite fix, docs/reports/e2e-link-drops.md |
| e2e-android | experimental | 42099651533 | - | none open | - |
| pair-background | experimental | 42099651533 | - | none open | - |
| call-audio-wifi | experimental | 42099651533 | - | none open | - |

The install helper is shared by call-ui, features, sms, share, photos, e2e-android and call-audio-wifi, so the same boot-time flap cannot fail their env stage.

## Run history, 2026-11-06 (workflow_dispatch unless noted)

Red jobs per run. Runs marked success can list a red experimental job (those jobs do not gate the run).

| Run | Head | Result | Red jobs |
| --- | --- | --- | --- |
| 42125553951 | 667f0a5ab | failure | sms, photos, features, share |
| 42133897536 | 7b35acff9 | success | - |
| 42155454627 | 149b92589 | failure | features, photos, share |
| 42159647633 | 68142649f | failure | share, features |
| 42167708317 | 7cbe7301c | success | contacts-calendar-cases, photos (experimental) |
| 42174932893 | 749747ffd | success | photos (experimental) |
| 42180468999 | 6a1614bf5 | failure | share, features |
| 42194747065 | f64dac41c | failure | call-ui, features |
| 42198230749 | 3c73287ef | failure | battery-optimisation |
| 42207429247 (schedule) | 84392e594 | success | - (all 11 jobs green) |
| 42226426174 | c5cdbd0e6 | failure | battery-optimisation, e2e-android, contacts-calendar-cases (battery stage: harness still looked for the old button after 0d4d03bf6 changed the denied-state wording) |
| 42234854200 | 4ff72908d | success | - (all 11 jobs green, after the harness fix 4ff72908d) |

## Flake causes found so far

- adb offline after boot: `adb: device offline` right after the emulator reports booted; a single install or a first command fails and the environment is declared dead (battery-optimisation, run 42198230749). Fixed by waiting for a steady adb transport and installing through `adb_install_retry` (07d188095); green in run 42207429247.
- photos `dev_listed` exit status: the check's non-zero exit status was treated as a failure although the list had been delivered; fixed in the photos script (green from run 42174932893).
- consent timeout: features waited for the consent dialog/state too briefly; the `consent()` helper now waits robustly and reports diagnostics.
- battery push race: the computer's battery level was pushed before the core knew the permit, so the phone never got the level; product fix in f64dac41c.
- link diag: share and features reds were link drops (`conn_aborted` with emulator socket churn); share retries and link diagnostics were added so a drop is told apart from a product failure.
- first-run wizard after force-stop: every phone restart now marks setup finished first (3c73287ef).
