// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings > "Report a problem": shows exactly what a prefilled GitHub issue link carries, and
//! opens it in the browser only when the user asks. The app makes no network request itself.
use crate::{
    i18n::{tr, tr_args},
    nothing::{self, S1},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::report::{self, Link};
use eframe::egui::{self, RichText, ScrollArea};
use std::sync::mpsc;

pub fn section() -> &'static str {
    tr("report.section")
}
pub fn intro() -> &'static str {
    tr("report.intro")
}
pub fn review() -> &'static str {
    tr("report.review")
}
pub fn collecting() -> &'static str {
    tr("report.collecting")
}

pub struct ReportPanel {
    link: Link,
    note: Option<String>,
    opening: Option<mpsc::Receiver<bool>>,
}

impl ReportPanel {
    pub fn new(diagnostics: &str) -> Self {
        Self {
            link: report::desktop_link(
                &report::repo_from_env(),
                diagnostics,
                env!("CARGO_PKG_VERSION"),
            ),
            note: None,
            opening: None,
        }
    }

    pub fn link(&self) -> &Link {
        &self.link
    }

    /// Draws the preview card. Returns `false` once the user closed it.
    pub fn show(&mut self, ui: &mut egui::Ui, pal: &Palette) -> bool {
        let ctx = ui.ctx().clone();
        if let Some(rx) = &self.opening
            && let Ok(opened) = rx.try_recv()
        {
            self.opening = None;
            self.note = Some(if opened {
                tr("report.opened").to_owned()
            } else {
                ctx.copy_text(self.link.preview());
                tr_args("report.no_browser", &[("url", &self.link.url)])
            });
        }
        let mut keep = true;
        nothing::card(ui, pal, |ui| {
            ui.add(
                egui::Label::new(RichText::new(tr("report.preview_title")).color(pal.text)).wrap(),
            );
            ui.add_space(S1);
            ScrollArea::vertical()
                .drag_to_scroll(false)
                .id_salt("report-preview")
                .max_height(150.0)
                .show(ui, |ui| {
                    let mut text = self.link.preview();
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(true),
                    );
                });
            ui.add_space(S1);
            if self.link.truncated {
                ui.add(egui::Label::new(theme::caption(pal, tr("report.truncated"))).wrap());
            }
            ui.add(egui::Label::new(theme::caption(pal, tr("report.nothing_sent"))).wrap());
            ui.add_space(S1);
            ui.horizontal_wrapped(|ui| {
                let idle = self.opening.is_none();
                if theme::pill_button(ui, pal, tr("report.open"), ButtonKind::Primary, idle)
                    .clicked()
                {
                    let (tx, rx) = mpsc::channel();
                    let url = self.link.url.clone();
                    let wake = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(report::open_in_browser(&url));
                        wake.request_repaint();
                    });
                    self.opening = Some(rx);
                }
                if theme::pill_button(ui, pal, tr("report.copy"), ButtonKind::Plain, true).clicked()
                {
                    ctx.copy_text(self.link.preview());
                    self.note = Some(tr("report.copied").to_owned());
                }
                if theme::pill_button(ui, pal, tr("report.close"), ButtonKind::Plain, true)
                    .clicked()
                {
                    keep = false;
                }
            });
            if let Some(note) = &self.note {
                ui.add_space(S1);
                ui.add(egui::Label::new(theme::caption(pal, note.as_str())).wrap());
            }
        });
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_link_matches_what_is_previewed() {
        let p = ReportPanel::new("== Versions ==\nservice: 0.1.0");
        let l = p.link();
        assert!(l.url.starts_with("https://github.com/"));
        assert!(l.preview().contains("service: 0.1.0"));
        assert!(report::decode(&l.url).contains("service: 0.1.0"));
    }

    #[test]
    fn statement_says_nothing_is_sent_automatically() {
        assert!(intro().contains("Nothing is sent automatically"));
        assert!(tr("report.nothing_sent").contains("Nothing is sent"));
    }
}
