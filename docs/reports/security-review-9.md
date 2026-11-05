# Security review 9: Linux per-user self-update

Scope: commits 327c9cd81 and f4cc1584f. `crates/bridge-update/src/install/` (`tar.rs`, `gz.rs`, `mod.rs`, tests),
`crates/bridge-service/src/update_cli.rs`, the `geekbridge-ctl update ...` wiring, the Settings button in
`crates/bridge-apps/src/update_ui.rs`, `docs/design/desktop-self-update.md`. Method: reading plus new tests; all tests
ran in temporary sandbox prefixes, never against `~/.local`, the live service or any control port.

## Result in one paragraph

The archive path is sound (nothing can be written outside the two destination directories, nothing is followed
through a link, no setuid bits, bombs end at fixed caps). Nine real weaknesses were found, none of them a remote
code-execution path (the archive hash comes from the release; an attacker who edits the release is the documented
non-goal); all nine are fixed with tests. Two items are accepted and documented.

## Checked, no change needed

| Area | Finding |
| --- | --- |
| Path traversal, absolute names, `.`/`..`, nested dirs, backslashes, control chars | `split_path` rejects each; only `top/file` or `top/` pass. Names written to disk come from the static `BINARIES`/`DOCS` lists, never from the archive. |
| Symlinks, hard links, devices, FIFOs, pax (`x`,`g`), GNU (`L`,`K`), sparse (`S`), contiguous (`7`), dumpdir | All are errors naming the entry (new test covers `S`, `7`, `D`, `K`, `g`). |
| Setuid/setgid/sticky | The header mode is reduced to one bit; files are created `0755`/`0644` through `O_EXCL` (new test feeds `07755`). |
| Duplicate entries | Rejected for files (now also for the top directory entry). |
| Decompression bomb | Header-declared sizes are checked before any byte is written (64 entries, 192 MiB/file, 512 MiB total); the inflater stops at total + 4 MiB; data after end-of-archive is limited to 1 MiB of zeros. No ratio cap is needed: output is bounded by absolute caps and memory is a 32 KiB window plus one block. |
| Truncated gzip, bad CRC/length, trailing garbage, multi-member gzip | Errors (new tests: trailing garbage, a second member, three stray zero bytes). |
| Inflater robustness | New deterministic mutation test (400 bit-flips/truncations over a real stream) runs gz + tar with no panic or hang; Huffman table construction and `decode` index only inside `sym`. |
| Verify-then-use | The archive is copied into a `0700` staging directory while it is hashed; everything afterwards reads the private copy. The staged binary is executed only after size and SHA-256 matched. Rename from staging to `versions/<v>` stays inside `lib/geekbridge` (same filesystem). |
| `current` swap | `symlink` to a temporary name + `rename` over `current`: atomic. `prune` and `remove_dir_all` on a link inside `versions/` remove the link, not its target. `link_name` only accepts exactly `versions/<one component>`. |
| Control-API client | `update install/rollback/restart` use `load_token(state_dir)` (private-file read) and the `--control-port` / `GEEKBRIDGE_CONTROL_PORT` value ctl was started with; `update_cli.rs` has no tests that connect anywhere. All calls have connect/read timeouts. |
| Settings button | `run_ctl` is given only `&'static [&'static str]` argument lists (`support`, `install --yes --no-restart`, `restart`); nothing user- or network-controlled reaches argv. Failure text goes through `display_text`. |
| `update-state.json` | Written through the atomic state-file writer; used only for display, never for a decision. |
| Error messages | Only local paths of the user's own install and entry names (first 60 graphic ASCII characters) appear. |

## Defects fixed

