# Share result packet: the phone now learns what became of its share (2026-11-04)

Follow-up to `docs/reports/share-e2e.md` ("No refusal message exists in the protocol"). A computer with receiving off, or one
whose user declined, never answered the phone: the Share screen showed "waiting for the computer (it may need Share files >
Allow this phone ...)" and then timed out, and a text or link was never acknowledged at all.

## What was built

A GeekBridge-only packet, `geekbridge.share.result`, computer to phone (`bridge_core::share_result`, wire format in
`docs/kdeconnect-interop.md`, "Share result").

* **Negotiation.** The phone declares it as an *incoming* capability; the computer lists it in `LOCAL_OUTGOING` and the
  existing `gate::negotiated_outgoing` sends it only to a phone that declared it. Like every `geekbridge.*` name it is removed
  from the plaintext identities (`Identity::strip_private`, commit 0e3f40157) and exists only in the identity inside TLS. A
  stock KDE Connect peer cannot declare it, so it is never offered or sent it.
* **Matching.** A share packet has no id. The phone numbers each share (the file's transfer id; for text and links a new
  `text_id`) and adds `geekbridgeShareId` to the `kdeconnect.share.request` body, **only** when the computer declared the
  type in its secure identity, so a stock computer sees an unchanged share packet. The answer quotes `transferId` (file) or
  `textId`.
* **Outcomes** (`outcome` plus a fixed `reason` code, `[a-z0-9_]{1,32}`): `accepted` (`user`), `declined` (`user`,
  `expired`), `receive_off` (`not_allowed`), `too_large` (`over_limit`), `busy` (`too_many_offers`, `too_many_transfers`),
  `saved` and `link_saved` (`kept`). Deviation from the brief's example list: there is no `opened_as_link`, because a link is
  never opened by the computer; `link_saved` says what actually happens.
* **Desktop** (`crates/bridge-service/src/share.rs`): `ShareManager` keeps the phone's id on each transfer and an answer
  queue per live link (`answers_up` / `answers_down`, bounded 64, generation-guarded like the other hubs); the link task
  (`daemon.rs`, `answer_link`) writes the packets. Answers are sent for: receive off (file, text, link), file too large,
  no room (offers or transfers), Accept (also auto-accept and a requested photo), Decline, an offer that lapsed, receive
  switched off while an offer waits, and text or link stored. Refused shares still count against the 64-packet rejected
  budget, as before.
* **Phone core** (`crates/bridge-android/src/transport/engine/share.rs`): believes an answer only from a computer that declared
  the type, only for a file it sent on that connection or a text id it handed to that connection, keeps the first answer
  (32 kept), and exposes them as `share_results` in `transport_status`. A refusal of a file that has not moved a byte cancels
  the transfer at once (state `declined`, `code` = the outcome), which ends the 2 minute payload wait. `share_send_text` now
  returns `{queued, text_id, result_expected}`.
* **Phone UI** (`Share.kt` `ShareAnswerText`, `ShareCoordinator`, `gb_share.xml`): plain words, for example "the computer
  is not set to receive files from this phone (on the computer: Share files > Allow this phone to send me files)", "the
  computer declined it", "received by the computer as a link (shown there, not opened)". A text waits up to 8 s for the
  answer ("sent, waiting for the computer's answer...") and keeps the session and link open that long; then it falls back to
  the earlier "sent. It only shows up if ..." line. **An older computer sends nothing and nothing changes for it**: the file
  keeps the "waiting ... Allow this phone ..." hint.

## Not changed

* Computer to phone offers: the computer still learns of a phone-side Decline only as a timeout (the phone sends no packet;
  that would be the mirror image of this work).
* `accepted` is shown only until bytes move; after that the progress line is the better news. A failure after `accepted`
  shows the transfer's own failure text.

## Tests

* Rust: `bridge_core::share_result` unit tests (round trip for each pairing, hostile ids and outcomes, reason hygiene);
  `bridge-service/tests/share_api.rs` (7 new tests over real links: receive off for file, text and link; accept, decline,
  expiry, receive switched off while waiting, auto-accept; saved and link_saved; too_large and busy; no id or no declared
  type or hostile id = silence); `stock_peer.rs` (a stock phone's refused share, even with the id key, gets no `geekbridge.`
  packet); `android_negotiation.rs` (stock never negotiated the type, a phone that declares it is, an inbound
  `geekbridge.share.result` is rejected); `bridge-android/tests/transport.rs` (ids only to a computer that declared the type,
  refusal ends the wait, accepted keeps the transfer going, unsolicited or malformed answers ignored and counted, identity
  capability list); fuzz target `share_result` and a regression test. The plaintext-identity test from 0e3f40157 already
  covers every `geekbridge.*` name.
* Kotlin: `ShareAnswerTest` (parsing and dropping, wording for every outcome, accepted yields to progress, old computer keeps
  today's wording, text wait and fallback).
* Docs: `docs/kdeconnect-interop.md`, `docs/privacy.md` (`privacy_doc` guard test green), `docs/android-contract.md`, CHANGELOG.
* Emulator suite `scripts/e2e/share.sh` (needs the emulator; not run in this environment): `receive_off` now requires the new
  words for the file and the text and fails if the old "waiting for the computer" hint is still shown; new steps
  `answer_declined` (Decline on the computer, the phone says so within seconds) and `answer_saved` (text and link
  acknowledged; the link is not opened).

## Verification

* `cargo fmt --all --check` exit 0; `cargo clippy --workspace --all-targets -- -D warnings` exit 0.
* `cargo test --workspace --release --no-fail-fast`: 2000 passed, 2 failed, both outside this change:
  `bridge-apps` `calendar::render_tests::stepping_and_jumping_move_the_shown_day_in_every_mode` (a day-gap assertion that
  depends on the calendar date) and `bridge-calls` `dbus_mock::signals_wake_the_watcher_and_quiet_times_out` (needs a
  session bus in this shell). Everything touched here is green: `bridge-core`, `bridge-service` (all test binaries),
  `bridge-android` (`transport` once showed the known `announcements_stay_paused...` UDP flake on this shared host and passed on
  two reruns), `bridge-fuzz`, `privacy_doc`.
* `gradlew testDebugUnitTest lintDebug` exit 0 (`ShareAnswerTest`: 7 tests).
* `scripts/check-doc-links.sh`: 0 broken links.
