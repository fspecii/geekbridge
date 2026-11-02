#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Add the GeekBridge paste-from-phone binding and the call/share window rules to your Hyprland config.
#
#   scripts/hyprland-apply.sh [geekbridge-ctl hotkey install OPTIONS...]     e.g. --key Super+Alt+V, --no-pin
#
# CHANGES YOUR CONFIG, but only after you have seen the exact diff and typed "y". The change is one marked
# block in your own bindings file (never an Omarchy default); `geekbridge-ctl hotkey install --apply` copies
# the file to <file>.bak.geekbridge.<unix-time> first. Undo: `geekbridge-ctl hotkey uninstall --apply`.
# Does not reload Hyprland (Omarchy reloads on save; otherwise run `hyprctl reload` yourself).
# Refuses to run without a terminal, so nothing can apply it by accident from a script or hook.
set -Eeuo pipefail

if [[ ! -t 0 || ! -t 1 ]]; then
  echo "hyprland-apply.sh needs an interactive terminal (it asks before changing your config); refusing." >&2
  exit 2
fi

for a in "$@"; do
  if [[ "$a" == "--apply" ]]; then
    echo "--apply is added by this script after you confirm; do not pass it." >&2
    exit 2
  fi
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ctl="${GEEKBRIDGE_CTL:-}"
if [[ -z "$ctl" ]]; then
  if command -v geekbridge-ctl >/dev/null 2>&1; then
    ctl="$(command -v geekbridge-ctl)"
  elif [[ -x "$ROOT/target/release/geekbridge-ctl" ]]; then
    ctl="$ROOT/target/release/geekbridge-ctl"
  else
    echo "geekbridge-ctl not found: install GeekBridge, put it on PATH, or set GEEKBRIDGE_CTL." >&2
    exit 1
  fi
fi
ctl="$(readlink -f "$ctl")"
if [[ "$ctl" == */target/debug/* ]]; then
  echo "warning: $ctl is a debug build; the hotkey would run that path." >&2
fi

echo "== What would change (dry run, nothing written yet) =="
if ! "$ctl" hotkey install "$@"; then
  echo "Nothing was changed." >&2
  exit 1
fi
echo
echo "Key: shown above. Config file: the '---' path in the diff."

read -r -p "Apply this change to your Hyprland config? [y/N] " answer
case "$answer" in
  y | Y | yes | YES) ;;
  *) echo "Cancelled; nothing was changed."; exit 0 ;;
esac

"$ctl" hotkey install --apply "$@"
echo
echo "Applied. Backups sit next to the file as *.bak.geekbridge.<time>."
echo "Check:  $ctl hotkey status"
echo "Undo:   $ctl hotkey uninstall --apply"
echo "Reload: hyprctl reload   (not run for you)"
