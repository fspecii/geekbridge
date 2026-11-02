# SPDX-License-Identifier: GPL-3.0-or-later
# shellcheck shell=bash
# Throwaway-AVD helpers (source after scripts/android-env.sh and scripts/lib/scratch.sh).
#
#   GB_BASE_AVD   AVD to clone from (default gbbase). Create it once (scripts/emulator-call.sh with
#                 GB_EMU_AVD=gbbase create-avd); keep it small: no snapshots, 2G RAM, 6G data.
#   GB_KEEP_AVD=1 keep clones made by gb_avd_ensure on exit (default: deleted)
#
# gb_avd_ensure NAME   if AVD NAME already exists, use it untouched. Otherwise clone the base AVD under
#                      the short name NAME and remove the clone on exit (unless GB_KEEP_AVD=1).
#                      Sets GB_AVD_CLONED=1 when it made a clone.
# gb_avd_on_exit CMD   run CMD at exit (stop the emulator you started), before clones are deleted.
# gb_avd_started PID   register an emulator process this script started; it is stopped on exit.
# Both run via the EXIT trap chain, after any trap the script already installed.
GB_BASE_AVD="${GB_BASE_AVD:-gbbase}"
_gb_avd_clones=() _gb_avd_pids=() _gb_avd_hooks=()

# gb_avd_on_exit CMD   run CMD (a shell string, e.g. "adb emu kill") at exit, before clones are removed
gb_avd_on_exit() { _gb_avd_hooks+=("$1"); _gb_avd_arm; }

_gb_avd_exit() {
    local rc="${1:-0}" pid n h i
    for h in "${_gb_avd_hooks[@]}"; do eval "$h" || true; done
    for pid in "${_gb_avd_pids[@]}"; do
        kill -0 "$pid" 2>/dev/null && { kill "$pid" 2>/dev/null; sleep 1; kill -9 "$pid" 2>/dev/null; } || true
    done
    for n in "${_gb_avd_clones[@]}"; do
        if [[ "${GB_KEEP_AVD:-0}" == 1 ]]; then echo "AVD kept: $n" >&2; continue; fi
        for i in 1 2 3 4 5 6 7 8 9 10; do   # give a stopping emulator a moment to exit
            pgrep -af "qemu.*-avd $n( |\$)|emulator.* -avd $n( |\$)" >/dev/null 2>&1 || break; sleep 1
        done
        # never remove an AVD with a live emulator, and only ones whose config says we cloned them
        pgrep -af "qemu.*-avd $n( |\$)|emulator.* -avd $n( |\$)" >/dev/null 2>&1 && continue
        grep -q '^gb.clone=1' "$ANDROID_AVD_HOME/$n.avd/config.ini" 2>/dev/null || continue
        rm -rf -- "${ANDROID_AVD_HOME:?}/$n.avd" "${ANDROID_AVD_HOME:?}/$n.ini"
    done
    return "$rc"
}
_gb_avd_trap_set=0
_gb_avd_arm() { ((_gb_avd_trap_set)) || { _gb_trap_prepend _gb_avd_exit; _gb_avd_trap_set=1; }; }

gb_avd_started() { _gb_avd_pids+=("$1"); _gb_avd_arm; }

gb_avd_ensure() {
    local name="${1:?avd name}" home="${ANDROID_AVD_HOME:?}" base="$GB_BASE_AVD"
    [[ "$name" =~ ^[A-Za-z0-9_]{1,24}$ ]] || { echo "gb_avd_ensure: bad AVD name '$name'" >&2; return 1; }
    GB_AVD_CLONED=0
    [[ -d "$home/$name.avd" && -r "$home/$name.ini" ]] && return 0
    if [[ ! -d "$home/$base.avd" ]]; then
        echo "AVD '$name' is missing and so is the base AVD '$base'. Create one:" >&2
        echo "  scripts/bootstrap-android.sh --with-emulator && GB_EMU_AVD=$name scripts/emulator-call.sh create-avd" >&2
        return 1
    fi
    gb_preflight "$home" || return 1
    mkdir -p "$home"
    # -a keeps sparse files sparse; --reflink uses a CoW copy where the filesystem supports it.
    cp -a --reflink=auto -- "$home/$base.avd" "$home/$name.avd" || { rm -rf -- "${home:?}/$name.avd"; return 1; }
    rm -f -- "$home/$name.avd"/*.lock "$home/$name.avd"/multiinstance.lock 2>/dev/null
    rm -rf -- "$home/$name.avd/snapshots"
    sed -e "s|^path=.*|path=$home/$name.avd|" -e "s|^path.rel=.*|path.rel=avd/$name.avd|" "$home/$base.ini" >"$home/$name.ini"
    sed -i -e "s|^AvdId=.*|AvdId=$name|" -e "s|^avd.ini.displayname=.*|avd.ini.displayname=$name|" "$home/$name.avd/config.ini"
    echo 'gb.clone=1' >>"$home/$name.avd/config.ini"
    _gb_avd_clones+=("$name"); _gb_avd_arm; GB_AVD_CLONED=1
    echo "AVD '$name' cloned from '$base'$([[ "${GB_KEEP_AVD:-0}" == 1 ]] || echo ' (removed on exit; GB_KEEP_AVD=1 keeps it)')" >&2
}
