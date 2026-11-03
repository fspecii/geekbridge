# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Helpers for reproducible release artefacts (source it). docs/reproducible-builds.md explains each choice.

# gb_repro_rustflags SRC_DIR -> RUSTFLAGS text: no build-machine path ends up in a binary (panic messages, file!(),
# build-script output) and symbols are stripped (the release profile already says strip = "symbols"; repeated so a
# changed profile cannot silently reintroduce debug paths).
gb_repro_rustflags() {
  local src="$1" cargo_home="${CARGO_HOME:-$HOME/.cargo}" sysroot
  sysroot="$(rustc --print sysroot)"
  printf '%s' "--remap-path-prefix=$src=/build/geekbridge --remap-path-prefix=$cargo_home/registry/src=/build/cargo-registry"
  printf ' %s' "--remap-path-prefix=$cargo_home=/build/cargo-home --remap-path-prefix=$sysroot=/build/rust-sysroot -C strip=symbols"
}

# gb_repro_tar_gz OUT.tar.gz PARENT_DIR NAME -> tar.gz of PARENT_DIR/NAME with sorted entries, fixed mtime
# (SOURCE_DATE_EPOCH, required), root ownership, no gzip name/time header, GNU format (no atime/ctime fields).
gb_repro_tar_gz() {
  local out="$1" parent="$2" name="$3"
  [[ -n "${SOURCE_DATE_EPOCH:-}" ]] || { echo "SOURCE_DATE_EPOCH is not set" >&2; return 1; }
  tar --sort=name --format=gnu --mtime="@$SOURCE_DATE_EPOCH" --owner=0 --group=0 --numeric-owner \
      -cf - -C "$parent" "$name" | gzip -n -9 > "$out"
}
