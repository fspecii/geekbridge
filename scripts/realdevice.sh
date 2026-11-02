#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Real-phone test session helper (see docs/real-device-quickstart.md).
#
#   scripts/realdevice.sh [--serial SERIAL] [--adb-port PORT] [--dry-run] COMMAND [ARGS]
#
# Commands
#   check [--apply]            adb, device authorisation, Android version/API/OEM, free space, Bluetooth/Wi-Fi,
#                              same-subnet test, desktop binaries/service and the host firewall for ports
#                              1716-1764 TCP+UDP. Prints the exact allow commands; runs them only with --apply.
#   firewall [--apply]         only the firewall part of `check`
#   install [--apk FILE] [--build] [--no-grant] [--no-start]
#                              locate (or --build) the debug APK, `adb install -r`, grant the runtime permissions
#                              adb is allowed to grant, start the app
#   desktop [run-desktop.sh options]
#                              start the service (private state dir) + home window, print the pairing QR and name
#   logs                       dist/realdevice/<timestamp>/: filtered logcat, dumpsys battery/batterystats/deviceidle/
#                              meminfo/services, redacted `geekbridge-ctl diagnose`, README.md
#   battery-baseline [--minutes N] [--label NAME]
#                              dumpsys batterystats --reset and record level/charge counter/time
#   battery-report             drain since the baseline (level, charge counter, per-hour, app power/wake locks)
#   screens [LABEL] [--count N] [--interval S]
#                              adb screencap into dist/realdevice/screens/
#
# --dry-run prints every adb/sudo command instead of running it. --serial (or $ANDROID_SERIAL) picks the phone
# when several are attached. --adb-port (or $ANDROID_ADB_SERVER_PORT) uses a dedicated adb server, e.g. to
# rehearse on an emulator without touching other sessions.
# Environment: GEEKBRIDGE_ADB (adb binary), GEEKBRIDGE_REALDEVICE_STATE, GEEKBRIDGE_CONTROL_PORT.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
OUT="$ROOT/dist/realdevice"
BIN="${GEEKBRIDGE_BIN_DIR:-$ROOT/target/release}"
STATE="${GEEKBRIDGE_REALDEVICE_STATE:-$ROOT/target/realdevice-state}"
CTRL_PORT="${GEEKBRIDGE_CONTROL_PORT:-17381}"
PKG=dev.geekbridge.android
MIN_API=26
FW_LO=1716 FW_HI=1764
SERIAL="${ANDROID_SERIAL:-}" DRY=0

c() { if [[ -t 1 ]]; then printf '\033[%sm%s\033[0m' "$1" "$2"; else printf '%s' "$2"; fi; }
N_OK=0 N_WARN=0 N_FAIL=0
say() { printf '\n%s %s\n' "$(c 36 '==>')" "$(c 1 "$*")"; }
ok() { N_OK=$((N_OK+1)); printf '  %s %s\n' "$(c 32 'PASS')" "$*"; }
warn() { N_WARN=$((N_WARN+1)); printf '  %s %s\n' "$(c 33 'WARN')" "$*"; }
bad() { N_FAIL=$((N_FAIL+1)); printf '  %s %s\n' "$(c 31 'FAIL')" "$*"; }
info() { printf '       %s\n' "$*"; }
die() { printf '%s %s\n' "$(c 31 'error:')" "$*" >&2; exit 1; }
summary() {
  printf '\n%s  %s pass, %s warn, %s fail\n' "$(c 1 'Result:')" "$N_OK" "$N_WARN" "$N_FAIL"
  ((N_FAIL == 0))
}

# --------------------------------------------------------------------------- global options
while (($#)); do
  case "$1" in
    --serial) shift; SERIAL="${1:?--serial needs a value}" ;;
    --adb-port) shift; export ANDROID_ADB_SERVER_PORT="${1:?--adb-port needs a value}" ;;
    --dry-run) DRY=1 ;;
    -h|--help|help) sed -n '3,30p' "${BASH_SOURCE[0]}"; exit 0 ;;
    --) shift; break ;;
    -*) die "unknown option $1 (try --help)" ;;
    *) break ;;
  esac
  shift
done
CMD="${1:-}"; shift || true
[[ -n $CMD ]] || { sed -n '3,30p' "${BASH_SOURCE[0]}"; exit 2; }

# --------------------------------------------------------------------------- helpers
find_adb() {
  if [[ -n ${GEEKBRIDGE_ADB:-} ]]; then printf '%s' "$GEEKBRIDGE_ADB"
  elif command -v adb >/dev/null 2>&1; then command -v adb
  elif [[ -x $ROOT/.tools/android-sdk/platform-tools/adb ]]; then printf '%s' "$ROOT/.tools/android-sdk/platform-tools/adb"
  fi
}
ADBBIN="$(find_adb)"

# Run (or, with --dry-run, just print) a state-changing command.
run() {
  if ((DRY)); then printf '  [dry-run] %s\n' "$*"; return 0; fi
  "$@"
}
A() { "$ADBBIN" -s "$SERIAL" "$@"; }                 # adb for the chosen phone
sh_() { A shell "$@" 2>/dev/null | tr -d '\r'; }      # adb shell, CRLF stripped
prop() { sh_ getprop "$1"; }

need_adb() { [[ -n $ADBBIN && -x $ADBBIN ]] || die "adb not found. Install android-tools (pacman -S android-tools) or run scripts/bootstrap-android.sh (then $ROOT/.tools/android-sdk/platform-tools/adb is used)."; }

