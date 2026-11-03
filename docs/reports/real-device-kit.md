# Real-device kit

Date: 2026-11-03. Scope: make the next session on the Fold5 (SM-F946B) and the Omarchy PC efficient and every failure
diagnosable remotely. Tested without a phone (stub `geekbridge-ctl`, one sandbox service on random ports with its own
state dir; the live service and ports 1716, 17381, 8000 were not touched).

## What was added

| File | Purpose |
| --- | --- |
| `docs/real-device-test-plan.md` | Rewritten from the 280-line release matrix into a 12-step ordered checklist (about 2 h): time, expected result and "if it fails run X" per step. Covers pairing, battery exemption and sharing, notification mirroring, Messages permissions (including the Android 13 restricted-settings path), PC dial without Bluetooth, clipboard and its reconnect, goodbye, the screen-off heartbeat, why-disconnected, a baseline and a final report. Bluetooth audio, Windows/macOS and the memory/drain measurements are listed as out of scope with the commands to use later. |
| `scripts/realdevice-report.sh` | One command, one redacted file (and `.tar.gz`) in `~/.cache/geekbridge-reports/`, mode 600. `geekbridge-ctl report` already exists but is a prefilled public bug report (diagnose only), so this is a separate script. Sections: SHA-256 and version of the nine release binaries; `doctor`, `status` JSON, `events`, `clipboard status` (counters), `calling`, `update status`; `phone-state` and `phone-log` for each paired phone (debug builds; others say so); ufw/firewalld/nft state (read-only, notes when root is needed), listening ports 1716-1764; service log tail (`--service-log`, `dist/realdevice/service.log` or journald); with adb: model, app version, installed APK hash, battery whitelist, standby bucket, Doze; the APK(s) linked from `--page URL` hashed and compared with the installed one, local build outputs hashed. It never calls `notifications`, `messages`, `agenda` or clipboard paste (the test greps the script for that). |
| `scripts/lib/report_redact.py` | The filter, run over the whole report: private-key and certificate blocks, the control token and any `--needle-file` literal, pairing links, content/name/secret JSON values, free-text `body=`, `contact X`, `answers as "X"`, long hex/base64 runs, e-mail, phone numbers, MAC, last two IPv4 octets, user name in paths. Certificate fingerprints are shortened, `HASH` lines, timestamps, reason codes and versions are kept. |
| `scripts/realdevice.sh link-watch` | Consistent with the existing subcommands (same `$BIN`, `$STATE`, `$CTRL_PORT`, `ok/warn/bad/summary`, `--dry-run`, help header). Options `--minutes N` (10), `--interval S` (5), `--device ID`, `--log FILE`. Logs `UP`, `DOWN reason=CODE tries= last_seen_ago=`, `STILL` (code changed during an outage) with timestamps to `dist/realdevice/link-watch-*.log`; summary: time-weighted uptime % per phone (PASS >= 99, WARN >= 95, FAIL below), outage list with the codes seen, longest outage, per-code sample counts and the next command for `link_timed_out`, `phone_silent` and PC-side causes. A service that does not answer counts as DOWN `service_unreachable` (not blamed on the phone). Ctrl-C still prints the summary. No adb needed. |
| `scripts/tests/realdevice-kit-test.sh` | 20 s, no phone, no service. Redaction canaries (control token, SMS, notification and clipboard text, contact name, four phone-number formats, e-mail, private key, pairing secret, device name) must be absent from the report text and from the extracted tarball while versions, hashes, reason codes and timestamps survive; report file mode; no work dir left; served-page APK hash against a local http server; link-watch on scripted sequences (steady, outage with two reason codes, service outage, down from the start). Wired into `check.yml` (Linux). |

## Findings while testing

* The first canary run leaked free-text lines (service log `body=...`, `clipboard=...`, `contact Name`, doctor's
  `answers as "Name"`); the redactor now covers those forms. Redaction of plain text is deliberately greedy (a log line
  that mentions `message` or `contact` loses the rest of the line); the structured sections keep their codes.
* Device ids are redacted as long hex in the status JSON, so the report labels phone sections with the first 8 characters.
* Not verifiable here: the adb section (no phone), the real format of `phone-state`/`phone-log` output (a debug APK
  was not available; canaries are in the stub), `--page` against the real served page (tested with a local server).
  Redaction of phone-log content that is neither JSON nor in the forms above relies on the generic number, e-mail and
  secret rules, so read the report once before posting it publicly.
* `geekbridge-service --version` is not supported (unknown argument), so only `geekbridge-ctl` shows a version next to its
  hash; the service version appears in `doctor`/`status`.
* In the sandbox the doctor correctly reported ufw without a 1716-1764 rule; the report carries that.
