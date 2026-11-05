// SPDX-License-Identifier: GPL-3.0-or-later
//! Walks the AccessKit tree each desktop screen produces and checks that every control a screen
//! reader can focus or activate has a name. Run headless: egui builds the tree as soon as
//! `enable_accesskit` is called, no window system or assistive technology is involved.
use crate::{
    apps, home_app::HomeApp, shell::Shell, state::DeviceInfo, state::DiscoveryModel, theme,
};
use bridge_core::AppKind;
use eframe::egui::{
    self,
    accesskit::{Action, Node, NodeId, Role, TreeUpdate},
};
use std::collections::HashMap;

pub struct Tree {
    pub nodes: HashMap<NodeId, Node>,
    pub root: NodeId,
}

/// Roles a screen reader announces as something to operate; each needs a name.
fn is_control(node: &Node) -> bool {
    // Selectable text is clickable (to place a caret) but is read as text, not as a control.
    (node.supports_action(Action::Click) && !matches!(node.role(), Role::Label))
        || matches!(
            node.role(),
            Role::Button
                | Role::CheckBox
                | Role::RadioButton
                | Role::Switch
                | Role::TextInput
                | Role::MultilineTextInput
                | Role::Slider
                | Role::ComboBox
                | Role::Link
                | Role::Tab
                | Role::SpinButton
        )
}

/// What a screen reader says first. Plain text nodes carry it as their value; controls must have
/// a label (a text field's value is what the user typed, which is empty at first).
pub(crate) fn name(node: &Node) -> &str {
    if is_control(node) {
        node.label().unwrap_or("").trim()
    } else {
        node.label().or_else(|| node.value()).unwrap_or("").trim()
    }
}

impl Tree {
    /// Controls without a name, as "Role at (x, y)".
    pub fn unnamed_controls(&self) -> Vec<String> {
        let mut bad: Vec<String> = self
            .nodes
            .values()
            .filter(|n| is_control(n) && name(n).is_empty())
            .map(|n| {
                let b = n.bounds().map(|b| (b.x0 as i32, b.y0 as i32));
                format!(
                    "{:?} at {:?} focus={} bounds={:?}",
                    n.role(),
                    b,
                    n.supports_action(Action::Focus),
                    n.bounds()
                )
            })
            .collect();
        bad.sort();
        bad
    }

    pub fn controls(&self) -> usize {
        self.nodes.values().filter(|n| is_control(n)).count()
    }

    pub fn find(&self, label: &str) -> Option<&Node> {
        self.nodes.values().find(|n| name(n) == label)
    }

    pub fn has_name_starting(&self, prefix: &str) -> bool {
        self.nodes.values().any(|n| name(n).starts_with(prefix))
    }

    /// Indented outline: role, name, state. Used for docs/accessibility.md.
    pub fn outline(&self) -> String {
        fn walk(t: &Tree, id: NodeId, depth: usize, out: &mut String) {
            let Some(n) = t.nodes.get(&id) else { return };
            let text = name(n);
            let interesting = is_control(n) || !text.is_empty() || n.live().is_some();
            let depth = if interesting {
                let mut line = format!("{}{:?}", "  ".repeat(depth), n.role());
                if !text.is_empty() {
                    line += &format!(" \"{text}\"");
                }
                if n.is_disabled() {
                    line += " [disabled]";
                }
                if let Some(l) = n.live() {
                    line += &format!(" [live={l:?}]");
                }
                if n.supports_action(Action::Click) {
                    line += " [click]";
                }
                out.push_str(&line);
                out.push('\n');
                depth + 1
            } else {
                depth
            };
            for c in n.children() {
                walk(t, *c, depth, out);
            }
        }
        let mut out = String::new();
        walk(self, self.root, 0, &mut out);
        out
    }
}

/// Runs `frames` frames of `ui` at `size` with AccessKit on and returns the final tree.
pub fn capture(size: egui::Vec2, ui: impl FnMut(&egui::Context)) -> Tree {
    capture_scaled(size, 1.0, ui)
}

