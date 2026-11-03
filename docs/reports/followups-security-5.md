# Follow-ups from security review 5

## 1. Android button titles over 48 characters

`NotificationMirror.onAction` compared the unclipped action titles with the clipped title the computer sends. The fix is
in the working tree edits of the test-gaps work (`NotificationMirror.kt`: `onAction` clips with the same
`MirrorRules.clip(.., MAX_ACTION)`, drops empties and takes the first three; `MirrorListenerService.kt`: `press` matches the
real button by the same clipping). Test: `NotificationMirrorTest.aLongOrPaddedButtonTitleStillPressesTheRealButton`
(a 72-character title and a padded title both press the real button). Those files carry the other agent's uncommitted edits,
so they are committed with that work, not with this change.

## 2. Phone-side dial protection

Before: a paired computer with call access could dial any plain number with no sign on the phone. Now three layers, all on
the phone and independent of the computer's UI:

| layer | where | default |
| --- | --- | --- |
| Rate limit: at most 3 dial requests per computer per 10 minutes (sliding window, counted on arrival so failed attempts cannot probe; 16 computers remembered) | Rust phone core, `engine/calls.rs` `DialLimiter`; over the limit the core answers `rate_limited` itself and Kotlin never hears of it | on |
| Visible notice: toast plus a notification "Dialing <number> requested by <computer>" when a call is really about to be placed (not for refused ones). The computer name is cleaned (no control/bidi/invisible characters, 40 chars) | Kotlin `CallControlEngine` + `AndroidDialPrompts` | on |
| On-phone confirmation: heads-up notification "Place this call?" with Call / Deny; 45 s to answer, at most 3 open; nothing is dialed until Call; re-checked against the phone's state at that moment; unanswerable (notifications blocked) means declined | Kotlin engine, `DialDecisionReceiver` (not exported, immutable PendingIntents); switch in Setup, Answer calls step | off |

Why confirmation is off by default: every dial is already confirmed on the computer (sheet or `--yes`), and a second prompt
per call defeats dialing from the PC. The default (notice plus a limit of 3 per 10 minutes) bounds the damage of a
compromised or misbehaving computer to three visible calls and lets the owner hang up; owners who want to be the final gate
turn the switch on.

PC messaging: two new reasons in the shared fixed vocabulary, `rate_limited` ("The phone limits how many calls a computer
can place; wait a few minutes.") and `declined` ("The phone's owner did not confirm that call."). The desktop already shows
`LinkFailure::message()` as the notice for any refusal, so the call window and `geekbridge-ctl call dial` show them with no
UI change. An older computer maps unknown reasons to "failed".

Tests: Rust `calls_link` (new reasons round-trip, distinct text), `engine/calls.rs` (window, per-computer allowance,
bounded memory), `bridge-android/tests/calls_transport.rs`
`a_computer_that_dials_too_often_is_refused_on_the_phone_and_told_why` (3 events carrying the computer name, 4th answered
`rate_limited` on the wire, no event, other actions unaffected). Kotlin `CallControlTest`: notice text and no notice when
the dial cannot be placed, computer-name cleaning, confirmation (nothing dialed until Call, single use), deny/expiry/
unaskable all `declined`, state re-check after confirming, pending cap and reset, wire words and `computer` parsing.

Not tested here: the Android notification/receiver classes themselves (device or emulator only).

## 3. Per-phone banner rate limit

`Mirror::banner_allowed(device, now)` keeps one token bucket per phone (burst and gap unchanged; 32 phones remembered,
least recently seen forgotten). `daemon.rs` passes the phone id. Tests (unit tests in `mirror.rs`):
`a_chatty_phone_cannot_use_up_another_phones_banners`, `the_banner_limiter_remembers_a_bounded_number_of_phones`; the
existing burst test now passes a device.
(The existing burst test in the other agent's uncommitted `tests/mirror_caps.rs` needed the new `device` argument; that edit is in the working tree, not in this commit.)

## Verification

`cargo fmt --check` 0; `cargo clippy -p bridge-core -p bridge-service -p bridge-android --all-targets -- -D warnings` 0;
`cargo test -p bridge-core -p bridge-service -p bridge-android` all green (including the other agent's uncommitted tests);
`./gradlew --offline testDebugUnitTest` exit 0 (CallControlTest 29 cases, plus the other agent's in-flight Battery/NotificationMirror tests, which passed).
