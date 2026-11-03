<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Adding a language (contributor guide)

This is the short, hands-on version of [translating](translating.md) (which has the full rules for the file formats).
It shows how to add one language on both platforms, and how the tests keep a translation honest. The worked example
is Esperanto (`eo`), chosen because it is small and not shipped. The example files live in the test tree as
fixtures, are checked on every test run, and are **never packaged**.

## Before you start

* English is the source on both platforms. Untranslated keys fall back to English, so a partial translation is fine.
* Nothing is translated twice: the Android app (phone) and the desktop apps (Home, Calls, Messages, Contacts,
  Calendar, Clipboard, Share) have separate files. Do the one you can test first.
* The CLI (`geekbridge-ctl`, `doctor`), logs and diagnostics reports stay English on purpose: people paste them
  into bug reports.
* Your translation is licensed GPL-3.0-or-later like the rest of the project; do not copy text from KDE Connect or
  from services whose terms forbid reuse.

## Android

1. Create `android/app/src/main/res/values-eo/` (BCP 47 with a region: `values-pt-rBR`; with a script:
   `values-b+sr+Latn`). `values-night`, `values-v31` and similar are not languages.
2. Copy only the files you want to translate from `res/values/` (`gb_ui.xml`, `gb_phonedata.xml`, ...) and keep the
   `name=` attributes. Leave out strings marked `translatable="false"`.
3. Rules the tests enforce (`ResourcesI18nTest`):
   * Keep every placeholder, with its number: `%1$s`, `%2$d`. You can reorder them in the sentence, which is why
     they are numbered. A translation that adds, drops or renames one fails.
   * `<plurals>`: give the quantities your language needs (CLDR: `zero one two few many other`). `other` is required.
   * Escape `\'`, `&amp;`, `\n` as English does.
   * No keys that English does not have (lint `ExtraTranslation`, an error). A missing translation is only a
     warning (`MissingTranslation`), because Android falls back to English.
4. Names and numbers the app inserts (a computer's name, a phone number) are wrapped by the app in Unicode
   isolates (`Bidi.isolate` / `Bidi.ltr`), so do not add direction marks to your text. For a right-to-left
   language, also check the layout with the pseudo-locale below.
5. Try it: `./gradlew :app:testDebugUnitTest :app:lintDebug` (needs JDK 17 and the SDK, see [builds](builds.md)),
   then set the phone or emulator to your language.

### Worked example: `eo`

The fixture is `android/app/src/test/i18n-fixtures/values-eo/gb_fixture.xml`:

```xml
<resources>
    <string name="dial_allow">Voki</string>
    <!-- Word order differs from English: %2$s (the computer) comes first. -->
    <string name="call_dial_notice">Komputilo %2$s petis vokon al %1$s</string>
    <plurals name="mirror_apps_count">
        <item quantity="one">%d aplikaĵo elektita</item>
        <item quantity="other">%d aplikaĵoj elektitaj</item>
    </plurals>
</resources>
```

`ResourcesI18nTest.everyLocaleKeepsEnglishKeysAndPlaceholders` loads every `values-<language>` folder, shipped or
fixture, and compares keys and placeholders with English. To ship a real language, move the file(s) to
`src/main/res/values-eo/` and delete the fixture; the same test then guards the shipped copy.

### Pseudo-locales and right-to-left

Debug builds enable `en-XA` (accented, about 40% longer: unaccented text on screen is a hard-coded string) and
`ar-XB` (right-to-left, mirrored: anything stuck to the left or right is a layout bug). `AndroidManifest.xml` sets
`supportsRtl="true"`; layouts use `start`/`end` (`marginStart`, `setPaddingRelative`), never `left`/`right`.
Lint fails the build on `HardcodedText`, `SetTextI18n`, `RtlHardcoded`, `RtlSymmetry`, `StringFormatInvalid`,
`StringFormatMatches`, `StringFormatCount` and `ExtraTranslation`.

```sh
adb shell cmd locale set-app-locales dev.geekbridge.android --locales ar-XB
```

## Desktop

1. Copy `crates/bridge-i18n/i18n/en.properties` to `crates/bridge-i18n/i18n/eo.properties` (or only the keys you
   translate). The file name is the language tag: `eo`, `pt-BR`.
2. Translate the values. Never change keys. `{name}` placeholders must stay, but can move; `{n}` is the count in
   plurals; `key.one` / `key.other` (and `zero two few many` where needed) are separate keys, `other` is required.
   If your language's plural rule is missing from `plural_category` in `crates/bridge-i18n/src/lib.rs`, add it in the
   same pull request.
3. Do not paste invisible direction characters (LRM, RLM, embeddings, isolates): the test rejects them.
4. Register the file in `BUNDLED` in `i18n.rs`: `("eo", include_str!("../i18n/eo.properties"))`.
5. Try it without rebuilding: `GEEKBRIDGE_I18N_DIR=crates/bridge-i18n/i18n GEEKBRIDGE_LANG=eo cargo run -p bridge-apps --bin geekbridge`.
   `GEEKBRIDGE_LANG=en-XA` shows the pseudo-locale: unaccented text is a literal that is not in the catalogue.
6. `cargo test -p bridge-apps --test i18n_catalogue` must pass.

### Worked example: `eo`

The fixture is `crates/bridge-apps/tests/fixtures/eo.properties`:

```properties
home.nav.home = Hejmo
home.whatsnew.title = Ĝisdatigita al {version}
home.launch_failed = Ne eblis startigi {name}: {error}
phone.events.one = {n} evento
phone.events.other = {n} eventoj
```

`fixture_translations_pass_the_translation_checks` runs every `tests/fixtures/*.properties` through the same checks
as a bundled language (known keys, same placeholders, plural `other`, no hidden direction controls), and
`a_fixture_is_never_bundled` fails if `eo` appears in `BUNDLED`.

## For developers adding UI text

* Android: add the string to `res/values/*.xml`; a sentence with a name or number takes positional arguments
  (`Dialing %1$s requested by %2$s`), never `getString(a) + " " + getString(b)`. Counts are `<plurals>`. Wrap values
  you did not choose (names, numbers, notification text) in `Bidi.isolate(...)` / `Bidi.ltr(...)`.
* Desktop: add the key to `en.properties` and use `tr`, `tr_args`, `tr_n`. Two translated pieces become one key with
  placeholders (`translated_fragments_are_not_concatenated` fails on `tr(..) + ...`). The literal guard
  (`no_new_literals_outside_the_catalogue`) fails on new visible text; its allowlist
  `tests/i18n_allowlist.txt` only shrinks. Mark a product name or terminal text `// i18n-ok: reason`.
* Desktop banners (`notify-send`, Windows toasts, macOS notifications) are built in `bridge-service` and wrap
  phone-chosen text with `notify_text::isolate`. The egui windows do not lay out right-to-left text yet, so
  right-to-left languages are an Android-only goal for now; `display_text` strips direction controls from device
  names so a hostile name cannot reorder the window.
