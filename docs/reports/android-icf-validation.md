# Android lld ICF validation (2026-11-03)

Question: can `-C link-arg=-Wl,--icf=all` (measured at about -100 kB in size-memory-round-3.md) be enabled for the Android
libraries? Result: yes, enabled in `scripts/android-env.sh` (the single place the Android rustflags live; both
`build-android.sh` and `release-local.sh` source it). `GB_ANDROID_ICF=0` disables it.

## Sizes (release profile `android`, stripped; APK = unsigned `assembleRelease`, R8 on)

| | without ICF | with ICF | delta |
| --- | --- | --- | --- |
| arm64 library | 2 859 600 B | 2 758 592 B | -101 008 B |
| x86_64 library | 3 271 400 B | 3 220 952 B | -50 448 B |
| arm64-only release APK (`-Pgb.abis=arm64-v8a`, the phone build) | 3 295 347 B | 3 194 339 B | -101 008 B |
| arm64 + x86_64 release APK | 6 574 428 B | 6 425 676 B | -148 752 B |

## 16 KB page alignment

`llvm-readelf -lW` on all four libraries (both ABIs, both variants): the three LOAD segments each have Align 0x4000. ICF does
not change this; `build-android.sh` also still enforces it.

## Address-identity audit

grep of `crates/` for `ptr::eq`, `addr_eq`, `TypeId`/`type_id`, function pointers cast to `usize`/`*const`, and `fn(..) ==`:
no hits outside integer casts and `libc` sockaddr casts. Nothing compares function addresses or vtable identity. Dependencies
(tokio `Waker::will_wake`) only use pointer equality as an optimisation, and folded functions have identical code, so a
false "equal" is harmless. Rust does not guarantee distinct addresses for distinct functions anyway.

## Device validation

The only AVD image available is x86_64, so the code that ran was the x86_64 library built with ICF; the arm64 library is
built with the same flags and linker but was not executed (no arm64 emulator, no phone this round). The real-phone test
planned for 2026-11-02 remains the arm64 check.

Debug APK (`scripts/e2e-android.sh`, which builds through `build-android.sh` and so picks up ICF), AVD gb36:

* ICF, full run 1: env, battery pass; **pair FAILED** ("desktop never listed the phone as paired"), reconnect stages failed
  as a consequence, the rest skipped. Not reproduced and not explained: the identical APK (same hash) passed pair in the
  three following runs. Other agents were running emulators and soak tests on the same machine at the time.
* ICF, pair + reconnect only: pass (pair, reconnect, reconnect after restart, trust listed).
* ICF, full run (`--rebuild`): env, battery, pair, reconnect x3, clip_d2p, file_d2p, share_text_d2p, file_p2d, update pass.
  clip_p2d, clip_p2d_window, clip_p2d_locked fail ("no connected phone has Clipboard switched on").
* Baseline (`GB_ANDROID_ICF=0`), pair + reconnect: pass. Baseline run of env..clip_p2d: pair passes, **clip_d2p fails
  ("link did not negotiate clipboard") and clip_p2d/_window/_locked fail with the same message as with ICF**. So the clipboard
  p2d failures are not caused by ICF; they look like a pre-existing e2e or build problem (clipboard capability not negotiated
  in some runs) and should be looked at separately.

Not run: Kotlin unit tests and host `cargo test -p bridge-android` (ICF is a link flag for the Android targets only).

## Verdict and caveats

Enabled. One unexplained pair failure in four ICF runs (zero in the two baseline pair runs) is too little to attribute to
ICF given the loaded machine and an identical binary passing afterwards, but it is on record; if pairing flakes reappear on
a real phone, set `GB_ANDROID_ICF=0` first to rule it out. arm64 execution is unverified until the real-phone run.

Cleanup: emulator stopped, e2e scratch and the build scratch removed; no AVD clones were created (gb36 is the e2e script's
own AVD).
