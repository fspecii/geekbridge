// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::{a11y_tests::capture, theme};
use bridge_core::{AppKind, firewall::Tool};

fn facts() -> Facts {
    Facts {
        service: Service::Up,
        computer_name: Some("Desk".into()),
        nearby: vec![],
        firewall: Verdict::Fine,
        autostart: Autostart::On,
        update: Some("GeekBridge v1.2.3 is up to date.".into()),
        copied: None,
    }
}

fn tree(f: &Facts) -> crate::a11y_tests::Tree {
    let f = f.clone();
    let t = capture(egui::vec2(560.0, 900.0), move |ctx| {
        theme::apply(ctx, AppKind::Clipboard);
        let pal = theme::current(ctx, AppKind::Clipboard);
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                card(ui, &pal, &f);
            });
        });
    });
    assert_eq!(
        t.unnamed_controls(),
        Vec::<String>::new(),
        "{}",
        t.outline()
    );
    t
}

fn has(t: &crate::a11y_tests::Tree, name: &str) -> bool {
    t.find(name).is_some()
}

#[test]
fn the_guide_shows_only_while_no_phone_is_paired_and_the_service_has_answered() {
    assert!(!visible(Service::Checking, 0));
    assert!(visible(Service::Up, 0));
    assert!(!visible(Service::Up, 1));
    // A stopped service is the most useful time to explain how to start it, paired or not.
    assert!(visible(Service::Down, 0) && visible(Service::Down, 2));
}

#[test]
fn the_default_guide_has_three_steps_the_link_name_update_and_autostart() {
    let t = tree(&facts());
    for name in [
        "Step 1",
        "Step 2",
        "Step 3",
        "Open releases page",
        "Copy link",
        "Pair a phone",
        "Rename",
        "QR code for the GeekBridge releases page",
    ] {
        assert!(has(&t, name), "{name}\n{}", t.outline());
    }
    for text in [
        "Install the GeekBridge app on your phone",
        "Pair the phone with this computer",
        "Choose optional features",
        "Desk",
        "GeekBridge v1.2.3 is up to date.",
        "The background service starts when you log in.",
    ] {
        assert!(
            t.has_name_starting(text) || has(&t, text),
            "{text}\n{}",
            t.outline()
        );
    }
    // No firewall problem, service running, autostart on: no sudo text and no copy of it.
    assert!(!has(&t, "Copy firewall commands"));
    assert!(!has(&t, "Copy start command"));
}

#[test]
fn a_blocking_firewall_offers_the_exact_sudo_lines_to_copy() {
    let mut f = facts();
    f.firewall = Verdict::Blocking(Tool::Ufw);
    let t = tree(&f);
    assert!(has(&t, "Copy firewall commands"), "{}", t.outline());
    assert!(t.has_name_starting("Your firewall (ufw) is blocking phones"));
    assert!(
        t.has_name_starting("sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto tcp")
    );
    assert!(t.has_name_starting("GeekBridge never runs sudo"));
    assert_eq!(
        copy_text(CopyKind::Firewall, &f),
        firewall::commands(Tool::Ufw)
    );
}

#[test]
fn rules_that_need_root_get_a_softer_note_with_the_same_fix() {
    for (v, tool) in [
        (Verdict::Unverified(Tool::Ufw), "ufw"),
        (Verdict::Unverified(Tool::Firewalld), "firewalld"),
    ] {
        let mut f = facts();
        f.firewall = v;
        let t = tree(&f);
        assert!(
            t.has_name_starting(&format!("Your firewall ({tool}) is on")),
            "{}",
            t.outline()
        );
        assert!(!t.has_name_starting("Your firewall (ufw) is blocking"));
        assert!(has(&t, "Copy firewall commands"));
        assert!(copy_text(CopyKind::Firewall, &f).starts_with("sudo "));
    }
    let mut f = facts();
    f.firewall = Verdict::Unverified(Tool::Firewalld);
    assert!(copy_text(CopyKind::Firewall, &f).contains("firewall-cmd --reload"));
}

