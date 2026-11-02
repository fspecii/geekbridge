// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless egui driver shared by the view tests: runs an app body for a few frames at a chosen
//! window size with a fixed discovery model, and can feed it pointer and key events. No window
//! system is involved; painting is exercised (the shapes are produced) but never rasterised.
use crate::{
    connection::ControlState,
    shell::{AppView, Env},
    state::{DeviceInfo, DiscoveryModel},
    theme,
};
use bridge_core::AppKind;
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use std::time::{Duration, Instant};

pub struct Rig {
    pub ctx: egui::Context,
    pub app: AppKind,
    pub size: Vec2,
    pub model: DiscoveryModel,
    pub control: ControlState,
    time: f64,
}

pub fn paired_model() -> DiscoveryModel {
    DiscoveryModel::Ready {
        discovery_error: None,
        devices: vec![DeviceInfo {
            id: "p1".into(),
            name: "Pixel 8".into(),
            kind: "phone".into(),
            address: "192.168.1.20".into(),
            trusted: true,
            advertises_feature: true,
        }],
    }
}

impl Rig {
    pub fn new(app: AppKind, width: f32, height: f32) -> Self {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        theme::apply(&ctx, app);
        Self {
            ctx,
            app,
            size: egui::vec2(width, height),
            model: paired_model(),
            control: ControlState::default(),
            time: 0.0,
        }
    }

    pub fn input(&mut self, events: Vec<Event>) -> RawInput {
        self.time += 0.05;
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)),
            time: Some(self.time),
            events,
            ..Default::default()
        }
    }

    /// One frame with `events`; returns the full output (shapes, commands, platform output).
    pub fn frame(&mut self, view: &mut dyn AppView, events: Vec<Event>) -> egui::FullOutput {
        let input = self.input(events);
        let pal = theme::current(&self.ctx, self.app);
        let (app, model, control) = (self.app, self.model.clone(), self.control.clone());
        self.ctx.run(input, |ctx| {
            let env = Env {
                app,
                api_port: 1,
                model: &model,
                pal: &pal,
                control: &control,
            };
            view.show(ctx, &env);
        })
    }

    /// `n` quiet frames (layout settles over a few).
    pub fn settle(&mut self, view: &mut dyn AppView, n: usize) -> egui::FullOutput {
        let mut out = self.frame(view, vec![]);
        for _ in 1..n {
            out = self.frame(view, vec![]);
        }
        out
    }

    pub fn click(&mut self, view: &mut dyn AppView, at: Pos2) {
        self.frame(view, vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.frame(
                view,
                vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                }],
            );
        }
        self.frame(view, vec![]);
    }

    pub fn key(&mut self, view: &mut dyn AppView, key: Key) {
        self.key_with(view, key, Modifiers::NONE);
    }

    pub fn key_with(&mut self, view: &mut dyn AppView, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(
                view,
                vec![Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                }],
            );
        }
    }

    pub fn type_text(&mut self, view: &mut dyn AppView, text: &str) {
        self.frame(view, vec![Event::Text(text.into())]);
    }
}

/// Number of painted shapes in a frame: a cheap "something was drawn" check.
pub fn shapes(out: &egui::FullOutput) -> usize {
    out.shapes.len()
}

/// Polls until `f` holds (feeds fetch on a worker thread).
pub fn wait_until(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
