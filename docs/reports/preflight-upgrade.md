# Pre-flight: upgrading the paired Fold5 and the PC service (2026-11-03)

Dated snapshot, not living documentation. Everything below was rehearsed on an emulator and in scratch directories; the
live service (ports 1716, 17381, 8000), its state directory and the phone were not touched. What could not be rehearsed
(the Samsung installer screens, Chrome's download warnings, the real Wi-Fi) is marked **not verified**.

## Verdict

The upgrade is safe **if you follow the steps below and not the quickstart table**. Three things in the old instructions
could have cost you the pairing; they are fixed in this change:

1. `scripts/realdevice.sh desktop --fresh` (quickstart step 6, test plan step 1) **wiped `target/realdevice-state`**, the
   directory that holds the paired phone and this computer's identity. It is now refused while pairings exist, and with
   `--forget-pairings` the old state is moved aside, never deleted.
2. `desktop` reused the running service. The one running now (pid started 03:03) is older than the binaries on disk
   (rebuilt 19:56), so you would have tested the old service. It is now refused with the hint `--restart`.
3. A build signed with another key cannot update the phone's app (`App not installed`, and the only way out is to uninstall,
   which deletes the pairings). `scripts/realdevice.sh install` now compares the signing key and versionCode with the app on
   the phone and stops before installing.

## Signing key: the served APK updates the phone's build

| Check | Result |
| --- | --- |
| Signer of `dist/android/serve/geekbridge-debug.apk` (the file port 8000 serves, rebuilt 19:50) | SHA-256 `4d711ab401391ad4041c7dadd070371eb658d303a08b39de29d1df690a9ca3c3` |
| Every other APK built here (32 signed APKs: all debug builds since 2026-11-02, the rc.1 release APKs `geekbridge-android.apk` and `-arm64.apk`, the v0.0.1 dry run, the worktree builds) | all `4d711ab4...`, none different |
| Key behind it | `.tools/android-user/debug.keystore` (what `scripts/android-env.sh` sets as `ANDROID_USER_HOME`). The other debug key on this machine, `~/.android/debug.keystore` (`90:12:A8:EF...`), signed none of them |
| A clean worktree build (`scripts/build-android.sh`, gradle with tests and lint, exit 0) | `4d711ab4...` as well |

So the debug build on the phone (installed from this machine) and rc.1 both have the same signer as what you serve now.
The risk is a build made **outside** `scripts/android-env.sh` (Android Studio, a bare `gradle assembleDebug` in a fresh
shell): it would be signed with `~/.android/debug.keystore` and refused by the phone.

How to tell, before touching the phone:

```bash
cd /home/ambsd/.t3/projects/geekbridge
scripts/e2e/upgrade-preflight.sh signer --apk dist/android/serve/geekbridge-debug.apk      # prints the digest; expect 4d711ab4...
# with the phone on USB: compare with what is installed (realdevice.sh install does this for you and stops on a mismatch)
adb shell pm path dev.geekbridge.android            # package:/data/app/.../base.apk
adb pull <that path> /tmp/installed.apk && scripts/e2e/upgrade-preflight.sh signer --apk /tmp/installed.apk --apk dist/android/serve/geekbridge-debug.apk
```

Version numbers: every debug build is `versionCode 10099` (`0.1.0`); the rc.1 release APK is `10051` (`0.1.0-rc.1`).
Same code over same code is allowed (Android replaces the app, data kept), a lower code is refused. **Do not scan
`dist/android/release-qr.png`** on a phone that has the debug build: it points to the rc.1 arm64 APK (code 10051), which Android
refuses as a downgrade. `apk-qr.png` decodes to `http://192.168.1.150:8000/geekbridge-debug.apk`, which is right.

Because the version label stays `0.1.0`, the screen cannot tell you the update happened. Evidence that it did:
`adb shell dumpsys package dev.geekbridge.android | grep lastUpdateTime`, or `scripts/realdevice-report.sh --page
http://192.168.1.150:8000/geekbridge-debug.apk --apk dist/android/serve/geekbridge-debug.apk` (the "served APK" line says
same as installed), or `geekbridge-ctl phone-state` (debug consent is on) which names the commit.

`dist/android/SHA256SUMS` is stale (it lists `a1e87c2e...`, the served file is `fb0aab3a...` at review time; whoever copied the
APK to `serve/` did not refresh it). It is not part of any step below; `build-android.sh` and `realdevice.sh install --build`
refresh it, a manual `cp` does not.

## What was run

All results are in `dist/android/logs/upgrade-preflight-results.tsv` of the worktree (not committed; `dist/` is ignored). The
script is `scripts/e2e/upgrade-preflight.sh` (own AVD `gb36u`, console 5724, adb server 16124, service ports 17580/17581/17516/1830).

**Phone, emulator, real pairing, then `adb install -r` over the old app** (what the package installer does with a downloaded APK):

