# Security review 5: notification mirroring, PC dial, battery, drop_idle, Response::from_json

Scope: commits 57ca75d89, da9283b01, 69eadcc53, 510ca7ab8. Viewpoints: malicious paired phone, LAN peer,
local app on the PC or phone, notification content author.

## Fixed (with tests)

| id | finding | fix | test |
| --- | --- | --- | --- |
| S5-1 | The desktop banner slot (`x-canonical-private-synchronous`) was a hash of the notification id only. A second paired phone choosing the same id (Android ids are guessable: `0\|pkg\|id\|null\|uid`) made the server replace the first phone's banner, including its buttons. | Slot is now `<phone id>-<hash>`, alphanumerics only. | `two_phones_with_the_same_notification_id_never_share_a_banner_slot` |
| S5-2 | Every banner spawned a thread plus a `notify-send --wait` process with no cap and no deadline. A server that ignores the expiry, or a flood paced just under the rate limit (one per 2 s), grew processes and threads without bound. | At most 8 open banners (extra ones are only listed); each process is killed after 60 s; output read is capped at 256 bytes. | `open_banners_are_capped_and_a_stuck_one_is_killed` |
| S5-3 | A button title made only of invisible/bidi characters cleaned to an empty label, so `notify-send` got `--action=a0=` and could refuse the whole banner (a phone could suppress banners). | Empty label falls back to `Action`. | `a_button_whose_label_cleans_to_nothing_still_gets_a_label` |
| S5-4 | `codepages::drop_idle` took any `r-xp` mapping whose path starts with `/`, including `/memfd:...` (JIT or sealed code), `/dev/...` and SysV objects, and mappings holding privately written (copy-on-write) pages (debugger breakpoints, text relocations, hot patches). `MADV_DONTNEED` on those reverts the bytes to the backing object, which can be zeros: a crash or silently undone patch. | Reads `/proc/self/smaps`; skips non-plain-file paths and any mapping with `Anonymous`, `Private_Dirty` or `Shared_Dirty` above zero. | `leaves_memfd_device_and_privately_modified_mappings_alone` (existing test `dropping_idle_code_keeps_the_process_working` still passes on the real process) |

## Checked, no change needed

- notify-send argument injection: all phone text follows `--`; button keys are indexes (`a0..a2`), labels are cleaned and `=`-free; markup, bidi and zero-width characters are neutralised by `notify_text::plain`; newlines are removed (control filter); lengths are bounded (app 48, title 80/160, text 160, label 32). Existing test covers it.
- Dismiss/action spoofing across phones and apps: every IPC path is keyed by `(device_id, id)`; the mirror is per device; the phone side only resolves ids it issued itself (`MirrorIds`), re-checks the app allow-list, `clearable` and the offered button titles against what is showing. Opaque ids (`gbN`) never reveal the Android key.
- Replay of `.action`: packets travel inside the TLS session, so an off-path peer cannot replay. A paired and allowed computer can press repeatedly; that is the granted capability.
- Content in logs, events and ctl: packets are parsed without logging, no event kind carries titles/texts, the Android side is documented as non-logging. `ctl notifications` prints content on purpose (same as SMS); the IPC needs the token file (0600).
- Memory floods: 50 notifications per phone, 8 phones, 3 h TTL, fields clipped at parse (id 128, app 64, title 160, text 600, 3 x 48); cleared on consent withdrawal, link loss, unpair. Phone side: 128 remembered ids, 15 per bulk answer, core token bucket.
- Revocation mid-session: `ActiveFeatures.notifications` is recomputed from the live Notification-access setting on every call (`enabled = active().notifications && linkRunning`), and `onListenerDisconnected` triggers a resync. After revocation no post is sent and dismiss/action requests are refused; the listener is also gone, so `press` finds no service.
- Manifest: `MirrorListenerService` is `exported=true` only with `BIND_NOTIFICATION_LISTENER_SERVICE` (required for listeners; only the system can bind). The `<queries>` entry is the narrow launcher intent, not `QUERY_ALL_PACKAGES`. No new content provider or receiver.
- PendingIntent: the app never creates one; it only sends the source app's own action intent. No implicit or mutable PendingIntent introduced.
- Dial: number is validated three times (core `valid_dial_number`, the Rust phone engine, the Kotlin regex `\+?[0-9]{1,20}`), so no USSD/MMI (`*`, `#`), no separators; `Uri.fromParts("tel", ...)` keeps it data; only when the phone is idle; `CallRequest.toString` omits the number; `Sensitive` wrapper on the computer.
- Battery: strict shape (charge -1..=100, bool), consent-gated both ends, unsolicited packets dropped unread, payload-bearing packets counted as rejected.
- `Response::from_json`: reads only a `kind` tag then the same serde types; no top-level payload type uses `deny_unknown_fields`, depth limit is serde_json's default 128, input already size-bounded by the IPC frame limit. Both parsers accept and reject the same documents in the tests.
- madvise otherwise: only `r-xp`, so JIT (`rwx`) and writable mappings are never touched; other threads running dropped pages simply fault them back in.

## Residual risk / recommendations (not changed)

1. Dial confirmation is on the computer only; a paired computer that was allowed "calls" could dial a premium-rate number without a phone-side prompt. Consider a phone-side minimum interval between dials or a per-session confirmation.
2. `banner_allowed` is one global token bucket: one chatty phone can use up banners for the others. A per-phone bucket would be fairer.
3. A phone can claim any `appName` and title text; a banner can therefore imitate another app. Putting the phone name in the summary would make the origin visible.
4. Notification ids are sequential per phone and one id space serves all allowed computers, so an allowed computer can act on a notification only sent to another allowed computer. Both hold the same grant, so no boundary is crossed.
5. Android: another change set in the working tree (NotificationMirrorTest) expects long or padded button titles to press the real button (`onAction` currently compares the unclipped title list with the clipped title the computer sends, so buttons over 48 characters never work). Not touched here to avoid colliding with that work; `./gradlew testDebugUnitTest` currently fails only on those uncommitted tests (5 failures in `BatteryReportTest` and `NotificationMirrorTest`).

## Verification

`cargo fmt --check`, `cargo clippy -p bridge-core -p bridge-service --all-targets -- -D warnings` (only error left is a `type_complexity` in uncommitted `call_control.rs` test code belonging to other work), `cargo test -p bridge-core -p bridge-service` all green.
