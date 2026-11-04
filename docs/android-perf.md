# Android footprint: measurements, budgets, how to reproduce

Measured 2026-11-01 on an **emulator** (AVD `gb36d`, Android 16 / API 36, x86_64, software GPU, host load average 25-40
from other jobs). Real phones differ: ARM code, a different zygote/ART image, real radios and Doze, a real battery
model. Treat every number as a regression baseline, not as a phone number. Reproduce with
`scripts/android-perf.sh` (stages: `size setup startup reconnect transfer bigxfer bigcancel idle nopair nopair_phone`; it uses its own AVD, adb server
port 16041, desktop service ports and scratch builds, and signs with a throwaway key, so it does not disturb other
emulators or `android/app/build`).

## Budgets

> Re-measured 2026-11-05 at 6a4122922: APK 7.10 MB / arm64-only 3.55 MB / dex 0.58 MB are over the budgets below, cold start median 877 ms, idle PSS 21 MB. See [reports/android-release-audit.md](reports/android-release-audit.md).

| Metric | Budget | Measured (release, R8) | Status |
| --- | --- | --- | --- |
| Release APK, phone + emulator ABIs | <= 6.5 MB | 6.12 MB | ok |
| Release APK, phone only (`-Pgb.abis=arm64-v8a`) | <= 3.5 MB | 2.99 MB | ok |
| `classes.dex` (release) | <= 0.5 MB | 0.35 MB (was 2.8 MB unshrunk) | ok |
| Cold start `am start -W` (HomeActivity) | <= 1000 ms (emulator) | 765 ms median of 7 (750-814, first 1107 once) | ok |
| Idle PSS, app closed, link service paired + connected | <= 35 MB | 22.8-23.4 MB just after closing; 26.5 MB after 30 min | ok |
| Threads, same state | <= 32 | 26-27 after closing, 31 after 30 min | ok |
| Growth over 50 reconnects (after GC) | <= 1 MB, no thread/fd growth | +0.2 MB, threads 27 -> 27, fds 112 -> 110 | ok |
| Alarms / jobs / wake locks owned by the app while idle | 0 | 0 / 0 / 0 | ok |
| Foreground service when nothing is paired | not running | not running (unpaired on the phone or on the desktop) | ok |
| 100 MB transfer leaves no growth | <= 1 MB PSS, no fd/thread growth | **Over budget on the first transfer only:** +4.6 MB after GC, then -3.5 to -0.9 MB on the next five; after-GC PSS stays 31.1-32.9 MB. No fd growth, threads +1 at most (see "Large transfers") | see note |

## Size

| | Before | After |
| --- | --- | --- |
| Release APK, both ABIs (same native libs) | 6 828 733 B (`isMinifyEnabled=false`) | 6 090 997 B unsigned (6 121 780 B aligned + signed) |
| `classes.dex` | 2.44 MB + 0.80 MB | 0.35 MB |
| Release APK, arm64-v8a only | n/a | 2 985 892 B |
| Debug APK | 7 182 959 B (unchanged) | |
| `libgeekbridge_android.so` arm64 / x86_64 (stripped, profile `android`) | 2 738 736 / 3 129 336 B | unchanged (already `opt-level=z`, fat LTO, `strip`, no `panic=abort` on purpose) |
| Resources (`resources.arsc` + `res/`) | 35 KB | unchanged |

Installed size (emulator, release): `/data/app/...` 6.4 MB (the APK; native libraries are stored uncompressed and
mapped from the APK, nothing is extracted) + 144 KB app data. Native libraries are 5.9 of the 6.1 MB, so the lever for
phones is the ABI split: `./gradlew -Pgb.abis=arm64-v8a assembleRelease` packages (and `verifyNativeLibs` checks) only
that ABI; the default stays phone + emulator. Compressing the libraries would cut the download by about 2.9 MB but
adds the extracted copy to the installed size and breaks the 16 KiB-aligned mmap-from-APK layout, so it is not done.

Changes: `android/app/build.gradle.kts` (release: `isMinifyEnabled`, `isShrinkResources`, new `-Pgb.abis`),
`android/app/proguard-rules.pro` (keep the JNI `NativeBridge` natives and the `*Action.INSTANCE` fields that
`QuickActions` loads with `Class.forName`). The release build ran pairing, reconnects, a 5 MB file receive and the
Home quick actions with R8 on.