| Scenario | Old app | New app | Result |
| --- | --- | --- | --- |
| A | earlier real-device debug build (2026-11-02 17:35, debuggable, code 10099) | worktree build of origin/main (10099) | 22 PASS, 0 FAIL (1 SKIP: downgrade, same code) |
| B | rc.1 release APK (R8, not debuggable, code 10051) | same | 18 PASS, 0 FAIL (1 SKIP: private files cannot be read from a release build, checked through behaviour instead) |
| first run | the same earlier debug build | the APK served at the time | PASS apart from three harness defects fixed afterwards |

Checked in both: a real pairing (the desktop dials the phone, both sides compare the code); desktop consents sms, clipboard,
contacts; in A also the phone's per-computer opt-ins (`geekbridge_grants`), auto-accept, the device name and the sha256 of every
private file; after the upgrade: the new app **restarts its link by itself within 8 s** (`MY_PACKAGE_REPLACED`), no crash in the
crash buffer, the computer reconnects **without a pairing code**, the desktop still lists the phone, consents are all still there
(the upgrade adds the `debug` consent when a debuggable build connects, that is expected and was the only change), the trust
record (certificate pin) is byte-identical, ping works, installing the same APK again works, an APK signed with a throwaway key is
refused with `INSTALL_FAILED_UPDATE_INCOMPATIBLE`, a lower versionCode with `INSTALL_FAILED_VERSION_DOWNGRADE`, and after both
refusals the link is still connected without a code. `adb install -r -d` of rc.1 over the debuggable build works (the way back, see below).