| # | Finding | Fix |
| --- | --- | --- |
| 1 | Stale-lock takeover race: two installers (or the service's start-up `recover`) that both saw a dead pid in `.lock` both deleted it and both created a new one, and both held the lock. A pid reused by an unrelated process made the lock permanent. | Lock is now `flock(LOCK_EX\|LOCK_NB)` on the lock file, opened `O_NOFOLLOW`, re-checked by inode after locking. The kernel releases it on death. New tests: second locker is `Busy`; recovery ignores a held lock but takes a lock file nobody holds. |
| 2 | Later updates overwrote a user's own `bin/<name>` link: the refusal only existed in the first migration; the tail loop (and rollback) replaced any entry. | `replaceable()` (missing, plain file, or our own `../lib/geekbridge/...` link) is checked for all nine names before anything is staged and in rollback. Test: a link made after the first update survives the second update and a rollback. |
| 3 | Recovery deleted any `.staging-*` entry and any `.*.tmp-*` file in `bin/` and in `lib/geekbridge`, including a user's `~/.local/bin/.tool.tmp-x`. | Only `.staging-<digits>`, `.staging-mig-<digits>` and `.<current\|previous\|binary>.tmp-<digits>` are removed. Test. |
| 4 | `bin/` that is itself a symlink (dotfile setups) made the relative links resolve in the wrong directory, breaking every `bin/<name>` after migration. | `support_for` refuses (download only, with the reason) when `bin` or `lib/geekbridge` is a link. Test. |
| 5 | A world-writable `bin/` or `lib/geekbridge` let any local user swap the links the user later runs. | Same check refuses `mode & 0o002`. Test. |
| 6 | Run as root with `--prefix` (which skips the home check), `update install` would convert a root-owned system prefix such as `/usr/local` into the versioned layout. | `support_for` returns download-only when the effective uid is 0. (`install.sh --system` stays package-style: manage with the package manager.) |
| 7 | Staging directory was created with the default umask and then `chmod`ed: a short window of `0755`. | `DirBuilder::mode(0o700)`; test inspects the mode at the `Staged` step. |
| 8 | `--version` of the staged binary ran in the caller's working directory, and a timeout killed only the child, not what it started. | Working directory is the private staging tree, the child gets its own process group and the whole group is killed on timeout. Environment was already cleared, stdin null, 10 s limit. |
| 9 | Header numbers were parsed with `from_str_radix`, which accepts a leading `+` (and whitespace trimming was broader than tar's); a flood of repeated `top/` directory entries bypassed the entry limit and duplicate check; a FIFO as the downloaded archive would hang the installer; `last_outcome` printed control characters from an edited `update-state.json`. | Octal fields are digits 0-7 only; the top directory entry may appear once and counts toward the limit; the archive is opened `O_NONBLOCK` and must be a regular file; `last_outcome` strips control characters and caps each field. Tests for each. |

## Accepted, documented

* **Restart race.** `Calls` is read, then `systemctl --user try-restart` is run; a call that starts in between is
  cut. There is no atomic "restart unless a call" in the service API. The window is the duration of one IPC round
  trip plus a process spawn. A server-side `RestartIfIdle` request would close it.
* **Staged `--version` is not sandboxed** (no network or filesystem namespace, same uid). It runs only after the hash
  matched, so it is exactly the code the user chose to install; a namespace sandbox needs `unshare`/seccomp that
  the project deliberately does not ship. The mitigations above (cleared environment, private cwd, process group,
  timeout) bound its reach.
* A migration labels the files found in `bin/` with the service-reported running version without executing them; a
  user who hand-mixed versions gets a mislabelled "previous". Same-user tampering with `previous` or `versions/` is out
  of scope (the user can already edit `~/.local/bin`).

## ctl size

Stripped release `geekbridge-ctl` is 1 766 344 B with these fixes (1 764 712 B before: +1.6 kB), under the 1.79 MB
budget. Symbol accounting of an unstripped build: `bridge_update::install` 54 kB, `update_cli` 7 kB, `sha2` 6 kB,
state-file serde and `std::fs`/formatting glue the rest of the 131 kB. The largest single functions are
`tar::extract` (7.6 kB), `install::run` (7.3 kB), `Gunzip::read` (4.4 kB) and `record` (4.2 kB, the `json!` tree).
No trim worth its risk was found: the installer is already one set of non-generic functions (the generic
`extract<Gunzip<File>>` is instantiated once), the service links only `support_here`/`recover`, and gating the
installer out of ctl would remove the feature. A possible later saving of about 3 kB is replacing `json!` in
`record` with a hand-written map; not done.
