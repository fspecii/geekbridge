// SPDX-License-Identifier: GPL-3.0-or-later
//! The first-run guide on Home: what a brand-new user sees before any phone is paired.
//!
//! Three steps (install the Android app, pair, choose optional features) plus the things that
//! most often block a first run: a firewall that drops the phone's traffic, a background service
//! that is not running or not started at login. Everything here is explanation and text to copy;
//! no command is ever run (the firewall lines need `sudo`, which only the user may type).
use crate::{
    i18n::{tr, tr_args},
    nothing::{self, S1, S2},
    state::DiscoveryModel,
    theme::{self, ButtonKind, Palette},
};
use bridge_core::firewall::{self, Tool, Verdict};
use eframe::egui::{self, RichText, vec2};
use std::path::{Path, PathBuf};

/// Where the Android app is published (the latest release lists the APK).
pub const RELEASES_URL: &str = "https://github.com/fspecii/geekbridge/releases/latest";

/// Is the background service started at login?
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Autostart {
    On,
    Off,
    /// Not Linux, or not determinable: the row is not shown.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Checking,
    Up,
    Down,
}

/// Which text a "Copy" button puts on the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    Link,
    Firewall,
    Service,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    OpenReleases,
    PairView,
    SettingsView,
    Copy(CopyKind),
}

/// Everything the card shows, gathered by the window (so the card itself is a pure function).
#[derive(Clone, Debug)]
pub struct Facts {
    pub service: Service,
    /// This computer's name as phones see it (only known while the service runs).
    pub computer_name: Option<String>,
    /// Phones announcing themselves on this network, not yet paired.
    pub nearby: Vec<String>,
    pub firewall: Verdict,
    pub autostart: Autostart,
    /// One line about the app's own version/update state, when known.
    pub update: Option<String>,
    /// The button that was just clicked (its label reads "Copied").
    pub copied: Option<CopyKind>,
}

/// Should Home show the guide? Not while the first answer is pending (no flicker), and not once
/// a phone is paired (the Home tiles take over).
pub fn visible(service: Service, paired: usize) -> bool {
    match service {
        Service::Checking => false,
        Service::Down => true,
        Service::Up => paired == 0,
    }
}

pub fn nearby_phones(model: &DiscoveryModel) -> Vec<String> {
    match model {
        DiscoveryModel::Ready { devices, .. } => devices
            .iter()
            .filter(|d| !d.trusted)
            .map(|d| crate::state::display_text(&d.name))
            .collect(),
        _ => Vec::new(),
    }
}

/// The text for a Copy button.
pub fn copy_text(kind: CopyKind, facts: &Facts) -> String {
    match kind {
        CopyKind::Link => RELEASES_URL.to_owned(),
        CopyKind::Firewall => firewall::commands(tool_of(facts.firewall).unwrap_or(Tool::Ufw)),
        CopyKind::Service => service_command().to_owned(),
    }
}

fn tool_of(v: Verdict) -> Option<Tool> {
    match v {
        Verdict::Blocking(t) | Verdict::Unverified(t) => Some(t),
        Verdict::Fine => None,
    }
}

/// How to start the service now and at every login.
pub fn service_command() -> &'static str {
    if cfg!(target_os = "linux") {
        "systemctl --user enable --now geekbridge-service.service" // i18n-ok: a command
    } else {
        "geekbridge-ctl autostart enable" // i18n-ok: a command
    }
}

/// Where there is no systemd user session (minimal installs, the tarball): the launcher that starts the service directly.
pub fn no_systemd_command() -> Option<&'static str> {
    cfg!(target_os = "linux").then_some("geekbridge-service-launch") // i18n-ok: a command
}

/// Reads whether the service starts at login: a systemd user unit enabled for `default.target`, or an
/// XDG autostart entry, in any of `config_dirs` (user config first, then system dirs).
pub fn autostart_in(config_dirs: &[PathBuf], xdg_dirs: &[PathBuf]) -> Autostart {
    let wants = "default.target.wants/geekbridge-service.service";
    let unit = config_dirs.iter().any(|d| exists_link(&d.join(wants)));
    let desktop = xdg_dirs
        .iter()
        .any(|d| d.join("geekbridge-service.desktop").is_file());
    if unit || desktop {
        Autostart::On
    } else {
        Autostart::Off
    }
}

fn exists_link(p: &Path) -> bool {
    p.symlink_metadata().is_ok()
}