# Sets SERIAL (and IS_EMU). Returns 1 with a message if no usable phone.
pick_device() {
  need_adb
  local line s st devs=() real=() emu=() other=()
  while read -r s st _; do
    [[ -z $s || $s == List ]] && continue
    case "$st" in
      device) devs+=("$s"); if [[ $s == emulator-* ]]; then emu+=("$s"); else real+=("$s"); fi ;;
      unauthorized) other+=("$s unauthorized") ;;
      offline) other+=("$s offline") ;;
      *) other+=("$s $st") ;;
    esac
  done < <("$ADBBIN" devices 2>/dev/null | tr -d '\r' | sed 1d)
  if [[ -n $SERIAL ]]; then
    local found=0; for s in "${devs[@]}"; do [[ $s == "$SERIAL" ]] && found=1; done
    ((found)) || { for line in "${other[@]}"; do [[ $line == "$SERIAL "* ]] && PICK_ERR="$line"; done; PICK_ERR="${PICK_ERR:-$SERIAL is not attached}"; return 1; }
  elif ((${#real[@]} == 1)); then SERIAL="${real[0]}"
  elif ((${#real[@]} > 1)); then PICK_ERR="several phones attached (${real[*]}); pass --serial"; return 1
  elif ((${#emu[@]} == 1)); then SERIAL="${emu[0]}"
  elif ((${#emu[@]} > 1)); then PICK_ERR="only emulators attached (${emu[*]}); pass --serial"; return 1
  else
    PICK_ERR="no device. ${other[*]:-}"; return 1
  fi
  IS_EMU=0; [[ $SERIAL == emulator-* ]] && IS_EMU=1
  return 0
}
need_device() {
  if ! pick_device; then
    printf '%s %s\n' "$(c 31 'error:')" "${PICK_ERR}" >&2
    echo "       Plug the phone in, enable Developer options > USB debugging, accept the RSA prompt, then 'scripts/realdevice.sh check'." >&2
    exit 1
  fi
}

ip2int() { local a b c d; IFS=. read -r a b c d <<<"$1"; echo $(( (a<<24)|(b<<16)|(c<<8)|d )); }
int2ip() { echo "$(( ($1>>24)&255 )).$(( ($1>>16)&255 )).$(( ($1>>8)&255 )).$(( $1&255 ))"; }
mask_of() { echo $(( (0xFFFFFFFF << (32-$1)) & 0xFFFFFFFF )); }
net_of() { local a="${1%/*}" l="${1#*/}"; echo "$(int2ip $(( $(ip2int "$a") & $(mask_of "$l") )))/$l"; }
same_net() { # addr1/len addr2/len -> 0 if equal networks under both masks
  local a1="${1%/*}" l1="${1#*/}" a2="${2%/*}" l2="${2#*/}" l
  for l in "$l1" "$l2"; do
    (( ($(ip2int "$a1") & $(mask_of "$l")) == ($(ip2int "$a2") & $(mask_of "$l")) )) || return 1
  done
}
# PC global IPv4 addresses as addr/len, one per line, without container bridges.
pc_addrs() { ip -4 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth|virbr|lxc|podman|cni)/ {print $4}'; }
pc_lan_cidr() { # the network of the default route's interface
  local dev; dev="$(ip -4 route show default 2>/dev/null | awk '{for(i=1;i<NF;i++) if($i=="dev"){print $(i+1); exit}}')"
  local a; a="$(ip -4 -o addr show dev "$dev" scope global 2>/dev/null | awk '{print $4; exit}')"
  [[ -n $a ]] && net_of "$a"
}

# Masks things that should not end up in a pasted bug report: pairing links and tokens, MAC addresses,
# the last two IPv4 octets. With "numbers" also phone-number-like digit runs.
redact() {
  local sedx=(-E
    -e 's#geekbridge://pair\?[^[:space:]"]*#geekbridge://pair?<redacted>#g'
    -e 's/([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}/xx:xx:xx:xx:xx:xx/g'
    -e 's/\b([0-9]{1,3})\.([0-9]{1,3})\.[0-9]{1,3}\.[0-9]{1,3}\b/\1.\2.x.x/g')
  if [[ ${1:-} == numbers ]]; then
    sedx+=(-e 's/\+[0-9][0-9 ()-]{6,}[0-9]/<number>/g' -e 's/\b[0-9]{10,}\b/<digits>/g')
  fi
  sed "${sedx[@]}"
}

ctl() { timeout 20 "$BIN/geekbridge-ctl" --state-dir "$STATE" --control-port "$CTRL_PORT" "$@"; }

# --------------------------------------------------------------------------- firewall
# Reads only world-readable files, so it works without root. Sets FW_KIND, FW_STATE (open|blocked|unknown|none).
fw_detect() {
  FW_KIND=none FW_STATE=none FW_DETAIL=""
  if command -v ufw >/dev/null 2>&1 && { [[ "$(systemctl is-active ufw 2>/dev/null)" == active ]] \
       || grep -qi '^ENABLED=yes' /etc/ufw/ufw.conf 2>/dev/null; }; then
    FW_KIND=ufw
    local pol t u
    pol="$(sed -n 's/^DEFAULT_INPUT_POLICY="\{0,1\}\([A-Z]*\).*/\1/p' /etc/default/ufw 2>/dev/null)"
    FW_DETAIL="ufw active, default incoming policy ${pol:-unknown}"
    if [[ -r /etc/ufw/user.rules ]]; then
      t=0 u=0
      grep -qE -- "-p tcp .*--dports? ([0-9,:]*,)?($FW_LO:$FW_HI)( |,|\$)" /etc/ufw/user.rules && t=1
      grep -qE -- "-p udp .*--dports? ([0-9,:]*,)?($FW_LO:$FW_HI)( |,|\$)" /etc/ufw/user.rules && u=1
      if ((t && u)); then FW_STATE=open; FW_DETAIL+="; rules for $FW_LO:$FW_HI tcp+udp present"
      elif [[ $pol == ACCEPT ]]; then FW_STATE=open; FW_DETAIL+=" (everything allowed)"
      else FW_STATE=blocked; FW_DETAIL+="; no complete $FW_LO:$FW_HI rule (tcp:$t udp:$u)"; fi
    else FW_STATE=unknown; FW_DETAIL+="; /etc/ufw/user.rules not readable (check: sudo ufw status)"; fi
  elif command -v firewall-cmd >/dev/null 2>&1 && [[ "$(systemctl is-active firewalld 2>/dev/null)" == active ]]; then
    FW_KIND=firewalld
    local ports; ports="$(firewall-cmd --list-ports 2>/dev/null)"
    FW_DETAIL="firewalld active, open ports: ${ports:-none}"
    if grep -q "$FW_LO-$FW_HI/tcp" <<<"$ports" && grep -q "$FW_LO-$FW_HI/udp" <<<"$ports"; then FW_STATE=open; else FW_STATE=unknown
      FW_DETAIL+=" (rich rules are not inspected; check: sudo firewall-cmd --list-all)"; fi
  elif command -v nft >/dev/null 2>&1 && [[ "$(systemctl is-active nftables 2>/dev/null)" == active ]]; then
    FW_KIND=nftables
    local rs; rs="$(nft list ruleset 2>/dev/null)"
    if [[ -z $rs ]]; then FW_STATE=unknown; FW_DETAIL="nftables active, ruleset needs root (check: sudo nft list ruleset | grep -E '$FW_LO|input')"
    elif grep -qE "dport (\{[^}]*)?$FW_LO(-$FW_HI)?" <<<"$rs"; then FW_STATE=open; FW_DETAIL="nftables rule mentioning $FW_LO found"
    elif grep -qE 'policy (drop|reject)' <<<"$rs"; then FW_STATE=blocked; FW_DETAIL="nftables with a drop policy and no rule for $FW_LO"
    else FW_STATE=open; FW_DETAIL="nftables active, no drop policy"; fi
  else
    FW_DETAIL="no active ufw / firewalld / nftables service found"
  fi
}

fw_commands() { # prints allow commands for FW_KIND; NET = LAN cidr or empty (then any source)
  local net="$1" src=""
  case "$FW_KIND" in
    ufw)
      [[ -n $net ]] && src="from $net "
      echo "sudo ufw allow ${src}to any port $FW_LO:$FW_HI proto tcp comment 'GeekBridge'"
      echo "sudo ufw allow ${src}to any port $FW_LO:$FW_HI proto udp comment 'GeekBridge'" ;;
    firewalld)
      if [[ -n $net ]]; then
        echo "sudo firewall-cmd --add-rich-rule='rule family=\"ipv4\" source address=\"$net\" port port=\"$FW_LO-$FW_HI\" protocol=\"tcp\" accept'"
        echo "sudo firewall-cmd --add-rich-rule='rule family=\"ipv4\" source address=\"$net\" port port=\"$FW_LO-$FW_HI\" protocol=\"udp\" accept'"
      else
        echo "sudo firewall-cmd --add-port=$FW_LO-$FW_HI/tcp --add-port=$FW_LO-$FW_HI/udp"
      fi ;;
    nftables)
      [[ -n $net ]] && src="ip saddr $net "
      echo "sudo nft insert rule inet filter input ${src}tcp dport $FW_LO-$FW_HI accept   # adjust table/chain to your ruleset"
      echo "sudo nft insert rule inet filter input ${src}udp dport $FW_LO-$FW_HI accept" ;;
  esac
}
fw_revert() {
  local net="$1" src=""
  case "$FW_KIND" in
    ufw) [[ -n $net ]] && src="from $net "
      echo "sudo ufw delete allow ${src}to any port $FW_LO:$FW_HI proto tcp"
      echo "sudo ufw delete allow ${src}to any port $FW_LO:$FW_HI proto udp" ;;
    firewalld) echo "(firewalld rules above are runtime-only: 'sudo firewall-cmd --reload' removes them)" ;;
    nftables) echo "sudo nft list ruleset -a   # find the two handles, then: sudo nft delete rule inet filter input handle N" ;;
  esac
}

firewall_section() { # [apply]
  local apply="${1:-}" net cmds line
  say "Host firewall (ports $FW_LO-$FW_HI TCP+UDP, inbound from the phone)"
  fw_detect
  net="$(pc_lan_cidr)"
  case "$FW_STATE" in
    none) ok "$FW_DETAIL"; return 0 ;;
    open) ok "$FW_KIND: $FW_DETAIL"; return 0 ;;
    unknown) warn "$FW_KIND: $FW_DETAIL" ;;
    blocked) bad "$FW_KIND: $FW_DETAIL. The phone cannot reach this PC until this is opened." ;;
  esac
  echo "       Allow commands (limited to your LAN ${net:-<unknown, so any source>}); nothing is applied without --apply:"
  cmds="$(fw_commands "$net")"
  while IFS= read -r line; do echo "         $line"; done <<<"$cmds"
  echo "       Remove them after the session:"
  while IFS= read -r line; do echo "         $line"; done < <(fw_revert "$net")
  if [[ $apply == apply ]]; then
    [[ $FW_KIND == nftables ]] && { warn "not applying nftables rules automatically (table/chain names differ per setup)"; return 0; }
    echo "       --apply given: running the allow commands now (sudo may ask for your password)."
    while IFS= read -r line; do
      line="${line%%   #*}"
      if ((DRY)); then printf '  [dry-run] %s\n' "$line"; else bash -c "$line" || bad "failed: $line"; fi
    done <<<"$cmds"
    ((DRY)) || { fw_detect; [[ $FW_STATE == open ]] && ok "firewall now open: $FW_DETAIL" || warn "re-check: $FW_DETAIL"; }
  fi
}

# --------------------------------------------------------------------------- check
oem_hint() { # prints "skin|wizard hint"
  local man model skin="" hint=""
  man="$(prop ro.product.manufacturer | tr 'A-Z' 'a-z')"
  [[ -n $(prop ro.miui.ui.version.name) ]] && skin="MIUI/HyperOS $(prop ro.miui.ui.version.name)"
  [[ -z $skin && -n $(prop ro.mi.os.version.name) ]] && skin="HyperOS $(prop ro.mi.os.version.name)"
  [[ -z $skin && -n $(prop ro.build.version.oneui) ]] && skin="One UI $(prop ro.build.version.oneui)"
  [[ -z $skin && -n $(prop ro.build.version.oplusrom) ]] && skin="ColorOS/OxygenOS $(prop ro.build.version.oplusrom)"
  [[ -z $skin && -n $(prop ro.oxygen.version) ]] && skin="OxygenOS $(prop ro.oxygen.version)"
  [[ -z $skin && -n $(prop ro.build.version.emui) ]] && skin="EMUI $(prop ro.build.version.emui)"
  [[ -z $skin && -n $(prop ro.vivo.os.version) ]] && skin="Funtouch/OriginOS $(prop ro.vivo.os.version)"
  [[ -z $skin && -n $(prop ro.build.nothing.version) ]] && skin="Nothing OS $(prop ro.build.nothing.version)"
  [[ -z $skin && -n $(prop ro.nothing.version) ]] && skin="Nothing OS $(prop ro.nothing.version)"
  case "$man" in
    samsung*) hint="Samsung: after the wizard also Settings > Battery > Background usage limits > Never sleeping apps" ;;
    xiaomi*|redmi*|poco*) hint="Xiaomi: also Security > Permissions > Autostart ON, app info > Battery saver > No restrictions, lock in Recents" ;;
    oneplus*|oppo*|realme*) hint="OnePlus/OPPO: also Battery > Battery optimisation > Don't optimise, allow auto-launch/background activity" ;;
    google*) hint="Pixel: the system dialog from the wizard is all that is needed" ;;
    huawei*|honor*) hint="Huawei/Honor: also App launch > Manage manually (auto-launch, secondary launch, run in background)" ;;
    nothing*) hint="Nothing OS (near-stock): the wizard dialog; check Battery > Battery optimisation" ;;
    *) hint="no vendor-specific steps known for '$man': use the wizard dialog and report what the vendor screen looked like" ;;
  esac
  printf '%s|%s' "${skin:-stock/unknown skin}" "$hint"
}

