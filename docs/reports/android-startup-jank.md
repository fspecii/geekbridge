# Android startup, jank, size and memory round (2026-11-03)

Scope: cold start, `Application.onCreate`, native library load, time to first frame, jank, main-thread work, APK contents, memory, process layout.
Method and caveats first, because the numbers depend on them.

## Setup

* AVD clone `gbperfa` (copy of `gb36x`: Android 16 / API 36, x86_64, 2 GB, software GPU `swiftshader_indirect`), console port 5640, adb server
  port 16090, scratch under `/home/ambsd/.cache/gb-andperf-scratch`. Nothing used the live service or ports 1716, 17381, 8000. Emulator and adb server
  were stopped afterwards and the clone deleted.
* Release APKs (R8, resource shrinking) signed with the debug key so `install -r` keeps app data between builds. "Before" is origin/main
  (`a5e44bccf`), "after" is the same tree plus this round's changes, installed alternately.
* The host was loaded by other jobs (load average 3 to 13). Wall-clock numbers moved by +-300 ms between consecutive rounds of the *same* APK, so
  wall-clock A/B differences under 10 % cannot be claimed. Process CPU ticks (`/proc/PID/stat`) and StrictMode counts are less sensitive and are used where possible.
* State: the first runs used an AVD that was already paired (real keystore, core starting a link); an uninstall to test a baseline profile erased that
  state, so the A/B runs below use "setup finished, nothing paired" (no foreground service, core not started). Both arms always used the same state.
* No real phone: ART on a phone is ahead-of-time compiled, has a real GPU and different IO; treat as relative.

## Cold start: where the time goes

Instrumented release build (temporary logging, not committed), `+N` = ms since process start:

| Step | Time |
| --- | --- |
| `GeekBridgeApp.onCreate` starts / takes | +20 / 6-8 ms |
| `HomeActivity.onCreate` (AppState, UiKit, Screen, controller) | 6-11 ms, ends +61-68 |
| `onStart` (link service sync, offers, controller start) | 37-46 ms |
| Screen builds (state changes rebuild the page): 1st / 2nd / 3rd | 18-32 / 5-7 / 5-7 ms |
| First draw | +237-266 ms |
| Next draw after it | +835-890 ms (`Choreographer: Skipped 36 frames`) |
| `am start -W` TotalTime | 891-983 ms |

The 600 ms gap after the first draw is not app code: thread dumps (`kill -3` every 120 ms from +450 ms) show the main thread inside
`HardwareRenderer.nSetStopped` -> `RenderProxy::setStopped` -> `future::get`, i.e. waiting for the HWUI render thread, which is rasterising on a software GL
renderer. For comparison the system Settings app starts in 535-700 ms on the same emulator. The Rust core is not loaded on the main thread:
`controller.start` runs on the `geekbridge-transport` executor and `NativeBridge` loads `libgeekbridge_android.so` on first use there. Native load
and JNI init are therefore off the critical path; Application work (6-8 ms) is negligible.

Release cold start, 7-9 starts per arm (ms): before 922 931 944 968 920 912 960 969 910 (quiet moment, paired), after-change rounds were
936-1015 when the host load was 12; interleaved rounds under load: before 999-1496, after 988-1558. **No cold-start change can be claimed.**

A baseline profile was tried (AGP compiles `src/main/baseline-prof.txt` into `assets/dexopt/baseline.prof` without AndroidX): wildcard rules yield an empty
profile after R8 mapping; explicit per-method rules (4 519, generated from the compiled classes) yielded a 676-byte profile, and
`cmd package compile -m speed-profile` still reported `status=verify` on the emulator, i.e. the profile was not consumed. `-m speed`
(everything compiled) did not give a clear win under the load either. Dropped; a profile generated on a real phone with Macrobenchmark is the way to
test this properly and is listed as follow-up.

## Main-thread work (StrictMode, debug build, 3 cold starts)

Before: two disk-read violations per start in `HomeActivity.onStart` (the `getFilesDir` stat and `DeviceName.current`, which loaded and parsed the
`geekbridge_device_name` preferences file on the main thread: 142, 143, 163, 194, 331, 335 ms reported) plus `CallControlRuntime.init` (23-27 ms)
and `AppState.init` / `PhoneDataRuntime.init` (25-47 ms) on the boot-receiver path. Cause: the warm-up thread in `GeekBridgeApp` preloaded only some of the
preference files, started late, and the main thread then read the others itself.