#[test]
fn a_stopped_service_leads_with_the_command_that_starts_it() {
    let mut f = facts();
    f.service = Service::Down;
    f.computer_name = None;
    f.update = None;
    f.autostart = Autostart::Off;
    let t = tree(&f);
    assert!(t.has_name_starting("The background service is not running"));
    assert!(has(&t, "Copy start command"), "{}", t.outline());
    assert!(
        !has(&t, "Rename"),
        "the name is unknown without the service"
    );
    assert!(
        copy_text(CopyKind::Service, &f).contains("geekbridge")
            && !copy_text(CopyKind::Service, &f).contains("sudo")
    );
}

#[test]
fn autostart_off_with_a_running_service_explains_how_to_turn_it_on() {
    let mut f = facts();
    f.autostart = Autostart::Off;
    let t = tree(&f);
    assert!(t.has_name_starting("The background service does not start at login yet."));
    assert!(has(&t, "Copy start command"));
    f.autostart = Autostart::Unknown;
    let t = tree(&f);
    assert!(!t.has_name_starting("The background service"));
}

#[test]
fn nearby_phones_are_named_and_counted() {
    let mut f = facts();
    assert!(tree(&f).has_name_starting("No phone has announced itself yet."));
    f.nearby = vec!["Pixel 8".into()];
    assert!(tree(&f).has_name_starting("Found nearby: Pixel 8"));
    f.nearby = vec!["Pixel 8".into(), "Galaxy".into(), "Moto".into()];
    assert!(tree(&f).has_name_starting("Found nearby: Pixel 8 and 2 more"));
}

#[test]
fn a_clicked_copy_button_reads_copied() {
    let mut f = facts();
    f.copied = Some(CopyKind::Link);
    let t = tree(&f);
    assert!(has(&t, "Copied") && !has(&t, "Copy link"));
}

#[test]
fn nearby_phones_skips_paired_ones_and_other_states() {
    use crate::state::DeviceInfo;
    let dev = |name: &str, trusted| DeviceInfo {
        id: name.into(),
        name: name.into(),
        kind: "phone".into(),
        address: "192.168.1.2".into(),
        trusted,
        advertises_feature: true,
    };
    let model = DiscoveryModel::Ready {
        discovery_error: None,
        devices: vec![dev("A", false), dev("B", true)],
    };
    assert_eq!(nearby_phones(&model), vec!["A".to_owned()]);
    assert!(nearby_phones(&DiscoveryModel::Checking).is_empty());
}

#[test]
fn autostart_is_read_from_the_systemd_wants_link_or_an_xdg_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("systemd/user");
    let xdg = dir.path().join("autostart");
    std::fs::create_dir_all(cfg.join("default.target.wants")).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();
    let (c, x) = (vec![cfg.clone()], vec![xdg.clone()]);
    assert_eq!(autostart_in(&c, &x), Autostart::Off);
    #[cfg(unix)]
    {
        // A dangling link still counts: `systemctl enable` writes it, the unit file may live elsewhere.
        std::os::unix::fs::symlink(
            "/usr/lib/systemd/user/geekbridge-service.service",
            cfg.join("default.target.wants/geekbridge-service.service"),
        )
        .unwrap();
        assert_eq!(autostart_in(&c, &x), Autostart::On);
        std::fs::remove_file(cfg.join("default.target.wants/geekbridge-service.service")).unwrap();
    }
    std::fs::write(xdg.join("geekbridge-service.desktop"), "[Desktop Entry]\n").unwrap();
    assert_eq!(autostart_in(&c, &x), Autostart::On);
}

#[test]
fn every_copyable_text_is_plain_and_the_link_is_the_releases_page() {
    let f = facts();
    assert_eq!(copy_text(CopyKind::Link, &f), RELEASES_URL);
    assert!(
        RELEASES_URL.starts_with("https://github.com/")
            && RELEASES_URL.ends_with("/releases/latest")
    );
    // The QR code on screen encodes exactly that link.
    let (modules, w) = crate::connection::qr_modules(RELEASES_URL).unwrap();
    assert!(w > 20 && modules.len() == w * w);
}