check_phone() {
  say "Phone"
  need_adb
  ok "adb: $("$ADBBIN" version 2>/dev/null | head -1) ($ADBBIN)"
  [[ -n ${ANDROID_ADB_SERVER_PORT:-} ]] && info "adb server port $ANDROID_ADB_SERVER_PORT"
  if ! pick_device; then
    bad "no usable phone: $PICK_ERR"
    info "Enable Developer options > USB debugging, use a data cable, accept 'Allow USB debugging?' (tick 'Always allow')."
    info "'unauthorized' means the RSA prompt on the phone is still waiting; 'offline': unplug, replug, or: adb kill-server"
    "$ADBBIN" devices -l 2>/dev/null | sed 1d | sed '/^$/d;s/^/       /'
    return 1
  fi
  ok "device authorised: $SERIAL$( ((IS_EMU)) && echo ' (emulator: rehearsal only)')"
  local rel api man model abis hint skin spl
  rel="$(prop ro.build.version.release)"; api="$(prop ro.build.version.sdk)"
  man="$(prop ro.product.manufacturer)"; model="$(prop ro.product.model)"
  abis="$(prop ro.product.cpu.abilist)"; spl="$(prop ro.build.version.security_patch)"
  hint="$(oem_hint)"; skin="${hint%%|*}"; hint="${hint#*|}"
  info "device : $man $model, Android $rel (API $api), security patch ${spl:-?}"
  info "skin   : $skin"
  info "ABIs   : $abis"
  if [[ -z $api ]]; then bad "could not read the Android API level"
  elif ((api < MIN_API)); then bad "Android API $api is below the app's minSdk $MIN_API"
  elif ((api < 29)); then warn "Android $rel (API $api): works, but the test plan assumes Android 10+; some call/background rows may differ"
  else ok "Android $rel / API $api is supported"; fi
  info "battery wizard to expect: $hint"

  local free_kb free_mb
  free_kb="$(sh_ df -k /data | awk 'NR==2 {print $4}')"
  if [[ $free_kb =~ ^[0-9]+$ ]]; then
    free_mb=$((free_kb/1024))
    if ((free_mb < 300)); then bad "only ${free_mb} MB free on /data (need ~300 MB for install and file-transfer tests)"
    elif ((free_mb < 1500)); then warn "${free_mb} MB free on /data (500 MB file test needs ~1 GB)"
    else ok "${free_mb} MB free on /data"; fi
  else warn "could not read free space"; fi

  local bt wifi
  bt="$(sh_ settings get global bluetooth_on)"
  case "$bt" in 1) ok "phone Bluetooth is on" ;; 0) warn "phone Bluetooth is OFF (needed for the call/hands-free test)" ;; *) warn "phone Bluetooth state unknown" ;; esac
  wifi="$(sh_ cmd wifi status | head -1)"; [[ -z $wifi ]] && wifi="$(sh_ settings get global wifi_on)"
  if grep -qiE 'enabled|^1$' <<<"$wifi"; then ok "phone Wi-Fi is on"; else warn "phone Wi-Fi: ${wifi:-unknown} (GeekBridge pairs over the LAN, Wi-Fi must be on)"; fi

  # same subnet
  local pip pipcidr mine hit=0 iface
  pip="$(sh_ ip -4 -o addr show | awk '$2 ~ /^(wlan|swlan|wifi)/ {print $2, $4; exit}')"
  # The emulator's "Wi-Fi" is eth0 behind NAT (10.0.2.x); accept it so the rest of the check can be rehearsed.
  ((IS_EMU)) && [[ -z $pip ]] && pip="$(sh_ ip -4 -o addr show | awk '$2 == "eth0" {print $2, $4; exit}')"
  if [[ -z $pip ]]; then
    bad "phone has no Wi-Fi IPv4 address (not connected to a Wi-Fi network?)"
  else
    iface="${pip%% *}"; pipcidr="${pip#* }"
    info "phone Wi-Fi: ${pipcidr%/*}x ($iface)  [masked]"
    while read -r mine; do
      [[ -z $mine ]] && continue
      if same_net "$pipcidr" "$mine"; then hit=1; ok "phone and this PC are on the same subnet ($(net_of "$mine"))"; fi
    done < <(pc_addrs)
    if ((!hit)); then
      local mine_nets="" m; while read -r m; do [[ -n $m ]] && mine_nets+="$(net_of "$m") "; done < <(pc_addrs)
      if ((IS_EMU)); then warn "emulator is NAT-ed ($(net_of "$pipcidr")), PC is ${mine_nets}: expected for an emulator"
      else bad "phone ($(net_of "$pipcidr")) and this PC (${mine_nets% }) are on different subnets"; fi
      info "Guest Wi-Fi / client isolation / a repeater on another subnet also break discovery. QR pairing needs a routable path to the PC."
    fi
  fi
  PHONE_FOUND=1
}

