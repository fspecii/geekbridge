// SPDX-License-Identifier: GPL-3.0-or-later
//! First-time pairing on the computer: the "Link a phone" card with its live QR code, one-tap Pair, the
//! plain-language problem line, "Test connection" and the "what to do next" card.
//!
//! A child module of `connection` so it can use `Connections`' private state; it owns no protocol
//! logic. Every decision still goes to the service as an explicit request.

use super::*;
use crate::i18n::{tr, tr_args};
use crate::theme::ButtonKind;
use bridge_core::pairing_help::ConnectionTest;
use qrcode::{Color as QrColor, EcLevel, QrCode};
use std::time::Instant;

/// Renew the code a little before the service would refuse it.
const RENEW_BEFORE: Duration = Duration::from_secs(10);

/// What the user can open from the "what to do next" card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextStep {
    Clipboard,
    Share,
}

/// A friendly sentence and one thing to try.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub headline: String,
    pub advice: String,
}

impl Problem {
    pub fn from_reason(reason: &str) -> Self {
        let e = bridge_core::pairing_help::explain_failure(reason);
        Self {
            headline: e.headline.into(),
            advice: e.advice.into(),
        }
    }
}

#[derive(Default)]
pub(super) struct PairUi {
    qr_open: bool,
    qr_shown_at: Option<Instant>,
    qr_cache: Option<(String, Vec<bool>, usize)>,
    /// Phones whose "Pair" button was tapped: pair as soon as the link is up.
    auto_pair: BTreeSet<String>,
    known_paired: Option<BTreeSet<String>>,
    next_steps: Option<String>,
    /// The Pair view was drawn this recently; the code is only kept alive while someone can see it.
    card_seen: Option<Instant>,
    /// The code was shown automatically once (first run); after that only the button opens it.
    auto_opened: bool,
    /// When the phone's scan was first noticed (the "confirm on phone" state ends after a while).
    scanned_since: Option<Instant>,
    /// Since when the service has been writing a trust record (the "Saving the link…" line waits a few seconds).
    saving_since: Option<Instant>,
}

/// A trust-record write that takes longer than this is reported to the user as a busy disk.
const SAVING_AFTER: Duration = Duration::from_secs(3);

/// Whether a write that started at `since` has been going on long enough to say so.
pub(super) fn saving_is_slow(since: Option<Instant>, now: Instant) -> bool {
    since.is_some_and(|s| now.saturating_duration_since(s) >= SAVING_AFTER)
}

/// How long a code stays alive after the Pair view was last drawn (a hidden view must not keep issuing tokens).
const HIDDEN_GRACE: Duration = Duration::from_secs(4);
/// How long "Phone scanned" is believed with no result.
const SCAN_STATE_MAX: Duration = Duration::from_secs(90);

/// The live line under the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    Making,
    Waiting,
    Scanned,
}

/// Dark/light modules of `text` as a QR code (`width` modules per side), or `None` if too long.
pub fn qr_modules(text: &str) -> Option<(Vec<bool>, usize)> {
    let code = QrCode::with_error_correction_level(text.as_bytes(), EcLevel::M).ok()?;
    let width = code.width();
    let dark = code
        .to_colors()
        .into_iter()
        .map(|c| c == QrColor::Dark)
        .collect();
    Some((dark, width))
}

pub fn draw_qr(ui: &mut egui::Ui, modules: &[bool], width: usize, side: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    // Always dark on white, with a quiet zone: scanners need the contrast in both themes.
    painter.rect_filled(rect, 8.0, egui::Color32::WHITE);
    let quiet = 2.0;
    let cell = side / (width as f32 + 2.0 * quiet);
    for (i, dark) in modules.iter().enumerate() {
        if *dark {
            let (x, y) = ((i % width) as f32, (i / width) as f32);
            let min = rect.min + egui::vec2((x + quiet) * cell, (y + quiet) * cell);
            painter.rect_filled(
                egui::Rect::from_min_size(min, egui::vec2(cell + 0.5, cell + 0.5)),
                0.0,
                egui::Color32::from_rgb(0x11, 0x11, 0x11),
            );
        }
    }
    response
}

fn test_view(ui: &mut egui::Ui, pal: &Palette, test: &ConnectionTest) {
    ui.add(egui::Label::new(theme::semibold(&test.headline, 14.0)).wrap());
    for step in &test.steps {
        ui.add(
            egui::Label::new(theme::secondary(
                pal,
                format!(
                    "{}  {} - {}",
                    if step.ok { "✔" } else { "✖" },
                    display_text(&step.label),
                    display_text(&step.detail)
                ),
            ))
            .wrap(),
        );
    }
    for line in &test.advice {
        ui.add(egui::Label::new(theme::caption(pal, format!("• {}", display_text(line)))).wrap());
    }
}

