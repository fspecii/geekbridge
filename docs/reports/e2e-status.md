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
