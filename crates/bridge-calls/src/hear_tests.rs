// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

const ADDR: &str = "AA:BB:CC:00:11:22";

fn phone(connected: bool, hfp: bool) -> PhoneFacts {
    PhoneFacts {
        name: "Galaxy Z Fold5".into(),
        address: ADDR.into(),
        paired: true,
        trusted: true,
        connected,
        hfp_gateway: true,
        hfp_connected: hfp,
        card: Some("bluez_card.AA_BB_CC_00_11_22".into()),
        card_profile: Some("audio-gateway".into()),
        card_has_gateway_profile: true,
        voice_streams: false,
    }
}

fn ready_facts() -> Facts {
    Facts {
        adapters: vec!["hci0".into()],
        btusb_loaded: true,
        rfkill: vec![Rfkill::default()],
        service_active: Some(true),
        powered: Some(true),
        wireplumber_running: Some(true),
        wp: WpConfig::default(),
        telephony_present: Some(true),
        phones: vec![phone(true, true)],
        audio: Some(AudioFacts {
            server_name: "PulseAudio (on PipeWire 1.6.9)".into(),
            pipewire: true,
            output: Some(("alsa_output.usb-HyperX".into(), "HyperX".into(), false)),
            input: Some(("alsa_input.usb-HyperX".into(), "HyperX Mono".into(), true)),
            missing_pick: vec![],
            input_muted: false,
        }),
        bluetoothctl: true,
        partial: false,
        remembered: None,
    }
}

fn get<'a>(s: &'a [Step], id: &str) -> &'a Step {
    s.iter().find(|x| x.id == id).unwrap()
}

#[test]
fn a_complete_setup_is_ready_and_lists_every_step_in_order() {
    let s = evaluate(&ready_facts());
    let ids: Vec<&str> = s.iter().map(|x| x.id).collect();
    assert_eq!(
        ids,
        [
            "adapter",
            "rfkill",
            "service",
            "powered",
            "wireplumber",
            "telephony",
            "paired",
            "connected",
            "hfp",
            "card",
            "devices",
            "voice"
        ]
    );
    assert!(ready(&s), "{}", render(&s));
    assert_eq!(get(&s, "voice").state, State::Skip);
    let text = render(&s);
    assert!(text.contains("Result: ready"));
    assert!(text.contains("HyperX Mono (your pick)"));
    assert!(text.contains("HyperX (system default)"));
}

#[test]
fn the_owners_machine_today_fails_at_the_adapter_and_skips_what_depends_on_it() {
    let mut f = ready_facts();
    f.adapters.clear();
    f.rfkill.clear();
    f.powered = None;
    f.phones.clear();
    let s = evaluate(&f);
    let a = get(&s, "adapter");
    assert_eq!(a.state, State::Fail);
    assert!(
        a.detail
            .contains("btusb driver is loaded but found no device")
    );
    assert!(a.fix.as_ref().unwrap().contains("dual-mode"));
    for id in ["rfkill", "powered", "paired", "connected", "hfp"] {
        assert_eq!(get(&s, id).state, State::Skip, "{id}");
    }
    assert_eq!(first_problem(&s).unwrap().id, "adapter");
    assert!(render(&s).contains("not ready yet"));
}

#[test]
fn radio_service_and_power_each_have_their_own_fix() {
    let mut f = ready_facts();
    f.rfkill = vec![Rfkill {
        soft: true,
        hard: false,
    }];
    assert_eq!(
        get(&evaluate(&f), "rfkill").fix.as_deref(),
        Some("rfkill unblock bluetooth")
    );
    f.rfkill = vec![Rfkill {
        soft: true,
        hard: true,
    }];
    assert!(get(&evaluate(&f), "rfkill").detail.contains("hard"));
    let mut f = ready_facts();
    f.service_active = Some(false);
    let s = evaluate(&f);
    assert!(
        get(&s, "service")
            .fix
            .as_ref()
            .unwrap()
            .contains("enable --now bluetooth")
    );
    assert_eq!(get(&s, "powered").state, State::Skip);
    let mut f = ready_facts();
    f.powered = Some(false);
    assert!(
        get(&evaluate(&f), "powered")
            .fix
            .as_ref()
            .unwrap()
            .starts_with("bluetoothctl power on")
    );
}

