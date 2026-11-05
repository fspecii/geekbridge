# Desktop self-update (Linux per-user install)

Status: implemented for the Linux per-user install (`install.sh` into `~/.local`). Windows and macOS are download
only. Code: `crates/bridge-update/src/install/` (`mod.rs`, `gz.rs`, `tar.rs`, tests), `crates/bridge-service/src/update_cli.rs`
(`geekbridge-ctl update install|rollback|restart|support`), the Settings card in `crates/bridge-apps/src/update_ui.rs`.
User-facing description: [updates.md](../updates.md). Origin: finding D1 in [reports/updater-review.md](../reports/updater-review.md).

## Goal and non-goals

Goal: a user clicks **Install update** and the version they were offered replaces the running one safely, with a way back.

Non-goals: silent or automatic installs (nothing installs without a click or `geekbridge-ctl update install`); updating
files owned by a package manager; Windows and macOS; the dev channel; refreshing the systemd unit, desktop entries
and icons (the release archive `geekbridge-linux-<arch>.tar.gz` holds only the nine executables and documents;
re-running `install.sh` refreshes the rest); a signature scheme for desktop archives (none exists yet, see below).

## What is trusted

The archive's SHA-256 and size come from `SHA256SUMS` of the same GitHub release (cross-checked with GitHub's asset
`digest` when present) over https to an allow-listed host. That protects against corruption, truncation and a bad CDN
edge; it does **not** protect against someone who can edit the release itself (the archive and its checksum file change
together). Self-update therefore does not make the desktop more trustworthy than "download and unpack by hand": it
makes the same trust safer to *use* (no half-copied files, no unchecked archive structure, a way back). A release
signing key for desktop archives is the real fix and is a separate decision. The user-visible consequences are in the
Settings note and in [updates.md](../updates.md).

## Threat model

| Threat | Defence |
| --- | --- |
| Archive differs from what was published (corruption, partial download, swap on disk between download and install) | The archive is copied into private staging while hashing; size and SHA-256 must equal the published values. Everything after reads the private copy. |
| Hostile or malformed archive: `..` or absolute names, symlinks or hard links, devices, pax/GNU extension records, duplicate names, nested directories, second top directory, odd characters | `tar.rs` accepts only regular files directly in one top directory; everything else is an error naming the entry. Files are created with `O_EXCL`, so nothing is written through a link. Modes come from one bit (owner executable); never setuid. Header numbers are octal digits only; the top directory entry may appear once. The staging directory is created `0700` in one step. |
| Decompression bomb, huge declared sizes, many entries | Limits: 64 entries, 192 MiB per file, 512 MiB total, and the gzip layer stops at the same total. A header that declares more is refused before a byte is written. |
| Truncated or corrupt archive | Header checksums, exact body lengths, gzip CRC-32 and length trailer, no data after the trailer or after the end-of-archive blocks. |
| Wrong build (old version re-tagged, wrong CPU, swapped binary) | `geekbridge-service --version` of the staged binary must equal the tag's version; every executable must be an ELF for this CPU and marked executable; all nine must be present. |
| Downgrade, pre-release, odd tag | Tag must be exactly `vX.Y.Z`, not a pre-release, and strictly newer than the running version. Rolling back is a separate, explicit command. |
| Package-managed files modified | The install is refused unless `bin/` is inside `$HOME`, owned and writable by the user, and the prefix has no `share/geekbridge/package-managed` marker (shipped by the `.deb` and the Arch package). It is also refused when run as root, when `bin/` or `lib/geekbridge` is a link (the relative links in `bin/` would resolve elsewhere) or writable by everyone. |
| User's own links or files overwritten | A `bin/<name>` that is not a regular file or one of our own links (a link the user made) stops the install before anything changes, on the first update and on every later one; rollback skips such a name. Recovery deletes only leftovers named exactly like ours (`.staging-<pid>`, `.<name>.tmp-<pid>`). |
| Restart during a call | The service decides itself (`RestartIfIdle`): under the call hub's lock it refuses when any call row exists (ringing, dialing, active, held) or a phone with Calls allowed connected less than 10 s ago has not reported its state (unknown). The refusal and the decision to leave are one step, so a ring cannot start in between (it waits for the lock and finds a service that is already leaving). A file in flight also refuses unless `--force`. Any error reaching the service means no restart. |
| New version does not start | After a restart the service must report the new version within 20 s; if not, GeekBridge goes back to the previous version and restarts it. |
| Two installers at once | `flock(2)` on `lib/geekbridge/.lock` (non-blocking; the kernel drops it when the holder dies, so there is no stale-lock takeover race). |
| Staged binary misbehaves when asked its version | It only runs after the archive's SHA-256 and size were verified; empty environment, no stdin, working directory in the private staging tree, its own process group, 10 s limit, whole group killed on timeout. No network or filesystem sandbox: it is code from the same release the user chose to install, running as the same user. |

