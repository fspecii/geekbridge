# Bundled fonts

The desktop apps embed these fonts (see `crates/bridge-apps/src/theme.rs`, `EMBEDDED_FONTS`). egui's
`default_fonts` feature is deliberately **off**: it would also embed `Ubuntu-Light`, which is under the
Ubuntu Font Licence 1.0 (free, but not GPL-compatible). Every font below is under the SIL Open Font
Licence 1.1 or an MIT-style licence, both of which can be combined with GPL-3.0-or-later software.

| File | Font | Licence | Licence text |
|---|---|---|---|
| `NotoSans-Regular-subset.ttf` | Noto Sans 2.015 (notofonts/latin-greek-cyrillic), subset | OFL-1.1 | `OFL-NotoSans.txt` |
| `NotoEmoji-Regular.ttf` | Noto Emoji (monochrome) | OFL-1.1 | `OFL-NotoEmoji.txt` |
| `emoji-icon-font.ttf` | emoji-icon-font | MIT | `LICENSE-emoji-icon-font-MIT.txt` |
| `Hack-Regular.ttf` | Hack (monospace), derived from Bitstream Vera and DejaVu | MIT + Bitstream Vera | `LICENSE-Hack.txt` |

`NotoEmoji-Regular.ttf`, `emoji-icon-font.ttf` and `Hack-Regular.ttf` are the unmodified files from the
`epaint_default_fonts` crate (MIT OR Apache-2.0 packaging; the fonts keep their own licences above).

## Noto Sans subset

Produced with fonttools from the unmodified static Noto Sans Regular, keeping the name table (copyright
and licence strings), no font is renamed or modified beyond dropping glyphs and hinting:

```sh
pyftsubset NotoSans-Regular.ttf \
  --unicodes="U+0000-024F,U+0300-036F,U+0370-03FF,U+0400-052F,U+1E00-1EFF,U+2000-206F,U+20A0-20CF,U+2100-214F,U+2190-21FF,U+2212,U+2215,U+25A0-25FF,U+FFFD" \
  --layout-features='kern,liga,ccmp,locl,mark,mkmk' --no-hinting --desubroutinize --name-IDs='*' \
  --output-file=NotoSans-Regular-subset.ttf
```

That covers Latin, Latin Extended (incl. Vietnamese), Greek, Cyrillic, general punctuation, currency
symbols, arrows and geometric shapes. Other scripts (CJK, Arabic, Indic, ...) come from the system fonts
when present; the UI prefers the platform UI font (Segoe UI, San Francisco, Noto Sans, ...) and uses these
only as a fallback.

The OFL "Reserved Font Name" clause is not triggered: the subset is not distributed under a different
name than the original font file's family name, and its name table is unchanged.
