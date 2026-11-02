# Chaos / failure-injection findings

> Status: snapshot from 2026-11-02, see git history for later changes. Numbers and findings below describe that point in time, not necessarily the current tree.

Tests: `crates/bridge-chaos` (the TCP fault proxy: latency, bandwidth cap, loss as late delivery,
random connection drop, reset after N bytes, half-open silence, byte-at-a-time fragmentation,
delayed handshake) driving `bridge-service/tests/chaos_{link,transfer,clipboard}.rs`,
`bridge-android/tests/chaos.rs` and `bridge-update/tests/chaos_http.rs`.

* Short (CI) mode: plain `cargo test -p bridge-service --test chaos_link --test chaos_transfer --test chaos_clipboard`,
  `-p bridge-android --test chaos`, `-p bridge-update --test chaos_http` (about two minutes).
* Soak mode: `GEEKBRIDGE_CHAOS=soak` (1000 reconnect cycles, 200 MB files, repeated rounds); run by
  `scripts/soak.sh` as part 3 (`--no-chaos` skips it, `--chaos-only` runs only it).
* Disk full is simulated with `RLIMIT_FSIZE` in a re-executed child process (no root, tmpfs or loop
  device needed; Unix only).

| ID | Finding | Status |
|----|---------|--------|
| C-1 | Outgoing file never fetched by the phone (phone vanished after the offer) kept its payload listener, file handle and transfer slot "Active" for the whole accept timeout (120 s) after the link died. | **Fixed**: `ShareManager::link_lost` (called from the link task's exit) gives up unfetched outgoing transfers 10 s after the link is lost (`link_lost` code). Test `an_unfetched_outgoing_offer_does_not_outlive_its_link`. |
| C-2 | Dead-link detection. Desktop (Linux): keepalive 30 s + 3 x 10 s and `TCP_USER_TIMEOUT` 60 s on every link socket, so a vanished peer is noticed in at most ~60 s. Verified armed (`link_sockets_have_the_keepalive_timer_armed`). | OK as is. |
| C-3 | No application-level ping was added. KDE Connect has no ping reply, and a stock Android phone shows a "Ping!" notification for every `kdeconnect.ping`, so a periodic ping would spam users and still could not detect a peer that is alive at the TCP level but frozen. A half-open path where a middlebox keeps ACKing (the proxy's blackhole mode) is therefore undetectable on an idle link; it surfaces as soon as data is sent (write timeout 5 s) or a transfer's idle timeout (30 s) fires. | Won't fix (protocol limit). |
| C-4 | Android side: keepalive probes start after 120 s idle, then 4 x 30 s, no user timeout: a vanished desktop is noticed after about 4 minutes. Deliberate trade-off for battery (docs/android-power.md); the desktop side, which drives the UI, notices within a minute. Verified armed (`accepted_sockets_have_the_keepalive_timer_armed`). | Accepted; revisit only if Android polish wants it (not edited here). |
| C-5 | Windows: `configure_tcp` cannot set the keepalive probe count (socket2 limitation), so Windows uses the OS default of 10 probes: 30 s + 10 x 10 s, about 130 s, and has no `TCP_USER_TIMEOUT`. | **Fixed**: new `bridge-kde/src/keepalive.rs` (used by `configure_tcp`). socket2 sets idle and interval through the `SIO_KEEPALIVE_VALS` ioctl; a cfg(windows) `setsockopt` (windows-sys `Win32_Networking_WinSock`) adds `TCP_KEEPCNT` = 3 and `TCP_MAXRT` = 60 s (Windows 10 1703+, best effort), so dead-link detection is about 60 s. The ioctl cannot set the probe count, hence the extra options. Type-checked for `x86_64-pc-windows-msvc` with `.tools/windows-calls-check/check-windows.sh` and a separate target dir (not run on real Windows); unit tests `probe_budget_detects_a_dead_link_within_a_minute`, `arming_an_ordinary_socket_succeeds`. |
| C-6 | Pairing under latency, 30% late-delivered chunks, byte-at-a-time delivery, a 3 s slow handshake and an 8 KB/s cap all complete; a handshake stalled past the 10 s TLS deadline and a reset in the middle of pairing leave no link, no stale challenge and no trust record, and the same device pairs normally afterwards. | No defect. |
| C-7 | 100 reconnect cycles (1000 in soak) with mixed resets/fragmentation/drops: file descriptors, thread count and RSS stay flat on both the service and the Android engine; the event log stays bounded; the proxy ends with 0 live connections. | No defect. |
| C-8 | 200 MB file cancelled midway by the receiver, the sender killed (reset), the network gone silent (idle timeout fires as `timeout` within seconds), reset mid-stream, a link flap mid-transfer, two concurrent transfers (one cancelled, the other byte-exact), cancel of an outgoing send: the transfer always reaches a final state, no `.part` or reserved file remains and descriptors return to baseline. | No defect. |
| C-9 | Disk full on the receiver (write error mid-file): transfer fails `io_error`, partial and reserved files removed, sender learns, the service keeps working and the next file is received. | No defect. |
| C-10 | A text copied in the first poll interval (up to 1.5 s) after a link (re)connects is taken as the clipboard watcher's baseline and never sent to the phone. Deliberate (it avoids sending stale clipboard contents on connect) but it silently loses a very fast copy. | **Fixed**: the watcher now reads the clipboard immediately when it starts (and is woken on every enable) instead of after the first poll interval, so the baseline is taken at link-up and a copy made right after is a normal change. Stale content is still the baseline and never sent. `chaos_clipboard.rs` now copies 300 ms after reconnect and expects the send (loop-suppression tests unchanged and green). A copy in the few ms between link-up and that first read can still be taken as baseline; accepted. |
| C-11 | Clipboard: 50 copies in 150 ms collapse to a few sends ending on the newest value with no duplicates; late phone echoes after a newer local copy are never bounced back and the system goes quiet; 200 phone updates apply in order; nothing stale is resent after a flap. | No defect. |
| C-12 | Call-control `Answer` racing a link reset never hangs; the ringing row is cleared when the link dies and only a fresh request reaches the phone after reconnect (40 rounds in soak). | No defect. |
| C-13 | Update checker against slow-loris head/body, silence, self and endless redirects, 100 KB garbage, endless header, immediate close and a bad chunk size (API and download): every case errors inside the configured deadline, nothing is left in the download directory, threads do not grow, and the next good release is accepted. | No defect. |
| C-14 | Test-harness note: an early failure that looked like a receiver `io_error` was a real `ENOSPC`/quota on `/tmp` from leaked temp dirs; the harness now owns its temp dirs. The real code cleaned up correctly (see C-9). | Closed. |

Design calls skipped: C-3 (application ping, protocol limit) and C-4 (Android battery trade-off) stay as documented; no other defects found.

Not covered: real packet loss on a real network (the proxy models loss as late delivery because TCP
hides drops), Windows/macOS behaviour (Linux `/proc` probes), and the Android app process under Doze.
