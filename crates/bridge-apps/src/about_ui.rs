// SPDX-License-Identifier: GPL-3.0-or-later
//! "About GeekBridge" in Settings: version, build commit and update channel, the licence, the
//! independence statement, the project address and buttons for the licence text and the
//! third-party notices. The facts and wording are shared with `geekbridge-ctl about`
//! (`bridge_core::about`).
use crate::{
    a11y,
    i18n::{tr, tr_args},
    nothing::S1,
    theme::{self, ButtonKind, Palette},
    update_ui::open_in_desktop,
};
use bridge_core::about::{self, About, DocTarget};
use eframe::egui;

/// The text lines of the section, in the active language.
pub fn lines(about: &About) -> Vec<String> {
    about::lines(about, &|key| tr(key).to_owned(), &|key, args| {
        let args: Vec<(&str, &dyn std::fmt::Display)> = args
            .iter()
            .map(|(k, v)| (*k, v as &dyn std::fmt::Display))
            .collect();
        tr_args(key, &args)
    })
}

/// What a button opens: the installed file, else the page on GitHub. Only these fixed
/// targets are ever passed to the system opener.
fn open(target: &DocTarget) {
    let (program, args) = open_command(std::env::consts::OS, target);
    match program {
        "" => open_in_desktop(&target.open_arg()),
        _ => {
            if let Ok(mut child) = std::process::Command::new(program).args(args).spawn() {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
    }
}

/// The program that shows an installed document as text, or `""` for the system default. `LICENSE` has
/// no extension and `THIRD_PARTY_NOTICES.md` usually no association, so Windows (`explorer`) would ask
/// "How do you want to open this file?" and a Mac may find no application: Notepad and `open -t`
/// (the default text editor) always work. Web pages and Linux use the system opener.
fn open_command(os: &str, target: &DocTarget) -> (&'static str, Vec<String>) {
    match (os, target) {
        ("windows", DocTarget::File(p)) => ("notepad", vec![p.display().to_string()]),
        ("macos", DocTarget::File(p)) => ("open", vec!["-t".into(), p.display().to_string()]),
        _ => ("", Vec::new()),
    }
}

pub fn show(ui: &mut egui::Ui, pal: &Palette, about: &About) {
    let all = lines(about);
    // First line is the headline; the rest are facts. The two document locations are shown
    // by the buttons below, so they are not repeated as text.
    let shown = all.len().saturating_sub(2);
    for (i, line) in all.iter().take(shown).enumerate() {
        let text = if i == 0 {
            theme::secondary(pal, line.clone())
        } else {
            theme::caption(pal, line.clone())
        };
        ui.add(egui::Label::new(text).wrap());
    }
    ui.add_space(S1);
    ui.horizontal_wrapped(|ui| {
        for (n, label, name) in [
            (0, "about.open_license", "about.a11y.license"),
            (1, "about.open_notices", "about.a11y.notices"),
            (2, "about.open_project", "about.a11y.project"),
        ] {
            let response = theme::pill_button(ui, pal, tr(label), ButtonKind::Plain, true);
            if a11y::named(response, tr(name)).clicked() {
                match n {
                    0 => open(&about::license_target()),
                    1 => open(&about::notices_target()),
                    _ => open_in_desktop(about::PROJECT_URL),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_documents_open_as_text_on_windows_and_macos_only() {
        let file = DocTarget::File("C:\\GeekBridge\\LICENSE".into());
        assert_eq!(
            open_command("windows", &file),
            ("notepad", vec!["C:\\GeekBridge\\LICENSE".to_owned()])
        );
        assert_eq!(open_command("macos", &file).1[0], "-t");
        assert_eq!(open_command("linux", &file).0, "");
        let url = DocTarget::Url(about::LICENSE_URL);
        for os in ["windows", "macos", "linux"] {
            assert_eq!(
                open_command(os, &url).0,
                "",
                "web pages use the system opener"
            );
        }
    }

    #[test]
    fn every_fact_is_in_english_text() {
        let text = lines(&About {
            commit: Some("0123456789ab".into()),
            channel: Some("stable".into()),
        })
        .join("\n");
        assert!(text.contains(&format!("GeekBridge v{}", about::VERSION)));
        assert!(text.contains("commit 0123456789ab"));
        assert!(text.contains("Update channel: stable"));
        assert!(text.contains("Licence: GPL-3.0-or-later"));
        assert!(text.contains(
            "Independent implementation compatible with the KDE Connect protocol; not affiliated with KDE."
        ));
        assert!(text.contains("https://github.com/fspecii/geekbridge"));
        assert!(!text.contains("{") && !text.contains("about."));
    }
}
