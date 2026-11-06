# Android footprint: measurements, budgets, how to reproduce

Measured 2026-11-01 on an **emulator** (AVD `gb36d`, Android 16 / API 36, x86_64, software GPU, host load average 25-40
from other jobs). Real phones differ: ARM code, a different zygote/ART image, real radios and Doze, a real battery
model. Treat every number as a regression baseline, not as a phone number. Reproduce with
`scripts/android-perf.sh` (stages: `size setup startup reconnect transfer bigxfer bigcancel idle nopair nopair_phone`; it uses its own AVD, adb server
port 16041, desktop service ports and scratch builds, and signs with a throwaway key, so it does not disturb other
emulators or `android/app/build`).

## Budgets

> Re-measured 2026-11-06 at 0af48d0ae (see "Re-measurement 2026-11-06" below): universal APK 7.13 MB unsigned, arm64-only 3.56 MB, dex 0.59 MB, all inside the budgets; cold start unchanged against 6a4122922 on the same AVD (A/B); idle PSS 26.7 MB, 32-33 threads, no alarm, job or wake lock of its own. Earlier: 6a4122922 measured 7.10 / 3.55 / 0.58 MB, cold start 877 ms, idle PSS 21 MB. See [reports/android-release-audit.md](reports/android-release-audit.md).

**Enforced in CI.** The budgets for the first three rows live in one file, `scripts/android-budgets.env`, in exact decimal bytes
(**MB here is 10^6 B, not MiB**: 3 560 987 B is quoted as 3.56 MB, which only works as bytes / 10^6): universal APK 7 250 000 B, arm64-only
APK 3 600 000 B, `classes.dex` 600 000 B. `scripts/android-size-check.sh` reads it, prints a table (bytes, budget, headroom) and exits 1 on
overflow; it runs in the CI job "Android / unit tests, lint, debug APK" (step "Release APK size budgets", also written to the job summary)
and, for the universal APK, at the end of `scripts/android-perf.sh size`. CI builds the **release** variant (R8 and resource shrinking on, unsigned,
same CI-profile native libraries as the debug APK there) twice: universal, then `-Pgb.abis=arm64-v8a`. Only packaging and the final steps rerun
the second time (7 s locally), and Gradle caches and wrapper are cached by `actions/cache`. Measured values are unsigned (zipalign + signing adds
about 10 kB); `classes.dex` is the sum of the uncompressed `classes*.dex` entries. Raising a budget means editing the env file and explaining the
raise in "Size" below in the same commit.

| Metric | Budget | Measured (release, R8) | Status |
| --- | --- | --- | --- |
| Release APK, phone + emulator ABIs | <= 7.25 MB (was 6.5) | 7.10 MB (7 102 616 B, CI profile, 2026-11-05) | ok |
| Release APK, phone only (`-Pgb.abis=arm64-v8a`) | <= 3.6 MB (was 3.5) | 3.55 MB (3 549 719 B) | ok |
| `classes.dex` (release) | <= 0.6 MB (was 0.5) | 0.58 MB (576 680 B; 2.8 MB unshrunk) | ok |
| Cold start `am start -W` (HomeActivity) | <= 1000 ms (emulator) | 765 ms median of 7 (750-814, first 1107 once) | ok |
| Idle PSS, app closed, link service paired + connected | <= 35 MB | 22.8-23.4 MB just after closing; 26.5 MB after 30 min | ok |
| Threads, same state | <= 32 | 26-27 after closing, 31 after 30 min | ok |
| Growth over 50 reconnects (after GC) | <= 1 MB, no thread/fd growth | +0.2 MB, threads 27 -> 27, fds 112 -> 110 | ok |
| Alarms / jobs / wake locks owned by the app while idle | 0 | 0 / 0 / 0 | ok |
| Foreground service when nothing is paired | not running | not running (unpaired on the phone or on the desktop) | ok |
| 100 MB transfer leaves no growth | <= 1 MB PSS, no fd/thread growth | **Over budget on the first transfer only:** +4.6 MB after GC, then -3.5 to -0.9 MB on the next five; after-GC PSS stays 31.1-32.9 MB. No fd growth, threads +1 at most (see "Large transfers") | see note |

