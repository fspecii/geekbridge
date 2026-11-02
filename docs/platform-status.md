# Platform status: what is verified on which OS

Written 2026-11-02. The author machine is Linux; Windows and macOS code has never run on a real Windows or Mac by
us. This table says, per capability, how far each OS has been checked. Do not read more into a row than its level.

Levels (a row can have several):

* **type-check**: compiles and passes `clippy -D warnings` for `x86_64-pc-windows-msvc`, `aarch64-apple-darwin`,
  `x86_64-apple-darwin` (`scripts/xcheck.sh`, also CI job `cross-targets`). Proves nothing about behaviour.
* **unit**: a host test exercises the logic with injected inputs (pure command-line builders, path rules, parsers),
  so it also runs on Linux. The OS call itself is not exercised.
* **CI**: `.github/workflows/check.yml` runs it on a `windows-latest` / `macos-latest` runner (build, `cargo test
  --workspace`, `scripts/smoke-binaries.sh`). **CI is configured but has not run yet** on these runners; until the
  first green run a CI cell means "will be exercised", and the first failures are expected to be informative, not
  surprising. Update this file after the first run.
* **device**: tried by a person on real hardware. Nothing below has this level for Windows or macOS.

| Capability | Linux | Windows | macOS | Notes |
|---|---|---|---|---|
| Build the workspace, all tests compile | CI + unit | type-check, CI | type-check, CI | `cargo build/test --workspace --locked` on all three |
| `cargo test --workspace` | CI | CI | CI | Unix-only tests are `cfg(unix)`; D-Bus tests are `cfg(target_os = "linux")` and skip with a reason without `dbus-daemon` (`GEEKBRIDGE_REQUIRE_DBUS=1` makes the skip a failure, set only on Linux CI); `hotkey_ctl` (Hyprland) is `cfg(unix)`; `wayland_clipboard` and `openssl_interop` are opt-in |
| Binaries start (`geekbridge-ctl --version`, `geekbridge-service --help`, `geekbridge-messages --help`) | CI | CI | CI | `scripts/smoke-binaries.sh` |
| Fresh-HOME behaviour (`ctl diagnose` fails cleanly, no panic) | CI | CI | CI | same script |
| Live service on private ports, `ctl status`/`diagnose` against it | CI | CI (non-blocking) | CI (non-blocking) | `smoke-binaries.sh --live`; the Windows and macOS legs report a red step without failing the job |
| Control API transport | CI | type-check, CI | type-check, CI | **Loopback TCP, 127.0.0.1 only**, not a Unix socket or named pipe. Every request is authenticated with a 256-bit token (HMAC challenge/response with domain separation, `bridge_core::ipc`), the token file lives in the private state dir. `ctl` finds it through `default_state_dir()` (same function for service and ctl) or `--state-dir` / `GEEKBRIDGE_STATE_DIR` |
| State directory location | unit | unit (path rule), type-check | unit (path rule), type-check | `%LOCALAPPDATA%\geekbridge\service`, `~/Library/Application Support/geekbridge/service`, `$XDG_DATA_HOME` or `~/.local/share`. `geekbridge-pair` now uses the same function (it used `$HOME/.local/share` on every OS before) |
| Private state permissions | unit + CI | type-check, CI (`fsutil::windows` tests: DACL, symlinks, locks, replace) | unit + CI | Unix: dir 0700, files 0600, owner and mode checked, symlinks refused. Windows: protected DACL with the current user and SYSTEM, SID verification, handle-based checks, `MoveFileExW(REPLACE_EXISTING \| WRITE_THROUGH)` |
| Atomic replace of an existing file | CI | type-check, CI | CI | temp file in the same directory + rename (Unix) / `MoveFileExW` (Windows) |
| Single instance | unit | type-check | type-check | Not a lock file: the service binds fixed TCP ports (control, discovery, inbound), so a second copy fails to start with an explained bind error. State files use a per-file exclusive lock (`fsutil::lock_exclusive`). The GUI apps are not single-instance on purpose |
| Default save folder | unit | unit, type-check | unit | `~/Downloads/GeekBridge`; Windows uses `USERPROFILE` before `HOME` (Git Bash sets `HOME` to a POSIX path); Linux honours `XDG_DOWNLOAD_DIR`. No `~` is expanded anywhere (paths come from the OS, never from user text) |
| Received file names | unit + CI | unit | unit | Separators of both kinds stripped, control characters and `<>:"\|?*` replaced (so `x:stream` cannot address an NTFS stream), trailing dots/spaces trimmed, reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`, also with trailing space or extension) prefixed, 255-byte limit. Names differing only by case never overwrite each other (`create_new` is atomic on case-insensitive volumes too) |
| Long paths (> 260) | CI | type-check | n/a | Rust's std applies the `\\?\` prefix itself; the 255-byte name cap keeps single components valid. Not exercised on a real Windows |
| Clipboard text | CI (X11/Wayland tools, opt-in live) | type-check | type-check | Windows via `powershell` (`Get/Set-Clipboard`, console window suppressed), macOS via `pbpaste`/`pbcopy` pinned to UTF-8 (`LC_CTYPE`), a LaunchAgent has no locale |
| Clipboard images | opt-in live | **not supported** | **not supported** | Reported as "unsupported" by the backend; text only. Needs Win32 `CF_DIB`/PNG or `NSPasteboard`; not done because it cannot be checked here |
| Incoming call / transfer / text notices | unit | unit (command builder), type-check | unit (command builder), type-check | Linux: `notify-send` with buttons. Windows: PowerShell toast (WinRT, app id of Windows PowerShell); macOS: `osascript display notification`. Text reaches the helper as an argument / environment variable, never as script source (unit-tested). Banners have no buttons: answering happens in the Calls / Share windows |
| Desktop call audio and control | CI (signalling only) + unit | type-check, host tests of the engine | type-check, 119 host tests with the mock feature | Real Bluetooth/HFP audio is device-only on every OS and has not been tested anywhere |
| Autostart | unit | unit (reg.exe command), type-check | unit (plist), type-check | `geekbridge-ctl autostart status/enable/disable`: XDG autostart file, LaunchAgent plist, `HKCU\...\Run` value. No admin rights; effective at next login |
| Firewall | n/a | unit (text) | unit (text) | `geekbridge-ctl firewall` prints the exact `netsh advfirewall` rules (admin PowerShell) or `socketfilterfw` commands; first-run OS prompts are the expected path. Ports TCP+UDP 1716-1764. Nothing changes the firewall by itself |
| GUI apps start without a console | n/a | type-check | n/a | Release builds on Windows use the GUI subsystem; helper processes started by them use `CREATE_NO_WINDOW`. A debug build keeps its console. (Consequence: `--help` of a release GUI app prints nothing on Windows) |
| Update check and download | CI (mock server) | CI | CI | Download + SHA-256 verified before anything is offered. Assets are `.zip` / `.tar.gz` archives, not installers: "open" reveals the verified file (`explorer`, `open -R`, `xdg-open`); nothing is run or installed by GeekBridge. A `.msi`/`.dmg` hand-off does not exist because those are not published |
| Hyprland hotkey, waybar | CI (unit) | n/a | n/a | Linux only |
| Packaging (MSIX/DMG/app bundle) | n/a | template only | scripts only | `packaging/windows`, `packaging/macos`: not built in CI |

## Known gaps (not fixed, by design or for lack of a machine)

* No Windows/macOS clipboard images, no notification buttons there.
* Windows `powershell` is Windows PowerShell 5.1 (always present); PowerShell 7 is not required.
* Toasts from an unpackaged app depend on Windows' notification settings (focus assist may hide them).
* The macOS `osascript` banner is attributed to "Script Editor" in Notification Center and needs that app allowed.
* Antivirus holding a freshly written file can make the final rename fail once; the transfer then reports an error
  instead of retrying.
* None of the above has been run by a person on a Windows PC or a Mac.

## Reading CI results

1. `Rust / windows-latest` and `Rust / macos-latest` are the first runtime signal. Start with the failing step name.
2. `Smoke test a live service` is allowed to fail on those two OSes; its log shows `service.log` on failure.
3. `Type-check Windows and macOS targets` must be green before and after any change to shared crates.
4. After the first green run, move the Windows/macOS cells above from "CI (pending)" semantics to plain CI and record
   the run URL here.
