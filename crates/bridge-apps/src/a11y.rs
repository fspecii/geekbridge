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

/// Names a text field for a screen reader (egui does not use the hint text as its name) and
/// hands the response back so it can wrap `ui.add(TextEdit...)` in place.
pub fn named(response: egui::Response, name: &str) -> egui::Response {
    describe(&response.ctx, response.id, name, None);
    response
}

/// egui's scroll bars take Tab focus and carry no name; this names the bars of a scroll area.
pub trait ScrollBars {
    fn bars(self, ctx: &egui::Context) -> Self;
}

impl<R> ScrollBars for egui::scroll_area::ScrollAreaOutput<R> {
    fn bars(self, ctx: &egui::Context) -> Self {
        for (axis, name) in ["Horizontal scroll bar", "Vertical scroll bar"]
            .into_iter()
            .enumerate()
        {
            // egui derives the bar's id as `area_id.with(axis)`; it only exists when drawn.
            let id = self.id.with(axis);
            if ctx.read_response(id).is_some() {
                describe(ctx, id, name, None);
            }
        }
        self
    }
}

/// Names the top-level window for a screen reader (egui leaves the root node unnamed).
pub fn window_title(ctx: &egui::Context, title: &str) {
    #[cfg(feature = "accesskit")]
    ctx.accesskit_node_builder(egui::accesskit_root_id(), |node| node.set_label(title));
    #[cfg(not(feature = "accesskit"))]
    let _ = (ctx, title);
}
