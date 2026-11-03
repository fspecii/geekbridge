<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# i18n sweep 2 (2026-11-03)

Scope: the strings added on 2026-11-02/03 (phone data, notification mirroring, battery, PC dial, onboarding, Home
window), on Android and desktop.

## Verified state

* Android: `testDebugUnitTest` and `lintDebug` exit 0 with the changes below (JDK 17 from `scripts/android-env.sh`).
* Desktop: `cargo test -p bridge-apps` and `cargo test -p bridge-service --lib` passed after the changes
  (200 + 298 tests, the i18n catalogue tests included). The last edit (two tests in `mirror.rs`/`os_notify.rs` were
  not added) and the final `cargo fmt --check`, `cargo clippy -D warnings`, full workspace test run, commit and push
  were **not** completed: the sandbox ran out of disk quota on the volume holding the tool temp directory
  (`EDQUOT`, `/tmp` is shared with many leftover files), so no shell command could run afterwards. See "Not done".

## Findings and fixes

### Android

| Finding | Fix |
| --- | --- |
| Dial notice was an English string built in Kotlin: `"Dialing $number requested by ${name}"`, plus the fallback `"a paired computer"` | `call_dial_notice` (`%1$s`, `%2$s`) and `call_computer_fallback` in `gb_ui.xml`; `CallRules.dialNotice/cleanComputer` and `CallControlEngine` take a `Strings` |
| Phone numbers and computer names were embedded in sentences with nothing to stop a right-to-left name reordering the line | New `Bidi.isolate` (FSI..PDI) and `Bidi.ltr` (LRI..PDI) applied to the dial notice, contact lines, paired/discovered/connection names and prompts (`LinkSection`), file offers (`OfferDialogs`, `ShareNotifications`), QR pairing |
| Translatable fragments concatenated in code: `sub + "\n" + state`, `pick + " · " + count`, `"1. " + text`, `"• $line"`, `"${i+1}.  $line"`, `"✔  " + label + " – " + detail`, `time + " · " + detail`, `"$shown\n\n" + note`, `"“$name”  ·  " + word` | Joiner strings with positional arguments: `list_bullet`, `list_numbered`, `pd_sub_with_state`, `pd_pick_with_state`, `net_step_ok/bad`, `cal_time_detail`, `offer_message_with_note`, `qr_confirm_name_unverified` |
| `Ui.kt` section label used `setPadding(dp(6),0,0,0)`: padding on the physical left, wrong in RTL | `setPaddingRelative` |
| Lint had `HardcodedText` and `SetTextI18n` only | `lint.xml` adds errors for `RtlHardcoded`, `RtlSymmetry`, `RtlEnabled`, `MissingDefaultResource`, `ExtraTranslation`, `StringFormatInvalid`, `StringFormatMatches`, `StringFormatCount`, `ImpliedQuantity`; `MissingTranslation` stays a warning (a partial translation falls back to English, the default language must be complete) |

Already fine: `supportsRtl="true"`, `start`/`end` margins in code, pseudo-locales on in debug and off in release,
all `gb_phonedata.xml` strings use numbered placeholders or `<plurals>` (`mirror_apps_count`, `pd_calendar_shared_count`),
no hard-coded notification, toast or dialog text in the new Kotlin.

