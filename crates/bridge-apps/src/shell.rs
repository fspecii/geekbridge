//! What the four apps share: the window setup, background discovery, and a slim connection
//! strip at the bottom with expandable details. Everything above the strip belongs to the app.
use crate::{
    cli::ThemeChoice,
    connection::{Connections, ControlState},
    icons::Icon,
    identity,
    poller::Poller,
    state::DiscoveryModel,
    theme::{self, Palette},
    update_ui::Updates,
};
use bridge_core::AppKind;
use eframe::egui::{self, Frame, Margin, RichText, ScrollArea, Stroke, ThemePreference};

/// Read-only context handed to an app each frame.
pub struct Env<'a> {
    pub app: AppKind,
    pub api_port: u16,
    pub model: &'a DiscoveryModel,
    pub pal: &'a Palette,
    pub control: &'a ControlState,
}

/// Per-app content. Each executable supplies exactly one. The app lays out its own panels
/// (toolbar, sidebar, central area); the connection strip has already claimed the bottom.
impl Env<'_> {
    pub fn blocked_line(&self, verb: &str) -> Option<String> {
        self.control.blocked_line(self.model, verb)
    }
}

pub trait AppView {
    fn show(&mut self, ctx: &egui::Context, env: &Env);
}

pub struct Shell {
    app: AppKind,
    api_port: u16,
    view: Box<dyn AppView>,
    model: DiscoveryModel,
    poller: Poller,
    details_open: bool,
    connections: Connections,
    updates: Updates,
}

impl Shell {
    pub fn new(app: AppKind, api_port: u16, ctx: egui::Context, view: Box<dyn AppView>) -> Self {
        let control_ctx = ctx.clone();
        let update_ctx = ctx.clone();
        let poller = Poller::spawn(app, api_port, move || ctx.request_repaint());
        Self {
            app,
            api_port,
            view,
            model: DiscoveryModel::Checking,
            poller,
            details_open: false,
            connections: Connections::new(control_ctx),
            updates: Updates::new(update_ctx),
        }
    }

    fn connection_panel(&mut self, ctx: &egui::Context, pal: &Palette) {
        egui::TopBottomPanel::bottom("connection")
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(pal.toolbar)
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| {
                let r = ui.max_rect();
                ui.painter().hline(
                    r.x_range().expand(12.0),
                    r.top() - 0.5,
                    Stroke::new(1.0_f32, pal.separator),
                );
                if self.details_open {
                    self.details(ui, pal);
                    theme::hairline(ui, pal);
                }
                self.strip(ui, pal);
            });
    }

    fn strip(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let strip = self.connections.state.strip(&self.model);
        ui.horizontal(|ui| {
            ui.set_height(30.0);
            theme::dot(ui, theme::status_color(pal, strip.tone), strip.hollow);
            let label = RichText::new(&strip.label).size(12.5).color(pal.text2);
            ui.add(egui::Label::new(label).truncate());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (icon, text) = if self.details_open {
                    (Icon::ChevronDown, "Hide details")
                } else {
                    (Icon::ChevronUp, "Details")
                };
                if theme::tool_button(
                    ui,
                    pal,
                    icon,
                    Some(text),
                    true,
                    "Show or hide connection details",
                )
                .clicked()
                {
                    self.details_open = !self.details_open;
                }
                if theme::tool_button(ui, pal, Icon::Refresh, None, true, "Check again").clicked() {
                    self.poller.refresh();
                    self.connections.refresh();
                }
            });
        });
    }

    fn details(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let headline = self.model.headline();
        ScrollArea::vertical()
            .max_height((ui.ctx().screen_rect().height() * 0.4).clamp(120.0, 280.0))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    theme::dot(ui, theme::status_color(pal, headline.tone), false);
                    ui.label(theme::semibold(&headline.title, 14.0));
                });
                ui.add(
                    egui::Label::new(theme::secondary(pal, self.model.guidance(self.api_port)))
                        .wrap(),
                );
                ui.add_space(2.0);
                ui.label(theme::overline(pal, "Available now"));
                ui.add(
                    egui::Label::new(theme::secondary(
                        pal,
                        identity::works_without_phone(self.app),
                    ))
                    .wrap(),
                );
                let devices = self.model.devices();
                if !devices.is_empty() {
                    ui.add_space(2.0);
                    ui.label(theme::overline(pal, "Nearby phones"));
                    for d in devices {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(theme::semibold(&d.name, 13.0));
                            ui.label(theme::caption(
                                pal,
                                if d.trusted {
                                    "Paired"
                                } else {
                                    "Found, not paired"
                                },
                            ));
                        });
                    }
                }
                self.connections.details(ui, pal, &self.model);
                self.updates.details(ui, pal);
                egui::CollapsingHeader::new(theme::caption(pal, "Technical details"))
                    .id_salt("technical")
                    .show(ui, |ui| {
                        for line in self.model.technical_lines(self.app, self.api_port) {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(line).monospace().size(11.0).color(pal.text2),
                                )
                                .wrap(),
                            );
                        }
                    });
                ui.add_space(6.0);
            });
    }
}

impl eframe::App for Shell {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(model) = self.poller.latest() {
            self.model = model;
        }
        crate::pace::observe(ctx, &self.model);
        self.connections.update();
        let pal = theme::current(ctx, self.app);
        self.connection_panel(ctx, &pal);
        self.updates.banner(ctx, &pal);
        let env = Env {
            app: self.app,
            api_port: self.api_port,
            model: &self.model,
            pal: &pal,
            control: &self.connections.state,
        };
        self.view.show(ctx, &env);
        self.connections.dialogs(ctx, &pal);
    }
}

pub fn run(
    app: AppKind,
    api_port: u16,
    theme_choice: ThemeChoice,
    view: Box<dyn AppView>,
) -> eframe::Result {
    let [w, h] = identity::initial_size(app);
    let [min_w, min_h] = identity::min_size(app);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(identity::window_title(app))
            .with_app_id(identity::app_id(app))
            .with_inner_size([w, h])
            .with_min_inner_size([min_w, min_h])
            .with_decorations(true),
        ..Default::default()
    };
    eframe::run_native(
        &identity::window_title(app),
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx, app);
            cc.egui_ctx.set_theme(match theme_choice {
                ThemeChoice::System => ThemePreference::System,
                ThemeChoice::Light => ThemePreference::Light,
                ThemeChoice::Dark => ThemePreference::Dark,
            });
            Ok(Box::new(Shell::new(
                app,
                api_port,
                cc.egui_ctx.clone(),
                view,
            )))
        }),
    )
}

/// The remaining area: a flat panel in the app's content color.
pub fn central<R>(
    ctx: &egui::Context,
    pal: &Palette,
    margin: Margin,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(pal.content).inner_margin(margin))
        .show(ctx, add)
        .inner
}