check_desktop() {
  say "This PC"
  local b missing=0
  for b in geekbridge-service geekbridge-ctl geekbridge geekbridge-calls geekbridge-share geekbridge-clipboard; do
    [[ -x $BIN/$b ]] || { bad "missing $BIN/$b"; missing=1; }
  done
  ((missing)) && info "build: cargo build --release --locked -p bridge-apps -p bridge-service" || ok "release binaries present in target/release"
  if [[ -x $BIN/geekbridge-ctl ]] && ctl status >/dev/null 2>&1; then
    ok "service answers (control port $CTRL_PORT, state dir $STATE)"
    ctl calling 2>&1 | head -4 | sed 's/^/       calling: /'
  else
    warn "service not running: scripts/realdevice.sh desktop"
  fi
  [[ -n ${WAYLAND_DISPLAY:-} ]] && ok "Wayland session ($WAYLAND_DISPLAY), compositor ${XDG_CURRENT_DESKTOP:-?}" \
    || warn "WAYLAND_DISPLAY not set in this shell; run from a terminal inside Hyprland (clipboard and popups need it)"
  if command -v wl-paste >/dev/null 2>&1 && command -v wl-copy >/dev/null 2>&1; then ok "wl-clipboard present"; else warn "wl-copy/wl-paste missing (pacman -S wl-clipboard)"; fi
  local btp
  if command -v bluetoothctl >/dev/null 2>&1; then
    btp="$(bluetoothctl show 2>/dev/null | awk -F': ' '/Powered:/ {print $2}')"
    case "$btp" in yes) ok "PC Bluetooth controller powered on" ;; no) warn "PC Bluetooth is powered off (bluetoothctl power on)" ;;
      *) warn "no PC Bluetooth controller found (calls need one; Answer/Decline popups work only with it)" ;; esac
  else warn "bluetoothctl not installed"; fi
  local wifi_pc; wifi_pc="$(ip -4 -o addr show scope global 2>/dev/null | awk '$2 !~ /^(docker|br-|veth|lo)/ {printf "%s %s; ", $2, $4}')"
  info "PC addresses: ${wifi_pc:-none}"
}

