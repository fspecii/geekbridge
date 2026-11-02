// SPDX-License-Identifier: GPL-3.0-or-later
//! The single entry window (`geekbridge`). It owns no protocol logic: pairing is the shared
//! `Connections` panel, clipboard sync is the Clipboard app's `sync_section`, updates are
//! `Updates`, and sending files hands them to `geekbridge-share`.
use crate::a11y::ScrollBars as _;
use crate::{
    apps::clipboard::sync_body,
    call_feed::CallFeed,
    cli::ThemeChoice,
    clipboard_link::{ClipLink, pull_line},
    connection::{Connections, NextStep},
    icons::{self, Icon},
    identity, localtime,
    nothing::{self, PITCH_LABEL, PITCH_TITLE, S1, S2, S3, S4, S5},
    phone_feed::{self, Feed},
    poller::Poller,
    state::DiscoveryModel,
    theme::{self, ButtonKind, Palette},
    update_ui::Updates,
};
use bridge_core::{
    AppKind,
    ipc::Request,
    phone_view::{AgendaView, MessagesView},
};
use eframe::egui::{
    self, Frame, Key, Margin, Modifiers, RichText, ScrollArea, Stroke, ThemePreference, vec2,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// The palette family used for this window (the neutral system-blue one).
const LOOK: AppKind = AppKind::Clipboard;

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

/// Windows: do not let a console program started by a GUI-subsystem app open a console window.
/// No-op elsewhere.
pub(crate) fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
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

/// What the content area shows. The apps themselves open in their own windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Home,
    Pair,
    Settings,
}

const SIDEBAR_W: f32 = 236.0;
const GAP: f32 = 12.0;
const TILE_MIN_W: f32 = 200.0;
const TILE_H: f32 = 168.0;

pub struct HomeApp {
    api_port: u16,
    model: DiscoveryModel,
    poller: Poller,
    connections: Connections,
    updates: Updates,
    clip: ClipLink,
    view: View,
    hotkey_draft: Option<String>,
    notice: Option<String>,
    diagnostics: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    calls: Option<CallFeed>,
    agenda: Option<Feed<AgendaView>>,
    agenda_day: String,
    messages: Option<Feed<MessagesView>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// One short line each for the home tiles: what is true right now, in plain words.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PhoneLine {
    dots: &'static str,
    name: String,
    detail: String,
    linked: bool,
    alert: bool,
}