#[test]
fn wireplumber_roles_and_backend_are_checked_against_what_geekbridge_needs() {
    let mut f = ready_facts();
    f.wp.roles = Some(vec!["a2dp_sink".into(), "a2dp_source".into()]);
    f.wp.files = vec!["/home/u/.config/wireplumber/wireplumber.conf.d/x.conf".into()];
    let w = get(&evaluate(&f), "wireplumber").clone();
    assert_eq!(w.state, State::Fail);
    assert!(w.detail.contains("leaves out hfp_hf") && w.detail.contains("x.conf"));
    assert!(w.fix.unwrap().contains("hfp_hf"));
    let mut f = ready_facts();
    f.wp.backend = Some("ofono".into());
    let w = get(&evaluate(&f), "wireplumber").clone();
    assert_eq!(w.state, State::Fail);
    assert!(w.fix.unwrap().contains("\"native\""));
    let mut f = ready_facts();
    f.wireplumber_running = Some(false);
    assert_eq!(get(&evaluate(&f), "wireplumber").state, State::Fail);
    let mut f = ready_facts();
    f.wp.telephony = Some(false);
    f.telephony_present = Some(false);
    let s = evaluate(&f);
    assert!(
        get(&s, "telephony")
            .fix
            .as_ref()
            .unwrap()
            .contains("telephony-dbus-service = true")
    );
    assert_eq!(get(&s, "hfp").state, State::Skip);
}

#[test]
fn phone_states_say_what_to_do_on_the_phone_and_here() {
    let mut f = ready_facts();
    f.phones.clear();
    assert!(
        get(&evaluate(&f), "paired")
            .fix
            .as_ref()
            .unwrap()
            .contains("pair ADDR")
    );
    let mut f = ready_facts();
    f.phones[0].trusted = false;
    let p = get(&evaluate(&f), "paired").clone();
    assert_eq!(p.state, State::Warn);
    assert_eq!(p.fix.unwrap(), format!("bluetoothctl trust {ADDR}"));
    let mut f = ready_facts();
    f.phones = vec![phone(false, false)];
    let s = evaluate(&f);
    assert_eq!(get(&s, "connected").state, State::Fail);
    let mut f = ready_facts();
    f.phones = vec![phone(true, false)];
    let h = get(&evaluate(&f), "hfp").clone();
    assert_eq!(h.state, State::Fail);
    let fix = h.fix.unwrap();
    assert!(fix.contains("Samsung One UI") && fix.contains(HFP_AG_UUID));
}

#[test]
fn an_auto_connect_rule_without_hfp_is_called_out() {
    // The owner's own fragment: bluez5.auto-connect = [ a2dp_sink a2dp_source ].
    let mut f = ready_facts();
    f.wp.auto_connect = Some(vec!["a2dp_sink".into(), "a2dp_source".into()]);
    f.wp.files = vec!["bluetooth-a2dp-autoconnect.conf".into()];
    let h = get(&evaluate(&f), "hfp").clone();
    assert_eq!(h.state, State::Warn);
    assert!(h.fix.unwrap().contains("hfp_ag"));
    f.phones = vec![phone(true, false)];
    let h = get(&evaluate(&f), "hfp").clone();
    assert!(h.detail.contains("never asks for it"));
}

