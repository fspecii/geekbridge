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
    node.supports_action(Action::Click)
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
                format!("{:?} at {:?}", n.role(), b)
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
            assert!(tree.controls() >= 2, "{app:?}: connection strip controls exist");
            assert!(tree.find("Check again").is_some(), "{app:?}: refresh button is named");
            for c in tree.unnamed_controls() {
                problems.push(format!("{app:?} details={details}: {c}"));
            }
        }
    }
    problems.sort();
    problems.dedup();
    assert!(problems.is_empty(), "unnamed controls:\n{}", problems.join("\n"));
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