/// As [`capture`] at a display scale (1.0 = 100%, 1.5 = 150%). `size` is the window in points.
pub fn capture_scaled(
    size: egui::Vec2,
    pixels_per_point: f32,
    mut ui: impl FnMut(&egui::Context),
) -> Tree {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut nodes = HashMap::new();
    let mut root = None;
    for _ in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(pixels_per_point),
                    ..Default::default()
                },
            ))
            .collect(),
            ..Default::default()
        };
        let out = ctx.run(input, &mut ui);
        let update: TreeUpdate = out
            .platform_output
            .accesskit_update
            .expect("accesskit enabled, so every frame yields a tree update");
        root = update.tree.map(|t| t.root).or(root);
        nodes = update.nodes.into_iter().collect();
    }
    Tree {
        nodes,
        root: root.expect("the tree names its root"),
    }
}

fn paired() -> DiscoveryModel {
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

fn shell_tree(app: AppKind, details: bool, model: DiscoveryModel) -> Tree {
    let ctx = egui::Context::default();
    let mut shell = Shell::new(app, 1, ctx, apps::view_for(app));
    shell.details_open = details;
    let mut first = true;
    capture(egui::vec2(900.0, 640.0), move |ctx| {
        if first {
            theme::apply(ctx, app);
            first = false;
        }
        shell.model = model.clone();
        shell.frame(ctx);
    })
}

const SHELL_APPS: [AppKind; 5] = [
    AppKind::Calls,
    AppKind::Messages,
    AppKind::Agenda,
    AppKind::Contacts,
    AppKind::Clipboard,
];

#[test]
fn every_control_in_the_shell_apps_has_a_name() {
    let mut problems = Vec::new();
    for app in SHELL_APPS {
        for (details, model) in [
            (false, DiscoveryModel::Checking),
            (false, paired()),
            (true, paired()),
            (
                true,
                DiscoveryModel::ServiceUnavailable {
                    reason: "refused".into(),
                },
            ),
        ] {
            let tree = shell_tree(app, details, model);
            assert!(
                tree.controls() >= 2,
                "{app:?}: connection strip controls exist"
            );
            assert_eq!(
                tree.nodes[&tree.root].label(),
                Some(crate::identity::window_title(app).as_str()),
                "{app:?}: the window is named"
            );
            assert!(
                tree.nodes
                    .values()
                    .any(|n| n.live() == Some(eframe::egui::accesskit::Live::Polite)),
                "{app:?}: the connection status is a polite live region"
            );
            assert!(
                tree.find("Check again").is_some(),
                "{app:?}: refresh button is named"
            );
            for c in tree.unnamed_controls() {
                problems.push(format!("{app:?} details={details}: {c}"));
            }
        }
    }
    problems.sort();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "unnamed controls:\n{}",
        problems.join("\n")
    );
}

#[test]
fn home_window_controls_have_names() {
    let ctx = egui::Context::default();
    let mut home = HomeApp::new(1, &ctx);
    let mut first = true;
    let tree = capture(egui::vec2(960.0, 680.0), move |ctx| {
        if first {
            theme::apply(ctx, AppKind::Calls);
            first = false;
        }
        home.frame(ctx);
    });
    assert!(tree.controls() >= 3);
    assert_eq!(
        tree.unnamed_controls(),
        Vec::<String>::new(),
        "{}",
        tree.outline()
    );
}

#[test]
fn home_whats_new_card_is_named_and_reads_as_text() {
    let ctx = egui::Context::default();
    let notes = bridge_core::whats_new::parse("- First change\n- Second change\n", "9.8.7");
    let mut home = HomeApp::new(1, &ctx).with_whats_new(notes);
    let tree = capture(egui::vec2(960.0, 680.0), move |ctx| {
        theme::apply(ctx, AppKind::Calls);
        home.frame(ctx);
    });
    for name in [
        "Full release notes",
        "Dismiss",
        "First change",
        "Updated to 9.8.7",
    ] {
        assert!(tree.find(name).is_some(), "{name}: {}", tree.outline());
    }
    assert_eq!(
        tree.unnamed_controls(),
        Vec::<String>::new(),
        "{}",
        tree.outline()
    );
}