**PC service, real state copied**: `target/realdevice-state` (SM-F946B, `sms clipboard telephony debug`, receive on, 1 pinned
certificate `209341ad...`) copied to scratch, the new service started on the copy with `--no-announce` and loopback-only listeners
(so the real phone never sees it): it opens (no state notes, no `.corrupt` file), lists SM-F946B as paired, consents, certificate
pin, `identity.pem` and `share.json` identical, and the sha256 of every file in the original is the same before and after. New
features are off for the existing pairing (for example *photos*): turn them on deliberately (`geekbridge-ctl consent <id> photos on`
and the phone's per-computer switch). `geekbridge-ctl doctor` on the copy failed one check: **ufw has no rule for 1716-1764**
(`/etc/ufw/user.rules`; 8000 is allowed from 192.168.1.0/24). That is step 3 below.

Unit and script tests added (written before the code, run in CI): `scripts/tests/apk-signer-test.sh` (21 checks: signer parsing,
verdicts ok / same-version / downgrade / other-key / unsigned, and `realdevice.sh install` refusing a different key or a downgrade
before it reaches `adb install`) and `scripts/tests/run-desktop-test.sh` (13 checks: `--fresh` refused with pairings and
harmless without, `--forget-pairings` keeps the old state, a stale service is refused, `--restart` keeps the state).

## Safe upgrade steps (about 20 minutes)

Run in a normal terminal inside Hyprland, in `/home/ambsd/.t3/projects/geekbridge`. Do not run `desktop --fresh`.

| # | Step | Command | Expect |
| --- | --- | --- | --- |
| 1 | Is the served APK current? | `git log -1 --format=%cd -- android crates/bridge-android crates/bridge-core`, then compare with `stat -c %y dist/android/serve/geekbridge-debug.apk`. If a source commit is newer than the APK: `scripts/build-android.sh`, then `cp dist/android/geekbridge-debug.apk dist/android/serve/ && (cd dist/android && sha256sum geekbridge-debug.apk > SHA256SUMS)` (copy when nobody is downloading) | APK newer than the last Android/core commit; `scripts/e2e/upgrade-preflight.sh signer --apk dist/android/serve/geekbridge-debug.apk` prints `4d711ab4...` |
| 2 | Back up the PC state with the service stopped | `scripts/run-desktop.sh --stop && cp -a target/realdevice-state target/realdevice-state.pre-upgrade` | `stopped service`; a copy exists |
| 3 | Firewall for the phone link (ufw has no GeekBridge rule now) | `scripts/realdevice.sh firewall --apply` | `PASS ufw: ... rules for 1716:1764 tcp+udp present` |
| 4 | Binaries current? | `cargo build --release --locked -p bridge-apps -p bridge-service` (no-op if nothing changed since 19:56) | `Finished` |
| 5 | Start the new service on the **same** state | `scripts/realdevice.sh desktop` (it also prints a QR; do not scan it, you are already paired). Close any old GeekBridge home window first, a new one opens | `service up`; `eval "$(scripts/run-desktop.sh --env)"; geekbridge-ctl status` lists SM-F946B under `paired` (link may say not connected until the phone redials) |
| 6 | Phone on the same Wi-Fi (192.168.1.x, no VPN). Chrome: open the URL, or scan `dist/android/apk-qr.png` | `http://192.168.1.150:8000/geekbridge-debug.apk` | Chrome downloads `geekbridge-debug.apk` (8 MB) after "this type of file can harm your device": **Keep / Download anyway** (**not verified on the Fold5**) |
| 7 | Open the downloaded file. First time from Chrome, Android says unknown apps are not allowed from this source | **Settings**, allow *Install unknown apps* for Chrome, go back | installer page "Do you want to update this app?" (**not verified**) |
| 8 | Tap **Update** (not Uninstall). Play Protect "Scan app?": *Don't send* or *Install anyway* | | "App updated", then **Open** |
| 9 | Prove it is the new file | `adb shell dumpsys package dev.geekbridge.android \| grep lastUpdateTime`, or `scripts/realdevice-report.sh --page http://192.168.1.150:8000/geekbridge-debug.apk --apk dist/android/serve/geekbridge-debug.apk` | time of step 8 / "same as installed" |
| 10 | Open the app once and look | `geekbridge-ctl status`, `geekbridge-ctl ping <phone id>` | Home shows the computer name and ONLINE within about 30 s; no pairing dialog; link phase `connected`; ping answers |
| 11 | Continue with `docs/real-device-test-plan.md` from step 3 (you are already paired, skip step 2) | | the checklist's own expectations |

Wired variant (replaces 6-8): `scripts/realdevice.sh install`. It prints `update path: same signing key ..., versionCode 10099 ->
10099`, installs, grants the permissions adb may grant and starts the app.

## What to do if ...

| If | Cause and fix |
| --- | --- |
| Step 7/8 says **App not installed** (or "...as package conflicts with an existing package") | Different signing key (`INSTALL_FAILED_UPDATE_INCOMPATIBLE`), or a lower versionCode (you scanned `release-qr.png`). On USB, `scripts/realdevice.sh install` names which and stops. Rebuild with `scripts/build-android.sh` (it uses the project key), copy to `serve/`, retry. Only if the phone's copy is signed with a key you do not have: `adb uninstall dev.geekbridge.android` loses the pairing; then pair again by QR and `geekbridge-ctl unpair <old phone id>` (the phone gets a new id). |
| Chrome offers no *Install unknown apps* / the installer is blocked outright | Samsung **Auto Blocker** (Settings > Security and privacy > Auto Blocker, One UI 6.1+) blocks sideloading and blocks USB commands; switch it off for the test and on again after (**not verified**). Work-profile or parental restrictions do the same. |
| Chrome refuses or deletes the download | Use *Keep*; or download on the PC and `adb install -r dist/android/serve/geekbridge-debug.apk`. Page not found / times out: same Wi-Fi? (`ufw` allows 8000 only from 192.168.1.0/24); `curl -I http://192.168.1.150:8000/geekbridge-debug.apk` from the PC must say 200. |
| The version still says 0.1.0 afterwards | Normal: all debug builds are 0.1.0 / 10099. Use step 9. |
| Home says "Waiting for ..." after the update | Open the app (the update restarts the link by itself, a force-stop does not). Then `geekbridge-ctl test-connection`; the usual causes are the firewall rule from step 3 and discovery after a PC restart (quickstart T1, T5). The pairing is not the problem unless the next row applies. |
| The phone shows a pairing code / the PC lists no phone | The trust record is gone on one side. PC side: `scripts/run-desktop.sh --stop`, `rm -rf target/realdevice-state && cp -a target/realdevice-state.pre-upgrade target/realdevice-state`, `scripts/realdevice.sh desktop`. If the phone side lost it, scan a fresh QR (`scripts/realdevice.sh desktop` prints one; consents on the PC stay). Not seen in any rehearsal. |
| `desktop` says the running service is older than its binary | `scripts/realdevice.sh desktop --restart` (stops, keeps the state, starts the new binary). |
| `desktop --fresh` says it would forget 1 paired phone | Correct. Do not use it for an upgrade. To start over on purpose: `--fresh --forget-pairings`; the old state is kept in `target/realdevice-state.before-fresh-<time>` (stop the service and `mv` it back to undo). |
| Service log shows `state_newer`, `reset`, `recovered` or `corrupt` | `docs/state-formats.md` (rules 4 and 6). Nothing was lost by the rehearsal on the real state. The `.bak` next to each file is the previous generation; `cp <file>.bak <file>` with the service stopped. |
| You need the previous app build back | Installed build is debuggable, so a downgrade keeps the data: `adb install -r -d <older.apk>` (rc.1 over debug works, tried on the emulator). Without adb Android refuses; uninstalling loses the pairing. |
| `geekbridge-ctl status` says connected but a feature is off after the update | New features start off for an existing pairing (photos, notification mirroring): `geekbridge-ctl consent <id> <feature> on` plus the phone's switch for this computer. Existing consents were all kept. |
| Something else | `scripts/realdevice-report.sh --page http://192.168.1.150:8000/geekbridge-debug.apk --apk dist/android/serve/geekbridge-debug.apk` and `scripts/realdevice.sh logs`. |

## Not covered

The Samsung package-installer screens, Chrome's download warnings, Auto Blocker and Play Protect behaviour (no hardware here);
the in-app updater's installer hand-off for this exact pair (covered earlier, `docs/reports/e2e-findings.md`); an old service
binary reading the new state in reverse (the formats did not change: `docs/state-formats.md`, golden v1 fixtures).