Out of scope: an attacker who already runs code as the user (they can replace `~/.local/bin` anyway), and a compromised
GitHub release (above).

## Layout

```
<prefix>/bin/<name>                          -> ../lib/geekbridge/current/bin/<name>   (nine executables)
<prefix>/lib/geekbridge/versions/<version>/bin/<name>
<prefix>/lib/geekbridge/versions/<version>/doc/README.md ...
<prefix>/lib/geekbridge/current              -> versions/<version>
<prefix>/lib/geekbridge/previous             -> versions/<version>      (rollback target)
<prefix>/lib/geekbridge/.staging-<pid>/      (only while installing)
<prefix>/lib/geekbridge/.lock
```

`install.sh` leaves regular files in `bin/`. The first self-update copies the running version into `versions/<running>`,
points `current` at it, then replaces each `bin/<name>` by a link (one atomic `rename` per file; every intermediate state
runs the same bits). `systemd` units and `.desktop` entries name `<prefix>/bin/...`, so they follow `current` without being
touched. The service launcher script (`geekbridge-service-launch`, rewritten by `install.sh`) is not part of the archive
and is left alone. A running process keeps its open binary because the old version stays on disk (one version back).

## Steps and crash analysis

All steps run under the lock; each is a single filesystem operation that is atomic or leaves only ignorable debris.

| # | Step | If the process dies right after | State seen by the next start |
| --- | --- | --- | --- |
| 1 | create `.staging-<pid>`, copy and verify archive, unpack, validate, run `--version` | before any layout change | debris in `.staging-*`, removed by recovery; install untouched |
| 2 | (first time) copy running files to `.staging-mig-*`, rename to `versions/<running>` | no `current` yet | `bin/` still the original files; orphan `versions/<running>` is removed by recovery |
| 3 | (first time) `current` -> `versions/<running>` | `bin/` files still original | working; recovery keeps `versions/<running>`; step 4 completes it next time |
| 4 | (first time) each `bin/<name>` replaced by a link | some links, some files | every name resolves to the same bits either way |
| 5 | rename staging tree to `versions/<new>` | `current` unchanged | orphan `versions/<new>`, removed; the retry re-extracts |
| 6 | `previous` -> old | `current` unchanged | `previous` equals `current`; harmless |
| 7 | `current` -> `versions/<new>` (atomic `rename` of a symlink) | **committed** | new version live; a retry finishes links and pruning instead of failing |
| 8 | missing `bin/` links, prune older versions, record `last_install` | after 7 | new version live, maybe one extra old version until the next recovery |

`recover()` runs at every service start (and at the start of every install or rollback): it removes `.staging-*` and
`.*.tmp-*` leftovers, deletes version directories that neither `current` nor `previous` names, and, if `current` points
at a directory without `bin/geekbridge-service`, points it at `previous` instead. It does nothing while another live
process holds the lock. The test `a_crash_at_any_step_leaves_a_working_install_and_the_next_start_recovers` interrupts an
install after every single step (including the migration), checks that every `bin/<name>` still runs, runs recovery and
then the install again.

## Restart

Swapping the files never touches a running process. Restart is a separate decision:

* `geekbridge-ctl update install` asks `Restart the GeekBridge service now?` (`--yes` answers yes, `--no-restart`
  skips it). The card installs with `--no-restart` and then offers **Restart service now**.
