# Disk hygiene

Rust build trees, emulator disks and Gradle output are the big disk users here. One `target/` once reached
197 GB from per-task scratch builds, and each AVD costs 2-6 GB. These tools keep that bounded; none of them
deletes anything unless you pass `--apply`.

## Look, then clean

```sh
make disk                      # scripts/disk-report.sh: sizes, idle age, "safe to delete" per item; deletes nothing
make clean-dev                 # dry run: lists what it would remove and prints "would free X GB"
make clean-dev APPLY=1         # delete the items marked safe
make clean-dev WHAT=target HOURS=24 KEEP_AVD="gb36u gb36q"      # narrower; same flags as scripts/clean-dev.sh
```

An item is **safe to delete** only when all of these hold: it is one of the shapes below, it is regenerable,
it has been idle longer than `--older-than` hours (default 12; newest mtime in its top three levels), no
running process references it (any `/proc/<pid>` cwd, command line, environment or open file; for AVDs also
`-avd NAME`), and it is at least `GB_MIN_ITEM_MB` (50) big. Symlinks are never followed. Before every
delete the path is checked again against the list of shapes, and anything else is refused.

| `--what` | What it owns | Never touched |
|---|---|---|
| `target` | `<repo>/target/<name>/` | `debug`, `release`, `android` (the normal builds) |
| `avd` | `.tools/android-user/avd/<name>.avd` + `.ini` | `gbbase gbcall gbnothing gbhome`, `--keep-avd NAME`, any AVD with a running emulator |
| `cache` | `~/.cache/gb-scratch/*` entries marked `.gb-owned` (and `target-shared`), `~/.cache/gb-*` | Gradle caches and `dist/` (reported only) |
| `tmp` | `$GB_TMP/gb-<name>.XXXXXX`, `gb-test-*`, `gb-callui-xdg-<uid>`, `geekbridge-emulate-call.*` | everything else in `/tmp` |

`scripts/clean-tmp.sh` still exists for the minutes-based, `/tmp`-only variant.

## Where scripts put scratch

* **One root:** `GB_SCRATCH` (default `~/.cache/gb-scratch`). A script that needs private scratch calls
  `gb_scratch_init <task>` from `scripts/lib/scratch.sh`, which creates `$GB_SCRATCH/<task>-<pid>/` (marked
  `.gb-owned`) and removes it on a successful exit. A failed run keeps it for debugging;
  `GB_KEEP_SCRATCH=1` keeps it always; `make clean-dev` reaps it once idle.
* **Shared cargo target:** ad-hoc builds (yours, or an agent's) should not invent `target/<name>` dirs. Call
  `gb_shared_target` (or `export CARGO_TARGET_DIR=~/.cache/gb-scratch/target-shared`). It warns above
  `GB_TARGET_CAP_GB` (40). Builds sharing it queue on cargo's lock rather than each growing a new tree.
  The normal `target/` stays for `cargo build`, `make check` and so on.
* `scripts/e2e-android.sh` (and the scripts that source it) redirect the `target/<name>` cargo dirs and the
  APK scratch copy to the above; `GB_LEGACY_TARGET=1` restores the old locations.

## Emulators

`scripts/lib/avd.sh` gives `gb_avd_ensure NAME`: an AVD that already exists is used as is; a missing one is
cloned from the small base AVD `gbbase` under the short name, and removed on exit unless `GB_KEEP_AVD=1`.
Emulators a script started are stopped on exit (`gb_avd_on_exit`). Create the base once:

```sh
GB_EMU_AVD=gbbase scripts/emulator-call.sh create-avd
```

## Free-space pre-flight

`scripts/e2e-android.sh`, `emulator-call.sh start`, `golden-android.sh`, `build-android.sh` and
`coverage.sh` refuse to start with a clear message when free space is below `GB_MIN_FREE_GB` (default 15 GB).
Set it lower for a one-off (`GB_MIN_FREE_GB=5`), or free space with `make clean-dev`. The check does not
see a per-user quota on `/tmp`; scripts keep their scratch out of it (`GB_TMP`, `scripts/lib-tmp.sh`).

## Tests

`scripts/tests/clean-dev-test.sh` builds a fake repo, `HOME` and processes (a sleeping process whose cwd is
inside a fake `target/` subdir, one holding a file open, one named in a command line, a fake `qemu -avd`) and
checks that the cleaner skips them and everything not ours. `scripts/tests/scratch-avd-test.sh` covers the
scratch, pre-flight and AVD-clone helpers. Neither touches the real tree.
