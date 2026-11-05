# Release scripts review (read-only pass)

Method: read each script line by line, as a shellcheck-style pass. The shell was unavailable (EDQUOT on the
Claude temp directory), so nothing was executed and neither `bash -n` nor `shellcheck` was run. Re-run both before merging.

Scope: `scripts/release-local.sh`, `release-dev.sh`, `make-keystore.sh`, `package-linux.sh`, `lib/readme.sh`,
`lib/repro.sh`, `lib/signpass.sh`, `lib-tmp.sh`, `check-release-isolation.sh`, `check-doc-links.sh`,
`gen-whats-new.sh`, `perf-check.sh`, `packaging/linux/install.sh` (there is no `scripts/install*.sh`).

## Checks that passed

- Secrets: keystore passwords only travel as files (`-storepass:file`, `-keypass:file`, `--ks-pass file:`). `signpass.sh` writes
  its copy with umask 077 under `$TMPDIR` (the private `$WORK/tmp`) and it is removed after signing. The debug keystore password
  `android` is the public Android default. `make-keystore.sh` never prints the password and refuses to overwrite or to write into a git tree.
- Temp files: `mktemp` always under `${TMPDIR:-/tmp}` or `$GB_TMP`; `release-local.sh` redirects `TMPDIR` into the scratch tree.
- `rm -rf` with variables: `release-local.sh` guards `$WORK` (suffix of `$BASE`, never `/` or `$HOME`); `install.sh --purge-data`
  only deletes absolute paths ending in `/geekbridge`; `package-linux.sh` and `release-dev.sh` only remove `mktemp` results.
- Different cwd: every script `cd`s to a root computed from `BASH_SOURCE`; `release-local.sh` resolves `--notes-file` against the original cwd.
- Reproducibility: `gb_repro_tar_gz` uses sorted names, fixed mtime, numeric owner, GNU format, `gzip -n`; `ar rcD`; deb members via `--sort=name --mtime`.
- `readme.sh` sed expressions: already-absolute links are not re-matched (the third rule excludes `:`); `\#` is a literal in the `#`-delimited expressions.
- `check-doc-links.sh`, `gen-whats-new.sh`: quoting and `set -e` behaviour are fine.
- `grep -q` on here-strings (not pipes) in `release-local.sh` is safe.

## Fixes made

1. `check-release-isolation.sh`: the `aapt2` lookup `ls ... | sort | tail` ran under `pipefail`; with no build-tools directory the
   command substitution failed and `set -e` ended the script silently, before the "aapt2 not found" message. Added `|| true`.
   Added dependency checks for `unzip` and `strings` with a message (previously a bare "command not found" mid-run).
   The `grep -c` pipelines are SIGPIPE-safe because `grep -c` reads all input.
2. `release-dev.sh`: same silent exit for the `build-tools` lookup (`|| true`); a `cargo` presence check with a hint for `--skip-desktop`.
3. `release-local.sh`:
   - the keytool signer-pin pipeline could abort silently on a wrong password file or alias (pipefail, stderr discarded), so the
     helpful `die` below it never ran; now `|| true`.
   - Windows verification used `... | grep -q` at the end of pipes: `grep -q` exits early, the writer can get SIGPIPE and
     under `pipefail` a *match* could be reported as "no match" (missed bad import, or a false "not PE32+"). Replaced with plain
     `grep ... >/dev/null`.
   - `--with-windows` now checks for `zip unzip file objdump python3` up front instead of failing after the 20 minute cross-build.
4. `make-keystore.sh`: `head -c 512 /dev/urandom | tr | head -c 32` could trip `pipefail` through SIGPIPE and exit with the password file
   half-written and no message; the length check right after is the real test, so the pipeline is now `|| true`.
   The fingerprint pipeline also gets `|| true` so its explicit error message is reached.
5. `package-linux.sh`:
   - `umask 022`: directory modes in the tarball and `.deb` come from `mkdir -p`, so a umask of 002 changed the bytes.
   - `md5sums` sorted with `LC_ALL=C` (locale-dependent order changed the control tarball).
   - `Installed-Size` uses `du -sk --apparent-size` (allocated blocks differ by filesystem; the value is now a stable sum of file sizes).
   - dependency checks for `ar tar gzip xz md5sum install`.
6. `perf-check.sh`: the waits for `dbus-daemon` and `tray-fake-watcher` were unbounded `until` loops that hung forever if the
   process died; they now fail with a message. `ctxt` tolerates a thread vanishing during the `/proc/*/task/*/status` read
   (cat error under pipefail would kill the script).
7. `packaging/linux/install.sh`: `--prefix` with an empty, relative or `/` value is refused (empty gave `/bin`, relative made
   `Exec=` lines relative); a missing argument now gives a message.

## Not changed, for the maintainer

- `release-local.sh` Windows zip is built with `zip -qr`: entry order and mtimes are not reproducible. It is marked experimental; use `find | sort | zip -X -@` with `touch -d @$SOURCE_DATE_EPOCH` if that ever matters.
- `release-dev.sh` writes `builtAt` from the wall clock (the manifest is not part of the reproducibility claim) and `rm -f "$OUT"/geekbridge-*` trusts `--out`.
- `release-local.sh` treats `scripts/e2e/wayland.sh` as dirt in no mode, while `release-dev.sh` excludes it explicitly; the exclusion in `release-dev.sh` looks like a local-workflow leftover and should be dropped.
- `gen-whats-new.sh` uses `LC_ALL=C.UTF-8`, absent on macOS; with a byte-oriented awk the 140-character cap counts bytes and the output could differ between machines. `--check` on macOS may therefore disagree.
- `perf-check.sh` picks ports with `RANDOM` and does not retry on collision.
- `install.sh` builds `sed` expressions from `$bindir`; a prefix containing `|` or `&` would break them.
- Nothing was committed (shell unavailable).