## Size

Budgets raised 2026-11-05 (measured with the CI recipe: `--profile android`, `--icf=all`, `llvm-strip --strip-unneeded`, R8). The growth is
feature code, not a dependency or a leftover: arm64 library 2.74 -> 3.06 MB (x86_64 3.13 -> 3.55 MB), `classes.dex` 0.35 -> 0.58 MB across
~110 Kotlin files (calls, messages, calendar, widgets, updater), resources 111 kB `resources.arsc` + 60 kB of widget PNGs. The Wi-Fi call audio
code is not in the release library (no `opus`/`callwifi` strings; the `callwifi` feature is debug-only). Checked and not worth it:
`panic = "abort"` would drop most of the ~830 kB of unwind tables per library (`.eh_frame`, `.eh_frame_hdr`, `.gcc_except_table`) but lets one
bad request kill the app process instead of becoming an error reply, so it stays `unwind` on purpose; `-repackageclasses` and
`-allowaccessmodification` in R8 saved 1 kB of dex; `clap` in `bridge-kde` is only used by a bin and is not linked into the library. Dropping
x86_64 from the published APK is not done: the release docs publish the universal APK (which the updater reads) next to the phone-only one, and
the universal one is the emulator-capable build; `-Pgb.abis=arm64-v8a` (`geekbridge-android-arm64.apk`) is the phone-only artifact.

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

## Budget check at 803dda57d (2026-11-06)

First measurement through `scripts/android-size-check.sh` (scratch worktree, own Cargo target dir, CI recipe, release R8, unsigned): all three inside
the budgets, nothing to cut and no budget raise.

| Metric | Budget (B) | 803dda57d (B) | Headroom (B) |
| --- | --- | --- | --- |
| Release APK, universal | 7 250 000 | 7 133 956 | 116 044 |
| Release APK, arm64-v8a only | 3 600 000 | 3 563 443 | 36 557 |
| `classes.dex` | 600 000 | 595 236 | 4 764 |

`classes.dex` has the least room (0.8 %): the next Kotlin feature will probably trip it, and the CI failure is the prompt to look for savings
first. Already checked and exhausted: R8 and `shrinkResources` are on, `DebugNetHold` and the call-audio code are debug-only and absent from
the release dex, the library is stripped with a single ABI copy each, and the 112 kB `resources.arsc` plus 60 kB of widget PNGs are the only
resources of note. The APK is 86 % native library by size, so the dex budget is the one that binds.

### Dex savings 2026-11-06: Kotlin null-check intrinsics stripped from the release dex

`classes.dex` 594 196 B -> 567 180 B (-27 016 B, 4.5 %), headroom 5 804 -> 32 820 B; the budget stays 600 000. About 1 500 call sites of
`Intrinsics.checkNotNullParameter` / `checkNotNullExpressionValue` (a string constant plus an invoke each, and the parameter-name strings) were
the largest single item. `proguard-rules.pro` now has an `-assumenosideeffects` rule for those four methods (release only: debug builds and
unit tests keep the checks; `!!` and `lateinit` checks are not touched). Behaviour: they fire only when Java or the framework passes null into
a non-null Kotlin parameter, and without them the same null throws a little later at the first use.

Analysed and left alone: the dex is 340 kB of code in 2 260 methods (all app code; Kotlin stdlib is 28 kB), 65 kB of strings, 28 kB method ids;
debug-only code (`DebugNetHold`, call audio) is already absent; `-Xlambdas=class -Xsam-conversions=class` made the dex 2.7 kB larger and
`-Xstring-concat=inline` 0.1 kB larger; only two keep rules exist (`NativeBridge` natives, `*Action.INSTANCE`) and neither is broad. Checked with
the CI recipe (universal and arm64-only release; `scripts/android-size-check.sh` dex row ok, the APK rows only differ from CI because the local
native libraries are not the CI profile), `testDebugUnitTest`, `lintDebug`, and a smoke of an R8-minified, debuggable, debug-signed build of the
same rules on a private API 36 clone: install, first-run wizard, Home render, no app crash in logcat. The pairing e2e stages could not be used
as a check (the stale `scripts/e2e-android.sh` wizard navigation fails on the current first-run screens), so link/SMS/calls/share/update-check
are covered by the unchanged keep rules (the new rule only removes null-check calls), not by a run.