/// [`autostart_in`] with this user's real directories (Linux only); `GEEKBRIDGE_AUTOSTART_FIXTURE=on|off`
/// overrides it for screenshots and tests.
pub fn autostart_now() -> Autostart {
    match std::env::var("GEEKBRIDGE_AUTOSTART_FIXTURE").as_deref() {
        Ok("on") => return Autostart::On,
        Ok("off") => return Autostart::Off,
        Ok("unknown") => return Autostart::Unknown,
        _ => {}
    }
    if !cfg!(target_os = "linux") {
        return Autostart::Unknown;
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|h| h.join(".config")));
    let mut cfg_dirs: Vec<PathBuf> = config.iter().map(|c| c.join("systemd/user")).collect();
    cfg_dirs.push(PathBuf::from("/etc/systemd/user"));
    let mut xdg: Vec<PathBuf> = config.iter().map(|c| c.join("autostart")).collect();
    xdg.push(PathBuf::from("/etc/xdg/autostart"));
    autostart_in(&cfg_dirs, &xdg)
}

/// The firewall as this computer reports it. `GEEKBRIDGE_FIREWALL_FIXTURE` (`none`, `ufw-blocking`,
/// `ufw-unverified`, `firewalld`) replaces the real read for screenshots and tests.
pub fn firewall_now() -> Verdict {
    match std::env::var("GEEKBRIDGE_FIREWALL_FIXTURE").as_deref() {
        Ok("none") => return Verdict::Fine,
        Ok("ufw-blocking") => return Verdict::Blocking(Tool::Ufw),
        Ok("ufw-unverified") => return Verdict::Unverified(Tool::Ufw),
        Ok("firewalld") => return Verdict::Unverified(Tool::Firewalld),
        _ => {}
    }
    firewall::verdict(&firewall::read())
}

fn tool_name(t: Tool) -> &'static str {
    match t {
        Tool::Ufw => "ufw",
        Tool::Firewalld => "firewalld",
        Tool::WindowsDefender => "Windows Defender Firewall", // i18n-ok: product name
        Tool::MacAppFirewall => "macOS firewall",             // i18n-ok: product name
    }
}

fn copy_label(kind: CopyKind, copied: Option<CopyKind>) -> &'static str {
    if copied == Some(kind) {
        return tr("onboarding.copied");
    }
    match kind {
        CopyKind::Link => tr("onboarding.copy_link"),
        CopyKind::Firewall => tr("onboarding.copy_firewall"),
        CopyKind::Service => tr("onboarding.copy_service"),
    }
}

fn number(ui: &mut egui::Ui, pal: &Palette, n: u8, done: bool) {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), egui::Sense::hover());
    let fill = if done { pal.accent_fill } else { pal.control };
    ui.painter().circle_filled(rect.center(), 13.0, fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        n.to_string(),
        egui::FontId::new(13.0, theme::semibold_family()),
        if done { pal.on_accent } else { pal.text },
    );
    crate::a11y::describe(
        &resp.ctx,
        resp.id,
        &tr_args("onboarding.step_n", &[("n", &n)]),
        None,
    );
}

fn step<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    n: u8,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.horizontal_top(|ui| {
        number(ui, pal, n, false);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.add(egui::Label::new(theme::semibold(title, 14.5)).wrap());
            body(ui)
        })
        .inner
    })
    .inner
}

fn code_box(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    egui::Frame::new()
        .fill(pal.control)
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(text).monospace().size(12.0).color(pal.text)).wrap(),
            );
        });
}

fn small(ui: &mut egui::Ui, pal: &Palette, text: impl Into<String>) {
    ui.add(egui::Label::new(theme::caption(pal, text)).wrap());
}

fn qr(ui: &mut egui::Ui) {
    if let Some((modules, width)) = crate::connection::qr_modules(RELEASES_URL) {
        let r = crate::connection::draw_qr(ui, &modules, width, 108.0);
        r.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tr("onboarding.qr_name"))
        });
    }
}

