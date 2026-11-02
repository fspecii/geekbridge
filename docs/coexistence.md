# Running next to stock KDE Connect

Many people already run KDE Connect (the `kdeconnectd` daemon on the computer, the app on the phone). It uses the
same discovery protocol and the same ports as GeekBridge: **UDP 1716** for the "I am here" broadcast and **TCP
1716-1764** for the connection itself. GeekBridge is built to share the machine and the phone with it without
either getting in the other's way. Nothing here switches anything off for you.

Results below are from `scripts/e2e/coexist.sh` (stock `kdeconnectd` 26.08.2 on the desktop side, in a throw-away
user+network namespace with a private D-Bus and `HOME`: your real KDE Connect and `~/.config` are never touched)
and `scripts/e2e/coexist-android.sh` (stock KDE Connect Android 13700 and GeekBridge side by side on an emulator).

## What you can rely on

| Question | Answer |
| --- | --- |
| Do I have to stop KDE Connect first? | No. Start either one first, or both. |
| Which TCP port does GeekBridge use? | 1716 if free, otherwise the next free port up to 1764. The identity packet announces the port actually bound (`tcpPort`), so phones dial the right one. Checked on the wire in `coexist.sh` (P1: 1717, P2: 1716). |
| Which UDP port does it listen on for discovery? | 1716, bound with `SO_REUSEADDR` exactly like KDE Connect, so both programs can listen at once. |
| Do the two programs share an identity? | No. GeekBridge keeps its own device id and certificate in its own state directory. It never reads KDE Connect's configuration directory. The phone sees two separate devices (different ids, different certificate fingerprints), and only while each is running. |
| Can GeekBridge answer for KDE Connect's pairings, or the other way round? | No. A pairing request is tied to the connection it arrived on and to the device id and certificate on that connection. A request sent by KDE Connect to GeekBridge is a pending prompt in GeekBridge that needs your approval; it is not answered for you. GeekBridge never dials KDE Connect's listener on its own. |

## Ports, in detail

### Computer: stock daemon first, GeekBridge second

```
kdeconnectd    TCP 1716   UDP 1716
geekbridge     TCP 1717   UDP 1716  (shared)
```

GeekBridge logs one line, shows the same sentence in `geekbridge-ctl status` (on stderr, so the JSON stays machine
readable) and on the Home card of the desktop app:

> Using port 1717 because 1716 is in use by another program: this is fine

### Computer: GeekBridge first, stock daemon second

GeekBridge takes TCP 1716. Stock KDE Connect takes the next free port (observed: 1717) and binds UDP 1716 beside us.

### Discovery packets: who hears what

An identity broadcast is a UDP datagram to `255.255.255.255:1716` (and the subnet broadcast). When several sockets
are bound to the port with `SO_REUSEADDR`, Linux delivers every *broadcast* to **all** of them. So with both
programs running, each one hears the other's announcements and the phone's. Observed in `coexist.sh` with a third
listener added: all three got both identities, in both start orders. Datagrams sent to the computer's own address
(unicast) go to the most recently started socket only; that is a kernel rule, and discovery does not depend on it
because phones broadcast.

If another program holds UDP 1716 **without** `SO_REUSEADDR`, GeekBridge cannot share it. It still starts
(`coexist.sh` P3), reports "Cannot listen on UDP 1716 ... pair with the QR code or the phone's IP address instead", and
announces itself and accepts connections normally. The one thing lost is hearing phones announce themselves.

**Pairing path that always works: QR code or IP address.** Both bypass broadcasts completely. The QR carries
the port actually bound, so it is right even after a fallback (`coexist.sh` P3). The pairing screen's
"Can't connect?" help and [pairing.md](pairing.md) describe both.

If every TCP port 1716-1764 is taken, the service still starts but cannot accept phone connections; it logs that and
says so (`coexist.sh` P4). You can still connect *from* the computer to a phone's address.

Firewalls: allow TCP and UDP 1716-1764 for both programs.

## The phone with both apps

