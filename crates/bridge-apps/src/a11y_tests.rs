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
fn name(node: &Node) -> &str {
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
pub fn capture(size: egui::Vec2, mut ui: impl FnMut(&egui::Context)) -> Tree {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut nodes = HashMap::new();
    let mut root = None;
    for _ in 0..4 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
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
