# Final footprint, 2026-11-04

Table and method: [performance.md](../performance.md#final-footprint-2026-11-04). Raw data lives in the scratch dir of the run (not committed).

## Setup

* origin/main `3bfefaae7` in a clean worktree, `.tools` symlinked, private cargo target dirs under `~/.cache/gb-final`, `TMPDIR=~/.cache/gb-tmp`.
* Android: `scripts/build-android.sh --skip-host-tests` (gradle unit tests and lint ran: exit 0), then release APKs from the same libraries with
  `gradle assembleRelease -Pgb.abis=arm64-v8a[,x86_64]`. AVD clone `gbfoot` (console 5762, adb server 16262, control port 18341, Xvfb `:73`),
  `scripts/e2e/features.sh` with a new `footprint` stage. Everything was stopped afterwards and the clone removed. Never touched 1716, 17381, 8000.
* Disk: 72 GB free before and after.

## Findings

1. Phone idle is flat. 31 samples over 30 minutes with the link connected and every feature on: PSS 62.2-64.0 MB (avg 62.4), threads 35-36, fds 146, Java
   heap 14.0-14.4 MB, native 13.9-15.6 MB. First sample after the reset is the highest; no growth afterwards. No leak.
2. No wakeups of its own: no alarm entry in `dumpsys alarm`, no wake lock, no job in batterystats; 2.2 s of CPU in 30 minutes (1532 ms user, 636 ms system).
3. Features cost nothing at idle. PSS by step (calls only / +contacts,calendar,sms / +battery / +notifications / +photos): 57.97 / 57.92 / 57.74 / 58.10 / 57.91 MB.
4. Size grew, not memory. Release arm64 APK 3.19 -> 3.50 MB (+304 kB), of which the native library is +270 kB (2 758 592 -> 3 028 752 B stripped). Source added
   since the ICF measurement (11-03 06:01): ~6 700 lines in `bridge-core` (call-audio state and flow 2 100, firewall 680, photos link 480, why-disconnected 320,
   call overlay 300, share result 290, ipc 800) and 2 700 in `bridge-android`, plus `rqrr` (new dependency, QR decoding on the phone). Attribution per feature is
   by source volume only: no unstripped build was done, so no symbol-level split is claimed. The performance doc's "release arm64 ~3.2 MB" is now 3.5 MB.
5. Release APK on a fresh install: Home idle PSS 23.9 MB (11-03: 23.5), cold start 972-1137 ms (11-03: 910-970; the spread of one run is larger than the difference).
6. Desktop service RSS: the CI flips came from the sampling point. RSS is a sawtooth between 3.5 and 7.7 MB; a single read at 3 s + window lands anywhere. Measured
   timeline of the same binary: 3.4 MB after the 2 s trim, 7.1 MB at ~17 s (periodic work faulting in about 3.5 MB of code; the exact job was not pinned down), back to 3.9 MB when the 20 s probe trims. Locally the sample usually hit a trough (4.7 MB); on a slower runner it hits a crest (8.0-8.2 MB).
7. GUI processes (Xvfb, llvmpipe): 62-66 MB RSS each, equal to round 3; messages 64.7 MB under `perf-check --ui` (budget 75 MB), 3 wakeups/s (budget 12).

## Fixes

* `scripts/perf-check.sh`: idle RSS is the minimum of samples taken every 5 s over the window (>= 4 samples), peak printed for information; budgets unchanged, method documented.
* `memtrim::after_announce` (test: `the_announcement_trim_returns_resident_code_pages`): drops idle code pages after each LAN announcement. Steady sawtooth 5.0-5.6 -> 4.1-4.3 MB.
* `scripts/e2e/features.sh`: `footprint` stage (not in the default run), runtime directory moved from `/tmp` to the work dir.

## Not done

* Symbol-level size split of the Android library per feature (needs an unstripped build).
* Real-phone numbers; wakeups in `dumpsys batterystats` are only meaningful on a phone with the screen off.
* The service wakeup count (2-3/s) stays at the 3/s budget; unchanged since round 3.