Both Android apps run side by side (`scripts/e2e/coexist-android.sh`, 17 checks passed on an emulator with the stock
app 13700 and GeekBridge):

* Stock app first: it holds TCP 1716 and UDP 1716 (dual-stack socket). GeekBridge's TCP listener moves to **1717**
  and its UDP discovery socket (open while "Look for computers" is showing) binds 1716 as well, with `SO_REUSEADDR`.
  Each app heard the other (GeekBridge listed the stock app's device; the stock app dialled GeekBridge on 1717).
  Neither crashed.
* Another program on TCP 1716 (a shell-owned socket): GeekBridge starts anyway on 1717.
  GeekBridge's core does this itself; it used to fail with "Could not listen on that port" when 1716 was taken. A port
  chosen explicitly in *Developer options* is never changed.
* GeekBridge keeps its link running only while you are pairing or paired, so on a phone the order "GeekBridge first,
  stock app second" could not be held in the test; the stock app itself falls back to the next free port (seen on the
  desktop daemon, `coexist.sh` P2).
* Entry points do not collide: the share targets are `dev.geekbridge.android/.ShareActivity` and
  `org.kde.kdeconnect_tp/...share.ShareActivity`; the Quick Settings tiles are
  `dev.geekbridge.android/.ClipboardTileService` ("Send clipboard") and the stock app's own
  `ClipboardTileService`. Components and notification channels are scoped by package name, so the two cannot
  collide; the share sheet lists both.

On the **computer** the same phone shows up twice, once per program, and only if both are running. They are
different devices as far as the phone is concerned, so each needs its own pairing.

## Clipboard: two apps syncing one clipboard

With both programs running and the same phone paired to both, each one forwards the computer's clipboard to the
phone, and the phone may forward it back. Possible results: the item is written to the phone twice; a phone-side
copy that arrives on the computer is announced by one program and echoed by the other; or two syncing apps trade the
same text indefinitely.

GeekBridge's own loop guard (it ignores what it has just written) does not know about the other program, so the
mitigation is a warning, not automation:

* The service checks the session bus for `org.kde.kdeconnect` (every 20 s). If KDE Connect's daemon is there,
  `geekbridge-ctl clipboard status` prints a `note:` line and the clipboard settings in the desktop app show:
  *"KDE Connect is also running on this computer. If both sync the clipboard to the same phone, text can bounce back and
  forth: turn clipboard sync off in one of them."*
* Nothing is disabled automatically. GeekBridge clipboard sync is already opt-in per phone, and phone to computer
  is on demand only (a pull), so the common loop needs *KDE Connect's* clipboard plugin on, too. Switching either
  program's clipboard sync off for that phone removes the hazard.
* Linux only for now; on Windows and macOS the line is simply not shown.

## Limits and what was not tested

* Windows and macOS: the discovery socket is bound with address reuse, but Windows delivers a broadcast to only one
  of several sharing sockets, so there the two programs may not both hear phones. Use QR or IP pairing.
* Only stock KDE Connect 26.08.2 (desktop) and 13700 (Android) were used. KDE Connect on iOS, GSConnect, KDE Connect
  for Windows/macOS were not.
* Unicast delivery to a shared UDP port follows the kernel's rule (last binder) and was not exercised.
* The Android results are from an emulator, not a real phone, and notification channel contents were not inspected.

Full result files: [desktop](reports/coexist-desktop-results.txt), [Android](reports/coexist-android-results.txt).

## Reproducing

```
# Desktop (needs the extracted stock daemon tree from docs/kdeconnect-interop.md):
cargo build -p bridge-service --bins
scripts/e2e/coexist.sh /path/to/kdeconnect/root /path/to/out        # results.txt: PASS/FAIL per check
# Android (own AVD clone gbcoex, console port 5634, adb server 16058):
scripts/e2e/coexist-android.sh dist/android/geekbridge-debug.apk downloads/kdeconnect-android/org.kde.kdeconnect_tp_13700.apk /path/to/out
```
