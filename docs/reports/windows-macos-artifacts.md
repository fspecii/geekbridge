# Windows and macOS binaries from CI (artifacts only)

Workflow: `.github/workflows/build-binaries.yml` (`workflow_dispatch`, `contents: read`, no secrets, no tag, no
release; actions pinned by SHA like the others). Run: `gh workflow run build-binaries.yml --ref main`.
`release.yml` is unchanged (it builds the same binaries, but only as part of a release).

Jobs: `windows-x86_64` (windows-latest, zip), `macos-aarch64` (macos-latest, tar.gz), `macos-x86_64`
(cross-built on the arm64 runner, executed under Rosetta, tar.gz). Each archive holds `geekbridge`,
`geekbridge-service`, `geekbridge-ctl` and the six app binaries plus README, LICENSE and third-party notices.
A final `SHA256SUMS` job combines the per-archive sums and verifies them with `sha256sum -c`. Artifacts are kept
14 days. No universal binary: two separate macOS archives (a `lipo` step is a possible follow-up). Unsigned and
not notarized.

## Smoke test (run on each OS inside the job, before packaging)

`geekbridge-ctl --version/--help`, `geekbridge-service --help`, `--help` of the seven GUI binaries (never
started as windows); then `geekbridge-service` is started with a temp `--state-dir` and non-default ports, and
`geekbridge-ctl status` and `geekbridge-ctl doctor` run against it. The step fails if the service never answers
`status`; a non-zero `doctor` exit is recorded but does not fail the job.

## Runs

| Run | Result |
|---|---|
| 41568112819 | Builds OK on all three. Smoke failed: workflow bug (`bash -e` aborted on doctor's non-zero exit); Windows: service refused the state dir (below). |
| 41568917993 | Windows: same state-dir refusal. macOS arm64: smoke flaky-failed on `ctl --help \| head` under `pipefail` (SIGPIPE; workflow bug). macOS x86_64 passed, but doctor showed the two product bugs below. |
| 41569945066 | All green: windows-x86_64, macos-aarch64, macos-x86_64, SHA256SUMS. |

Final run, `ctl doctor` on all three (exit 0): Service ok, Listener ok (TCP 1716), Firewall ok, Desktop session
ok, Phones warn (none paired, expected), Clipboard ok (PowerShell on Windows, pbpaste/pbcopy on macOS), Calls
warn `platform_unavailable` (no phone/call stack on a runner). Both macOS archs ran, including x86_64 under
Rosetta.

## Findings

Fixed (commit `d87aa8fff`, test `native_desktops_do_not_need_wayland_or_x11_variables`; existing session tests
now use the explicit-OS helper so they pass on every OS):
- `ctl doctor` on Windows and macOS warned "neither WAYLAND_DISPLAY nor DISPLAY" and recommended a systemd
  command. Wrong on those systems; it is now an ok line.

Not changed, worth knowing:
- Windows: `geekbridge-service --state-dir <existing directory>` is refused (`state_insecure: DACL is not
  protected, so it inherits permissions`) when the directory was pre-created in an inheriting location such as
  `%TEMP%`. A directory the service creates itself works. This is the intended fail-closed behaviour, but the
  error does not tell the user to let the service create it or to protect the ACL; consider a hint.
- The firewall check is Linux-only (always "ok, no ufw/firewalld" on Windows/macOS) and `Calls` shows the raw
  token `platform_unavailable`; cosmetic.
- The GUI windows themselves were not started (headless runners); only `--help` was executed. No real phone or
  LAN pairing was exercised on Windows/macOS.

## Local inspection

Downloaded with `gh run download 41569945066` into `/home/ambsd/.cache/gb-tmp/artifacts-run41569945066` (not
executed). Archives list the expected files; `file` shows Mach-O arm64 and x86_64 for the macOS builds.

| Archive | SHA-256 |
|---|---|
| geekbridge-macos-aarch64.tar.gz | 1a630ff7b0799b8994d3698275b282a2abc65ca6301fbab928e0bc6a167a6b3a |
| geekbridge-macos-x86_64.tar.gz | aab7e8e6978bcaff880de016cd2a61cf185180f6d1b66734e6ddc2676e043596 |
| geekbridge-windows-x86_64.zip | 2f43aabcdb5bf70fc16bdc4ee3012c35b9b64835e530d48ada2fa86f24266c53 |

These match the artifact `SHA256SUMS`.
