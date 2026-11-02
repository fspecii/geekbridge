//! First-time pairing on the computer: the "Pair a phone" card, the QR window, one-tap Pair, the
//! plain-language problem line, "Test connection" and the "what to do next" card.
//!
//! A child module of `connection` so it can use `Connections`' private state; it owns no protocol
//! logic. Every decision still goes to the service as an explicit request.

use super::*;
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

fn draw_qr(ui: &mut egui::Ui, modules: &[bool], width: usize, side: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
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
        if self.pair.qr_open {
            let renew = match (&self.state.pair_qr, self.pair.qr_shown_at) {
                (Some(q), Some(at)) => {
                    at.elapsed() + RENEW_BEFORE >= Duration::from_millis(q.expires_in_ms)
                }
                _ => false,
            };
            if renew && !self.state.busy {
                self.pair.qr_shown_at = None;
                self.submit(Request::PairQr);
            }
            ctx.request_repaint_after(Duration::from_secs(1));
        }
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
                ui.label(theme::semibold(format!("✔ Paired with {name}"), 15.0));
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
        ui.add(
            egui::Label::new(theme::caption(
                pal,
                "Open GeekBridge on your phone. It appears below by name: tap Pair and confirm the same code on both screens. Or scan a QR code, which needs no code at all.",
            ))
            .wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            if theme::pill_button(
                ui,
                pal,
                "Pair with QR code",
                ButtonKind::Primary,
                !self.state.busy && self.state.status.is_some(),
            )
            .clicked()
            {
                self.pair.qr_open = true;
                self.pair.qr_shown_at = None;
                self.submit(Request::PairQr);
            }
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

    /// The QR window.
    pub(super) fn qr_window(&mut self, ctx: &egui::Context, pal: &Palette) {
        if !self.pair.qr_open {
            return;
        }
        let mut close = theme::escape_pressed(ctx);
        if let Some(q) = self.state.pair_qr.clone() {
            self.pair.qr_shown_at.get_or_insert_with(Instant::now);
            if self.pair.qr_cache.as_ref().map(|c| c.0.as_str()) != Some(q.payload.as_str()) {
                self.pair.qr_cache = qr_modules(&q.payload).map(|(m, w)| (q.payload.clone(), m, w));
            }
        }
        egui::Window::new("Pair with QR code")
            .id(egui::Id::new("pair-qr"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(320.0);
                ui.add(
                    egui::Label::new(theme::semibold(
                        "On your phone: open GeekBridge, tap \"Scan QR code\" and point the camera here.",
                        14.0,
                    ))
                    .wrap(),
                );
                ui.add_space(6.0);
                match (&self.state.pair_qr, &self.pair.qr_cache) {
                    (Some(q), Some((_, modules, width))) => {
                        ui.vertical_centered(|ui| draw_qr(ui, modules, *width, 260.0));
                        let left = Duration::from_millis(q.expires_in_ms)
                            .saturating_sub(
                                self.pair.qr_shown_at.map(|t| t.elapsed()).unwrap_or_default(),
                            )
                            .as_secs();
                        ui.add(
                            egui::Label::new(theme::caption(
                                pal,
                                format!(
                                    "{} · one use, refreshes in {left} s. No code to compare: your phone checks this computer's identity from the picture and still asks before it pairs.",
                                    display_text(&q.computer_name)
                                ),
                            ))
                            .wrap(),
                        );
                        egui::CollapsingHeader::new("Can't scan? Use the link").show(ui, |ui| {
                            let mut text = q.payload.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .desired_rows(3)
                                    .desired_width(300.0)
                                    .font(egui::TextStyle::Monospace),
                            );
                            if ui.button("Copy link").clicked() {
                                ui.ctx().copy_text(q.payload.clone());
                            }
                            ui.add(
                                egui::Label::new(theme::caption(
                                    pal,
                                    "Send this link to your phone yourself (not over an untrusted chat) and paste it into \"Scan QR code → Enter link\".",
                                ))
                                .wrap(),
                            );
                        });
                    }
                    _ => {
                        if let Some(error) = self.state.notice.as_ref().filter(|_| !self.state.busy) {
                            ui.add(egui::Label::new(theme::secondary(pal, error)).wrap());
                        } else {
                            ui.label(theme::caption(pal, "Making a one-time code…"));
                        }
                    }
                }
                if let Some(problem) = &self.state.problem {
                    ui.add(
                        egui::Label::new(RichText::new(&problem.headline).color(pal.accent)).wrap(),
                    );
                    ui.add(egui::Label::new(theme::caption(pal, &problem.advice)).wrap());
                }
                ui.add_space(6.0);
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        if close {
            self.pair.qr_open = false;
            self.submit(Request::PairQrCancel);
        }
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
