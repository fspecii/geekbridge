# Windows and macOS code review (no machine, code reading only)

Scope: the OS-specific paths of the service, the desktop apps and the updater, read against what
`docs/platform-status.md` claims. Nothing here was run on Windows or macOS; the fixes are checked by host unit
tests where the logic is pure, and by the `cross-targets` type-check job and `build-binaries.yml` for the rest.

## What each OS gets (as the code stands)

| Area | Windows | macOS |
|---|---|---|
| Notifications | `powershell.exe` toast through WinRT, app id of Windows PowerShell, title/body in environment variables (`os_notify.rs`) | `osascript` `display notification`, texts as `argv` (`os_notify.rs`) |
| Tray | `Shell_NotifyIconW` on its own thread (`tray/windows.rs`) | `NSStatusItem` code exists but `start` declines (no main-thread run loop): no icon |
| Clipboard text | `powershell` `Get/Set-Clipboard`, UTF-8 console encoding | `pbpaste`/`pbcopy`, `LC_CTYPE=UTF-8` |
| Clipboard images, HTML | unsupported (text only) | unsupported (text only) |
| State dir | `%LOCALAPPDATA%\geekbridge\service`, protected DACL checked in `bridge_kde::fsutil` | `~/Library/Application Support/geekbridge/service`, 0700/0600 |
| Autostart | `HKCU\...\Run` through `reg.exe` | LaunchAgent plist |
| Firewall | `netsh advfirewall` read (English only), copyable commands | `socketfilterfw` read, copyable commands |
| Single instance | fixed TCP/UDP ports; second service fails to bind (no `SO_REUSEADDR` is set anywhere in the service, so Windows does not allow a second bind) | same |
| Window class | not relevant (tiling rules are Wayland `app_id` / X11 class, `dev.geekbridge.*`) | not relevant |
| Global hotkey | `RegisterHotKey` on a thread (`hotkey_bind.rs`) | none (`unsupported`; use a Shortcuts quick action running `geekbridge-ctl clipboard paste-from-phone`) |
| Hyprland-only pieces | `hotkey_bind::HyprlandBinder`, `hypr_config`, `waybar`, `session_env` are Linux-only and not reached | same |
| Updater | picks `geekbridge-windows-x86_64.zip`; reveals the verified file in Explorer, never runs or replaces anything | picks `.dmg` (release) / `.zip` / `.tar.gz`; reveals with `open -R` |

## Updater

* Asset choice (`bridge-update/src/release.rs`): an asset must carry an OS word and an architecture word and a
  known extension; Android, `sha256` and oversize files are never chosen. Windows prefers `.msi`, `.zip`, `.exe`, so
  the published `geekbridge-windows-x86_64.zip` wins over `...-setup.exe`. macOS prefers `.dmg` (published by
  `release.yml`), then `.zip`. `aarch64` Windows gets no asset (none is built; x86_64 under emulation is not offered).
* Verification: the download is rejected unless `SHA256SUMS` lists the exact asset name; size and digest are checked
  while streaming, and the digest is recomputed by `verified_installer()` right before the file is revealed.
* Replacing a running `.exe` on Windows: not applicable. GeekBridge does not install or replace anything; it
  reveals a verified archive. No rename-the-running-exe dance is needed until a real self-update exists.
* macOS quarantine: a file written by this process has no `com.apple.quarantine` attribute (only browsers and a few
  other apps set it), so Gatekeeper does not assess it when the user opens the dmg/zip from Finder. The app inside
  is therefore not checked for a notarization ticket either. This is convenient but means the updater gives no
  Gatekeeper protection; the SHA-256 is the only integrity check. Not changed.

## Defects found and fixed in this change

1. **Updater ignored macOS `.tar.gz`** (`release.rs`). `build-binaries.yml` packages the Mac archives as
   `geekbridge-macos-<arch>.tar.gz`, but the macOS extension list was `.dmg .zip .pkg`, so such a release would
   offer no download. `.tar.gz` added (ranked after `.zip`). Test `macos_tar_gz_archives_are_recognised`.