#[test]
fn dot_matrix_text_is_exposed_as_words() {
    let tree = capture(egui::vec2(400.0, 200.0), |ctx| {
        theme::apply(ctx, AppKind::Calls);
        let pal = theme::current(ctx, AppKind::Calls);
        egui::CentralPanel::default().show(ctx, |ui| {
            crate::nothing::dot_text(ui, &pal, "Missed call", 3.0, pal.accent_fill, false);
        });
    });
    assert!(tree.find("Missed call").is_some(), "{}", tree.outline());
}

/// Prints the outline of every screen; `cargo test -p bridge-apps dump_outlines -- --ignored --nocapture`.
#[test]
#[ignore = "documentation helper"]
fn dump_outlines() {
    for app in SHELL_APPS {
        println!(
            "== {app:?} (paired, details open)\n{}",
            shell_tree(app, true, paired()).outline()
        );
    }
    let ctx = egui::Context::default();
    let mut home = HomeApp::new(1, &ctx);
    let tree = capture(egui::vec2(960.0, 680.0), move |ctx| home.frame(ctx));
    println!("== Home\n{}", tree.outline());
}

/// Names of the controls Tab visits, in order, until it wraps.
fn tab_stops(app: AppKind) -> Vec<String> {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut shell = Shell::new(app, 1, ctx.clone(), apps::view_for(app));
    theme::apply(&ctx, app);
    let mut stops: Vec<String> = Vec::new();
    let mut seen: Vec<NodeId> = Vec::new();
    for step in 0..300 {
        let mut events = Vec::new();
        if step >= 3 {
            events.push(egui::Event::Key {
                key: egui::Key::Tab,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 640.0),
            )),
            events,
            ..Default::default()
        };
        shell.model = DiscoveryModel::Checking;
        let out = ctx.run(input, |ctx| shell.frame(ctx));
        let update = out.platform_output.accesskit_update.unwrap();
        let focus = update.focus;
        let Some((_, node)) = update.nodes.iter().find(|(n, _)| *n == focus) else {
            continue;
        };
        let label = name(node).to_owned();
        if step < 3
            || node.role() == Role::Window
            || label.is_empty()
            || seen.last() == Some(&focus)
        {
            continue;
        }
        if seen.contains(&focus) {
            break;
        }
        seen.push(focus);
        stops.push(label);
    }
    stops
}

#[test]
fn tab_reaches_the_app_before_the_connection_strip() {
    for app in SHELL_APPS {
        let stops = tab_stops(app);
        let check = stops.iter().position(|s| s == "Check again");
        let details = stops.iter().position(|s| s == "Details");
        assert!(stops.len() >= 3, "{app:?}: {stops:?}");
        assert!(
            check.is_some() && details.is_some(),
            "{app:?}: strip is reachable: {stops:?}"
        );
        assert!(
            check.unwrap() >= stops.len() - 2 && details.unwrap() >= stops.len() - 2,
            "{app:?}: the strip must be the last two stops, got {stops:?}"
        );
    }
}

/// Every operable node with its on-screen size in points, as (name, width, height).
fn target_sizes(tree: &Tree) -> Vec<(String, f64, f64)> {
    tree.nodes
        .values()
        // The floating scroll bar is a 2 pt strip until hovered; wheel, touch-pad and keys scroll
        // too, so it is not the only way to operate the list.
        .filter(|n| is_control(n) && !name(n).ends_with("scroll bar"))
        .filter_map(|n| {
            let b = n.bounds()?;
            Some((name(n).to_owned(), b.x1 - b.x0, b.y1 - b.y0))
        })
        .collect()
}

fn undersized(tree: &Tree, ppp: f32, min_points: f64) -> Vec<String> {
    let mut bad: Vec<String> = target_sizes(tree)
        .into_iter()
        .filter(|(_, w, h)| *w < min_points || *h < min_points)
        .map(|(n, w, h)| {
            format!(
                "{n:?} {w:.0}x{h:.0} pt = {:.0}x{:.0} px at {ppp}",
                w * f64::from(ppp),
                h * f64::from(ppp)
            )
        })
        .collect();
    bad.sort();
    bad.dedup();
    bad
}

