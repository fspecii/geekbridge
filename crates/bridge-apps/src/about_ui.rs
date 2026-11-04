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
    open_in_desktop(&target.open_arg());
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