fn phone_line(state: &crate::connection::ControlState) -> PhoneLine {
    let Some(status) = &state.status else {
        return PhoneLine {
            dots: "...",
            name: "Looking for the service".into(),
            detail: "Checking the background service".into(),
            linked: false,
            alert: false,
        };
    };
    let linked = state.connected();
    let name = status
        .links
        .iter()
        .find(|l| l.phase == bridge_core::ipc::LinkPhase::Connected)
        .map(|l| l.name.clone())
        .or_else(|| status.paired.first().map(|p| p.name.clone()));
    if linked {
        PhoneLine {
            dots: "LINKED",
            name: crate::state::display_text(&name.unwrap_or_else(|| "Phone".into())),
            detail: "Connected over your network".into(),
            linked,
            alert: false,
        }
    } else if !status.challenges.is_empty() {
        PhoneLine {
            dots: "COMPARE",
            name: "Pairing code".into(),
            detail: "Check it matches your phone".into(),
            linked,
            alert: true,
        }
    } else if let Some(name) = name {
        PhoneLine {
            dots: "OFFLINE",
            name: crate::state::display_text(&name),
            detail: "Paired, not reachable right now".into(),
            linked,
            alert: false,
        }
    } else {
        PhoneLine {
            dots: "NO PHONE",
            name: "No phone paired".into(),
            detail: "Click to pair a phone".into(),
            linked,
            alert: false,
        }
    }
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
            view: View::Home,
            hotkey_draft: None,
            notice: None,
            diagnostics: None,
            calls: None,
            agenda: None,
            agenda_day: String::new(),
            messages: None,
        }
    }

    /// Run `geekbridge-ctl diagnose` off the UI thread; the redacted report lands on the clipboard.
    fn collect_diagnostics(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        let port = self.api_port.to_string();
        std::thread::spawn(move || {
            let exe = std::env::current_exe().ok();
            let out = no_window(&mut Command::new(sibling("geekbridge-ctl", exe.as_deref())))
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

    /// Start the live feeds behind the tiles only once a phone is paired, and keep the agenda
    /// request on today. Nothing is polled for a fresh install.
    fn ensure_feeds(&mut self, ctx: &egui::Context) {
        let paired = self
            .connections
            .state
            .status
            .as_ref()
            .is_some_and(|s| !s.paired.is_empty());
        if !paired {
            return;
        }
        if self.calls.is_none() {
            let wake = ctx.clone();
            self.calls = Some(CallFeed::spawn(move || wake.request_repaint()));
        }
        let today = localtime::now().date;
        let day = crate::calendar_model::fmt_date(today);
        let request = Request::AgendaView {
            query: String::new(),
            day: day.clone(),
            utc_offset_minutes: localtime::utc_offset_minutes(today),
            days: 2,
            people_limit: 1,
        };
        match &self.agenda {
            None => {
                let wake = ctx.clone();
                self.agenda = Some(phone_feed::agenda_feed(
                    move || wake.request_repaint(),
                    request,
                ));
                self.agenda_day = day;
            }
            Some(feed) if self.agenda_day != day => {
                feed.set_params(request);
                self.agenda_day = day;
            }
            Some(_) => {}
        }
        if self.messages.is_none() {
            let wake = ctx.clone();
            self.messages = Some(phone_feed::messages_feed(
                move || wake.request_repaint(),
                Request::MessagesView {
                    device_id: None,
                    thread_id: None,
                    query: None,
                    message_limit: 0,
                },
            ));
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let pick =
            |ctx: &egui::Context, key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, key));
        if pick(ctx, Key::Num1) {
            self.view = View::Home;
        } else if pick(ctx, Key::Num2) {
            self.view = View::Pair;
        } else if pick(ctx, Key::Num3) || pick(ctx, Key::Comma) {
            self.view = View::Settings;
        }
        if pick(ctx, Key::Num9) {
            self.notice = launch("geekbridge-share", &[]).err();
        }
        for (key, app) in [
            (Key::Num4, AppKind::Calls),
            (Key::Num5, AppKind::Messages),
            (Key::Num6, AppKind::Contacts),
            (Key::Num7, AppKind::Agenda),
            (Key::Num8, AppKind::Clipboard),
        ] {
            if pick(ctx, key) {
                self.notice = launch(identity::binary_name(app), &[]).err();
            }
        }
    }

    // ------------------------------------------------------------ sidebar

    fn sidebar(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::SidePanel::left("sidebar")
            .exact_width(SIDEBAR_W)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(pal.sidebar)
                    .inner_margin(Margin::symmetric(S3 as i8, S4 as i8)),
            )
            .show(ctx, |ui| {
                // Hairline against the content, like a translucent source list edge.
                let r = ui.max_rect();
                ui.painter().vline(
                    r.right() + S3,
                    r.y_range().expand(S4),
                    Stroke::new(1.0_f32, pal.separator),
                );
                ui.spacing_mut().item_spacing.y = 2.0;
                nothing::dot_text(ui, pal, "GEEKBRIDGE", PITCH_LABEL, pal.text, false);
                ui.add_space(S2);
                self.device_card(ui, pal);
                ui.add_space(S3);
                for (view, icon, label, key) in [
                    (View::Home, Icon::Home, "Home", "⌘1"),
                    (View::Pair, Icon::Phone, "Pair a phone", "⌘2"),
                    (View::Settings, Icon::Sliders, "Settings", "⌘3"),
                ] {
                    let r = theme::sidebar_item(
                        ui,
                        pal,
                        icon,
                        label,
                        Some(&key.replace(
                            '⌘',
                            if cfg!(target_os = "macos") {
                                "⌘"
                            } else {
                                "Ctrl+"
                            },
                        )),
                        self.view == view,
                        None,
                    );
                    if r.clicked() {
                        self.view = view;
                    }
                }
                ui.add_space(S3);
                ui.label(theme::overline(pal, "Open"));
                ui.add_space(S1);
                let mod_key = if cfg!(target_os = "macos") {
                    "⌘"
                } else {
                    "Ctrl+"
                };
                for (kind, icon, label, n) in [
                    (AppKind::Calls, Icon::Phone, "Calls", 4),
                    (AppKind::Messages, Icon::Chat, "Messages", 5),
                    (AppKind::Contacts, Icon::Person, "Contacts", 6),
                    (AppKind::Agenda, Icon::Calendar, "Calendar", 7),
                    (AppKind::Clipboard, Icon::Clipboard, "Clipboard", 8),
                ] {
                    let hint = format!("{mod_key}{n}");
                    if theme::sidebar_item(ui, pal, icon, label, Some(&hint), false, None).clicked()
                    {
                        self.notice = launch(identity::binary_name(kind), &[]).err();
                    }
                }
                let hint = format!("{mod_key}9");
                if theme::sidebar_item(ui, pal, Icon::Send, "Share files", Some(&hint), false, None)
                    .clicked()
                {
                    self.notice = launch("geekbridge-share", &[]).err();
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.add_space(S1);
                    nothing::status_dot(
                        ui,
                        pal,
                        self.model.service_reachable(),
                        false,
                        if self.model.service_reachable() {
                            "Background service running"
                        } else {
                            "Background service not running"
                        },
                    );
                });
            });
    }

    /// The phone at a glance: name, link state as a dotted row, and what to do when unlinked.
    fn device_card(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let line = phone_line(&self.connections.state);
        let card = Frame::new()
            .fill(pal.tile)
            .stroke(if pal.dark {
                Stroke::NONE
            } else {
                Stroke::new(1.0_f32, pal.separator)
            })
            .corner_radius(egui::CornerRadius::same(nothing::R_CARD))
            .inner_margin(Margin::same(S3 as i8));
        card.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 17.0, pal.control);
                icons::paint(
                    ui.painter(),
                    egui::Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
                    Icon::Phone,
                    pal.text,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.add(egui::Label::new(theme::semibold(&line.name, 13.5)).truncate());
                    ui.add(egui::Label::new(theme::caption(pal, &line.detail)).truncate());
                });
            });
            ui.add_space(S2);
            let on = if line.linked { pal.text } else { pal.text3 };
            let lit = if line.linked { 12 } else { 0 };
            nothing::dot_progress(ui, pal, 12, lit, on, ui.available_width().min(190.0));
        });
    }

    // ------------------------------------------------------------ tiles

    fn tile_body(
        ui: &mut egui::Ui,
        pal: &Palette,
        icon: Icon,
        dots: &str,
        alert: bool,
        title: &str,
        sub: &str,
    ) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 17.0, pal.control);
            icons::paint(
                ui.painter(),
                egui::Rect::from_center_size(rect.center(), vec2(16.0, 16.0)),
                icon,
                if alert { pal.accent } else { pal.text },
            );
            if alert {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, pal.accent_fill);
                });
            }
        });
        ui.add_space(S2);
        let width = ui.available_width();
        let color = if alert { pal.accent } else { pal.text };
        nothing::dot_text_fit(ui, pal, &[dots], PITCH_TITLE * 1.15, width, color, false);
        ui.add_space(S1);
        ui.add(egui::Label::new(theme::semibold(title, 15.0)).truncate());
        ui.add(egui::Label::new(theme::caption(pal, sub)).truncate());
    }

    fn tile_grid(&mut self, ui: &mut egui::Ui, pal: &Palette, hovering: bool) {
        let width = ui.available_width();
        let cols = nothing::columns_for(width, TILE_MIN_W, GAP);
        let w = nothing::tile_width(width, cols, GAP);
        let size = vec2(w, TILE_H);

        let phone = phone_line(&self.connections.state);
        let ringing = self
            .calls
            .as_ref()
            .and_then(|c| c.state().status)
            .map(|s| s.view)
            .and_then(|v| {
                v.ringing()
                    .map(|r| (true, r.caller_label()))
                    .or_else(|| v.live().map(|r| (false, r.caller_label())))
            });
        let unread: Option<u32> = self
            .messages
            .as_ref()
            .and_then(|f| f.state().view)
            .map(|v| v.conversations.iter().map(|c| c.unread).sum());
        let clip_line = self
            .clip
            .state
            .status
            .as_ref()
            .and_then(|s| s.last_pull.as_ref())
            .map(pull_line);
        let events = self.next_events();

        #[derive(Clone, Copy)]
        enum T {
            Phone,
            Calls,
            Messages,
            Clipboard,
            Share,
            Calendar,
        }
        let tiles = [
            T::Phone,
            T::Calls,
            T::Messages,
            T::Clipboard,
            T::Share,
            T::Calendar,
        ];
        for row in tiles.chunks(cols) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(GAP, GAP);
                for t in row {
                    let clicked = match t {
                        T::Phone => nothing::tile(ui, pal, "Phone link", size, true, |ui| {
                            Self::tile_body(
                                ui,
                                pal,
                                Icon::Phone,
                                phone.dots,
                                phone.alert,
                                &phone.name,
                                &phone.detail,
                            );
                        })
                        .clicked()
                        .then_some(None),
                        T::Calls => {
                            let (dots, alert, title, sub) = match &ringing {
                                Some((true, who)) => {
                                    ("RINGING", true, who.clone(), "Incoming call".to_owned())
                                }
                                Some((false, who)) => {
                                    ("IN CALL", false, who.clone(), "Call in progress".to_owned())
                                }
                                None if phone.linked => (
                                    "IDLE",
                                    false,
                                    "Calls".to_owned(),
                                    "No call right now".to_owned(),
                                ),
                                None => (
                                    "---",
                                    false,
                                    "Calls".to_owned(),
                                    "Needs a linked phone".to_owned(),
                                ),
                            };
                            nothing::tile(ui, pal, "Calls", size, true, |ui| {
                                Self::tile_body(ui, pal, Icon::Phone, dots, alert, &title, &sub);
                            })
                            .clicked()
                            .then_some(Some(AppKind::Calls))
                        }
                        T::Messages => {
                            let (dots, alert, sub) = match unread {
                                Some(0) => ("0".to_owned(), false, "No unread messages"),
                                Some(n) => (n.min(999).to_string(), true, "Unread conversations"),
                                None => ("---".to_owned(), false, "Needs a linked phone"),
                            };
                            nothing::tile(ui, pal, "Messages", size, true, |ui| {
                                Self::tile_body(ui, pal, Icon::Chat, &dots, alert, "Messages", sub);
                            })
                            .clicked()
                            .then_some(Some(AppKind::Messages))
                        }
                        T::Clipboard => {
                            let sub = clip_line
                                .clone()
                                .unwrap_or_else(|| "Nothing received yet".into());
                            nothing::tile(ui, pal, "Clipboard", size, true, |ui| {
                                Self::tile_body(
                                    ui,
                                    pal,
                                    Icon::Clipboard,
                                    "CLIP",
                                    false,
                                    "Clipboard",
                                    &sub,
                                );
                            })
                            .clicked()
                            .then_some(Some(AppKind::Clipboard))
                        }
                        T::Share => nothing::tile(ui, pal, "Send files", size, true, |ui| {
                            let (dots, alert) = if hovering {
                                ("DROP", true)
                            } else {
                                ("SEND", false)
                            };
                            Self::tile_body(
                                ui,
                                pal,
                                Icon::Send,
                                dots,
                                alert,
                                "Send files",
                                "Drop files anywhere here",
                            );
                        })
                        .clicked()
                        .then_some(Some(AppKind::Clipboard)),
                        T::Calendar => nothing::tile(ui, pal, "Calendar", size, true, |ui| {
                            Self::calendar_tile(ui, pal, &events);
                        })
                        .clicked()
                        .then_some(Some(AppKind::Agenda)),
                    };
                    match (t, clicked) {
                        (T::Phone, Some(_)) => self.view = View::Pair,
                        (T::Share, Some(_)) => self.notice = launch("geekbridge-share", &[]).err(),
                        (_, Some(Some(app))) => {
                            self.notice = launch(identity::binary_name(app), &[]).err();
                        }
                        _ => {}
                    }
                }
            });
            ui.add_space(GAP - ui.spacing().item_spacing.y.min(GAP));
        }
    }

    /// Up to two upcoming events: (time label, title).
    fn next_events(&self) -> Option<Vec<(String, String)>> {
        let view = self.agenda.as_ref()?.state().view?;
        let now = now_ms();
        let mut items: Vec<_> = view.events.iter().filter(|e| e.end_ms >= now).collect();
        items.sort_by_key(|e| e.start_ms);
        Some(
            items
                .into_iter()
                .take(2)
                .map(|e| {
                    let when = if e.all_day {
                        "All day".to_owned()
                    } else {
                        localtime::clock_label_local(e.start_ms)
                    };
                    (when, crate::state::display_text(&e.title))
                })
                .collect(),
        )
    }

    fn calendar_tile(ui: &mut egui::Ui, pal: &Palette, events: &Option<Vec<(String, String)>>) {
        let today = localtime::now().date;
        ui.horizontal(|ui| {
            nothing::dot_text(
                ui,
                pal,
                &format!("{:02}", today.day),
                PITCH_TITLE,
                pal.text,
                false,
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(today.month_name())
                        .size(13.0)
                        .color(pal.accent),
                );
            });
        });
        ui.add_space(S2);
        ui.add(egui::Label::new(theme::semibold("Calendar", 15.0)).truncate());
        match events {
            None => {
                ui.add(egui::Label::new(theme::caption(pal, "Needs a linked phone")).truncate());
            }
            Some(list) if list.is_empty() => {
                ui.add(egui::Label::new(theme::caption(pal, "Nothing coming up")).truncate());
            }
            Some(list) => {
                for (i, (when, title)) in list.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = S2;
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 12.0), egui::Sense::hover());
                        let c = if i == 0 { pal.accent_fill } else { pal.text3 };
                        ui.painter().circle_filled(r.center(), 3.0, c);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{title}  {when}"))
                                    .size(12.5)
                                    .color(pal.text2),
                            )
                            .truncate(),
                        );
                    });
                }
            }
        }
    }

    // ------------------------------------------------------------ views

    fn header(&self, ui: &mut egui::Ui, pal: &Palette, title: &str) {
        ui.horizontal(|ui| {
            nothing::dot_text(ui, pal, title, PITCH_TITLE, pal.text, false);
            let today = localtime::now().date;
            // The date yields to the title when the window (or a large text scale) leaves no room.
            if ui.available_width() > 130.0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{} {}", today.day, today.month_name()))
                            .size(13.0)
                            .color(pal.accent),
                    );
                });
            }
        });
        ui.add_space(S3);
    }

    fn pair_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        self.header(ui, pal, "PAIR");
        nothing::card(ui, pal, |ui| {
            match self.connections.pair_card(ui, pal, &self.model) {
                Some(NextStep::Clipboard) => {
                    self.notice = launch(identity::binary_name(AppKind::Clipboard), &[]).err();
                }
                Some(NextStep::Share) => self.notice = launch("geekbridge-share", &[]).err(),
                None => {}
            }
        });
    }

    fn settings_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let ctx = ui.ctx().clone();
        self.header(ui, pal, "SETTINGS");
        nothing::section(ui, pal, "This computer");
        nothing::card(ui, pal, |ui| self.connections.name_row(ui, pal));
        nothing::section(ui, pal, "Updates");
        nothing::card(ui, pal, |ui| self.updates.details(ui, pal));
        nothing::section(ui, pal, "Clipboard");
        nothing::card(ui, pal, |ui| {
            sync_body(ui, pal, &mut self.clip, &mut self.hotkey_draft)
        });
        nothing::section(ui, pal, "Diagnostics");
        nothing::card(ui, pal, |ui| {
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
            ui.add_space(S1);
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    format!(
                        "Background service on port {}: {}",
                        self.api_port,
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
    }
}

impl eframe::App for HomeApp {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        crate::memtrim::cap_atlas(raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame(ctx);
    }
}

impl HomeApp {
    /// One frame of the window. Separate from `eframe::App::update` so tests can drive it.
    pub fn frame(&mut self, ctx: &egui::Context) {
        crate::a11y::window_title(ctx, "GeekBridge");
        if let Some(model) = self.poller.latest() {
            self.model = model;
        }
        crate::pace::observe(ctx, &self.model);
        self.connections.update();
        self.clip.update();
        self.ensure_feeds(ctx);
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
        self.shortcuts(ctx);

        let files = ctx.input(|i| dropped_paths(&i.raw.dropped_files));
        if !files.is_empty() {
            self.notice = launch("geekbridge-share", &files).err();
        }
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());

        self.updates.banner(ctx, &pal);
        self.sidebar(ctx, &pal);
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal.content)
                    .inner_margin(Margin::symmetric(S5 as i8, S4 as i8))
                    .stroke(if hovering {
                        Stroke::new(2.0_f32, pal.accent_fill)
                    } else {
                        Stroke::NONE
                    }),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width() - 12.0);
                        match self.view {
                            View::Home => {
                                self.header(ui, &pal, "HOME");
                                self.tile_grid(ui, &pal, hovering);
                            }
                            View::Pair => self.pair_view(ui, &pal),
                            View::Settings => self.settings_view(ui, &pal),
                        }
                        if let Some(n) = &self.notice {
                            ui.add_space(S2);
                            ui.add(egui::Label::new(RichText::new(n).color(pal.text2)).wrap());
                        }
                    })
                    .bars(ui.ctx());
            });
        self.connections.dialogs(ctx, &pal);
    }
}

pub fn run(api_port: u16, theme_choice: ThemeChoice) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GeekBridge")
            .with_app_id("dev.geekbridge.GeekBridge")
            .with_inner_size([960.0, 680.0])
            .with_min_inner_size([560.0, 520.0])
            .with_drag_and_drop(true)
            .with_decorations(true),
        ..Default::default()
    };
    #[cfg(feature = "memstats")]
    crate::memstats::start();
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