/// WCAG 2.2 SC 2.5.8 (AA): pointer targets are at least 24 x 24 CSS px. A point is a CSS px, so the
/// size in points must not shrink with the display scale, and at 100% it is 24 device pixels.
#[test]
fn operable_targets_are_at_least_24_points_at_100_and_150_percent() {
    let mut problems = Vec::new();
    for ppp in [1.0_f32, 1.5] {
        let size = egui::vec2(900.0, 640.0);
        let mut trees = vec![("home".to_owned(), {
            let ctx = egui::Context::default();
            let mut home = HomeApp::new(1, &ctx);
            capture_scaled(size, ppp, move |ctx| {
                theme::apply(ctx, AppKind::Calls);
                home.frame(ctx);
            })
        })];
        for app in SHELL_APPS {
            for (details, model) in [(true, paired()), (false, paired())] {
                let ctx = egui::Context::default();
                let mut shell = Shell::new(app, 1, ctx, apps::view_for(app));
                shell.details_open = details;
                trees.push((
                    format!("{app:?} details={details}"),
                    capture_scaled(size, ppp, move |ctx| {
                        theme::apply(ctx, app);
                        shell.model = model.clone();
                        shell.frame(ctx);
                    }),
                ));
            }
        }
        for (screen, tree) in trees {
            for bad in undersized(&tree, ppp, 24.0) {
                problems.push(format!("{screen} @{ppp}: {bad}"));
            }
        }
    }
    problems.sort();
    problems.dedup();
    assert!(
        problems.is_empty(),
        "targets under 24 pt:\n{}",
        problems.join("\n")
    );
}

#[test]
fn target_size_in_points_does_not_shrink_with_scale() {
    let at = |ppp: f32| {
        let ctx = egui::Context::default();
        let mut shell = Shell::new(AppKind::Calls, 1, ctx, apps::view_for(AppKind::Calls));
        let t = capture_scaled(egui::vec2(900.0, 640.0), ppp, move |ctx| {
            theme::apply(ctx, AppKind::Calls);
            shell.model = paired();
            shell.frame(ctx);
        });
        let mut v: Vec<_> = target_sizes(&t)
            .into_iter()
            .map(|(n, w, h)| (n, w.round() as i32, h.round() as i32))
            .collect();
        v.sort();
        v
    };
    // Text widths may differ by a point or two between scales (glyph hinting); heights must not.
    let (a, b) = (at(1.0), at(1.5));
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(&b) {
        assert!(
            x.0 == y.0 && (x.1 - y.1).abs() <= 2 && x.2 == y.2,
            "{x:?} vs {y:?}"
        );
    }
}

/// A focused control must be drawn with an outline: the shared 2 pt ink ring, or the accent stroke
/// egui puts around a focused text field.
pub(crate) fn has_focus_indicator(
    shapes: &[egui::epaint::ClippedShape],
    pal: &theme::Palette,
) -> bool {
    use egui::epaint::Shape;
    fn walk(s: &Shape, pal: &theme::Palette, hit: &mut bool) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, pal, hit)),
            Shape::Rect(r) => {
                let st = r.stroke;
                *hit |= (st.color == theme::focus_color(pal) && st.width >= 2.0)
                    || (st.color == pal.accent && st.width >= 1.0);
            }
            Shape::Circle(c) => {
                let st = c.stroke;
                *hit |= st.color == theme::focus_color(pal) && st.width >= 2.0;
            }
            _ => {}
        }
    }
    let mut hit = false;
    shapes.iter().for_each(|c| walk(&c.shape, pal, &mut hit));
    hit
}