cmd_check() {
  local apply=""; [[ ${1:-} == --apply ]] && apply=apply
  PHONE_FOUND=0
  check_phone
  check_desktop
  firewall_section "$apply"
  summary
}

cmd_firewall() {
  local apply=""; [[ ${1:-} == --apply ]] && apply=apply
  firewall_section "$apply"; summary
}

# --------------------------------------------------------------------------- install
GRANTS=(android.permission.READ_CONTACTS android.permission.READ_CALENDAR android.permission.READ_PHONE_STATE
        android.permission.ANSWER_PHONE_CALLS android.permission.POST_NOTIFICATIONS)

find_apk() {
  if [[ -n ${APK_ARG:-} ]]; then APK="$APK_ARG"; return; fi
  APK="$ROOT/dist/android/geekbridge-debug.apk"
}
build_apk() {
  say "Building the debug APK (gradle --offline :app:assembleDebug)"
  if ((DRY)); then echo "  [dry-run] source scripts/android-env.sh; (cd android && gradle --offline :app:assembleDebug); cp ... dist/android/geekbridge-debug.apk"; return 0; fi
  # shellcheck disable=SC1091
  ( source "$ROOT/scripts/android-env.sh" && cd "$ROOT/android" && gradle --offline :app:assembleDebug ) || die "gradle build failed"
  mkdir -p "$ROOT/dist/android"
  cp "$ROOT/android/app/build/outputs/apk/debug/app-debug.apk" "$ROOT/dist/android/geekbridge-debug.apk" || die "built APK not found"
  ( cd "$ROOT/dist/android" && sha256sum geekbridge-debug.apk > SHA256SUMS )
  ok "dist/android/geekbridge-debug.apk updated"
}

cmd_install() {
  local build=0 grant=1 start=1 APK_ARG=""
  while (($#)); do
    case "$1" in
      --apk) shift; APK_ARG="${1:?--apk needs a file}" ;;
      --build) build=1 ;;
      --no-grant) grant=0 ;;
      --no-start) start=0 ;;
      *) die "install: unknown argument $1" ;;
    esac; shift
  done
  need_device
  ((build)) && build_apk
  find_apk
  [[ -f $APK ]] || die "APK not found: $APK (use --build, or --apk FILE)"
  say "Installing $(basename "$APK") on $SERIAL"
  info "$APK ($(du -h "$APK" | cut -f1)), built $(date -r "$APK" '+%F %T')"
  local newer; newer="$(find "$ROOT/android/app/src" "$ROOT/crates/bridge-android/src" -type f -newer "$APK" 2>/dev/null | head -3)"
  [[ -n $newer ]] && warn "sources are newer than this APK (e.g. ${newer%%$'\n'*}); rebuild with: scripts/realdevice.sh install --build"
  # ABI sanity: the APK must carry a library for one of the phone's ABIs.
  local abis lib
  abis="$(prop ro.product.cpu.abilist)"
  if command -v unzip >/dev/null 2>&1; then
    lib="$(unzip -Z1 "$APK" 2>/dev/null | sed -n 's#^lib/\([^/]*\)/.*#\1#p' | sort -u | tr '\n' ' ')"
    info "APK native ABIs: ${lib:-none}; phone: $abis"
    local match=0 a; for a in ${abis//,/ }; do [[ " $lib " == *" $a "* ]] && match=1; done
    ((match)) || { [[ -n $lib ]] && bad "APK has no native library for this phone's ABIs ($abis)"; }
  fi
  local res
  if ((DRY)); then echo "  [dry-run] adb -s $SERIAL install -r $APK"; res=Success
  else res="$(A install -r "$APK" 2>&1 | tr -d '\r' | tail -3)"; fi
  if grep -q Success <<<"$res"; then ((DRY)) && ok "would install (dry-run)" || ok "installed"
  else
    bad "install failed: $res"
    grep -q USER_RESTRICTED <<<"$res" && info "Xiaomi/Oppo/Vivo: Developer options > 'Install via USB' (needs a Mi account / SIM) must be ON."
    grep -q INSTALL_FAILED_UPDATE_INCOMPATIBLE <<<"$res" && info "A build signed with another key is installed: adb uninstall $PKG (loses pairings) and retry."
    grep -q INSUFFICIENT_STORAGE <<<"$res" && info "Free space on the phone."
    summary; return 1
  fi
  local api; api="$(prop ro.build.version.sdk)"; api="${api:-0}"
  info "installed version: $(sh_ dumpsys package $PKG | awk -F= '/versionName=/ {print $2; exit}')"
  if ((grant)); then
    say "Granting runtime permissions that adb may grant"
    local p
    for p in "${GRANTS[@]}"; do
      [[ $p == *POST_NOTIFICATIONS && $api -lt 33 ]] && { info "skip ${p##*.} (needs Android 13+)"; continue; }
      if ((DRY)); then echo "  [dry-run] adb -s $SERIAL shell pm grant $PKG $p"; continue; fi
      res="$(A shell pm grant "$PKG" "$p" 2>&1 | tr -d '\r')"
      if [[ -z $res ]]; then ok "granted ${p##*.}"; else warn "${p##*.}: $res"; fi
    done
    info "Left for you on purpose (exercise the in-app flows): battery exemption wizard, calendar editing (WRITE_CALENDAR),"
    info "SMS + call log (restricted: on Android 13+ App info > ⋮ > 'Allow restricted settings' first), 'install unknown apps'."
    info "Use --no-grant to test the first-run permission prompts instead."
  fi
  if ((start)); then
    say "Starting the app"
    if ((DRY)); then echo "  [dry-run] adb -s $SERIAL shell am start -n $PKG/.HomeActivity"
    else
      A shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
      res="$(A shell am start -n "$PKG/.HomeActivity" 2>&1 | tr -d '\r')"
      if grep -qi 'error\|exception' <<<"$res"; then
        A shell monkey -p "$PKG" -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1 && ok "started (monkey fallback)" || bad "could not start: $res"
      else ok "started"; fi
      sleep 2; local pid; pid="$(sh_ pidof "$PKG")"; [[ -n $pid ]] && ok "process running (pid $pid)" || warn "process not running 2 s after start; run: scripts/realdevice.sh logs"
    fi
  fi
  summary
}

