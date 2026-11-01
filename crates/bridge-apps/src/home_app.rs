//! The single entry window (`geekbridge`). It owns no protocol logic: pairing is the shared
//! `Connections` panel, clipboard sync is the Clipboard app's `sync_section`, updates are
//! `Updates`, and sending files hands them to `geekbridge-share`.
use crate::{
    apps::clipboard::sync_body,
    cli::ThemeChoice,
    clipboard_link::ClipLink,
    connection::{Connections, NextStep},
    identity,
    poller::Poller,
    state::DiscoveryModel,
    theme::{self, ButtonKind, Palette},
    update_ui::Updates,
};
use bridge_core::AppKind;
use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, ThemePreference};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The palette family used for this window (the neutral system-blue one).
const LOOK: AppKind = AppKind::Clipboard;

const APPS: [(AppKind, &str); 5] = [
    (AppKind::Calls, "Calls"),
    (AppKind::Messages, "Messages"),
    (AppKind::Contacts, "Contacts"),
    (AppKind::Agenda, "Calendar"),
    (AppKind::Clipboard, "Clipboard"),
];

/// Path of a sibling executable (same directory as this one), falling back to a `PATH` lookup.
pub fn sibling(name: &str, exe: Option<&Path>) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    exe.and_then(Path::parent)
        .map(|dir| dir.join(&file))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(file))
}

fn launch(name: &str, args: &[String]) -> Result<(), String> {
    let exe = std::env::current_exe().ok();
    Command::new(sibling(name, exe.as_deref()))
        .args(args)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {name}: {e}"))
}

/// Local, dropped file paths only (a drop of text or a web URL has no path and is ignored).
pub fn dropped_paths(files: &[egui::DroppedFile]) -> Vec<String> {
    files
        .iter()
        .filter_map(|f| f.path.as_ref())
        .filter(|p| p.is_file())
        .filter_map(|p| p.to_str().map(str::to_owned))
        .collect()
}

pub struct HomeApp {
    api_port: u16,
    model: DiscoveryModel,
    poller: Poller,
    connections: Connections,
    updates: Updates,
    clip: ClipLink,
    hotkey_draft: Option<String>,
    notice: Option<String>,
    diagnostics: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
}

impl HomeApp {
    pub fn new(api_port: u16, ctx: &egui::Context) -> Self {
        let repaint = ctx.clone();
        let poller = Poller::spawn(LOOK, api_port, move || repaint.request_repaint());
        let repaint = ctx.clone();
        Self {
            api_port,
            model: DiscoveryModel::Checking,
            poller,
            connections: Connections::new(ctx.clone()),
            updates: Updates::new(ctx.clone()),
            clip: ClipLink::spawn(move || repaint.request_repaint()),
            hotkey_draft: None,
            notice: None,
            diagnostics: None,
        }
    }

    /// Run `geekbridge-ctl diagnose` off the UI thread; the redacted report lands on the clipboard.
    fn collect_diagnostics(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        let port = self.api_port.to_string();
        std::thread::spawn(move || {
            let exe = std::env::current_exe().ok();
            let out = Command::new(sibling("geekbridge-ctl", exe.as_deref()))
                .args(["--control-port", &port, "diagnose"])
                .output();
            let _ = tx.send(match out {
                Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                Err(e) => Err(e.to_string()),
            });
            wake.request_repaint();
        });
        self.diagnostics = Some(rx);
    }

    fn section(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        ui.label(theme::overline(pal, "Pair a phone"));
        theme::group(ui, pal, |ui| {
            match self.connections.pair_card(ui, pal, &self.model) {
                Some(NextStep::Clipboard) => {
                    self.notice = launch(identity::binary_name(AppKind::Clipboard), &[]).err();
                }
                Some(NextStep::Share) => self.notice = launch("geekbridge-share", &[]).err(),
                None => {}
            }
        });

        ui.add_space(10.0);
        ui.label(theme::overline(pal, "Send files to phone"));
        theme::group(ui, pal, |ui| {
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    "Drop files anywhere on this window to send them to your phone. Files your phone sends back wait for your OK in Share.",
                ))
                .wrap(),
            );
            if theme::pill_button(ui, pal, "Open Share…", ButtonKind::Plain, true).clicked() {
                self.notice = launch("geekbridge-share", &[]).err();
            }
        });

        ui.add_space(10.0);
        ui.label(theme::overline(pal, "Apps"));
        theme::group(ui, pal, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (kind, label) in APPS {
                    if theme::pill_button(ui, pal, label, ButtonKind::Plain, true).clicked() {
                        self.notice = launch(identity::binary_name(kind), &[]).err();
                    }
                }
            });
        });

        ui.add_space(10.0);
        ui.label(theme::overline(pal, "Clipboard"));
        egui::CollapsingHeader::new(theme::semibold(
            "Phone clipboard sync, shortcut and last item",
            13.0,
        ))
        .id_salt("clip-sync")
        .default_open(false)
        .show(ui, |ui| {
            sync_body(ui, pal, &mut self.clip, &mut self.hotkey_draft)
        });

        ui.add_space(10.0);
        ui.label(theme::overline(pal, "Settings"));
        theme::group(ui, pal, |ui| {
            self.updates.details(ui, pal);
            ui.add_space(6.0);
            ui.label(theme::overline(pal, "Help"));
            ui.horizontal_wrapped(|ui| {
                let busy = self.diagnostics.is_some();
                let label = if busy {
                    "Collecting…"
                } else {
                    "Copy diagnostics"
                };
                if theme::pill_button(ui, pal, label, ButtonKind::Plain, !busy).clicked() {
                    self.collect_diagnostics(&ctx);
                }
            });
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    "Copies a support report with no names, addresses, keys or content, ready to paste into a bug report.",
                ))
                .wrap(),
            );
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    format!(
                        "Background service: {}",
                        if self.model.service_reachable() {
                            "running"
                        } else {
                            "not running (it starts at login; see the user guide)"
                        }
                    ),
                ))
                .wrap(),
            );
        });
        if let Some(n) = &self.notice {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(n).color(pal.text2)).wrap());
        }
    }
}