New tests, `android/app/src/test/java/dev/geekbridge/android/ResourcesI18nTest.kt`:
numbered placeholders when there are two or more (and no mixing), plurals have `other`, apostrophes escaped,
every locale folder (shipped or test fixture) keeps English keys and placeholders, a source scan that rejects
literals in `setContentTitle/Text`, `Toast.makeText`, dialog setters, `contentDescription`, `NotificationChannel`
(which lint's `HardcodedText` does not look at), and Bidi tests (Arabic name, hostile direction controls in a name).
`CallControlTest` and `FormatTest` were updated for the isolated text.

### Desktop

| Finding | Fix |
| --- | --- |
| Home window: 44 UI literals (sidebar, tiles, phone status, update card, diagnostics) incl. everything added yesterday | Moved to `en.properties` (`home.*`), `home_app.rs` removed from the allowlist |
| Feed results, day-cell accessibility label (built from `", today"` / `", has events"` fragments), offline hints, progress label, launch errors | Moved to `feed.*`, `cal.cd.*` (four whole-sentence forms), `identity.offline.*`, `nothing.progress`; `call_feed.rs`, `phone_feed.rs`, `dot_calendar.rs`, `nothing.rs`, `prefill.rs`, `identity.rs` removed from the allowlist |
| `connection.rs` appended a translated hint to an English sentence (`format!("... {}", tr("doctor.short"))`) | One key each with `{hint}`: `connection.no_token`, `connection.unreachable` (allowlist 66 -> 64) |
| Desktop banners (Linux `notify-send`, Windows toasts, macOS) composed phone-chosen names/titles/text next to English words with no direction isolation | `notify_text::isolate` / `banner_safe`; used in `mirror.rs` and `os_notify.rs` (calls, offers, texts, calendar, mirrored notifications) |

Allowlist: 17 files / 20 entries before; 12 files after (`home_app`, `call_feed`, `phone_feed`, `dot_calendar`,
`nothing`, `prefill`, `identity` gone; `connection` lowered). It did not grow.

New tests in `crates/bridge-apps/tests/i18n_catalogue.rs`: shared `check_translation` (known keys, same placeholders,
plural `other`, no hidden direction controls) run over `BUNDLED` and over `tests/fixtures/*.properties`; the `eo`
fixture is never bundled; placeholders are plain words and English holds no direction controls; translated fragments
are not concatenated (`tr(..) +`, `push_str(tr(..))`); pseudo-locale keeps every placeholder. `notify_text.rs` gained
tests for isolation and for a length cut never leaving an isolate open.

## Not done / decisions for the owner

1. **Final gates and the commit.** `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
   workspace tests, `git commit` and `git pull --rebase --autostash origin main && git push origin main` could not
   be run (disk quota, above). Files changed, to commit together (do not include `scripts/e2e/wayland.sh`, which was
   already modified by someone else): everything under `android/app/src/main/java/dev/geekbridge/android/`
   touched by `Bidi`, `res/values/gb_ui.xml`, `android/app/lint.xml`, the new `ResourcesI18nTest.kt`,
   `android/app/src/test/i18n-fixtures/`, `crates/bridge-apps/{i18n,src,tests}`, `crates/bridge-service/src/{notify_text,mirror,os_notify}.rs`,
   `docs/translations.md`, this report. Freeing space (`make clean-dev APPLY=1`, `target/` is 153 GB) is the first step.
2. **Desktop banner text is still English literals in `bridge-service`** (`os_notify.rs`, `desktop_notify.rs`,
   `mirror.rs`: "Incoming call", "File received", "New message from ...", "Dismiss", ...). `bridge-service` does not
   depend on `bridge-apps` (and must not pull in egui), so they cannot use the catalogue yet. The clean fix is to move
   `i18n.rs` into a small shared crate (or `bridge-core`) with the service keys in the same `en.properties`. I did not
   do it in a sweep that also edits other people's crates; it is the next item. They are not covered by the literal
   guard either; extend the guard to those three files when they move.
3. **egui has no right-to-left layout.** Isolates are deliberately not inserted into window text (they render as
   missing glyphs); `display_text` already strips direction controls from device names. Right-to-left is an Android
   goal; the desktop gets it when egui does.
4. 12 files (~480 literals: Calendar, Messages, Contacts, Clipboard and Share apps, call UI, pairing) remain on the
   allowlist; the migration is mechanical but large.
5. The two `mirror.rs`/`os_notify.rs` tests for the new isolation (a banner body in the order `title: text` with
   isolates) are not written; the unit tests in `notify_text.rs` cover the helper itself.
