# Re-pairing under load: a reproducer with fault injection

Soak 5 (`docs/reports/soak-5.md`, finding 2) lost two re-pairs of the stand-in phone at the "connected" step while the
host ran at load 150-170 with 78% I/O pressure; the phone then stayed unpaired until the next re-pair round. It never
reproduced at low load. This report replaces the wait for a loaded machine with injected delays, and records what they found.

## The harness

`crates/bridge-service/tests/repair_under_load.rs` (test code only; nothing is compiled into a release binary, so the
size budgets are untouched).

| Injection | How |
|---|---|
| Slow or failing trust-record writes (save, delete, lookup) | the service's trust store is wrapped in `SlowStore`: calls are serialised like the file lock, sleep before they land, and an insert can fail with the lock timeout |
| Slow consent / share / privacy saves | the existing `statefile::set_write_delay_ms` (process-wide, so one scenario at a time) |
| Slow TLS accept and identity exchange | `bridge_chaos::FaultProxy` `handshake_delay` in front of the stand-in phone and in front of the service's inbound listener |
| The Test-connection probe (`pairing_flow`) | not injected: it is a plain TCP connect on its own task and shares no lock with pairing; it was read, not exercised |

The stand-in phone has a fixed identity, a memory trust store, redials the service every 1.6-2.4 s, and answers a re-pair
request like the app does (the soak driver ignored it, see finding 3; `GB_REPAIR_IGNORE_REPAIR=1` brings that behaviour
back). A "user" task approves every link request. The driver is the soak's `repair_p0` (connect, pair, approve, wait for
"connected", up to three attempts; `Busy` is retried without costing an attempt).

Each scenario runs `GB_REPAIR_ITERATIONS` times (25 by default, 100 for the table below) with delays drawn from a seeded
generator (`GB_REPAIR_SEED`, default `0x5eed0005`); a failure message names the iteration, so it replays. After every
iteration `converge` checks: connected within 40 s; exactly one link on the service and exactly one on the phone (no
half-open leftovers); the trust record present and pinning the phone's certificate once every write has landed and the
event log has been quiet for 500 ms (a late delete would show then); the phone still trusting the service; the
Telephony consent as expected; and a ring from the phone appearing on the call list over the surviving link.

Run it on tmpfs (`TMPDIR=/dev/shm/...`): on the development host the real disk was at 50% I/O pressure during the work
and genuine fsync stalls (a worker in `btrfs_sync_log`) made `SetConsent` miss the 8 s control deadline, which is the
condition under study but not what the injected delays are for.

## Results (100 iterations each, final tree)

| Scenario | Result | Consent after |
|---|---|---|
| 1 re-pair while the previous unpair task still runs (delete 50-700 ms) | pass; connect is `Busy` until the unpair ends | forgotten 100/100 |
| 2 re-pair during a trust write slower than the phone's wait (100-1800 ms, 0-2 lock timeouts, wait 0.3-1.5 s) | pass (driver gives up in some iterations; the pairing finishes by itself) | forgotten 100/100 |
| 3 second connection from the same device inside the 5 s tie-break window, TLS/identity delays | pass | kept 100/100 |
| 4 old connection's teardown arriving after the new link's accept | pass: the teardown does not touch the trust record, consents or call state of the new link | kept 100/100 |
| 5 dial-back crossing an inbound dial (offset -100..+100 ms) | pass | kept 100/100 |
| 6 phone unpairs and re-pairs while the service's delete is slow (100-900 ms) | pass | kept 91, forgotten 9 (see below) |
| 7 phone redial overlapping a service-side unpair | pass after the fix below (failed at iteration 31 before) | forgotten 100/100 |
| 8 phone redial overlapping a slow pairing (insert 0.5-2.6 s) | pass | forgotten 100/100 |

## Findings

