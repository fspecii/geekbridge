<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Translating GeekBridge

GeekBridge is free software (GPL-3.0-or-later) and the user interface is ready for community translations. English is
the source language on both platforms. No translation ships yet, so the first one you add is a new file, not an edit.

**Your translation is a contribution under the project licence.** By submitting one you agree it is licensed
GPL-3.0-or-later like the rest of the code, and that you wrote it or have the right to submit it. Do not paste text
from KDE Connect, other projects or machine-translation services whose terms forbid reuse. Translators are credited
in the release notes and the git history.

## What gets translated

| Part | English source | Translation goes in |
| --- | --- | --- |
| Android app | `android/app/src/main/res/values/*.xml` | `android/app/src/main/res/values-<qualifier>/` with the same file names |
| Desktop apps (Home, Calls, Messages, Contacts, Calendar, Clipboard, Share), desktop banners (`banner.*`) and the tray menu (`tray.*`) | `crates/bridge-i18n/i18n/en.properties` | `crates/bridge-i18n/i18n/<tag>.properties` |
| Docs, changelog, `geekbridge-ctl` output, log lines, diagnostics reports | | not translated (they are for bug reports and developers) |

The desktop still has English text in a few screens that are on the allowlist in
`crates/bridge-apps/tests/i18n_allowlist.txt`; those move into the catalogue over time, and every one that moves is
shown to translators as a new key. Translating early is fine, untranslated keys fall back to English.

## Android

1. Copy the files you want from `res/values/` into `res/values-xx/` (`values-de`, `values-pt-rBR`,
   `values-b+sr+Latn` for BCP 47 tags with a script). Use the Android qualifier rules, see
   <https://developer.android.com/guide/topics/resources/providing-resources#QualifierRules>.