#[test]
fn the_card_profile_and_devices_steps() {
    let mut f = ready_facts();
    f.phones[0].card_profile = Some("off".into());
    let c = get(&evaluate(&f), "card").clone();
    assert_eq!(c.state, State::Warn);
    assert_eq!(
        c.fix.unwrap(),
        "pactl set-card-profile bluez_card.AA_BB_CC_00_11_22 audio-gateway"
    );
    f.phones[0].card = None;
    assert_eq!(get(&evaluate(&f), "card").state, State::Fail);
    let mut f = ready_facts();
    f.audio.as_mut().unwrap().input = None;
    assert_eq!(get(&evaluate(&f), "devices").state, State::Fail);
    let mut f = ready_facts();
    f.audio.as_mut().unwrap().missing_pick = vec!["alsa_output.gone".into()];
    let d = get(&evaluate(&f), "devices").clone();
    assert_eq!(d.state, State::Warn);
    assert!(d.detail.contains("alsa_output.gone"));
    let mut f = ready_facts();
    f.audio = None;
    assert_eq!(get(&evaluate(&f), "devices").state, State::Fail);
    let mut f = ready_facts();
    f.phones[0].voice_streams = true;
    assert_eq!(get(&evaluate(&f), "voice").state, State::Ok);
}

#[test]
fn wireplumber_text_is_read_like_wireplumber_reads_it() {
    let mut cfg = WpConfig::default();
    // The owner's real fragment.
    read_wp_text(
        &mut cfg,
        "a.conf",
        "## Auto-connect A2DP\nmonitor.bluez.rules = [\n  {\n    matches = [ { device.name = \"~bluez_card.*\" } ]\n    actions = {\n      update-props = {\n        bluez5.auto-connect = [ a2dp_sink a2dp_source ]\n      }\n    }\n  }\n]\n",
    );
    assert_eq!(
        cfg.auto_connect,
        Some(vec!["a2dp_sink".to_owned(), "a2dp_source".to_owned()])
    );
    assert_eq!(cfg.roles, None);
    // A later file wins; comments and quotes do not confuse it.
    read_wp_text(
        &mut cfg,
        "b.conf",
        "monitor.bluez.properties = {\n  # bluez5.roles = [ hsp_hs ]\n  bluez5.roles = [ \"a2dp_sink\", hfp_hf ]\n  bluez5.hfphsp-backend = \"native\"\n  bluez5.enable-msbc = false\n  \"bluez5.telephony-dbus-service\": true\n}\n",
    );
    assert_eq!(
        cfg.roles,
        Some(vec!["a2dp_sink".to_owned(), "hfp_hf".to_owned()])
    );
    assert_eq!(cfg.backend.as_deref(), Some("native"));
    assert_eq!(cfg.msbc, Some(false));
    assert_eq!(cfg.telephony, Some(true));
    assert_eq!(cfg.files, vec!["a.conf", "b.conf"]);
    // A key that only contains the name is not the key.
    let mut c2 = WpConfig::default();
    read_wp_text(&mut c2, "c.conf", "xbluez5.roles = [ x ]\n");
    assert_eq!(c2.roles, None);
    assert!(c2.files.is_empty());
}