* Before restarting, the service must be reachable (if it is not running there is nothing to restart). `geekbridge-ctl`
  then sends `RestartIfIdle {force}`; there is no separate `Calls` check. The service answers `Ack` (it is leaving),
  `Busy` with the reason (a call, an unknown call state, or a file in flight; nothing changed) or `Unsupported`.
* The service leaves by itself: after the `Ack` it says goodbye to the phones and exits with an error status, which
  the installed unit (`Restart=on-failure`, `RestartSec=5`) turns into a start of the new files. It does this only if
  `systemctl --user show` reports that this process is the `MainPID` of `geekbridge-service.service` with
  `Restart=on-failure` or `always`. A launch by `scripts/run-desktop.sh`, `geekbridge-service-launch` without a user
  manager, or a shell is never made to exit: ctl prints `Restart manually: ...` and the service keeps running the old
  version. Quitting from the tray still exits cleanly and is not restarted.
* Compatibility: `force` defaults to false and the request is additive. An older service answers `BadRequest`
  (unrecognised op), and a newer ctl then falls back to the former two steps (`Calls`, then
  `systemctl --user try-restart`), so the old race remains only until the service itself is a version that has
  `RestartIfIdle`. An older ctl never sends it.
* Afterwards `UpdateStatus.current_version` must equal the new version within 20 s, otherwise the previous version is
  restored and the service restarted again (`last_install` records `rolled_back_unhealthy`). The manual
  `update rollback` and `update restart` do not auto-rollback: they report.

## Failure modes

| Failure | Result |
| --- | --- |
| No space / permission denied while staging | Error, staging removed, install unchanged. |
| Checksum, size, archive or version validation fails | `failed` recorded in `update-state.json`, nothing live changes. |
| `bin/<name>` is a user link, or `geekbridge-service` missing in `bin/` | Refused before the layout changes (`Unsupported`). |
| Service not running at install | Files switched; restart reports "not running"; the new version starts next time. |
| Call in progress at restart | Restart refused with the reason; files are already switched; `update restart` later. |
| `install.sh` re-run after self-updates | Its regular files replace the links and it removes `lib/geekbridge`; the layout returns to plain. |
| Previous version removed by hand | `rollback` refuses ("no previous version"). |
| Clock or pid reuse | Lock staleness uses `/proc/<pid>`; at worst a retry waits for the other process to end. |

## Rollback

`geekbridge-ctl update rollback`: point `current` at `previous`, then `previous` at the version being left, record
`rolled_back`. Running it twice goes forward again. Only one older version is kept, deliberately: it bounds disk use
(`~/.local/lib/geekbridge` is two copies of the executables), and a rollback target older than the previous release is
rarely useful. A crash between the two link updates costs at most the version being left (recovery deletes it).

## Platform behaviour

| System | Behaviour |
| --- | --- |
| Linux, per-user install | Install update, rollback, restart as above. |
| Linux, `.deb` / Arch package / `install.sh --system` | "Update with your package manager." Nothing is modified. |
| Linux from a build folder or with an unusual layout | Download only, with the reason. |
| Windows, macOS | The module compiles to a stub: download and verify, **Download only**, the user installs. Unchanged behaviour. |
| Dev channel (any OS) | Download only: there is no tag to tie the staged binary's version to. |

## Size and dependencies

No new crates. Decompression is a small streaming inflate (`gz.rs`, RFC 1951/1952) because `flate2` is not in the
service or ctl dependency tree (it arrives through `image`/`tiff` in the UI crates only) and the installer logic had to
stay in `bridge-update`/`geekbridge-ctl`. The service carries only `support_here`/`recover` (see `docs/performance.md`
for the measured size and the budgets in `scripts/perf-check.sh`).

## Not done

* A signature on desktop archives (and so on the stable channel any real protection against a compromised release).
* Windows (running `.exe` cannot be replaced; needs a helper) and macOS (`.app` bundle, notarisation).
* Refreshing unit/desktop/icon files; migrating a service launcher script that `install.sh` customised.
* A staged-binary smoke test beyond `--version` (for example starting it on private ports).
* Keeping more than one old version; a UI button for rollback (CLI only).