/// The guide. Returns what the user asked for; the window carries it out.
pub fn card(ui: &mut egui::Ui, pal: &Palette, f: &Facts) -> Option<Action> {
    let mut act = None;
    nothing::section(ui, pal, tr("onboarding.title"));
    nothing::card(ui, pal, |ui| {
        ui.spacing_mut().item_spacing.y = S2;
        small(ui, pal, tr("onboarding.intro"));
        if f.service == Service::Down {
            ui.add(egui::Label::new(theme::semibold(tr("onboarding.service_off"), 14.5)).wrap());
            small(ui, pal, tr("onboarding.service_off_why"));
            code_box(ui, pal, service_command());
            copy_button(ui, pal, CopyKind::Service, f.copied, &mut act);
            if let Some(alt) = no_systemd_command() {
                small(ui, pal, tr("onboarding.service_no_systemd"));
                code_box(ui, pal, alt);
            }
            ui.add_space(S1);
        }
        // 1. install
        step(ui, pal, 1, tr("onboarding.s1.title"), |ui| {
            small(ui, pal, tr("onboarding.s1.body"));
            // Side by side when there is room; in a narrow window the buttons go under the code.
            let wide = ui.available_width() > 400.0;
            let links = |ui: &mut egui::Ui, act: &mut Option<Action>| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add(
                    egui::Label::new(
                        RichText::new("github.com/fspecii/geekbridge/releases") // i18n-ok
                            .size(12.0)
                            .color(pal.text2),
                    )
                    .wrap(),
                );
                ui.horizontal_wrapped(|ui| {
                    if theme::pill_button(
                        ui,
                        pal,
                        tr("onboarding.open_releases"),
                        ButtonKind::Plain,
                        true,
                    )
                    .clicked()
                    {
                        *act = Some(Action::OpenReleases);
                    }
                    copy_button(ui, pal, CopyKind::Link, f.copied, act);
                });
            };
            if wide {
                ui.horizontal_top(|ui| {
                    qr(ui);
                    ui.vertical(|ui| links(ui, &mut act));
                });
            } else {
                qr(ui);
                links(ui, &mut act);
            }
        });
        ui.add_space(S1);
        // 2. pair
        step(ui, pal, 2, tr("onboarding.s2.title"), |ui| {
            small(ui, pal, tr("onboarding.s2.body"));
            match f.nearby.as_slice() {
                [] => small(ui, pal, tr("onboarding.s2.none_nearby")),
                [one] => small(
                    ui,
                    pal,
                    tr_args("onboarding.s2.nearby_one", &[("name", one)]),
                ),
                many => small(
                    ui,
                    pal,
                    tr_args(
                        "onboarding.s2.nearby_many",
                        &[("name", &many[0]), ("count", &(many.len() - 1))],
                    ),
                ),
            }
            if theme::pill_button(ui, pal, tr("onboarding.pair"), ButtonKind::Primary, true)
                .clicked()
            {
                act = Some(Action::PairView);
            }
            firewall_note(ui, pal, f, &mut act);
        });
        ui.add_space(S1);
        // 3. optional features
        step(ui, pal, 3, tr("onboarding.s3.title"), |ui| {
            small(ui, pal, tr("onboarding.s3.body"));
        });
        ui.add_space(S2);
        theme::hairline(ui, pal);
        facts_rows(ui, pal, f, &mut act);
    });
    act
}

fn copy_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    kind: CopyKind,
    copied: Option<CopyKind>,
    act: &mut Option<Action>,
) {
    if theme::pill_button(ui, pal, copy_label(kind, copied), ButtonKind::Plain, true).clicked() {
        *act = Some(Action::Copy(kind));
    }
}

fn firewall_note(ui: &mut egui::Ui, pal: &Palette, f: &Facts, act: &mut Option<Action>) {
    let Some(tool) = tool_of(f.firewall) else {
        return;
    };
    ui.add_space(S1);
    let key = if matches!(f.firewall, Verdict::Blocking(_)) {
        "onboarding.fw.blocking"
    } else {
        "onboarding.fw.unverified"
    };
    ui.add(
        egui::Label::new(theme::semibold(
            tr_args(key, &[("tool", &tool_name(tool))]),
            13.5,
        ))
        .wrap(),
    );
    small(
        ui,
        pal,
        tr(if matches!(f.firewall, Verdict::Blocking(_)) {
            "onboarding.fw.why_blocking"
        } else {
            "onboarding.fw.why_unverified"
        }),
    );
    code_box(ui, pal, &firewall::commands(tool));
    small(ui, pal, tr("onboarding.fw.never_runs"));
    copy_button(ui, pal, CopyKind::Firewall, f.copied, act);
}

fn facts_rows(ui: &mut egui::Ui, pal: &Palette, f: &Facts, act: &mut Option<Action>) {
    if let Some(name) = &f.computer_name {
        ui.horizontal_wrapped(|ui| {
            ui.label(theme::overline(pal, tr("onboarding.name")));
            ui.add(egui::Label::new(theme::semibold(name, 13.5)).truncate());
            if theme::pill_button(ui, pal, tr("onboarding.rename"), ButtonKind::Plain, true)
                .clicked()
            {
                *act = Some(Action::SettingsView);
            }
        });
        small(ui, pal, tr("onboarding.name_why"));
    }
    if let Some(line) = &f.update {
        ui.label(theme::overline(pal, tr("update.section")));
        small(ui, pal, line);
    }
    match f.autostart {
        Autostart::On => {
            ui.label(theme::overline(pal, tr("onboarding.autostart")));
            small(ui, pal, tr("onboarding.autostart_on"));
        }
        Autostart::Off if f.service != Service::Down => {
            ui.label(theme::overline(pal, tr("onboarding.autostart")));
            small(ui, pal, tr("onboarding.autostart_off"));
            code_box(ui, pal, service_command());
            copy_button(ui, pal, CopyKind::Service, f.copied, act);
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "onboarding_tests.rs"]
mod tests;