1. **Defect, fixed: a phone dialing in while its unpair deletes the record ended "connected" with no record.**
   `Daemon::connect` already refused a new link to a phone being unpaired (7d6a63cd2), but an inbound link (`install_ready`)
   was not checked. The handshake decides "trusted" from the record as it is when the connection arrives; if the unpair's
   (slow) delete landed afterwards, the link stayed `Connected`, the record was gone, and the phone still trusted the
   computer. The next connection then had the computer unpaired and the phone paired, which for a phone that cannot answer
   a re-pair request is exactly soak 5's "unpaired until the next round". Two guards in `install_ready`: an arrival for a
   phone in `unpairing` is turned away (the phone's redial follows), and a trusted arrival is installed only if the record
   still exists with the same certificate. Regression: `an_inbound_dial_during_an_unpair_is_turned_away` (fails without the
   guard, deterministic) and scenario 7 (iteration 31 failed twice before the second guard).
2. **Teardown, tie-break and crossing paths hold.** Replacing or aborting an old session never deletes the new link's
   trust record or consent (scenarios 3-5); a late `remove` is serialised behind the fresh `insert` by the store lock and
   cannot overtake it. A pairing whose delete or insert exceeds the phone's wait still completes by itself.
3. **Why the stand-in stayed unpaired for good.** When the computer gives up a pairing after the phone has accepted (the
   driver's `Disconnect` aborts the session between the phone's accept and the finished trust write, or the write fails
   after the abort), the phone keeps trusting a computer without a record. The computer's next request reaches a phone that
   is `Trusted`, which surfaces as `PeerRequestedRePair`; the app shows a prompt, but the soak's stand-in
   (`soak_real.rs`) ignored the event, so the request timed out after 30 s every time. Reproduced (scenario 2, iteration 5,
   before the stand-in change). `soak_real.rs` now accepts the request as the user would, which turns "stuck until the next
   round" into a normal re-pair. Closed by the follow-up below: the service tells the phone.
4. **Consent on a phone-initiated unpair + re-pair (scenario 6) is not deterministic**: 9 of 100 forgot the consent, 91 kept
   it, depending on whether the old session or the new connection won the race. Same device id, so "kept" is the allowed
   outcome; "forgotten" is the safe one. Both leave the records consistent. Left as is.
5. Residual risk: the real file store holds its lock across the slow write and times out on contention, which the wrapper
   only models with injected `timed out` failures; the retry (3 attempts) is exercised, not the lock itself.

## Abandoned pairings: the phone is told (follow-up)

The phone's accept precedes the computer's trust write by protocol, so a computer that then gives up leaves a phone that
trusts a computer without a record (the next request of the computer shows a prompt and needs one tap). Now:

1. **The phone is told, best effort, once.** `PendingLink::pair` (bridge-kde) sends `kdeconnect.pair` with `pair=false`
   (the standard unpair; the phone forgets the computer at once) when it gives up after the peer accepted:
   * the trust write fails (already the case) or outlasts `PairingConfig::persist_timeout` (45 s, below the phone's 60 s
     wait after approving; the write keeps running as a task and may still land, see below);
   * the pairing future is cancelled (disconnect, link replaced, service stopping): a drop guard (`Abandon`) takes the
     link into one detached send bounded by the connection's write timeout. If the trust write is in flight at that
     moment the guard waits for it (at most `persist_timeout`) and sends nothing when it lands: record and phone then
     agree and only the connection is lost (scenario 2 relies on this; sending unconditionally broke it);
   * the service stopping: `goodbye_all` marks the pairing config `stopping` and ends links still pairing; the guard then
     sends at once without waiting for the write (the process may be gone before it lands or fails);
   * no retry loop anywhere. Without a runtime (the process already exiting) nothing is sent.
   A write that lands after the timeout leaves a record the phone no longer matches. That direction is safe: the
   phone's request is an ordinary prompt and nothing is trusted on its word (below).
2. **A phone that still presents as paired is never trusted on its word.** The service decides from its own record.
   Without one, such a phone is a pending link: any packet but a pair request gets the single `pair=false` hint (the
   phone forgets), a pair request is the normal prompt with the verification key, and nothing is stored until the user
   approves. Tested with auto-approval switched off.
3. **Android**: an unpair received within 90 s of this phone accepting a pairing is reported as `peer_unpaired` with code
   `link_unfinished` and worded "The computer did not finish linking - try again." (calm tone) instead of "The other
   device removed the link"; the core had already dropped the trust record and the connection, so the pairing list no
   longer shows a link. The mid-pairing case where the phone is the requester and waits for the computer's accept still
   surfaces the computer's `pair=false` as "declined", because the protocol cannot tell a decline from a failure there.

Tests (`repair_under_load.rs`): `a_trust_write_past_the_deadline_makes_the_phone_forget_the_service` (600 ms deadline,
write 2.5 s: the phone has no record within 1.8 s of accepting), `a_failing_trust_write_makes_the_phone_forget_the_service`,
`cancelling_a_pairing_during_a_trust_write_that_fails_tells_the_phone`, `..._that_lands_keeps_the_pairing`,
`stopping_the_service_during_a_trust_write_tells_the_phone`,
`a_phone_that_still_thinks_it_is_paired_gets_the_normal_prompt`; Android: `peer_unpairing_removes_trust_on_the_phone`
(code) and `TransportParserAbuseTest` (wording). Sizes: service 7,342,872 B (budget 7,380,000), ctl 1,712,568 B
(budget 1,738,000); Kotlin change is one condition and one string.

Open: scenario 4 (`old_teardown_after_the_new_accept`) fails sporadically at 100-300 iterations with "no link" after the
redial (iterations 11, 24, 33, 76 on different runs). It fails the same way on the tree without this change (3 of 3 runs at
300 iterations), and involves no pairing, so it is not caused by the abandon handling; not investigated here.

## Running

```
TMPDIR=/dev/shm/gb-repair cargo test -p bridge-service --test repair_under_load -- --test-threads=1
GB_REPAIR_ITERATIONS=100 GB_REPAIR_SEED=42 cargo test -p bridge-service --test repair_under_load -- --test-threads=1 --nocapture
GB_REPAIR_TRACE=1 ...   # prints what the phone's side saw
```
