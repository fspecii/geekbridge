//! Screen-reader details egui's `widget_info` cannot express: live regions and a name for widgets
//! that draw no text. All of it is a no-op unless the `accesskit` feature is built in and an
//! assistive technology is attached (egui only builds the accessibility tree on request).
use eframe::egui;

/// How urgently a screen reader should speak a change to the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    /// Wait for the current speech to finish (progress, status).
    Polite,
    /// Interrupt (an incoming call).
    Assertive,
}

/// Marks `id` as a live region, so changes to its name or value are announced without focus.
pub fn live_region(ctx: &egui::Context, id: egui::Id, live: Live) {
    #[cfg(feature = "accesskit")]
    ctx.accesskit_node_builder(id, |node| {
        node.set_live(match live {
            Live::Polite => egui::accesskit::Live::Polite,
            Live::Assertive => egui::accesskit::Live::Assertive,
        });
    });
    #[cfg(not(feature = "accesskit"))]
    let _ = (ctx, id, live);
}

/// Gives `id` an accessible role, e.g. `AlertDialog` for the incoming-call sheet.
#[cfg(feature = "accesskit")]
pub fn set_role(ctx: &egui::Context, id: egui::Id, role: egui::accesskit::Role) {
    ctx.accesskit_node_builder(id, |node| node.set_role(role));
}

/// Names a widget (and optionally describes its state) for a screen reader.
pub fn describe(ctx: &egui::Context, id: egui::Id, name: &str, value: Option<&str>) {
    #[cfg(feature = "accesskit")]
    ctx.accesskit_node_builder(id, |node| {
        node.set_label(name);
        if let Some(v) = value {
            node.set_value(v);
        }
    });
    #[cfg(not(feature = "accesskit"))]
    let _ = (ctx, id, name, value);
}