2. **Windows hotkey never ran `geekbridge-ctl`** (`hotkey_bind.rs`). `sibling_ctl()` looked for a file named
   `geekbridge-ctl`, which does not exist on Windows (`geekbridge-ctl.exe`), so it fell back to the bare name and
   only worked when the install folder was on `PATH`. It now uses the right file name, and the spawn uses
   `CREATE_NO_WINDOW` (ctl is a console program; every key press would have flashed a console window). Test
   `ctl_name_has_the_exe_suffix_only_on_windows`.
3. **Console window flashes from the desktop apps** (`bridge-core/src/firewall.rs`). The Home guide calls
   `firewall::read()` from a GUI-subsystem process, which runs `netsh` twice; each got its own visible console.
   `CREATE_NO_WINDOW` added.
4. **Service console stays open at login** (`bridge-service/src/main.rs`). `geekbridge-service.exe` is a console
   program, so the `Run` key start left a console (a Windows Terminal tab on Windows 11) open for the life of the
   service, and closing it kills the service. It now calls `FreeConsole()` when it is the only process attached to its
   console (a shell makes the count 2). Redirected output is not affected. Pure part tested
   (`a_console_shared_with_a_shell_is_kept`). A brief flash at login can still occur; only a GUI-subsystem build or a
   launcher removes it entirely.
5. **Tray window was message-only** (`tray/windows.rs`). Message-only windows receive no broadcast messages, so
   `TaskbarCreated` (explorer.exe restarted) never re-added the icon, and `SetForegroundWindow` does not work for
   them, so the popup menu would not close on an outside click. The window is now a hidden top-level window.
6. **Firewall verdict ignored Block rules** (`bridge-core/src/firewall.rs`). Choosing Cancel in the Defender prompt
   creates enabled Block rules for the program; with an allow rule also present the old check reported "fine".
   Block rules for the service program now win. Test additions in `windows_verdicts`.
7. **Risky-extension list** (`share_app.rs`) lacked several types that run code when "opened": `msc`, `jnlp`,
   `application`, `ws`, `vb`, `wsc`, `ps1xml`, `psc1`, `xll`, `wll`, `settingcontent-ms`, `appref-ms`,
   `library-ms` (Windows) and `scpt`, `scptd`, `terminal`, `tool`, `mobileconfig`, `webloc`, `inetloc`, `fileloc`
   (macOS). They are now revealed in the folder instead of opened. Test extended.

## Defects and gaps found, not fixed

* **Password-manager copies are synced on Windows and macOS.** Sensitivity can only be read on Linux (clipboard
  target `x-kde-passwordManagerHint`); elsewhere it is `Unknown`, and the default policy sends `Unknown` on automatic
  sync. Windows marks such copies with the formats `ExcludeClipboardContentFromMonitorProcessing` /
  `CanIncludeInClipboardHistory`, macOS with the pasteboard type `org.nspasteboard.ConcealedType`. Reading them needs
  a native clipboard API (or a per-poll PowerShell/osascript call); not implemented because it cannot be checked
  here. Mitigation today: automatic clipboard sync is opt-in per phone.
* **Clipboard polling on Windows starts `powershell.exe` every 1.5 s** (no change events there); each start costs
  roughly 0.2 s of CPU. A native `AddClipboardFormatListener` thread would remove it.
* **No clipboard images or HTML** on either OS (documented).
* `report::copy_to_clipboard` pipes UTF-8 into `clip.exe`, which uses the OEM code page: non-ASCII text in a
  diagnostics report would be garbled when copied through the CLI. The apps use egui's clipboard instead.
* `windows_host` / macOS call paths: Windows calling needs a packaged (MSIX) identity; macOS needs the separate
  `geekbridge-macos-calls` helper. Neither is in the `build-binaries` archives.
