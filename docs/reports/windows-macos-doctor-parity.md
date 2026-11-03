# Windows and macOS parity for doctor, autostart and state-dir help

## What exists / what changed

(a) Firewall (`crates/bridge-core/src/firewall.rs`, `crates/bridge-service/src/doctor.rs`)
- Windows: `read()` runs `netsh advfirewall show allprofiles state` and `netsh advfirewall firewall show rule name=all dir=in` (read-only, no elevation). Pure parsers `parse_netsh_state`, `parse_netsh_rules`, `windows_allows`. Fine when Defender is off or an enabled inbound Allow rule covers TCP and UDP 1716-1764 for geekbridge-service.exe (the program rule the Defender prompt creates, or a program-less port rule). Blocking otherwise. Unverified if the output has no English `Rule Name:` keys (localized Windows).
- macOS: `socketfilterfw --getglobalstate` and `--getappblocked <service>`; `parse_mac_state` (State = 0/1/2), `parse_mac_app` (permitted / blocked / "not part of"). Off = fine; block-all or blocked = fail; on and not listed = warn with guidance for the "Allow incoming connections" dialog.
- `Tool` gained `WindowsDefender` and `MacAppFirewall`; `commands_for(tool, exe)` gives the copyable `netsh advfirewall firewall add rule ...` lines (elevated PowerShell) and the `sudo socketfilterfw --add/--unblockapp` lines; `guidance(tool)` explains the dialog. Nothing is ever run elevated. The desktop Home guide and `geekbridge-ctl firewall` use the same text.

(b) Autostart (`autostart.rs`) already existed on all three systems (XDG `.desktop`, LaunchAgent plist, HKCU `Run` value via reg.exe). Added: `parse_reg_query`, `plist_program`, `stale_note` (status warns when the entry starts a different copy), `launchctl_commands` (start/stop now, user domain, no sudo, shown after enable/status on macOS). Startup-folder shortcut was not added: the Run key needs no extra tooling.

(c) `state_insecure` on startup now also prints `private::insecure_hint`: on Windows why (inherited ACL, e.g. %TEMP%), to choose a not-yet-existing `--state-dir`, or protect with `icacls "<dir>" /inheritance:r /grant:r "%USERNAME%:(OI)(CI)F" "SYSTEM:(OI)(CI)F"`; on Unix the chmod fix. Terminal only, never sent to clients.

Tests: fixture-based unit tests for all parsers, verdict tables, exact command text, plist shape and round trip. Fuzz target `doctor_ufw` also feeds the new parsers.

## Checks
fmt, `clippy --workspace --all-targets -D warnings`, workspace tests all exit 0 on Linux. `scripts/xcheck.sh` could not run locally (no rustup, targets not installed); the CI type-check job is the cross-check.

## CI
- build-binaries run 41587141284 (all jobs green). `doctor` Firewall line: Windows "Windows Defender Firewall is on and has an inbound rule for geekbridge-service on TCP and UDP 1716-1764" (the runner probably matched a broad existing allow rule, not a GeekBridge one: not proof the program rule path works); macOS arm64 and x86_64 "application firewall is off". The block and unlisted paths are covered only by fixtures.
- The commit 821511aba broke the type-check and test jobs: I staged two fuzz-target hunks at the wrong lines (another agent had uncommitted edits in the same file). Fixed in the next commit (verified in a clean worktree with clippy).

## Not done / caveats
- `socketfilterfw --getappblocked` wording is parsed by meaning from memory of its output; real output was not captured. Verify on a Mac with the firewall on.
- Onboarding text still says "GeekBridge never runs sudo for you" (en.properties `onboarding.fw.never_runs`) and "terminal" on Windows; left to avoid golden churn.