# --------------------------------------------------------------------------- desktop
cmd_desktop() {
  say "Starting the desktop side"
  local args=(--qr "$@")
  if ((DRY)); then echo "  [dry-run] $ROOT/scripts/run-desktop.sh ${args[*]}"; return 0; fi
  "$ROOT/scripts/run-desktop.sh" "${args[@]}" || exit $?
  fw_detect
  [[ $FW_STATE == blocked ]] && printf '\n%s\n' "$(c 33 "firewall ($FW_KIND) is blocking 1716-1764: run scripts/realdevice.sh firewall")"
  cat <<EOF

How to pair (QR):
  1. Phone on the SAME Wi-Fi as this PC. Open GeekBridge > Add computer > Scan QR code.
  2. Point the camera at the QR above (or the one in the GeekBridge window) and tap the GeekBridge link it shows.
  3. The phone asks "Pair with <name>?" - Continue. No code comparison is needed (the QR pins the certificate).
  4. Check: geekbridge-ctl status shows the phone under "paired" with link phase "connected":
       eval "\$(scripts/run-desktop.sh --env)"; geekbridge-ctl status
  QR expired (2 min) or used? Print a new one:  scripts/realdevice.sh desktop
  Cannot connect?  geekbridge-ctl test-connection   (explains the failing step in plain words)
EOF
}