impl Connections {
    /// Per-frame bookkeeping: finish one-tap pairs, notice new pairings, renew the QR code.
    pub fn pair_tick(&mut self, ctx: &egui::Context) {
        let Some(status) = self.state.status.clone() else {
            return;
        };
        if status.saving.is_empty() {
            self.pair.saving_since = None;
        } else {
            let since = *self.pair.saving_since.get_or_insert_with(Instant::now);
            if !saving_is_slow(Some(since), Instant::now()) {
                ctx.request_repaint_after(SAVING_AFTER.saturating_sub(since.elapsed()));
            }
        }
        let ids: BTreeSet<String> = status.paired.iter().map(|p| p.device_id.clone()).collect();
        match &self.pair.known_paired {
            None => self.pair.known_paired = Some(ids),
            Some(known) => {
                if let Some(new) = status.paired.iter().find(|p| !known.contains(&p.device_id)) {
                    self.pair.next_steps = Some(display_text(&new.name));
                    self.pair.qr_open = false;
                    self.pair.known_paired = Some(ids);
                    if !self.state.busy {
                        self.submit(Request::PairQrCancel);
                    }
                }
            }
        }
        if !self.state.busy
            && let Some(link) = status.links.iter().find(|l| {
                l.phase == LinkPhase::Unpaired && self.pair.auto_pair.contains(&l.device_id)
            })
        {
            let id = link.device_id.clone();
            self.pair.auto_pair.remove(&id);
            self.submit(Request::Pair {
                device_id: id,
                mode: ipc::PairMode::Request,
            });
        }
        if self.state.qr_scanned {
            let since = *self.pair.scanned_since.get_or_insert_with(Instant::now);
            if since.elapsed() > SCAN_STATE_MAX {
                self.state.qr_scanned = false;
                self.pair.scanned_since = None;
            }
        } else {
            self.pair.scanned_since = None;
        }
        if self.pair.qr_open {
            let seen = *self.pair.card_seen.get_or_insert_with(Instant::now);
            if seen.elapsed() > HIDDEN_GRACE {
                // Someone left the Pair view: stop issuing tokens and revoke the one on screen.
                self.pair.qr_open = false;
                self.pair.card_seen = None;
                self.submit(Request::PairQrCancel);
                return;
            }
            if let Some(q) = &self.state.pair_qr {
                self.pair.qr_shown_at.get_or_insert_with(Instant::now);
                if self.pair.qr_cache.as_ref().map(|c| c.0.as_str()) != Some(q.payload.as_str()) {
                    self.pair.qr_cache =
                        qr_modules(&q.payload).map(|(m, w)| (q.payload.clone(), m, w));
                }
            }
            let renew = match (&self.state.pair_qr, self.pair.qr_shown_at) {
                (Some(q), Some(at)) => {
                    at.elapsed() + RENEW_BEFORE >= Duration::from_millis(q.expires_in_ms)
                }
                _ => false,
            };
            if renew && !self.state.busy && !self.state.qr_scanned {
                self.pair.qr_shown_at = None;
                self.submit(Request::PairQr);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }

    /// True once the trust record of `device_id` has been being written for a few seconds.
    pub(super) fn saving_slow(&self, device_id: &str) -> bool {
        saving_is_slow(self.pair.saving_since, Instant::now())
            && self
                .state
                .status
                .as_ref()
                .is_some_and(|s| s.saving.iter().any(|d| d == device_id))
    }

    /// "Saving the link… (your disk is busy)", spoken politely by a screen reader.
    pub(super) fn say_saving(ui: &mut egui::Ui, pal: &Palette) {
        let r = ui.add(egui::Label::new(theme::caption(pal, tr("pair.saving"))).wrap());
        crate::a11y::live_region(ui.ctx(), r.id, crate::a11y::Live::Polite);
    }

    /// What the live line under the code says.
    pub fn link_status(&self) -> LinkStatus {
        if self.state.pair_qr.is_none() {
            LinkStatus::Making
        } else if self.state.qr_scanned {
            LinkStatus::Scanned
        } else {
            LinkStatus::Waiting
        }
    }

    fn start_qr(&mut self) {
        self.pair.qr_open = true;
        self.pair.qr_shown_at = None;
        self.pair.card_seen = Some(Instant::now());
        self.state.qr_scanned = false;
        self.submit(Request::PairQr);
    }

    /// One tap: dial the phone, then ask it to pair; the code appears on both screens.
    pub(super) fn one_tap_pair(&mut self, device_id: &str) {
        self.pair.auto_pair.insert(device_id.to_owned());
        self.submit(Request::Connect {
            device_id: device_id.to_owned(),
            endpoint: None,
        });
    }

    /// The first-time card: QR button, nearby phones, problems, test, and what to do next.
    pub fn pair_card(
        &mut self,
        ui: &mut egui::Ui,
        pal: &Palette,
        discovery: &DiscoveryModel,
    ) -> Option<NextStep> {
        let mut action = None;
        if let Some(name) = self.pair.next_steps.clone() {
            theme::group(ui, pal, |ui| {
                ui.label(theme::semibold(
                    tr_args("pair.paired", &[("name", &name)]),
                    16.0,
                ));
                ui.label(theme::caption(pal, "What to do next"));
                ui.add(
                    egui::Label::new(
                        "1. Try the clipboard: turn on Clipboard sync in the phone app (Home > Settings), copy some text on the phone, then open Clipboard and click Paste from phone.",
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        "2. Send a file: drop it on this window. To receive from the phone, open Share files and tick \"Allow this phone to send me files\", then share to \"Send to computer\".",
                    )
                    .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    if theme::pill_button(ui, pal, "Open Clipboard", ButtonKind::Primary, true)
                        .clicked()
                    {
                        action = Some(NextStep::Clipboard);
                    }
                    if theme::pill_button(ui, pal, "Send a file…", ButtonKind::Plain, true)
                        .clicked()
                    {
                        action = Some(NextStep::Share);
                    }
                    if theme::pill_button(ui, pal, "Done", ButtonKind::Plain, true).clicked() {
                        self.pair.next_steps = None;
                    }
                });
            });
            ui.add_space(8.0);
        }
        self.pair.card_seen = Some(Instant::now());
        // First run (no phone paired yet): the code is simply there, like a web login.
        if !self.pair.auto_opened
            && !self.pair.qr_open
            && !self.state.busy
            && self
                .state
                .status
                .as_ref()
                .is_some_and(|s| s.paired.is_empty() && s.inbound_listener.is_some())
        {
            self.pair.auto_opened = true;
            self.start_qr();
        }
        self.link_hero(ui, pal);
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(theme::caption(
                pal,
                "Or open GeekBridge on your phone: it appears below by name. Tap Link and confirm the same code on both screens.",
            ))
            .wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            if theme::pill_button(
                ui,
                pal,
                "Test connection",
                ButtonKind::Plain,
                !self.state.busy && self.state.status.is_some(),
            )
            .clicked()
            {
                self.submit(Request::TestConnection {
                    device_id: None,
                    endpoint: None,
                });
            }
        });
        if let Some(problem) = &self.state.problem {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(RichText::new(&problem.headline).strong().color(pal.accent))
                    .wrap(),
            );
            ui.add(egui::Label::new(theme::caption(pal, &problem.advice)).wrap());
        }
        if let Some(test) = self.state.test.clone() {
            ui.add_space(6.0);
            theme::group(ui, pal, |ui| test_view(ui, pal, &test));
        }
        ui.add_space(6.0);
        self.details(ui, pal, discovery);
        action
    }

    /// Escape stops showing the code (the same as the Stop button).
    pub(super) fn qr_window(&mut self, ctx: &egui::Context, _pal: &Palette) {
        if self.pair.qr_open && theme::escape_pressed(ctx) {
            self.stop_qr();
        }
    }

    fn stop_qr(&mut self) {
        self.pair.qr_open = false;
        self.state.qr_scanned = false;
        self.submit(Request::PairQrCancel);
    }

    /// "Link a phone": the code, big, with a live line under it. No refresh button: it renews itself.
    fn link_hero(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add(egui::Label::new(theme::semibold(tr("pair.link.title"), 20.0)).wrap());
        if !self.pair.qr_open {
            ui.add(egui::Label::new(theme::caption(pal, tr("pair.link.how"))).wrap());
            if theme::pill_button(
                ui,
                pal,
                tr("pair.link.start"),
                ButtonKind::Primary,
                !self.state.busy && self.state.status.is_some(),
            )
            .clicked()
            {
                self.start_qr();
            }
            return;
        }
        ui.add(egui::Label::new(theme::caption(pal, tr("pair.link.how"))).wrap());
        ui.add_space(6.0);
        let status = self.link_status();
        match (&self.state.pair_qr, &self.pair.qr_cache) {
            (Some(q), Some((_, modules, width))) => {
                let side = ui.available_width().clamp(160.0, 300.0);
                ui.vertical_centered(|ui| {
                    let r = draw_qr(ui, modules, *width, side);
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Label,
                            true,
                            tr("pair.link.qr_name"),
                        )
                    });
                });
                ui.add_space(6.0);
                self.status_line(ui, pal, status);
                ui.add(
                    egui::Label::new(theme::caption(
                        pal,
                        tr_args(
                            "pair.link.renews",
                            &[("name", &display_text(&q.computer_name))],
                        ),
                    ))
                    .wrap(),
                );
                let cant_scan =
                    egui::CollapsingHeader::new(tr("pair.link.cant_scan")).show(ui, |ui| {
                        let mut text = q.payload.clone();
                        crate::a11y::named(
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .desired_rows(3)
                                    .desired_width(300.0)
                                    .font(egui::TextStyle::Monospace),
                            ),
                            tr("pair.link.link_name"),
                        );
                        if ui.button(tr("pair.link.copy")).clicked() {
                            ui.ctx().copy_text(q.payload.clone());
                        }
                        ui.add(
                            egui::Label::new(theme::caption(pal, tr("pair.link.link_hint"))).wrap(),
                        );
                    });
                theme::ring_when_focused(ui, pal, &cant_scan.header_response);
            }
            _ => {
                if let Some(error) = self.state.notice.as_ref().filter(|_| !self.state.busy) {
                    ui.add(egui::Label::new(theme::secondary(pal, error)).wrap());
                } else {
                    self.status_line(ui, pal, status);
                }
            }
        }
        if let Some(problem) = &self.state.problem {
            ui.add(egui::Label::new(RichText::new(&problem.headline).color(pal.accent)).wrap());
            ui.add(egui::Label::new(theme::caption(pal, &problem.advice)).wrap());
        }
        ui.add_space(6.0);
        if theme::pill_button(ui, pal, tr("pair.link.stop"), ButtonKind::Plain, true).clicked() {
            self.stop_qr();
        }
    }

    fn status_line(&self, ui: &mut egui::Ui, pal: &Palette, status: LinkStatus) {
        let (key, scanned) = match status {
            LinkStatus::Making => ("pair.link.making", false),
            LinkStatus::Waiting => ("pair.link.waiting", false),
            LinkStatus::Scanned => ("pair.link.scanned", true),
        };
        let text = tr(key).to_string();
        let label = if scanned {
            theme::semibold(text.clone(), 15.0)
        } else {
            theme::secondary(pal, text.clone())
        };
        let r = ui.add(egui::Label::new(label).wrap());
        // Screen readers hear the state change.
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_kde::qrpair::QrPayload;

    fn sample() -> String {
        QrPayload {
            device_id: "0123456789abcdef0123456789abcdef".into(),
            name: "Work laptop".into(),
            cert_sha256: "ab".repeat(32),
            addresses: vec![
                "192.168.1.20:1716".parse().unwrap(),
                "10.0.0.5:1717".parse().unwrap(),
            ],
            token: "0123456789abcdef0123456789abcdef".into(),
        }
        .encode()
    }

    #[test]
    fn the_code_on_screen_decodes_back_to_the_payload() {
        let text = sample();
        let (modules, width) = qr_modules(&text).expect("fits in a QR code");
        // Render with the same quiet zone the painter uses, at 6 px per module.
        let (px, quiet) = (6usize, 4usize);
        let side = (width + 2 * quiet) * px;
        let mut img = vec![255u8; side * side];
        for (i, dark) in modules.iter().enumerate() {
            if *dark {
                let (mx, my) = ((i % width + quiet) * px, (i / width + quiet) * px);
                for y in my..my + px {
                    for x in mx..mx + px {
                        img[y * side + x] = 0;
                    }
                }
            }
        }
        let mut prepared =
            rqrr::PreparedImage::prepare_from_greyscale(side, side, |x, y| img[y * side + x]);
        let grids = prepared.detect_grids();
        assert_eq!(grids.len(), 1);
        let (_, decoded) = grids[0].decode().expect("decodes");
        assert_eq!(decoded, text);
        assert!(QrPayload::parse(&decoded).is_ok());
    }

    #[test]
    fn plain_language_problem_for_a_refused_qr() {
        let p = Problem::from_reason("That QR code has expired. Show a new one and scan it again.");
        assert!(p.headline.contains("QR code"));
        assert!(p.advice.contains("new QR code"));
    }
}

#[cfg(test)]
#[path = "pair_ui_tests.rs"]
mod render_tests;
