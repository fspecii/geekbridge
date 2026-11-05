# Decline-with-message review: NOT COMPLETED (feature not found in main working tree)

Reviewer note: Bash failed in this session with EDQUOT on /tmp/claude-1000, so no `git log`, `git show 6e9c971cb`,
or grep was possible. I could only open files by guessed path with Read. Nothing was edited.

## Result

I found no Decline-with-message code in the main checkout (local main 982d12456, working tree). Evidence:

- crates/bridge-service/src/call_control.rs (read lines 1-1560, whole hub): `CallAction` has only Answer / Reject / Hangup
  (act_inner, ~L1010-1067). `RingChoice` is only Answer / Decline (L150-153). `CallNotifier::ringing` offers only
  `--action=answer` / `--action=decline` / `--action=default` (L177-204). There is no reply/preset/SMS call anywhere in the hub.
- crates/bridge-core/src/calls_view.rs: `CallRow` (L53-72) has no reply field; no preset list, no "decline with message" target.
- crates/bridge-apps/src/call_overlay.rs: `Choice` is only Answer / Reject (L50-54); `Body::Ringing` has no message button.
- Guessed module names that do not exist: bridge-core/src/{call_replies,decline_reply,decline_message,quick_replies}.rs and
  bridge-service/src/{call_replies,decline_reply,decline_message,quick_replies}.rs.
- I could not open ipc.rs fully, daemon.rs, settings*.rs, phone_data.rs beyond line 120, or tests/call_chain.rs, so a part
  of the feature could still exist there, but the call-side entry points above show no hook for it.

Most likely causes: commit 6e9c971cb lives on another branch (packed-refs lists many branches, e.g. work, hard, fix11-13,
privacy-review) and is not merged into main, or the SHA is mistyped. Re-run the review with Bash working, or tell me the
branch, and I will review the real diff.

## Review of the nearest existing code (checked, for the same threat list)

These are observations on code that IS in main and will be the integration points for the feature.

1. Stale banner cannot act on a new ring: OK. call_control.rs L1525-1537 checks `hub.still_ringing(ring)` (ring number, not
   call id, since the link always names its call `link-current`) before `act`. A new decline-with-message action must go
   through the same closure/check and must capture the ring number, not just the ActionTarget.
2. Double click / repeat request: OK for Reject via send_link L1247-1253 (a second pending Reject returns Ok without sending).
   A decline-with-message MUST NOT rely on this: the dedupe returns Ok(()) for the second call, so a naive
   "decline then send SMS if Ok" would send the SMS twice. Fix: decide "first request for this ring" in the hub
   (e.g. a `replied: BTreeSet<u64>` keyed by ring number, inserted under `inner` before sending), and send the text only
   when the insert succeeded.
3. Decline succeeded but text failed: act_inner sets `inner.notice` from the decline result only (L1047-1051) and
   `history.declined` after Ok (L1052-1065). A feature must report the text failure separately (distinct notice such as
   "Declined; the message was not sent") and must not turn a successful decline into an error, nor mark the call as
   failed to decline.
4. Number never on a command line: the existing missed-call banner passes the number via environment
   (`open_app_with_env`, L241-256), good pattern. The ringing banner and `missed_args` pass only `notify_text::plain(caller, 80)`
   after a `--` separator (L199-202, L224-226), so dash-leading names are safe. Any preset label put into notify-send args
   must go through `notify_text::plain` and come after `--`; the SMS number must stay out of argv.
5. Consent gating: the link side only honours packets when `phone.enabled` (Telephony consent, L684). SMS send consent is a
   separate feature (`Feature::Sms` in phone_data.rs, default off). The new send path must check BOTH Telephony consent for
   the ring and Messages (Sms) consent plus `Caps.sms_send` for the same device, in the service, not the UI. I could not
   verify that it does.
6. Rate limiter: phone_data.rs documents an inbound per-device rate limit; the 10/min outbound limiter must be the same
   instance used by normal SMS sends. Not verified.
7. Not verifiable without the diff: preset index validation (out of range / empty list), 0600 storage and size/count caps,
   privacy doc consistency (docs/privacy*.md), tests/call_chain.rs coverage.

## Minimal checklist to apply once the code is reachable

- Server-side: resolve preset by index inside the service against the current list; reject index >= len and empty list with
  BadRequest; never accept free text from the wire.
- Cap preset count (e.g. 5-10) and length (e.g. 160 chars, strip control chars) on load AND on save; write with mode 0600
  via the existing private-file helper (bridge_kde::fsutil).
- One send per ring: key by ring number (item 2), keyed under the hub lock.
- Share the SMS limiter and consent checks with the normal send path (single function, no copy).
- Number from the hub's in-memory row (`Sensitive`), never from the request, never in argv, logs or call_trace
  (use `call_trace::tail`/hash like dial does, L1184-1192).