* macOS menu-bar icon inactive (documented). Windows ARM64 and macOS universal archives do not exist.
* `socketfilterfw --getappblocked` wording is read by meaning, never seen on a real Mac.

## Unknowns that need a real machine

1. PowerShell clipboard: no UTF-8 BOM in the output, `Set-Clipboard` with piped UTF-8 text keeps emoji and accents,
   `[Console]::OutputEncoding = UTF8` does not throw under `CREATE_NO_WINDOW` (hidden console).
2. WinRT toast through the PowerShell app id shows with Focus Assist off, and again after sign-out.
3. `osascript` banner shows (attributed to Script Editor; Notification Center permission for it).
4. `reg add ... /d "\"C:\Program Files\...exe\""` stores the quoted path; autostart `status` parses it back.
5. A DACL-protected `%LOCALAPPDATA%\geekbridge\service` is accepted when the parent was created by an installer
   running elevated or by a roaming profile.
6. Windows Defender prompt appears on first listen, and `ctl firewall` / Home guide agree with the result.
7. The hidden-window tray: icon appears, right-click menu closes on an outside click, icon returns after
   `taskkill /f /im explorer.exe && explorer`.
8. Unsigned binaries: SmartScreen (Windows) and Gatekeeper / ad-hoc signature requirement on Apple Silicon for
   the `.app` in the dmg (`build-app-dmg.sh --adhoc-sign` is not passed by `release.yml`).

## Manual test checklist (owner or contributor)

Windows 10/11, x86_64, from the `build-binaries.yml` zip, no admin rights:

- [ ] Unzip to a folder with a space in its path; run `geekbridge-ctl.exe --version`, then `geekbridge-service.exe`
      from a normal terminal. Defender prompt appears: Allow on private networks. `geekbridge-ctl doctor` is green.
- [ ] `geekbridge-ctl autostart enable`, sign out and in: the service runs, **no console window stays open**, `ctl status` answers.
- [ ] Tray icon visible; right-click menu opens and closes on an outside click; Quit works; restart explorer.exe and the icon returns.
- [ ] Pair a phone (QR from `geekbridge.exe`). Copy text with an accent and an emoji on the PC: it arrives; and the reverse.
- [ ] Incoming file: toast appears; file lands in `%USERPROFILE%\Downloads\GeekBridge`; a received `.exe` is only revealed.
- [ ] With clipboard sync on, press the paste-from-phone hotkey (default shown in the clipboard app) with the install
      folder NOT on `PATH`: text is pasted and no console flashes.
- [ ] Copy a password from a password manager: note whether it was sent to the phone (expected gap).
- [ ] `geekbridge-ctl firewall` output matches the Defender rules; run the printed `netsh` lines in an admin shell, status turns green.
- [ ] Updates: with a test release, "download" verifies and "show in folder" opens Explorer with the zip selected.

macOS 13+, arm64 and x86_64, from the `build-binaries.yml` tar.gz:

- [ ] `tar xzf`, then `xattr -l` on a binary (no quarantine expected if fetched by curl; a browser download will have it:
      note what the prompt says and the steps needed to open it).
- [ ] `./geekbridge-ctl --version`, start `./geekbridge-service`, accept the "incoming connections" dialog, `ctl doctor`.
- [ ] `geekbridge-ctl autostart enable`, then the printed `launchctl bootstrap` line; service runs; log out and in.
- [ ] Pair a phone; copy text with accents both ways (UTF-8 under a LaunchAgent with no locale).
- [ ] Incoming file and text banners appear (allow Script Editor in System Settings > Notifications if not).
- [ ] No menu-bar icon is expected; confirm nothing crashes. Hotkey is `unsupported`; create a Shortcuts quick action that runs `geekbridge-ctl clipboard paste-from-phone`.
- [ ] Updater on a test release: `.tar.gz` asset is offered and SHA-256 verified, "show in folder" selects it in Finder.

Report anything unexpected with `geekbridge-ctl diagnose` output (redacted) attached.