## Re-measurement 2026-11-06 (0af48d0ae, release, R8)

Same recipe as the audit (`scripts/android-perf.sh size`, CI profile native libraries, R8, throwaway-signed), on a private AVD clone
(API 36 google_apis x86_64, 2 GB RAM, own console/adb/service ports). New since 6a4122922: Update-available chip and 12 h check, first-run
changes, accessibility labels, SMS observer and receiver set, `GeekBridgeInCallService`, share parallel prefetch (`DebugNetHold` is debug-only and
not in the release dex).

| Metric | Budget | 6a4122922 | 0af48d0ae | Status |
| --- | --- | --- | --- | --- |
| Release APK, both ABIs, unsigned | <= 7.25 MB | 7 102 616 B | 7 131 300 B (+28.7 kB; 7 142 047 B aligned + signed) | ok |
| Release APK, arm64 only, unsigned | <= 3.6 MB | 3 545 623 B | 3 560 987 B (+15.4 kB) | ok, 39 kB left |
| `classes.dex` | <= 0.6 MB | 576 680 B | 590 644 B (590 884 B with the lazy SMS thread below) | ok, 9 kB left |
| `libgeekbridge_android.so` arm64 / x86_64 | | 3 058 896 / 3 552 000 B | 3 073 184 / 3 563 224 B | |
| `resources.arsc` | | 111 kB | 112 kB | |
| Cold start `am start -W` | <= 1000 ms (old AVD, load 25-40) | 877 ms median (load 15-97) | **1577 ms, same as 6a4122922's 1593 ms on this AVD** (A/B below) | no regression; this AVD is slower |
| Idle PSS, paired + connected, app closed, screen off | <= 35 MB | 22.8-26.5 MB (30 min) | 24.4 MB at start, 26.7 MB after 1 to 10 min, flat (26 658 to 26 682 kB, minute 1 to 9) | ok |
| Threads, same state | <= 32 | 26-27 closed, 31 after 30 min | 33 (32 with the lazy SMS thread), 115 fds, 10 sockets | 33 was 1 over; 32 with the lazy SMS thread is at the budget |
| Alarms / jobs / wake locks owned while idle | 0 | 0 / 0 / 0 | 0 / 0 / 0 | ok |
| Foreground service, nothing paired | not running | not running | not running after the computer unpairs (0 `LinkService` records, no listener on 1716) | ok |

* **Cold start, measured against the old build on the same AVD.** This fresh AVD is about 1.6x slower than the one behind the 877 ms figure (a system app, Settings,
  cold-starts in 905-1249 ms here, and the guest swaps), so the absolute number is not comparable. Interleaved, same AVD, `--no-incremental` installs, 3 rounds
  of 8 cold starts each: 6a4122922 median 1593 ms (1425-1720), 0af48d0ae median 1577 ms (1425-1832). The new Application.onCreate work (What's new check, update
  state) is on the same worker or a few preference reads; nothing measurable on the main thread. The old APK rebuilt from 6a4122922 is byte-identical in size
  to the audit's (7 102 616 B), so the A/B is of the audited build.
* **Idle, 10 min, screen off, unplugged** (`idle` stage, `IDLE_MIN=10`): PSS sampled every minute 26 658-26 682 kB (Java 3.6 MB, native 5.5 MB), RSS 132 MB, 33 threads,
  115 fds. CPU 36 ticks (0.36 s) over the 10 min, 270 voluntary context switches. A second 5 min run on the build with the lazy SMS thread: PSS 25.1 -> 27.3 MB,
  32 threads, 6 ticks.