/// Tab through each screen: every stop it lands on must show a visible focus indicator, in both
/// themes, and Tab must reach more than one control.
#[test]
fn every_tab_stop_draws_a_focus_indicator_in_both_themes() {
    let mut problems = Vec::new();
    for app in SHELL_APPS {
        for theme_choice in [egui::Theme::Light, egui::Theme::Dark] {
            let ctx = egui::Context::default();
            theme::install_fonts(&ctx);
            ctx.enable_accesskit();
            theme::apply(&ctx, app);
            ctx.set_theme(theme_choice);
            let mut shell = Shell::new(app, 1, ctx.clone(), apps::view_for(app));
            shell.details_open = true;
            let mut seen: Vec<egui::Id> = Vec::new();
            for step in 0..200 {
                let mut events = Vec::new();
                if step >= 3 {
                    events.push(egui::Event::Key {
                        key: egui::Key::Tab,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 640.0),
                    )),
                    events,
                    ..Default::default()
                };
                shell.model = paired();
                let _ = ctx.run(input, |ctx| shell.frame(ctx));
                let Some(id) = ctx.memory(|m| m.focused()) else {
                    continue;
                };
                if step < 3 || seen.last() == Some(&id) {
                    continue;
                }
                if seen.contains(&id) {
                    break;
                }
                seen.push(id);
                // The ring is painted by the widget on the frame after focus moved to it.
                shell.model = paired();
                let out = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(900.0, 640.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| shell.frame(ctx),
                );
                let pal = theme::current(&ctx, app);
                if !has_focus_indicator(&out.shapes, &pal) {
                    let who = out
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .and_then(|u| u.nodes.iter().find(|(n, _)| *n == u.focus))
                        .map(|(_, n)| format!("{:?} {:?} {:?}", n.role(), name(n), n.bounds()))
                        .unwrap_or_default();
                    // egui's own scroll bar handle takes Tab focus and cannot be restyled (documented).
                    if !who.contains("scroll bar") {
                        problems.push(format!(
                            "{app:?} {theme_choice:?}: stop {} ({who})",
                            seen.len()
                        ));
                    }
                }
            }
            if seen.len() < 4 {
                problems.push(format!(
                    "{app:?} {theme_choice:?}: Tab reached only {} widgets",
                    seen.len()
                ));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The dot-matrix lettering and status dots are decoration: one readable node for the words, no
/// extra focusable or operable node per dot.
#[test]
fn dot_matrix_and_status_dots_add_no_operable_nodes() {
    let tree = capture(egui::vec2(400.0, 200.0), |ctx| {
        theme::apply(ctx, AppKind::Calls);
        let pal = theme::current(ctx, AppKind::Calls);
        egui::CentralPanel::default().show(ctx, |ui| {
            crate::nothing::dot_text(ui, &pal, "Missed call", 3.0, pal.accent_fill, false);
            theme::dot(ui, pal.ok, false);
            theme::dot(ui, pal.err, true);
        });
    });
    assert_eq!(tree.controls(), 0, "{}", tree.outline());
    let named: Vec<_> = tree
        .nodes
        .values()
        .filter(|n| !name(n).is_empty() && n.role() != Role::Window)
        .collect();
    assert_eq!(named.len(), 1, "{}", tree.outline());
    let focusable = tree
        .nodes
        .values()
        .filter(|n| n.supports_action(Action::Focus) && n.role() != Role::Window)
        .count();
    assert_eq!(focusable, 0, "{}", tree.outline());
}

/// WCAG 2.2 SC 2.5.8: a pointer target under 24 x 24 points is still allowed when a 24-point circle
/// centred on it touches no other target. Targets that contain the small one (a row around its
/// button) do not count as neighbours.
fn crowded_small_targets(tree: &Tree) -> Vec<String> {
    let boxes: Vec<(String, accesskit_rect::R)> = tree
        .nodes
        .values()
        .filter(|n| is_control(n) && !name(n).ends_with("scroll bar"))
        .filter_map(|n| {
            let b = n.bounds()?;
            Some((
                name(n).to_owned(),
                accesskit_rect::R {
                    x0: b.x0,
                    y0: b.y0,
                    x1: b.x1,
                    y1: b.y1,
                },
            ))
        })
        .collect();
    let mut bad = Vec::new();
    for (n, r) in &boxes {
        if r.x1 - r.x0 >= 24.0 && r.y1 - r.y0 >= 24.0 {
            continue;
        }
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        for (m, o) in &boxes {
            if std::ptr::eq(r, o) || o.contains(r) {
                continue;
            }
            // Distance from the centre to the other rectangle.
            let dx = (o.x0 - cx).max(0.0).max(cx - o.x1);
            let dy = (o.y0 - cy).max(0.0).max(cy - o.y1);
            if (dx * dx + dy * dy).sqrt() < 12.0 {
                bad.push(format!(
                    "{n:?} {:.0}x{:.0} pt has {m:?} inside its 24 pt circle",
                    r.x1 - r.x0,
                    r.y1 - r.y0
                ));
            }
        }
    }
    bad.sort();
    bad.dedup();
    bad
}

mod accesskit_rect {
    pub struct R {
        pub x0: f64,
        pub y0: f64,
        pub x1: f64,
        pub y1: f64,
    }
    impl R {
        pub fn contains(&self, o: &R) -> bool {
            self.x0 <= o.x0 && self.y0 <= o.y0 && self.x1 >= o.x1 && self.y1 >= o.y1
        }
    }
}

/// What walking a screen with the keyboard found.
pub(crate) struct Audit {
    /// Names of the controls Tab landed on, in order, from the light-theme walk.
    pub stops: Vec<String>,
    pub problems: Vec<String>,
    /// The final accessibility tree (light theme).
    pub tree: Tree,
}

/// Tabs through the screen `frame` draws, in both themes, and reports: a stop without a visible
/// focus indicator or a name, any unnamed control, and any pointer target under 24 pt that is
/// crowded by another (SC 2.5.8).
pub(crate) fn audit_screen(
    what: &str,
    app: AppKind,
    size: egui::Vec2,
    mut frame: impl FnMut(&egui::Context),
) -> Audit {
    let mut problems = Vec::new();
    let mut stops_out = Vec::new();
    let mut tree_out = None;
    for theme_choice in [egui::Theme::Light, egui::Theme::Dark] {
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        theme::apply(&ctx, app);
        ctx.set_theme(theme_choice);
        let mut nodes: HashMap<NodeId, Node> = HashMap::new();
        let mut root = None;
        let mut run = |events: Vec<egui::Event>| {
            let out = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    events,
                    ..Default::default()
                },
                &mut frame,
            );
            let update = out.platform_output.accesskit_update.clone().unwrap();
            root = update.tree.as_ref().map(|t| t.root).or(root);
            nodes = update.nodes.iter().cloned().collect();
            (out, update)
        };
        for _ in 0..4 {
            run(vec![]);
        }
        let tab = vec![egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }];
        let mut seen: Vec<NodeId> = Vec::new();
        let mut stops = Vec::new();
        for _ in 0..120 {
            run(tab.clone());
            let (_, update) = run(vec![]);
            let focus = update.focus;
            let Some((_, node)) = update.nodes.iter().find(|(n, _)| *n == focus) else {
                continue;
            };
            if node.role() == Role::Window || seen.last() == Some(&focus) {
                continue;
            }
            if seen.contains(&focus) {
                break;
            }
            seen.push(focus);
            // The ring is painted on the frame after focus moved; this is that frame.
            let (out, _) = run(vec![]);
            let label = name(node).to_owned();
            let is_bar = label.ends_with("scroll bar");
            let pal = theme::current(&ctx, app);
            if !is_bar && !has_focus_indicator(&out.shapes, &pal) {
                problems.push(format!(
                    "{what} {theme_choice:?}: stop {label:?} ({:?}) draws no focus indicator",
                    node.role()
                ));
            }
            if label.is_empty() {
                problems.push(format!("{what} {theme_choice:?}: an unnamed tab stop"));
            }
            stops.push(label);
        }
        let tree = Tree {
            nodes,
            root: root.expect("root"),
        };
        for c in tree.unnamed_controls() {
            problems.push(format!("{what} {theme_choice:?}: unnamed control {c}"));
        }
        for c in crowded_small_targets(&tree) {
            problems.push(format!("{what} {theme_choice:?}: {c}"));
        }
        if theme_choice == egui::Theme::Light {
            stops_out = stops;
            tree_out = Some(tree);
        }
    }
    problems.sort();
    problems.dedup();
    Audit {
        stops: stops_out,
        problems,
        tree: tree_out.unwrap(),
    }
}