impl eframe::App for HomeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(model) = self.poller.latest() {
            self.model = model;
        }
        crate::pace::observe(ctx, &self.model);
        self.connections.update();
        self.clip.update();
        if let Some(rx) = &self.diagnostics
            && let Ok(result) = rx.try_recv()
        {
            self.diagnostics = None;
            self.notice = Some(match result {
                Ok(report) => {
                    ctx.copy_text(report);
                    "Diagnostics copied. Paste them where you ask for help.".to_owned()
                }
                Err(e) => format!("Could not collect diagnostics: {e}"),
            });
        }
        // A hidden window does no periodic work at all: it wakes only when a worker reports news.
        if !ctx.input(|i| i.viewport().minimized.unwrap_or(false)) {
            ctx.request_repaint_after(crate::pace::scaled(std::time::Duration::from_secs(1)));
        }
        let pal = theme::current(ctx, LOOK);

        let files = ctx.input(|i| dropped_paths(&i.raw.dropped_files));
        if !files.is_empty() {
            self.notice = launch("geekbridge-share", &files).err();
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        self.updates.banner(ctx, &pal);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal.content)
                    .inner_margin(Margin::symmetric(20, 16))
                    .stroke(if hovering {
                        Stroke::new(2.0_f32, pal.accent)
                    } else {
                        Stroke::NONE
                    }),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - 12.0);
                        ui.label(theme::semibold("GeekBridge", 20.0));
                        ui.label(theme::caption(
                            &pal,
                            format!("Service on port {}", self.api_port),
                        ));
                        ui.add_space(8.0);
                        self.section(ui, &pal);
                    });
            });
        self.connections.dialogs(ctx, &pal);
    }
}

pub fn run(api_port: u16, theme_choice: ThemeChoice) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GeekBridge")
            .with_app_id("dev.geekbridge.GeekBridge")
            .with_inner_size([520.0, 760.0])
            .with_min_inner_size([400.0, 480.0])
            .with_drag_and_drop(true)
            .with_decorations(true),
        ..Default::default()
    };
    eframe::run_native(
        "GeekBridge",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, LOOK);
            cc.egui_ctx.set_theme(match theme_choice {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Light => ThemePreference::Light,
                ThemeChoice::Dark => ThemePreference::Dark,
            });
            Ok(Box::new(HomeApp::new(api_port, &cc.egui_ctx)))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_falls_back_to_bare_name() {
        let p = sibling(
            "geekbridge-nonexistent",
            Some(Path::new("/nonexistent/bin/x")),
        );
        assert_eq!(p, PathBuf::from("geekbridge-nonexistent"));
    }

    #[test]
    fn sibling_prefers_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "tool.exe" } else { "tool" };
        std::fs::write(dir.path().join(name), b"").unwrap();
        let exe = dir.path().join("geekbridge");
        assert_eq!(sibling("tool", Some(&exe)), dir.path().join(name));
    }

    #[test]
    fn only_existing_local_files_are_dropped_through() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"x").unwrap();
        let mk = |path: Option<PathBuf>| egui::DroppedFile {
            path,
            ..Default::default()
        };
        let got = dropped_paths(&[
            mk(Some(f.clone())),
            mk(None),
            mk(Some(dir.path().to_path_buf())),
            mk(Some(dir.path().join("missing"))),
        ]);
        assert_eq!(got, vec![f.to_str().unwrap().to_owned()]);
    }
}