* **Alarms, jobs, wake locks, battery stats:** `dumpsys alarm` and `dumpsys jobscheduler` mention the package only in the standby-bucket and exemption tables (the
  `idle.alarms` / `idle.jobs` / `idle.wakelocks` counts the script prints are line counts of those tables and of `dumpsys power`'s acquire/release history, not live
  objects: `Wake Locks: size=4` held nothing of ours). `dumpsys batterystats`: no wake lock, no alarm, no job for the uid; `Foreground services: 10m 24s`, CPU 437 ms
  (user 358 ms, system 79 ms) over 10 min, mobile traffic of the whole device 2.5 kB in. The Update-available notice has no timer of its own: `UpdateNotice.maybeCheck` runs when Home opens and when the link service starts, gated to once a day (`UpdateNoticePolicy`, 7 days on a metered network), so it adds no alarm or job (none was seen in the 10 min). The only `AlarmManager`/timer code remains the Agenda widget tick.
* **Messages sync, 2 000 messages in the provider (100 threads x 20, written into `mmssms.db`):** the conversation list (100 threads) reaches the computer within 3 s
  of the link coming up. Phone PSS 32.8 MB before (just after starting the app, 9.0 MB Java) -> 33.3 MB right after the list -> 32.2 MB with the app open and 10
  threads refreshed -> 31.8 MB after closing the app and a GC; back to 27.3 MB after the history refresh burst and a GC. High-water mark unchanged (137 MB
  VmHWM, shared mappings). The 2 000 rows are not held in memory afterwards. The desktop's per-thread history paging
  (`messages-refresh DEVICE THREAD`) still showed one message per thread after 10 s in this run, so the full 2 000 never crossed the link here; that path is not
  measured on a device.
  **Reproduced and fixed (private API 36 emulator, 100 threads x 20 texts = 2 000 rows written through the SMS provider with explicit
  `thread_id`, `date`, `address`, `body`, type 1 and 2):** the earlier "one message per thread" was a real bug, two of them.
  (1) The phone read the provider with `QUERY_ARG_SORT_COLUMNS = [date, _id]` plus one `SORT_DIRECTION_DESCENDING`; the framework turns that
  into `ORDER BY date, _id DESC`, so the date came oldest first. The list carried the OLDEST text of each thread (`t100_m1` instead of
  `t100_m20`), the desktop then asked for `date <= oldest` and got the same one row back. Fixed with an explicit
  `date DESC, _id DESC` SQL sort (`newestFirstSql`, `SmsSortTest`); it only hid on small test phones. (2) With the sort fixed, 100
  `messages-refresh THREAD` calls issued back to back (0.2 s) landed 17 threads short (1 677 of 2 000, stuck): the phone core keeps 64
  undelivered requests and drops the oldest beyond that. The desktop now keeps at most 16 history requests out per phone and sends the
  rest as answers arrive (`PhoneData::pump_older`; `a_burst_of_refreshes_never_has_more_than_a_few_requests_out`); asking for a thread
  again while its answer is out re-sends at once. The send queue never reported "link is busy" in this run (0 of 100 refreshes).
  After the fix, three runs: all 2 000 texts held on the desktop in about 10 s from the first refresh (8 packets/s sustained by the phone's
  own packet limit), every thread at 20, phone PSS 55 MB before -> 65-67 MB after the burst (about 61-64 MB a minute later), desktop service RSS
  3-8 MB throughout (VmHWM 8.5 MB), no FATAL EXCEPTION or ANR in logcat.
* **Threads by name (idle, closed, paired):** 7 binder, 3 `geekbridge-call*`, 2 `geekbridge-data*`, 2 `gb-transport`, 1 each of `geekbridge-sms-status`, `geekbridge-link`,
  `geekbridge-transport`, plus ART and UI-toolkit threads. The one that was not needed at idle was the `geekbridge-sms-status` `HandlerThread`: `AndroidSmsSender`
  is built whenever the link starts, but only a send uses the thread. It is now created lazily (`IdleThreadsTest`). The rest are single-thread executors that start on
  first use, or link workers.
* **Not re-measured:** the 50-reconnect and large-transfer runs (no code under them changed), real phones. The paired/connected release build here has no
  `DebugNetHold`, so the emulator's redirected link can drop and redial about every 2 s (`reports/e2e-link-drops.md`); the idle numbers are of a link that was up at the
  start and at the end.

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