Fix: `PrefsWarmup.FILES` lists every preference file (incl. `geekbridge_device_name`), `GeekBridgeApp` starts the warm-up first thing in `onCreate`.
`PrefsWarmupTest` scans the sources and fails when a file opened with `getSharedPreferences` / a `FILE` constant is not listed.
After: one violation per start, the `getFilesDir` stat (181-241 ms; `ContextImpl.ensurePrivateDirExists` stats the directory on every call and the
emulator's first stat after `force-stop` is slow; the same stat happens on the warm-up thread). Left alone: passing a cached path would not avoid the
cold stat for whichever thread touches it first.

## Jank

`dumpsys gfxinfo`, Home scrolled with 6 swipe pairs, Setup with 4: Home 22-27 % janky frames (394-413 frames, p50 32-34 ms, p90 48-53 ms,
`Number Slow UI thread` 46-52), Setup 34-36 % (p50 36 ms). Identical before and after (22.4 % / 34.0 % after). 100 % of frames are "legacy janky" at 16 ms because the
software renderer cannot make 60 fps; Slow UI thread counts (31-52) are the app-attributable part and did not move beyond noise. The draw paths were still
wasteful, so they were fixed:

* `DotMatrixView.onDraw` called `uppercase()` and, for every dot, `DotFont.isLit` (a scan of the string with a boxed `Map<Char, ...>` lookup per glyph): now a
  grid is computed once (`DotFont.litGrid`). 73 us -> 2 us per frame on the JVM for a 25-character word (`DotFontGridTest`, asserts equality with `isLit` for every dot).
* `DotText.build` evaluated `DotFont.columns(line)` per dot and built a key string on every draw: columns hoisted, change detection on three fields.
* `DotProgress.onDraw` built two `ArrayList<Float>` (boxing) per draw, called for every progress update during transfers: reused `FloatArray` buffers.

No unit test can assert a frame-time win on an emulator; the equality test guards the behaviour.

## APK contents

Release 6 494 176 B before, 6 480 448 B after (-13 728 B). Breakdown after: native libraries 6 041 312 B (x86_64 3 253 672 + arm64 2 787 640,
stored, 93 %), `classes.dex` 500 412 B (R8; the 0.5 MB budget in android-perf.md is now at its limit), `resources.arsc` 99 408 B plus `res/` 93 223 B (the three widget
previews are 59 KB of that), manifest 16.7 KB. Removed: `kotlin/*.kotlin_builtins` (6 files, 46 KB stored / 11 KB compressed) and
`kotlin-tooling-metadata.json` through `packaging.resources.excludes`; the app has no reflection library. No fonts are bundled (the dot-matrix font is a table in `DotFont.kt`,
so `third_party/fonts` has no effect on the Android APK). Lint reports one unused string (`clip_tile_added`); resource shrinking already drops it from the release APK.
The phone-only APK (`-Pgb.abis=arm64-v8a`) remains the real size lever (about -3.3 MB).

## Memory and process layout

Unpaired, setup finished, `dumpsys meminfo` PSS (KB), before -> after: Home idle 23 515 -> 23 548 (second pair 23 645 -> 23 570), after scrolling 30 775 -> 30 610,
Setup 32 361 -> 32 230, Diagnostics 38 290 -> 37 431 (36 511 -> 36 498). The +7 MB while scrolling is HWUI graphics buffers; the framework trims them when the UI is hidden. Paired state
(earlier clone): Home idle 28.5-30.9 MB, UI left with the task kept 27.8-28.5 MB (Java heap 3.4 MB, native heap 11.7 MB, 28 threads), debug build 65 MB.

UI and service share one process (no `android:process` in the manifest), so the Rust core lives in the process the UI uses, but it is started from
`LinkService`/`HomeActivity` only when a computer is paired and loaded lazily, and nothing UI-related is retained once the task is gone (the earlier measurement
in android-perf.md: 22.8-23.4 MB closed, 26.5 MB after 30 min). A split was not done: a bare process hosting only the foreground service (core not started)
measured 7.7 MB PSS (14 threads), so two processes would add roughly that much while UI is open to save the 3-5 MB of UI-only heap. The 1 hour idle run was not repeated
in this round; the 30 minute run in android-perf.md is still the reference (host load made a new one meaningless).
Attempting to start the service with a synthetic `BOOT_COMPLETED` broadcast does not work from `adb` (`Background started FGS: Disallowed`), so "service only" was not
measured with the core running.

## Tests and checks

`./gradlew testDebugUnitTest` and `lintDebug` exit 0 (new: `PrefsWarmupTest`, `DotFontGridTest`). CI watched after push.

## Not done / follow-up

Baseline profile generated on a real phone and measured with Macrobenchmark; a real-device `gfxinfo`; cached-path or deferred `getFilesDir` only if a phone shows it;
a repeat of the 1 hour idle PSS run on a quiet host.
