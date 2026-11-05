# E2E link drops (sms and contacts-calendar-cases jobs)

Date 2026-11-05. Runs: Emulator end-to-end 42042455032 (jobs `sms`, `contacts-calendar-cases` failed) and 42049065692
(job `call-ui` failed). Question: does a product behaviour (tie-break, dial-back, heartbeat, goodbye, wifi bind redial,
call-trace writer, banner pool, locks) drop the link, or is it the emulator's network?

Verdict: **not product**, with one open question about why the socket sat on the emulator's cellular network (below).
No product code changed.

## 1. What the artifacts show

**sms job, drop at 14:29:06.** Phone logcat, in order:

```
14:29:06.167  SMSDispatcher / RILJ  SEND_SMS (the 4th text going out through the emulator modem)
14:29:06.218  ConnectivityService: releasing NetworkRequest [CELLULAR ... Uid 10147 (googlequicksearchbox)]
14:29:06.220  ConnectivityService: Update capabilities for net 101 : -FOREGROUND
14:29:06.221  Netd: Destroyed 1 sockets for netId 101 permission=1 in 1095us
14:29:06.224  GB link: event link_closed code=conn_aborted
14:29:06.230  GB link: close: kind=link_closed who=the connection closed why=conn_aborted connection=1
14:29:08.358  GB link: event redial          (the phone's own redial, 2.1 s later, inside its 1.6 to 2.4 s window)
14:29:08.366  netd networkSetPermissionForNetwork(101, 0)
```

Net 101 is the emulator's mobile data network (net 100 is WIFI, `[100 WIFI] validation passed` at 14:29:08.4). When the
last request that wanted it (the Google app's CELLULAR request) is released, Android drops FOREGROUND from the network,
raises its permission, and netd destroys the sockets on it with `SOCK_DESTROY`. A destroyed socket reads as
`ECONNABORTED`, which the core reports as `conn_aborted`. The phone's own link socket is the one destroyed: 3 ms between
the netd line and the core's event, and `Destroyed 1 sockets`. A reset from the peer would read `conn_reset`; a heartbeat
failure would read `ping_timeout`.

The computer's call trace (`call-trace.log`, ms epoch) agrees with the order: `link down` at 14:29:06.836, 0.6 s after
the phone's close (the emulator's NAT relay delays the FIN), `link up` at 14:29:08.99 after the phone's redial.

**cases job, drop at 14:29:39.6.** The phone log (app-tag lines only; the full logcat was not captured, since
`logcat-final.txt` is a one-line stub in that job) shows `connected computer 1` at 14:28:51.9 (47 s earlier), then
`link_closed code=conn_aborted` at 14:29:39.608, `link lost`, `redial` at 14:29:41.269 (1.66 s, inside the window) and
`connected ... connection=2` at 14:29:41.979. The computer's trace: down at 14:29:40.028, up at 14:29:41.840. Same signature
(`conn_aborted`, phone first, computer second). Netd lines are not available for this job, so the cause is inferred from
the code, not observed.

**call-ui job (42049065692).** The computer's trace has a `link down` 25 s after the first `link up` and the link comes back
only on the next stage; the phone logcat was not captured. Its failures are call-UI assertions (overlay answer, card),
and it is not evidence either way. Treat as unexplained.

## 2. Hypotheses checked

| Hypothesis | Result |
|---|---|
| Cross-dial tie-break closing the loser | No. The drop is `conn_aborted` on a connection that was 47 s old (cases) and over a minute old (sms). `CROSS_DIAL_WINDOW` is 5 s and applies only to a connection younger than that. No `duplicate_connection`, no second connection before the drop. |
| PC dial-back racing the phone's redial | No. The PC's dial-back needs a silent loss first and waits 4 s. The PC saw the link go down after the phone, and the phone's redial (2.1 s, 1.66 s) completed the link. The PC never dialed in either case. |
| Wi-Fi bind safety-net redial | No. `wifi bind set` was logged once, at start, in each process. No `wifi bind` line, restart, `IPv4 address set changed` or `wifi network available` close to either drop. |
| Heartbeat / ping timeout under CPU load | No. A heartbeat failure reads `ping_timeout` after 70 s of silence; the code here is `conn_aborted` 3 ms after the netd line, with traffic flowing (SMS) the second before. |
| Goodbye packet | No. A goodbye gives `peer_closed` with the user-disconnect reason and suppresses redial for 5 min; the phone redialed in about 2 s. |
| Call-trace writer thread or banner pool blocking a link task | No. Neither blocks the reader; a block would delay, not abort. The error is an OS-delivered `ECONNABORTED`. |
| glibc arena or malloc trim at session end | No. The phone side is Android's bionic; the abort is on the phone's socket. |
| SMS send path holding a lock | No. A held lock delays traffic; it cannot produce `ECONNABORTED`. The SMS only coincides in time: it is also what makes the Google/telephony stack release its cellular request. |

## 3. Why the phone's link socket is on net 101

Open. The artifacts prove the socket was destroyed as a member of netId 101, but not why a link whose dials are bound to
the Wi-Fi source address (`LinkService.pushWifiBind`, `Endpoint::connect_from`) carried that mark. Candidates, none
confirmed:

* In the emulator, both the Wi-Fi interface and the radio interface sit in 10.0.2.0/24 behind the same NAT gateway
  (10.0.2.2, the address the e2e scripts dial). A socket that is unbound, or whose bind falls back after the 4 s
  timeout, takes the default network's mark, and which network is "default" flips with validation (`reportNetworkConnectivity(100, false)`
  from uid 10147 appears at 14:29:08).
* `wifi-binding.md` already says a socket bound to an address that the default network does not own can connect and
  then die, and that this "is not caught by a connect-time fallback". This may be the same class.

A real phone with Wi-Fi as its only usable LAN path does not have a second network that reaches the computer, so the
trigger (a cellular-only request being released) does not hit the link socket there. A real device could in principle
see the same abort if the link ever sat on a non-default network that lost FOREGROUND; the 12-try redial would recover it
in about 2 s, as it did here. The debug dump's `Link health` section records the network at the time of a close, which
would show it on the Fold5.

## 4. Recommendations

1. Keep the e2e scripts' wait-for-link patch. The drop is recoverable and the product recovers by itself in 1.6 to 2.4 s.
2. Remove the cause in the emulator: before the suite, `adb shell svc data disable` (and wait for net 101 to go away), or
   hold a cellular request, so that no `-FOREGROUND` event can destroy a link socket. Not done here (scripts out of scope).
3. Capture the full logcat in the cases and call-ui jobs (`logcat-final.txt` is a stub), so the netd line can be confirmed
   next time. A one-line check for the e2e runner: `grep -c 'Destroyed [1-9][0-9]* sockets for netId' logcat`.
4. If the owner wants certainty about the socket's network on the emulator, log the socket's `SO_MARK` (`getsockopt`) once
   after connect in the debug build.
