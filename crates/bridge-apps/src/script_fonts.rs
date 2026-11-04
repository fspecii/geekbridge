// SPDX-License-Identifier: GPL-3.0-or-later
//! Fallback fonts for scripts the bundled Noto Sans subset does not cover (CJK, Arabic, Hebrew, Indic,
//! Thai, ...). A message in any language must not turn into empty boxes, but a CJK face is 20 MB, so
//! nothing is loaded until text that needs it is on screen: the window scans what it is about to draw,
//! asks the system for one font per script (`fc-match` on Linux, well-known paths elsewhere) and
//! installs it as a last-resort fallback. Nothing is bundled or downloaded. egui does not shape or
//! reorder text, so right-to-left and joining scripts show their glyphs in logical order.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    Cjk,
    Arabic,
    Hebrew,
    Devanagari,
    Bengali,
    Gurmukhi,
    Gujarati,
    Tamil,
    Telugu,
    Kannada,
    Malayalam,
    Thai,
    Georgian,
    Armenian,
    Ethiopic,
}

struct Info {
    script: Script,
    /// A letter the font must have; asked of `fc-match` as `:charset=`.
    probe: u32,
    ranges: &'static [(u32, u32)],
    paths: &'static [&'static str],
}

const INFO: &[Info] = &[
    Info {
        script: Script::Cjk,
        probe: 0x4E2D,
        ranges: &[
            (0x1100, 0x11FF),
            (0x2E80, 0x2FDF),
            (0x3000, 0x30FF),
            (0x3100, 0x312F),
            (0x3400, 0x4DBF),
            (0x4E00, 0x9FFF),
            (0xAC00, 0xD7AF),
            (0xF900, 0xFAFF),
            (0xFF00, 0xFFEF),
            (0x20000, 0x2FA1F),
        ],
        paths: &[
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/System/Library/Fonts/PingFang.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\YuGothR.ttc",
            "C:\\Windows\\Fonts\\malgun.ttf",
        ],
    },
    Info {
        script: Script::Arabic,
        probe: 0x0627,
        ranges: &[
            (0x0600, 0x06FF),
            (0x0750, 0x077F),
            (0x08A0, 0x08FF),
            (0xFB50, 0xFDFF),
            (0xFE70, 0xFEFF),
        ],
        paths: &[
            "/usr/share/fonts/noto/NotoNaskhArabic-Regular.ttf",
            "/System/Library/Fonts/GeezaPro.ttc",
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "C:\\Windows\\Fonts\\arial.ttf",
        ],
    },
    Info {
        script: Script::Hebrew,
        probe: 0x05D0,
        ranges: &[(0x0590, 0x05FF), (0xFB1D, 0xFB4F)],
        paths: &[
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
            "/System/Library/Fonts/ArialHB.ttc",
            "C:\\Windows\\Fonts\\arial.ttf",
        ],
    },
    Info {
        script: Script::Devanagari,
        probe: 0x0905,
        ranges: &[(0x0900, 0x097F), (0xA8E0, 0xA8FF)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansDevanagari-Regular.ttf",
            "/System/Library/Fonts/Kohinoor.ttc",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Bengali,
        probe: 0x0985,
        ranges: &[(0x0980, 0x09FF)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansBengali-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Gurmukhi,
        probe: 0x0A05,
        ranges: &[(0x0A00, 0x0A7F)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansGurmukhi-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Gujarati,
        probe: 0x0A85,
        ranges: &[(0x0A80, 0x0AFF)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansGujarati-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Tamil,
        probe: 0x0B85,
        ranges: &[(0x0B80, 0x0BFF)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansTamil-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Telugu,
        probe: 0x0C05,
        ranges: &[(0x0C00, 0x0C7F)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansTelugu-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Kannada,
        probe: 0x0C85,
        ranges: &[(0x0C80, 0x0CFF)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansKannada-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Malayalam,
        probe: 0x0D05,
        ranges: &[(0x0D00, 0x0D7F)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansMalayalam-Regular.ttf",
            "C:\\Windows\\Fonts\\Nirmala.ttf",
        ],
    },
    Info {
        script: Script::Thai,
        probe: 0x0E01,
        ranges: &[(0x0E00, 0x0E7F)],
        paths: &[
            "/usr/share/fonts/noto/NotoSansThai-Regular.ttf",
            "/System/Library/Fonts/Supplemental/Thonburi.ttc",
            "C:\\Windows\\Fonts\\LeelawUI.ttf",
        ],
    },
    Info {
        script: Script::Georgian,
        probe: 0x10D0,
        ranges: &[(0x10A0, 0x10FF), (0x1C90, 0x1CBF)],
        paths: &["/usr/share/fonts/noto/NotoSansGeorgian-Regular.ttf"],
    },
    Info {
        script: Script::Armenian,
        probe: 0x0531,
        ranges: &[(0x0530, 0x058F)],
        paths: &["/usr/share/fonts/noto/NotoSansArmenian-Regular.ttf"],
    },
    Info {
        script: Script::Ethiopic,
        probe: 0x1200,
        ranges: &[(0x1200, 0x139F)],
        paths: &["/usr/share/fonts/noto/NotoSansEthiopic-Regular.ttf"],
    },
];

/// Smallest code point any script above can need.
const FIRST: u32 = 0x0530;
const MAX_FONT_BYTES: u64 = 48 * 1024 * 1024;

fn info(script: Script) -> &'static Info {
    INFO.iter().find(|i| i.script == script).expect("listed")
}

/// Scripts in `text` that the bundled fonts do not cover, in the order of [`INFO`], each once.
pub fn scripts_in(text: &str) -> Vec<Script> {
    let mut found = [false; INFO.len()];
    for c in text.chars() {
        let cp = u32::from(c);
        if cp < FIRST {
            continue;
        }
        if let Some(i) = INFO
            .iter()
            .position(|i| i.ranges.iter().any(|&(a, b)| (a..=b).contains(&cp)))
        {
            found[i] = true;
        }
    }
    INFO.iter()
        .zip(found)
        .filter_map(|(i, f)| f.then_some(i.script))
        .collect()
}

/// `fc-match -f '%{file}|%{index}'` output: the file and the face number inside a collection.
pub fn parse_match(line: &str) -> Option<(PathBuf, u32)> {
    let (file, index) = line.trim().rsplit_once('|')?;
    (!file.is_empty()).then(|| (PathBuf::from(file), index.trim().parse().unwrap_or(0)))
}

fn from_fontconfig(probe: u32) -> Option<(PathBuf, u32)> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}|%{index}", &format!(":charset={probe:x}")])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| parse_match(&String::from_utf8_lossy(&out.stdout)))
        .flatten()
}

fn load(script: Script) -> Option<Arc<FontData>> {
    let i = info(script);
    let known = i.paths.iter().map(|p| (PathBuf::from(p), 0));
    let found = from_fontconfig(i.probe)
        .into_iter()
        .chain(known)
        .find(|(p, _)| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() < MAX_FONT_BYTES)
        })?;
    let bytes = std::fs::read(&found.0).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = found.1;
    Some(Arc::new(data))
}

/// Every script asked for so far, with its font when one was found (a miss is remembered too).
static LOADED: Mutex<Vec<(Script, Option<Arc<FontData>>)>> = Mutex::new(Vec::new());

fn bundled_only() -> bool {
    std::env::var_os("GEEKBRIDGE_BUNDLED_FONTS_ONLY").is_some()
}

/// Adds the fonts loaded so far behind everything else in the proportional family.
pub fn add_loaded(fonts: &mut FontDefinitions) {
    let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
    for (script, data) in loaded.iter() {
        if let Some(data) = data {
            let name = format!("script-{script:?}");
            fonts.font_data.insert(name.clone(), data.clone());
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push(name);
        }
    }
}

/// Makes sure text about to be drawn has glyphs: loads the fallback font of every script in `texts` that was
/// not asked for before and re-installs the fonts. Returns whether anything new was installed.
pub fn ensure<'a>(ctx: &egui::Context, texts: impl IntoIterator<Item = &'a str>) -> bool {
    if bundled_only() {
        return false;
    }
    let mut wanted = Vec::new();
    for t in texts {
        for s in scripts_in(t) {
            if !wanted.contains(&s) {
                wanted.push(s);
            }
        }
    }
    if wanted.is_empty() {
        return false;
    }
    let fresh: Vec<Script> = {
        let loaded = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        wanted
            .into_iter()
            .filter(|s| !loaded.iter().any(|(l, _)| l == s))
            .collect()
    };
    if fresh.is_empty() {
        return false;
    }
    let results: Vec<_> = fresh.into_iter().map(|s| (s, load(s))).collect();
    let any = results.iter().any(|(_, d)| d.is_some());
    LOADED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .extend(results);
    if any {
        crate::theme::install_fonts(ctx);
    }
    any
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_scripts_outside_the_bundled_fonts_are_found() {
        assert!(scripts_in("Hello Grüße Привет Ελλάδα Việt").is_empty());
        assert!(scripts_in("emoji \u{1F389} arrows \u{2192} \u{20AC}").is_empty());
        assert_eq!(scripts_in("你好 こんにちは 안녕"), vec![Script::Cjk]);
        assert_eq!(
            scripts_in("שלום مرحبا नमस्ते สวัสดี"),
            vec![
                Script::Arabic,
                Script::Hebrew,
                Script::Devanagari,
                Script::Thai
            ]
        );
        assert_eq!(
            scripts_in("a \u{20BB7}"),
            vec![Script::Cjk],
            "extension B ideograph"
        );
    }

    #[test]
    fn fontconfig_answers_name_the_file_and_the_face() {
        assert_eq!(
            parse_match("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc|2\n"),
            Some((
                PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"),
                2
            ))
        );
        assert_eq!(
            parse_match("/a/b.ttf|x"),
            Some((PathBuf::from("/a/b.ttf"), 0))
        );
        assert_eq!(parse_match(""), None);
        assert_eq!(parse_match("|0"), None);
    }

    #[test]
    fn every_script_has_a_probe_inside_its_own_ranges() {
        for i in INFO {
            assert!(
                i.ranges.iter().any(|&(a, b)| (a..=b).contains(&i.probe)),
                "{:?}",
                i.script
            );
            assert!(i.ranges.iter().all(|&(a, _)| a >= FIRST), "{:?}", i.script);
        }
    }

    #[test]
    fn a_system_font_replaces_the_empty_boxes_once_text_needs_it() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let id = egui::FontId::proportional(14.0);
        let _ = ctx.run(Default::default(), |_| {});
        let has = |c: char| ctx.fonts(|f| f.has_glyph(&id, c));
        assert!(!has('\u{4E2D}'), "the bundled fonts have no CJK");
        ensure(&ctx, ["\u{4E2D}\u{6587}"]);
        crate::theme::install_fonts(&ctx); // also covers another test having loaded it first
        let _ = ctx.run(Default::default(), |_| {});
        if !has('\u{4E2D}') {
            eprintln!("skipped: no system font with CJK on this machine");
            return;
        }
        assert!(has('\u{6587}'));
        assert!(has('a'), "Latin still comes from the first font");
    }
}