Home-screen widgets (2026-11-02, [android-app.md](android-app.md#home-screen-widgets)): three previews (64 KB of PNG), seven
layout/drawable/xml resources and about 10 KB of dex. The release APK measured 6 313 629 B unsigned afterwards (budget 6.5 MB;
`classes.dex` 452 656 B, budget 0.5 MB), but that build also contains other people's concurrent native and Kotlin changes
(the `.so` files are not the sizes in the table above), so the widget share is the figure in this paragraph, not the delta to the
table. Runtime cost: no timer, no job, no wake lock; at most one inexact alarm and one Calendar `ContentObserver`, both only
while an Agenda widget is on the home screen; the worker thread exits 10 s after its last update.

## Runtime (release build, emulator)

* **Cold start:** 765 ms median (7 starts after `force-stop`: 765 778 750 761 814 773 764; one earlier set was 1107
  813 800 773 790 806 813 under heavier host load). Warm resume 43-66 ms. The previous UI-polish baseline was 925-1114 ms
  on the debug build.
* **Memory, app closed (task swiped away), `LinkService` running, paired and connected:** PSS 22.8-23.4 MB (Java 1.7-2.9,
  native 6.0-12), RSS about 129 MB, of which most is file-backed and shared (`RssAnon` about 57 MB). After 30 min idle,
  screen off: PSS 26.5 MB (Java 3.1, native 7.5), 31 threads, 110 fds, 9 sockets. The very first `idle.t0` snapshot
  has no PSS (the script parsed an empty `dumpsys meminfo` while the screen was turning off), so growth within the 30
  min is bounded by 23.4 -> 26.5 MB only; the 50-reconnect run, which does start from a clean closed state, stayed
  flat (below).
* **Threads / sockets:** the idle process has the ART and UI threads (about 21), binder threads and no extra Rust
  workers beyond what the link needs; open sockets 9-10 (TCP listener, UDP, the link, system sockets).
* **30 min idle, screen off, unplugged, app on the Doze whitelist:** `/proc/PID/stat` CPU 1.43 s (0.08 %), voluntary
  context switches 9709 -> 9723 (14 in 30 min). `dumpsys alarm` and `dumpsys jobscheduler` list the package only in
  the standby-bucket/exemption tables (no alarm or job of its own); `dumpsys power` shows no wake lock held by it;
  `dumpsys batterystats` shows no wake lock, alarm or job attributed to it, 30m36s of foreground service time and
  0.00117 mAh (the emulator's power model, meaningless in absolute terms). Raw dumps: `target/perf-run/raw/`.
* **50 reconnects (desktop disconnect + redial, app closed):** PSS 23 164 -> 23 373 KB after a forced GC (+209 KB),
  threads 27 -> 27, fds 112 -> 110, sockets 10 -> 10; flat from the 10th reconnect on. No leak seen.
* **Foreground service with nothing paired:** after the phone's own "Unpair" (Diagnostics) the service is gone while
  the screen is open and after closing the app, no service notification remains, and a simulated
  `MY_PACKAGE_REPLACED` does not restart it.

## Large transfers (`scripts/android-perf.sh bigxfer`, release build, 2026-11-01)

Six runs in one session, in the order listed, with PSS, threads and fds sampled every few seconds. "After GC" is taken 10 s after the end,
with the app closed. The SHA-256 of each completed file equals the source. Emulator numbers: `d2p` crosses the emulated Wi-Fi, `p2d`
the emulator's redirect on `eth0`, so the speeds say nothing about a phone.

| Run | Result | Time | Phone PSS KB: before -> peak -> after GC | Threads | Fds | Desktop RSS KB (peak) |
| --- | --- | --- | --- | --- | --- | --- |
| 100 MB desktop -> phone | SHA equal | ~7 s | 28170 -> 38213 -> 32777 | 30 -> 32 -> 30 | 144 -> 147 -> 113 | 6788 |
| 100 MB phone -> desktop | SHA equal | ~3 s | 35655 -> 37883 -> 32934 | 30 -> 32 -> 30 | 144 -> 175 -> 113 | 6640 |
| 500 MB desktop -> phone | SHA equal | ~26 s | 35642 -> 37237 -> 32126 | 30 -> 32 -> 30 | 144 -> 146 -> 112 | 6640 |
| 500 MB phone -> desktop | SHA equal | ~6 s | 32676 -> 36796 -> 31666 | 30 -> 33 -> 31 | 144 -> 177 -> 113 | 6708 |
| 500 MB desktop -> phone, cancelled at 128 MB | no partial file | ~16 s | 34229 -> 37876 -> 31109 | 31 -> 32 -> 31 | 144 -> 144 -> 112 | 6704 |
| 500 MB phone -> desktop, cancelled at 194 MB | no partial file | ~10 s | 32711 -> 36866 -> 31831 | 31 -> 34 -> 32 | 144 -> 175 -> 113 | 6704 |

* **Memory:** peak PSS stays under 38.3 MB at 500 MB, so nothing buffers a whole file. The first run's +4.6 MB after GC is measured from a
  low starting point and does not repeat or accumulate; its cause was not established.
* **Threads:** the slow rise across runs (30 -> 32) was checked by name: the extra ones are the framework's binder pool, not app threads.
  The app's own threads are unchanged apart from the new `geekbridge-link` wait thread (+1 compared with the earlier 26-27 idle figure;
  idle was not re-measured with it).
* **Fds and sockets:** sockets 12 -> 10 or 11, fds back to 112-113 after closing the app. No wake locks; foreground services 1 -> 1 throughout.
* **Listeners:** none left on the host afterwards.

## Open items found while measuring

1. **Resolved: the 100 MB and 500 MB transfers.** Root cause and evidence: [e2e-findings.md](reports/e2e-findings.md) E2E-14. The receiver bound its
   payload socket to the control link's address, and Android sent it out through the default network, whose NAT killed it after the first data.
   Other findings from the attempts: phone -> desktop through the Share sheet showed a stale result for every share after the first (E2E-16).
   Driving that in this harness needs the chooser tapped by label, a URI grant made as the shell user (a grant made by root does not let the
   app open the file), and a payload port free on both host and guest: another emulator may own 1739-1742 (`p2d_ports`).
   A leak check across a completed transfer is in "Large transfers" above.
2. **Resolved: unpairing on the desktop now stops the phone's service** while a link is up (E2E-15). A computer that unpairs while the phone
   is unreachable still cannot tell it.
3. An emulator ANR (SurfaceFlinger transaction stall, `Input dispatching timed out`) killed the app once under host load
   40; the trace shows the main thread inside `SurfaceControl.nativeApplyTransaction`, not app code.
4. Lint found a real `NewApi` error (`InputStream.readNBytes`, API 33, minSdk 26) in `PhoneDataAndroid.kt`; fixed with a
   bounded read loop. Unit tests (272) and `lintDebug` pass on the working tree.

## Not verified

Real phones (battery, Doze after hours, OEM task killers), the arm64 build at runtime, release signing, behaviour
over a real Wi-Fi roam, and payload routing on a real phone whose control link is on a non-default network.