2. Translate the text, keep the `name=` attributes.
   * Leave strings marked `translatable="false"` out of your copy.
   * Keep placeholders exactly as written, and keep their numbers: `%1$s`, `%2$d`. You may reorder them in the
     sentence, that is why they are numbered.
   * `<plurals>`: provide every quantity your language needs (`zero`, `one`, `two`, `few`, `many`, `other`, see the
     CLDR table at <https://www.unicode.org/cldr/charts/latest/supplemental/language_plural_rules.html>). `other` is
     required.
   * `\'` is an apostrophe, `&amp;` an ampersand, `\n` a line break. Keep them.
3. Build and look at it (`scripts/build-android.sh`, or the commands in [builds](builds.md)); set the phone or the
   emulator to your language. Missing strings show in English, and lint reports them (`MissingTranslation`).
4. `whats_new.json` is generated from the changelog; translating it is optional and done per language file, ask in
   the pull request.

### Pseudo-locales

The debug build has Android's pseudo-locales switched on (`isPseudoLocalesEnabled`). On the device choose
**English (XA)** (accented, about 40% longer) or **Arabic (XB)** (right-to-left, mirrored) under Settings > System >
Languages (enable Developer options first), or from a shell:

```sh
adb shell cmd locale set-app-locales dev.geekbridge.android --locales en-XA
adb shell settings put system font_scale 1.5     # also test 1.0 and 1.5
```

In en-XA every translated string looks like `[Ĝéééķ Ɓŕíďğé ...]`. **Plain English text on a screen is a hard-coded
string** and a bug; text that is cut off or overlaps is a layout bug. Report either as an issue with a screenshot.
Lint fails the build on `HardcodedText` and `SetTextI18n` so new ones do not slip in.

## Desktop

The desktop apps and the service (banners, tray) read one catalogue: `crates/bridge-i18n/i18n/en.properties`, loaded by `crates/bridge-i18n/src/lib.rs`. The crate `bridge-i18n` has no dependencies, so the service does not pull in the UI toolkit.

### File format

A Java `.properties` subset (one `key = value` per line):

```properties
# A comment for translators.
update.later = Later
update.available = Update available: v{version}
phone.events.one = {n} event
phone.events.other = {n} events
report.no_browser = No browser could be opened.\n{url}
```

* **Keys never change** once released; the English text may. Do not translate or rename keys.
* `{name}` is a named placeholder; keep it, move it where your grammar wants it. `{n}` is always the count in a plural.
* `\n` is a line break, `\\` a backslash, `{{` and `}}` literal braces.
* **Plurals** are separate keys, `key.one`, `key.other`, and where your language needs them `key.zero`, `key.two`,
  `key.few`, `key.many`. `key.other` is required. The rule for choosing is CLDR's, implemented in `plural_category`
  in `i18n.rs`; if your language is missing there, add its rule in the same pull request.
* A translation may leave a key out, English is shown for it. Keys that do not exist in English, or a placeholder
  that English does not have, fail the tests.

### Adding a language

1. Copy `en.properties` to `crates/bridge-i18n/i18n/<tag>.properties`, where `<tag>` is `de`, `pt-BR`, ... (language,
   optionally `-REGION`). `pt-BR` falls back to `pt`, then to English.
2. Register it in `BUNDLED` in `crates/bridge-i18n/src/lib.rs`: `("de", include_str!("../i18n/de.properties"))`.
3. Run `cargo test -p bridge-apps`. The catalogue tests check keys and placeholders.
4. Try it without rebuilding: `GEEKBRIDGE_I18N_DIR=crates/bridge-i18n/i18n GEEKBRIDGE_LANG=de geekbridge`.

The language is chosen from `GEEKBRIDGE_LANG`, then `LC_ALL`, `LC_MESSAGES`, `LANG`. Dates, month and weekday names
and the 12/24 hour clock come from `localtime.rs` (English, German, French, Spanish, Portuguese, Italian today, from
`LC_TIME`) and are a separate small table.

Limits to know about: the embedded fonts cover Latin, Greek and Cyrillic; other scripts need a system font, and
right-to-left layout is not supported by the desktop toolkit yet, so Arabic and Hebrew desktop translations are not
usable until that lands. Android has no such limits.

### Pseudo-locale

`GEEKBRIDGE_LANG=en-XA geekbridge` shows every catalogue string accented and about 40% longer, inside `[ ]`:

* English letters without accents on screen are literals that are not in the catalogue yet;
* a closing `]` that is missing means the text is clipped;
* long lines should wrap or truncate cleanly and never overlap a neighbour.

`scripts/screenshots-desktop-nothing.sh` captures every window; use
`GEEKBRIDGE_LANG=en-XA PREFIX=i18n-en-XA scripts/screenshots-desktop-nothing.sh light` for pseudo-locale images in
`dist/screenshots/desktop/`. The golden images in `tests/golden` are always rendered in English.

### For developers adding UI text

Never write user-visible text as a literal in UI code: add a key to `en.properties` and use `tr("key")`,
`tr_args("key", &[("name", &value)])` or `tr_n("key", count, &[])`. Do not build a sentence from pieces; give the
whole sentence its own key with placeholders (word order differs between languages). The test
`crates/bridge-apps/tests/i18n_catalogue.rs` fails when a UI source file gains a user-visible literal. A technical
string that only looks like text can carry a `// i18n-ok` comment on its line.

## Glossary: keep these untranslated

| Term | Rule |
| --- | --- |
| GeekBridge | Product name. Never translated, never inflected into something else. |
| KDE Connect | Name of another project and protocol. Never translated. |
| GitHub, Android, Wi-Fi, Bluetooth, SMS, MMS, RCS, QR | Keep as is, or the usual local form for Wi-Fi/QR if your language has one. |
| `geekbridge-ctl`, `geekbridge-service`, command lines, file names, key names such as `Ctrl+Shift+V`, URLs | Keep exactly, they are typed by users. |
| SHA-256, GPL-3.0-or-later, SPDX identifiers | Keep. |

Product concepts to translate consistently, choose one word per language and keep it: *pair / paired* (connect a
phone and a computer once, with approval), *unpair*, *clipboard*, *share*, *calendar*, *contacts*, *messages*,
*diagnostics*. When a term in your language already exists in Android's or your desktop's own UI, use that one.

## Tone

* Plain, calm, direct, second person ("you"); the informal form where your language has both and it is not rude.
* Say what happens and what the user can do next. No jokes, no exclamation marks, no blame ("You did..." becomes
  "That did not work...").
* Same meaning, not same word order. Keep the privacy statements exactly as strong as the English ones, for example
  "Nothing is sent automatically" must stay a clear promise.
* Buttons are short verbs ("Send", "Pair"), not sentences. Titles are not full stops.
* Keep sentence case if your language uses it.

## Weblate-compatible layout (no service is set up)

The file layout is chosen so a Weblate (or similar) project can be added later without moving files; nothing here
needs a hosted service today.

| Component | File format in Weblate | File mask | Template (source) |
| --- | --- | --- | --- |
| Android, one component per source file (`gb_ui`, `gb_home`, `gb_pairing`, `gb_phonedata`, `gb_widgets`, `gb_clipboard`, `strings`) | Android String Resource | `android/app/src/main/res/values-*/gb_ui.xml` | `android/app/src/main/res/values/gb_ui.xml` |
| Desktop | Java Properties (UTF-8) | `crates/bridge-i18n/i18n/*.properties` | `crates/bridge-i18n/i18n/en.properties` |

Notes: both are monolingual (a template file with keys, one file per language). Android's own qualifier names
(`values-pt-rBR`) differ from the desktop tags (`pt-BR`); Weblate maps them with its language-code settings. Desktop
plurals are plain separate keys (`.one`, `.other`), which Weblate shows as individual strings; treat them as a pair.
Translation commits must go through the normal pull-request review and the `i18n_catalogue` test, with the licence
statement above.