# --------------------------------------------------------------------------- logs
cmd_logs() {
  need_device
  local ts dir pid
  ts="$(date +%Y%m%d-%H%M%S)"; dir="$OUT/$ts"
  say "Collecting into dist/realdevice/$ts/"
  if ((DRY)); then echo "  [dry-run] would write logcat*.txt, dumpsys-*.txt, diagnose.txt, README.md to $dir"; return 0; fi
  mkdir -p "$dir"; chmod 700 "$dir"
  pid="$(sh_ pidof "$PKG" | awk '{print $1}')"
  local man model rel api; man="$(prop ro.product.manufacturer)"; model="$(prop ro.product.model)"
  rel="$(prop ro.build.version.release)"; api="$(prop ro.build.version.sdk)"
  {
    echo "device: $man $model, Android $rel (API $api), $(oem_hint | cut -d'|' -f1)"
    echo "build : $(prop ro.build.display.id)"
    echo "app   : $(sh_ dumpsys package $PKG | awk -F= '/versionName=|versionCode=/ {printf "%s ", $0}')"
    echo "pid   : ${pid:-not running}"
  } >"$dir/phone-info.txt"
  ok "phone-info.txt"

  if [[ -n $pid ]]; then
    A logcat -d -v threadtime --pid="$pid" 2>/dev/null | tr -d '\r' | redact numbers >"$dir/logcat-app.txt"
    ok "logcat-app.txt (pid $pid, $(wc -l <"$dir/logcat-app.txt") lines)"
  else
    warn "app process not running: logcat-app.txt skipped (the tagged log below may still show why it died)"
  fi
  A logcat -d -v threadtime 2>/dev/null | tr -d '\r' | grep -iE 'geekbridge|bridge_android|AndroidRuntime|FATAL EXCEPTION|ANR in|am_proc_died|am_kill|ActivityManager.*(Killing|died)' \
    | redact numbers >"$dir/logcat-tagged.txt"
  ok "logcat-tagged.txt ($(wc -l <"$dir/logcat-tagged.txt") lines; includes crashes/ANRs/kills of any app that name geekbridge)"
  A logcat -d -b crash -v threadtime 2>/dev/null | tr -d '\r' | redact numbers >"$dir/logcat-crash.txt"
  [[ -s $dir/logcat-crash.txt ]] && warn "logcat-crash.txt is NOT empty: $(wc -l <"$dir/logcat-crash.txt") lines" || ok "crash buffer empty"

  dump() { # file title command...
    local f="$1" t="$2"; shift 2
    { echo "### $t"; "$@" 2>&1 | tr -d '\r'; } | redact >"$dir/$f"
  }
  dump dumpsys-battery.txt "dumpsys battery" A shell dumpsys battery
  dump dumpsys-batterystats.txt "dumpsys batterystats $PKG" A shell dumpsys batterystats "$PKG"
  { echo "### dumpsys batterystats --charged (estimated power use, wake locks and alarms for geekbridge)"
    A shell dumpsys batterystats --charged 2>/dev/null | tr -d '\r' | grep -iE -B1 -A12 'Estimated power use' | head -40
    A shell dumpsys batterystats --charged 2>/dev/null | tr -d '\r' | grep -iE 'wake|alarm|job' | grep -i geekbridge
  } | redact >"$dir/dumpsys-batterystats-summary.txt"
  { echo "### deviceidle"
    echo "deep : $(sh_ dumpsys deviceidle get deep)"; echo "light: $(sh_ dumpsys deviceidle get light)"
    echo "--- whitelist entries for $PKG (present = battery-optimisation exemption)"
    A shell dumpsys deviceidle whitelist 2>/dev/null | tr -d '\r' | grep -i "$PKG"
    echo "--- ignoring-battery-optimisation per package manager"
    A shell dumpsys package "$PKG" 2>/dev/null | tr -d '\r' | grep -iE 'ignoring|standby'
    echo "--- app standby bucket: $(sh_ am get-standby-bucket "$PKG")"
  } | redact >"$dir/dumpsys-deviceidle.txt"
  dump dumpsys-meminfo.txt "dumpsys meminfo $PKG" A shell dumpsys meminfo "$PKG"
  { echo "### services / alarms / permissions"
    A shell dumpsys activity services "$PKG" 2>/dev/null | tr -d '\r' | head -60
    echo "--- alarms"; A shell dumpsys alarm 2>/dev/null | tr -d '\r' | grep -i "$PKG" | head -20
    echo "--- granted permissions"; A shell dumpsys package "$PKG" 2>/dev/null | tr -d '\r' | grep -E 'granted=|versionName|lastUpdateTime'
    echo "--- appops (non-default)"; A shell cmd appops get "$PKG" 2>/dev/null | tr -d '\r'
  } | redact >"$dir/dumpsys-package-services.txt"
  ok "dumpsys-*.txt (battery, batterystats, deviceidle, meminfo, package/services)"

  if [[ -x $BIN/geekbridge-ctl ]] && ctl status >/dev/null 2>&1; then
    ctl diagnose 2>&1 | redact >"$dir/diagnose.txt"; ok "diagnose.txt (geekbridge-ctl diagnose, extra-masked)"
  else warn "service not running: diagnose.txt skipped"; echo "service was not running" >"$dir/diagnose.txt"; fi
  [[ -f $OUT/service.log ]] && { tail -n 400 "$OUT/service.log" | redact numbers >"$dir/service-log-tail.txt"; ok "service-log-tail.txt"; }

  cat >"$dir/README.md" <<EOF
# GeekBridge real-device log bundle $ts

Phone: $man $model, Android $rel (API $api) - serial not recorded.
Collected: $(date '+%F %T %z') with \`scripts/realdevice.sh logs\`.

| File | What it is | What to look for |
| --- | --- | --- |
| phone-info.txt | model, Android, app version, pid | pid "not running" = the app was dead when collected |
| logcat-app.txt | \`adb logcat -d\` of the app's pid (threadtime) | the lines named in the quickstart step you are checking |
| logcat-tagged.txt | whole log filtered for geekbridge, crashes, ANRs, process kills | \`am_proc_died\`/\`Killing\` = the OEM killed the app |
| logcat-crash.txt | Android crash buffer | must be empty |
| dumpsys-battery.txt | level, plugged state, temperature, charge counter | |
| dumpsys-batterystats*.txt | wake locks, alarms, jobs, estimated power use for the package | wake lock time, "Estimated power use" share |
| dumpsys-deviceidle.txt | Doze state, battery-optimisation whitelist | the package under whitelist = exemption granted |
| dumpsys-meminfo.txt | PSS / heap of the app | TOTAL PSS <= 60 MB background, <= 120 MB with Home open |
| dumpsys-package-services.txt | LinkService running?, alarms, granted permissions | \`LinkService\` listed after killing the app = restarted |
| diagnose.txt | \`geekbridge-ctl diagnose\` of the PC service | service version, links, clipboard/share/call counters |
| service-log-tail.txt | last 400 lines of dist/realdevice/service.log | |

Privacy: the files are masked (pairing links, MAC addresses, the last two IPv4 octets, long digit runs and
+numbers in logcat). dumpsys output is not scrubbed of everything - read it before attaching it to a public issue.
Android's log buffer is small (about 256 KB): collect right after the step you want to prove.
EOF
  ok "README.md"
  echo; echo "       $dir"
  summary
}

# --------------------------------------------------------------------------- battery
BASE="$OUT/battery-baseline.env"
RESULTS="$OUT/battery-results.tsv"
bat() { sh_ dumpsys battery | sed -n "s/^ *$1: *//p" | head -1; }
bat_plugged() { # prints "yes <source>" or "no"
  local u a w; u="$(bat 'USB powered')"; a="$(bat 'AC powered')"; w="$(bat 'Wireless powered')"
  [[ $u == true ]] && { echo "yes USB"; return; }; [[ $a == true ]] && { echo "yes AC"; return; }; [[ $w == true ]] && { echo "yes wireless"; return; }
  echo no
}

cmd_battery_baseline() {
  local minutes="" label=app
  while (($#)); do
    case "$1" in --minutes) shift; minutes="${1:?}" ;; --label) shift; label="${1:?}" ;; *) die "battery-baseline: unknown argument $1" ;; esac; shift
  done
  need_device
  say "Battery baseline ($label)"
  local lvl cc plug
  lvl="$(bat level)"; cc="$(bat 'Charge counter')"; plug="$(bat_plugged)"
  [[ $plug != no ]] && warn "phone is on power ($plug): drain numbers are meaningless. Go wireless and unplug:"
  if [[ $plug != no ]]; then
    local ip; ip="$(sh_ ip -4 -o addr show | awk '$2 ~ /^wlan/ {sub(/\/.*/,"",$4); print $4; exit}')"
    info "adb -s $SERIAL tcpip 5555 && adb connect ${ip:-PHONE_IP}:5555   # then unplug and use --serial ${ip:-PHONE_IP}:5555"
  fi
  if ((DRY)); then echo "  [dry-run] adb shell dumpsys batterystats --enable full-wake-history; --reset; write $BASE"; return 0; fi
  A shell dumpsys batterystats --enable full-wake-history >/dev/null 2>&1
  A shell dumpsys batterystats --reset >/dev/null 2>&1 && ok "batterystats reset" || warn "batterystats --reset failed"
  mkdir -p "$OUT"
  {
    echo "LABEL=$label"; echo "SERIAL=$SERIAL"; echo "START_EPOCH=$(date +%s)"; echo "START_LEVEL=$lvl"
    echo "START_CC=${cc:-}"; echo "PLANNED_MIN=${minutes:-}"; echo "START_PLUG=${plug%% *}"
    echo "START_DEEP=$(sh_ dumpsys deviceidle get deep)"
  } >"$BASE"
  ok "recorded level ${lvl:-?}%, charge counter ${cc:-n/a} uAh at $(date +%T) -> dist/realdevice/battery-baseline.env"
  info "Now: screen off, Wi-Fi on, phone on battery, GeekBridge connected (label '$label')."
  [[ -n $minutes ]] && info "Come back at $(date -d "+$minutes min" +%H:%M) and run: scripts/realdevice.sh battery-report"
  info "Control run (app stopped): adb shell am force-stop $PKG, then battery-baseline --label control and compare."
}

cmd_battery_report() {
  need_device
  [[ -f $BASE ]] || die "no baseline: run scripts/realdevice.sh battery-baseline first"
  # shellcheck disable=SC1090
  . "$BASE"
  say "Battery report ($LABEL)"
  local now elapsed lvl cc plug drain dcc per_h mah_h
  now="$(date +%s)"; elapsed=$(( (now - START_EPOCH) / 60 ))
  lvl="$(bat level)"; cc="$(bat 'Charge counter')"; plug="$(bat_plugged)"
  [[ $plug != no ]] && warn "phone is on power ($plug) now: the result is not a drain measurement"
  [[ -n ${PLANNED_MIN:-} ]] && ((elapsed < PLANNED_MIN)) && warn "only $elapsed of the planned $PLANNED_MIN minutes have passed"
  ((elapsed < 1)) && elapsed=1
  drain=$(( START_LEVEL - lvl ))
  per_h="$(awk -v d="$drain" -v m="$elapsed" 'BEGIN{printf "%.2f", d*60/m}')"
  info "elapsed         : $elapsed min"
  info "battery level   : $START_LEVEL% -> $lvl%  (drain $drain%, $per_h %/h; 1% steps, so short runs are coarse)"
  if [[ -n ${START_CC:-} && $START_CC =~ ^[0-9]+$ && $cc =~ ^[0-9]+$ ]]; then
    dcc=$(( START_CC - cc ))
    mah_h="$(awk -v d="$dcc" -v m="$elapsed" 'BEGIN{printf "%.1f", d/1000*60/m}')"
    info "charge counter  : $START_CC -> $cc uAh  (used $((dcc/1000)) mAh, $mah_h mAh/h)"
  fi
  info "Doze (deep)     : was ${START_DEEP:-?}, now $(sh_ dumpsys deviceidle get deep)"
  local dir="$OUT/battery-$(date +%Y%m%d-%H%M%S)-$LABEL"
  mkdir -p "$dir"
  A shell dumpsys batterystats "$PKG" 2>/dev/null | tr -d '\r' | redact >"$dir/batterystats-$PKG.txt"
  A shell dumpsys batterystats --charged 2>/dev/null | tr -d '\r' | redact >"$dir/batterystats-all.txt"
  A shell dumpsys battery 2>/dev/null | tr -d '\r' >"$dir/battery.txt"
  echo "--- app share (Estimated power use):"
  grep -iE -A14 'Estimated power use' "$dir/batterystats-all.txt" | grep -iE 'Capacity|Computed|Global|Uid|geekbridge|u0a' | head -12 | sed 's/^/       /'
  echo "--- wake locks / alarms / jobs (first lines of batterystats-$PKG.txt; read the whole file for the uid section):"
  grep -iE 'wake lock|wakelock|alarm|job' "$dir/batterystats-$PKG.txt" | head -12 | sed 's/^/       /'
  echo "--- thresholds (docs/real-device-test-plan.md section 9): app minus control <= 1.0 %/h; wake locks <= 2 min/h;"
  echo "    alarms <= 30/h; app share <= 2% and never top 3; Doze reaches IDLE."
  mkdir -p "$OUT"
  [[ -f $RESULTS ]] || printf 'time\tlabel\tminutes\tlevel_start\tlevel_end\tpct_per_hour\n' >"$RESULTS"
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$(date +%F_%T)" "$LABEL" "$elapsed" "$START_LEVEL" "$lvl" "$per_h" >>"$RESULTS"
  local other
  if [[ $LABEL == app ]]; then other="$(awk -F'\t' '$2=="control" {v=$6} END{print v}' "$RESULTS")"
  else other="$(awk -F'\t' '$2=="app" {v=$6} END{print v}' "$RESULTS")"; fi
  if [[ -n $other ]]; then
    local diff
    if [[ $LABEL == app ]]; then diff="$(awk -v a="$per_h" -v b="$other" 'BEGIN{printf "%.2f", a-b}')"
    else diff="$(awk -v a="$other" -v b="$per_h" 'BEGIN{printf "%.2f", a-b}')"; fi
    echo "--- app minus control (latest of each): $diff %/h  [pass: <= 1.0]"
  fi
  echo "       saved: $dir   history: dist/realdevice/battery-results.tsv"
  summary
}

# --------------------------------------------------------------------------- screens
cmd_screens() {
  local label="" count=1 interval=2
  while (($#)); do
    case "$1" in --count) shift; count="${1:?}" ;; --interval) shift; interval="${1:?}" ;; -*) die "screens: unknown argument $1" ;; *) label="$1" ;; esac; shift
  done
  need_device
  label="${label:-screen}"; label="${label//[^A-Za-z0-9_.-]/_}"
  local dir="$OUT/screens" i f
  say "Screenshots -> dist/realdevice/screens/"
  if ((DRY)); then echo "  [dry-run] adb -s $SERIAL exec-out screencap -p > $dir/<time>-$label.png (x$count)"; return 0; fi
  mkdir -p "$dir"
  case "$(sh_ dumpsys power | grep -oE 'mWakefulness=[A-Za-z]+' | head -1)" in
    mWakefulness=Awake) ;; *) warn "screen looks off or dozing: the capture will be black (adb shell input keyevent KEYCODE_WAKEUP)" ;; esac
  for ((i = 1; i <= count; i++)); do
    f="$dir/$(date +%Y%m%d-%H%M%S)-$label$( ((count > 1)) && printf -- '-%02d' "$i" ).png"
    if A exec-out screencap -p >"$f" 2>/dev/null && [[ -s $f ]]; then ok "$(basename "$f") ($(du -h "$f" | cut -f1))"
    else bad "screencap failed"; rm -f "$f"; fi
    ((i < count)) && sleep "$interval"
  done
  info "Banking apps and the password field are blacked out by FLAG_SECURE; that is expected."
  summary
}

# --------------------------------------------------------------------------- dispatch
case "$CMD" in
  check) cmd_check "$@" ;;
  firewall) cmd_firewall "$@" ;;
  install) cmd_install "$@" ;;
  desktop) cmd_desktop "$@" ;;
  logs) cmd_logs "$@" ;;
  battery-baseline) cmd_battery_baseline "$@" ;;
  battery-report) cmd_battery_report "$@" ;;
  screens) cmd_screens "$@" ;;
  *) die "unknown command '$CMD' (try --help)" ;;
esac