#[test]
fn files_are_read_in_wireplumbers_order() {
    let root = tempfile::tempdir().unwrap();
    let share = root.path().join("share");
    let etc = root.path().join("etc");
    let home = root.path().join("home");
    for d in [
        share.join("wireplumber/wireplumber.conf.d"),
        etc.join("wireplumber/wireplumber.conf.d"),
        home.join("wireplumber/wireplumber.conf.d"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(
        share.join("wireplumber/wireplumber.conf"),
        "bluez5.roles = [ hfp_hf ]\n",
    )
    .unwrap();
    std::fs::write(
        home.join("wireplumber/wireplumber.conf.d/10-a.conf"),
        "bluez5.roles = [ a2dp_sink ]\n",
    )
    .unwrap();
    std::fs::write(
        home.join("wireplumber/wireplumber.conf.d/20-b.conf"),
        "bluez5.roles = [ a2dp_sink hfp_hf ]\n",
    )
    .unwrap();
    std::fs::write(
        home.join("wireplumber/wireplumber.conf.d/notes.txt"),
        "bluez5.roles = [ x ]",
    )
    .unwrap();
    let cfg = read_wp_config(Some(&home), &etc, &share);
    assert_eq!(
        cfg.roles,
        Some(vec!["a2dp_sink".to_owned(), "hfp_hf".to_owned()])
    );
    assert_eq!(cfg.files.len(), 3);
}

#[test]
fn host_parsers() {
    assert_eq!(parse_powered("Controller X\n\tPowered: yes\n"), Some(true));
    assert_eq!(parse_powered("\tPowered: no\n"), Some(false));
    assert_eq!(parse_powered(""), None);
    let info = "Device AA:BB:CC:00:11:22 (public)\n\tName: Galaxy Z Fold5\n\tPaired: yes\n\tTrusted: no\n\tConnected: yes\n\tUUID: Handsfree Audio Gateway   (0000111f-0000-1000-8000-00805f9b34fb)\n";
    assert_eq!(
        parse_info(info),
        ("Galaxy Z Fold5".into(), true, false, true, true)
    );
    let d = tempfile::tempdir().unwrap();
    for (n, t, soft) in [("rfkill0", "wlan", "1"), ("rfkill1", "bluetooth", "1")] {
        let p = d.path().join(n);
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("type"), format!("{t}\n")).unwrap();
        std::fs::write(p.join("soft"), soft).unwrap();
        std::fs::write(p.join("hard"), "0").unwrap();
    }
    assert_eq!(
        rfkill_in(d.path()),
        vec![Rfkill {
            soft: true,
            hard: false
        }]
    );
    assert!(adapters_in(Path::new("/nonexistent-geekbridge")).is_empty());
}

#[test]
fn without_the_service_the_telephony_and_audio_steps_are_not_checked_rather_than_failed() {
    let mut f = ready_facts();
    f.partial = true;
    f.telephony_present = None;
    f.audio = None;
    for p in &mut f.phones {
        p.hfp_connected = false;
    }
    let s = evaluate(&f);
    for id in ["telephony", "hfp", "card", "devices"] {
        let st = get(&s, id);
        assert_eq!(st.state, State::Skip, "{id}");
        assert!(st.detail.contains("not checked"), "{id}");
    }
    assert!(ready(&s));
}

#[test]
fn the_wireplumber_fix_keeps_the_users_values_and_wins_over_them() {
    // Nothing to fix: no file.
    assert_eq!(wp_dropin(&WpConfig::default()), None);
    // The owner's machine: an A2DP-only auto-connect rule.
    let mut cfg = WpConfig::default();
    read_wp_text(
        &mut cfg,
        "bluetooth-a2dp-autoconnect.conf",
        "monitor.bluez.rules = [ { matches = [ { device.name = \"~bluez_card.*\" } ] actions = { update-props = { bluez5.auto-connect = [ a2dp_sink a2dp_source ] } } } ]\n",
    );
    cfg.roles = Some(vec!["a2dp_sink".into(), "a2dp_source".into()]);
    cfg.backend = Some("ofono".into());
    cfg.telephony = Some(false);
    let text = wp_dropin(&cfg).unwrap();
    // Read back the way WirePlumber would: user file first, ours last.
    let mut after = cfg.clone();
    read_wp_text(&mut after, WP_DROPIN, &text);
    assert_eq!(
        after.roles,
        Some(vec![
            "a2dp_sink".into(),
            "a2dp_source".into(),
            "hfp_hf".into()
        ])
    );
    assert_eq!(after.backend.as_deref(), Some("native"));
    assert_eq!(after.telephony, Some(true));
    assert_eq!(
        after.auto_connect,
        Some(vec![
            "a2dp_sink".into(),
            "a2dp_source".into(),
            "hfp_ag".into()
        ])
    );
    assert!(WP_DROPIN > "bluetooth-a2dp-autoconnect.conf" && WP_DROPIN > "99-anything.conf");
    // With the fix in place the checklist has nothing left to fix.
    assert_eq!(wp_dropin(&after), None);

    // Written beside the user's files, never over them; an older copy of ours is kept.
    let home = tempfile::tempdir().unwrap();
    let p = write_wp_dropin(home.path(), &cfg).unwrap().unwrap();
    assert!(p.ends_with("wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf"));
    write_wp_dropin(home.path(), &cfg).unwrap();
    assert!(p.with_extension("conf.bak").exists());
    assert_eq!(
        write_wp_dropin(home.path(), &WpConfig::default()).unwrap(),
        None
    );
    let read = read_wp_config(
        Some(home.path()),
        &home.path().join("etc"),
        &home.path().join("share"),
    );
    assert_eq!(read.backend.as_deref(), Some("native"));
}

#[test]
fn the_phone_set_up_through_geekbridge_is_the_one_checked() {
    let mut f = ready_facts();
    let mut other = phone(true, true);
    other.address = "11:22:33:44:55:66".into();
    other.name = "Old phone".into();
    other.card = None;
    f.phones.insert(0, other);
    // Without a remembered phone the one with a card wins over the first listed.
    assert_eq!(get(&evaluate(&f), "card").state, State::Ok);
    f.remembered = Some("11:22:33:44:55:66".into());
    let s = evaluate(&f);
    assert!(get(&s, "paired").detail.contains("Old phone"));
    assert_eq!(get(&s, "card").state, State::Fail);
}

#[test]
fn a_headset_only_role_list_is_not_enough_for_calls() {
    // PipeWire creates its telephony service only on the HFP hands-free connection.
    let mut f = ready_facts();
    f.wp.roles = Some(vec!["a2dp_sink".into(), "hsp_hs".into()]);
    f.wp.files = vec!["/home/u/.config/wireplumber/wireplumber.conf.d/x.conf".into()];
    assert_eq!(get(&evaluate(&f), "wireplumber").state, State::Fail);
    let text = wp_dropin(&f.wp).unwrap();
    assert!(
        text.contains("bluez5.roles = [ a2dp_sink hsp_hs hfp_hf ]"),
        "{text}"
    );
}

fn every_fix() -> Vec<WpConfig> {
    let mut v = Vec::new();
    for roles in [
        None,
        Some(vec![]),
        Some(vec!["a2dp_sink".to_owned(), "bap_sink".into()]),
    ] {
        for backend in [None, Some("ofono".to_owned())] {
            for telephony in [None, Some(false)] {
                for auto in [
                    None,
                    Some(vec!["a2dp_sink".to_owned(), "a2dp_source".into()]),
                ] {
                    v.push(WpConfig {
                        roles: roles.clone(),
                        backend: backend.clone(),
                        auto_connect: auto,
                        telephony,
                        ..WpConfig::default()
                    });
                }
            }
        }
    }
    v
}

#[test]
fn every_fragment_geekbridge_can_write_parses() {
    let mut written = 0;
    for cfg in every_fix() {
        if let Some(text) = wp_dropin(&cfg) {
            written += 1;
            check_spa_json(&text).unwrap_or_else(|e| panic!("{e}:\n{text}"));
        }
    }
    assert!(written > 10);
    // The checker knows WirePlumber's own syntax: the shipped main file passes it.
    if let Ok(t) = std::fs::read_to_string("/usr/share/wireplumber/wireplumber.conf") {
        check_spa_json(&t).unwrap();
    }
    check_spa_json("a = { b = [ 1 2 \"x y\" ] c: { d = e } } # note\nf = \"g # not a comment\"\n")
        .unwrap();
    for bad in [
        "monitor.bluez.properties = {\n  bluez5.roles = [ a2dp_sink hfp_hf\n}\n",
        "a = { b = \"x }\n",
        "a = { b }\n",
        "a = [ 1 } ]\n",
        "a = { b = c\n",
        "a = ]\n",
        "a\n",
    ] {
        assert!(check_spa_json(bad).is_err(), "accepted: {bad:?}");
    }
    assert!(
        check_spa_json("x = {\n\n  y = [ ]\n  z = \"open\n}")
            .unwrap_err()
            .contains("line 4")
    );
}

#[test]
fn a_fragment_wireplumber_does_not_survive_is_taken_back() {
    let mut cfg = WpConfig {
        roles: Some(vec!["a2dp_sink".into()]),
        ..WpConfig::default()
    };
    let home = tempfile::tempdir().unwrap();
    let file = home
        .path()
        .join("wireplumber/wireplumber.conf.d")
        .join(WP_DROPIN);
    let mut restarts = 0;
    // WirePlumber keeps running: applied.
    let r = apply_wp_dropin(
        home.path(),
        &cfg,
        &mut || {
            restarts += 1;
            Ok(())
        },
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(r, WpFix::Applied(file.clone()));
    assert_eq!(restarts, 1);
    let first = std::fs::read_to_string(&file).unwrap();
    // A second fix that WirePlumber does not survive: the first copy comes back.
    cfg.backend = Some("ofono".into());
    let r = apply_wp_dropin(
        home.path(),
        &cfg,
        &mut || {
            restarts += 1;
            Ok(())
        },
        &mut || Err("wireplumber.service is failed".into()),
    )
    .unwrap();
    assert!(
        matches!(r, WpFix::RolledBack { ref why, .. } if why.contains("failed")),
        "{r:?}"
    );
    assert_eq!(
        restarts, 3,
        "restarted again after taking the fragment back"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
    // No earlier copy: the fragment is removed altogether.
    let home2 = tempfile::tempdir().unwrap();
    let r = apply_wp_dropin(home2.path(), &cfg, &mut || Ok(()), &mut || {
        Err("crashed".into())
    })
    .unwrap();
    assert!(matches!(r, WpFix::RolledBack { .. }));
    assert!(
        !home2
            .path()
            .join("wireplumber/wireplumber.conf.d")
            .join(WP_DROPIN)
            .exists()
    );
    assert!(
        !home2
            .path()
            .join("wireplumber/wireplumber.conf.d")
            .join("zz-geekbridge-call-audio.conf.bak")
            .exists()
    );
    // Not restarted (no user systemd): written, and said so.
    let r = apply_wp_dropin(
        home2.path(),
        &cfg,
        &mut || Err("no systemd".into()),
        &mut || Ok(()),
    )
    .unwrap();
    assert!(matches!(r, WpFix::NotRestarted { ref why, .. } if why == "no systemd"));
    // Nothing temporary is left where WirePlumber reads.
    let left: Vec<_> = std::fs::read_dir(home2.path().join("wireplumber/wireplumber.conf.d"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(left, vec![WP_DROPIN.to_owned()]);
}

/// Security review 7 (S7-12): "no overwrite of user files". A link planted at the backup or temporary name must be
/// replaced, never written through: `std::fs::copy` and `fs::write` follow links.
#[cfg(unix)]
#[test]
fn the_drop_in_never_writes_through_a_link_at_its_backup_or_temporary_name() {
    let cfg = WpConfig {
        roles: Some(vec!["a2dp_sink".into()]),
        ..WpConfig::default()
    };
    let home = tempfile::tempdir().unwrap();
    let victim = home.path().join("victim.txt");
    std::fs::write(&victim, "the user's own file").unwrap();
    let p = write_wp_dropin(home.path(), &cfg).unwrap().unwrap();
    std::os::unix::fs::symlink(&victim, p.with_extension("conf.bak")).unwrap();
    std::os::unix::fs::symlink(&victim, p.with_extension("conf.tmp")).unwrap();
    write_wp_dropin(home.path(), &cfg).unwrap();
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "the user's own file"
    );
    assert!(
        std::fs::read_to_string(p.with_extension("conf.bak"))
            .unwrap()
            .contains("hfp_hf")
    );
}
