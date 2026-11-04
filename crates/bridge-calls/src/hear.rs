// SPDX-License-Identifier: GPL-3.0-or-later
//! "Hear calls on this computer": the step-by-step checklist behind `geekbridge-ctl calling` and
//! the doctor's call-audio line.
//!
//! Every step a Linux desktop needs before a phone call's voice can reach the computer's
//! headphones is checked in order, each with what was found and the exact command that fixes it:
//! adapter, rfkill, `bluetooth.service`, adapter power, WirePlumber's hands-free role and
//! backend, PipeWire's telephony service, the phone paired/trusted/connected, its hands-free
//! profile connected, its PipeWire card, and the devices the call will use.
//!
//! [`evaluate`] is pure over [`Facts`] so every outcome is tested with canned facts; [`gather`]
//! (Linux) reads sysfs, `systemctl`, `bluetoothctl`, the WirePlumber configuration, the telephony
//! service and the sound server. Nothing here changes anything.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// HFP audio gateway service class (the phone side of hands-free).
pub const HFP_AG_UUID: &str = "0000111f-0000-1000-8000-00805f9b34fb";
pub const REPORT: &str = "docs/reports/call-audio-pc-headset.md";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Ok,
    /// Works, but something will bite later (e.g. not trusted, card off).
    Warn,
    Fail,
    /// Not checked because an earlier step failed or it only applies during a call.
    Skip,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Step {
    pub id: &'static str,
    pub title: String,
    pub state: State,
    pub detail: String,
    pub fix: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Rfkill {
    pub soft: bool,
    pub hard: bool,
}

/// The `bluez5.*` settings WirePlumber will hand to PipeWire, as far as its files say.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WpConfig {
    /// `bluez5.roles`; `None` = not set (PipeWire's default includes `hfp_hf`).
    pub roles: Option<Vec<String>>,
    /// `bluez5.hfphsp-backend`; `None` = default (`native`).
    pub backend: Option<String>,
    /// `bluez5.auto-connect` from any rule or property; `None` = not set.
    pub auto_connect: Option<Vec<String>>,
    /// `bluez5.telephony-dbus-service`; `None` = default (on).
    pub telephony: Option<bool>,
    /// `bluez5.enable-msbc`; `None` = default (on).
    pub msbc: Option<bool>,
    /// Files that set any of the above, in the order read.
    pub files: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PhoneFacts {
    pub name: String,
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    /// The phone advertises the hands-free audio gateway service.
    pub hfp_gateway: bool,
    /// PipeWire's telephony service has an audio gateway for it (HFP connected).
    pub hfp_connected: bool,
    /// PipeWire card name, if PipeWire shows one.
    pub card: Option<String>,
    pub card_profile: Option<String>,
    pub card_has_gateway_profile: bool,
    /// The two call streams exist (a call's voice link is open right now).
    pub voice_streams: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AudioFacts {
    pub server_name: String,
    pub pipewire: bool,
    /// (name, description) the call will play on, and whether it is the user's pick.
    pub output: Option<(String, String, bool)>,
    pub input: Option<(String, String, bool)>,
    /// A pick that is not connected.
    pub missing_pick: Vec<String>,
    pub input_muted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Facts {
    pub adapters: Vec<String>,
    pub btusb_loaded: bool,
    pub rfkill: Vec<Rfkill>,
    pub service_active: Option<bool>,
    pub powered: Option<bool>,
    pub wireplumber_running: Option<bool>,
    pub wp: WpConfig,
    pub telephony_present: Option<bool>,
    pub phones: Vec<PhoneFacts>,
    /// `None`: the sound server could not be read.
    pub audio: Option<AudioFacts>,
    /// `bluetoothctl` could be run.
    pub bluetoothctl: bool,
    /// Only the basic facts were read (no telephony service, no sound server): the steps that
    /// need them say "not checked" instead of failing.
    pub partial: bool,
    /// Address of the phone set up through GeekBridge (`call-audio-phone.json`), checked first.
    pub remembered: Option<String>,
}

fn step(
    id: &'static str,
    title: &'static str,
    state: State,
    detail: impl Into<String>,
    fix: Option<String>,
) -> Step {
    Step {
        id,
        title: title.to_owned(),
        state,
        detail: detail.into(),
        fix,
    }
}

const WP_FRAGMENT: &str = "~/.config/wireplumber/wireplumber.conf.d/zz-geekbridge-call-audio.conf";

fn wp_fix(lines: &str) -> String {
    format!(
        "geekbridge-ctl call setup --fix-config does this for you (no sudo); by hand: create {WP_FRAGMENT} with:\n  monitor.bluez.properties = {{\n{lines}  }}\nthen: systemctl --user restart wireplumber"
    )
}

/// The ordered checklist. Later steps are skipped when an earlier one makes them meaningless.
pub fn evaluate(f: &Facts) -> Vec<Step> {
    let mut out = Vec::new();
    let skip = |id, title| {
        step(
            id,
            title,
            State::Skip,
            "skipped: fix the step above first",
            None,
        )
    };
    let unchecked = |id, title| {
        step(
            id,
            title,
            State::Skip,
            "not checked: the GeekBridge service checks this (start it: systemctl --user start geekbridge-service)",
            None,
        )
    };

    // 1. adapter
    let have_adapter = !f.adapters.is_empty();
    out.push(if have_adapter {
        step(
            "adapter",
            "Bluetooth adapter present",
            State::Ok,
            format!("controller {}", f.adapters.join(", ")),
            None,
        )
    } else {
        step(
            "adapter",
            "Bluetooth adapter present",
            State::Fail,
            format!(
                "no Bluetooth controller (/sys/class/bluetooth is empty{}). Answering from here still works; the voice stays on the phone.",
                if f.btusb_loaded { "; the btusb driver is loaded but found no device" } else { "" }
            ),
            Some("plug in a USB Bluetooth adapter (Bluetooth 4.0+ dual-mode Classic+LE; HFP needs Classic, not LE-only), straight into a USB port, then: dmesg | grep -i -E 'bluetooth|btusb|firmware' | tail; if it says firmware is missing: sudo pacman -S --needed linux-firmware and replug".into()),
        )
    });

    // 2. rfkill
    let hard = f.rfkill.iter().any(|r| r.hard);
    let soft = f.rfkill.iter().any(|r| r.soft);
    out.push(if !have_adapter {
        skip("rfkill", "Bluetooth radio not blocked (rfkill)")
    } else if hard {
        step(
            "rfkill",
            "Bluetooth radio not blocked (rfkill)",
            State::Fail,
            "hard-blocked (a hardware switch, key or BIOS setting)",
            Some(
                "turn the wireless switch/key on or enable Bluetooth in the firmware setup".into(),
            ),
        )
    } else if soft {
        step(
            "rfkill",
            "Bluetooth radio not blocked (rfkill)",
            State::Fail,
            "soft-blocked",
            Some("rfkill unblock bluetooth".into()),
        )
    } else {
        step(
            "rfkill",
            "Bluetooth radio not blocked (rfkill)",
            State::Ok,
            "not blocked",
            None,
        )
    });

    // 3. service
    // Unknown (systemd could not be asked) does not block the later steps.
    let service_ok = f.service_active != Some(false);
    out.push(match f.service_active {
        Some(true) => step("service", "bluetooth.service running", State::Ok, "active", None),
        Some(false) => step(
            "service",
            "bluetooth.service running",
            State::Fail,
            "not active",
            Some("sudo pacman -S --needed bluez bluez-utils && sudo systemctl enable --now bluetooth.service".into()),
        ),
        None => step("service", "bluetooth.service running", State::Warn, "could not ask systemd", Some("systemctl status bluetooth.service".into())),
    });

    // 4. powered
    out.push(if !have_adapter || !service_ok {
        skip("powered", "Adapter powered on")
    } else {
        match f.powered {
            Some(true) => step("powered", "Adapter powered on", State::Ok, "powered", None),
            Some(false) => step("powered", "Adapter powered on", State::Fail, "powered off", Some("bluetoothctl power on (to keep it on after reboots: AutoEnable=true under [Policy] in /etc/bluetooth/main.conf)".into())),
            None if !f.bluetoothctl => step("powered", "Adapter powered on", State::Fail, "bluetoothctl is not installed", Some("sudo pacman -S --needed bluez-utils".into())),
            None => step("powered", "Adapter powered on", State::Warn, "bluetoothctl show gave no answer", Some("bluetoothctl show".into())),
        }
    });

    // 5. WirePlumber role and backend
    // Only HFP carries calls: PipeWire's telephony service is created on the HFP hands-free
    // service-level connection (spa/plugins/bluez5/backend-native.c); `hsp_hs` alone is not enough.
    let roles_ok =
        f.wp.roles
            .as_ref()
            .is_none_or(|r| r.iter().any(|x| x == "hfp_hf"));
    let backend = f.wp.backend.as_deref().unwrap_or("native");
    let backend_ok = matches!(backend, "native" | "any");
    let wp_running = f.wireplumber_running != Some(false);
    out.push(if !wp_running {
        step("wireplumber", "PipeWire hands-free role (hfp_hf, native backend)", State::Fail, "WirePlumber is not running", Some("systemctl --user enable --now pipewire pipewire-pulse wireplumber".into()))
    } else if !roles_ok {
        step(
            "wireplumber",
            "PipeWire hands-free role (hfp_hf, native backend)",
            State::Fail,
            format!(
                "bluez5.roles = [ {} ] leaves out hfp_hf, so this computer cannot be a phone's hands-free unit (set in {})",
                f.wp.roles.as_deref().unwrap_or_default().join(" "),
                f.wp.files.join(", ")
            ),
            Some(wp_fix("    bluez5.roles = [ a2dp_sink a2dp_source bap_sink bap_source hfp_hf hfp_ag ]\n")),
        )
    } else if !backend_ok {
        step(
            "wireplumber",
            "PipeWire hands-free role (hfp_hf, native backend)",
            State::Fail,
            format!("bluez5.hfphsp-backend = {backend}: GeekBridge drives PipeWire's own (native) hands-free backend; with {backend} the telephony service it uses does not exist"),
            Some(wp_fix("    bluez5.hfphsp-backend = \"native\"\n")),
        )
    } else {
        step(
            "wireplumber",
            "PipeWire hands-free role (hfp_hf, native backend)",
            State::Ok,
            format!(
                "roles {}, backend {backend}{}",
                f.wp.roles.as_ref().map_or("default (includes hfp_hf)".to_owned(), |r| r.join(" ")),
                if f.wp.msbc == Some(false) { ", wideband (mSBC) off" } else { "" }
            ),
            None,
        )
    });

    // 6. telephony service
    let tel_ok = f.telephony_present == Some(true);
    out.push(match (f.telephony_present, f.wp.telephony) {
        (None, None) if f.partial => unchecked("telephony", "PipeWire telephony service (org.pipewire.Telephony)"),
        (Some(true), _) => step("telephony", "PipeWire telephony service (org.pipewire.Telephony)", State::Ok, "on the session bus", None),
        (_, Some(false)) => step("telephony", "PipeWire telephony service (org.pipewire.Telephony)", State::Fail, "switched off by bluez5.telephony-dbus-service = false", Some(wp_fix("    bluez5.telephony-dbus-service = true\n"))),
        (Some(false), _) => step("telephony", "PipeWire telephony service (org.pipewire.Telephony)", State::Fail, "not on the session bus", Some("systemctl --user restart wireplumber; then: busctl --user status org.pipewire.Telephony".into())),
        (None, _) => step("telephony", "PipeWire telephony service (org.pipewire.Telephony)", State::Warn, "the session bus could not be read", None),
    });

    // 7-10. the phone
    let gateways: Vec<&PhoneFacts> = f.phones.iter().filter(|p| p.hfp_gateway).collect();
    let phone = gateways
        .iter()
        .find(|p| {
            f.remembered
                .as_deref()
                .is_some_and(|r| r.eq_ignore_ascii_case(&p.address))
        })
        .or_else(|| {
            gateways
                .iter()
                .find(|p| p.hfp_connected && p.card.is_some())
        })
        .or_else(|| gateways.iter().find(|p| p.hfp_connected))
        .or_else(|| gateways.iter().find(|p| p.connected))
        .or_else(|| gateways.first())
        .copied();
    let blocked = !have_adapter || !service_ok || f.powered == Some(false);
    let label = |p: &PhoneFacts| {
        if p.name.is_empty() {
            p.address.clone()
        } else {
            format!("{} ({})", p.name, p.address)
        }
    };
    out.push(match phone {
        _ if blocked => skip("paired", "Phone paired and trusted"),
        None => step(
            "paired",
            "Phone paired and trusted",
            State::Fail,
            "no paired phone offers hands-free calling",
            Some("geekbridge-ctl call setup (pairs, trusts and connects the phone; compare the six-digit code on both screens). By hand: bluetoothctl, then scan on, pair ADDR, trust ADDR, connect ADDR".into()),
        ),
        Some(p) if !p.trusted => step(
            "paired",
            "Phone paired and trusted",
            State::Warn,
            format!("{} is paired but not trusted, so it will not reconnect by itself", label(p)),
            Some(format!("bluetoothctl trust {}", p.address)),
        ),
        Some(p) => step("paired", "Phone paired and trusted", State::Ok, label(p), None),
    });
    out.push(match phone {
        _ if blocked => skip("connected", "Phone connected"),
        None => skip("connected", "Phone connected"),
        Some(p) if !p.connected => step(
            "connected",
            "Phone connected",
            State::Fail,
            "paired but not connected (out of range, Bluetooth off on the phone, or connected to something else)",
            Some(format!("bluetoothctl connect {}", p.address)),
        ),
        Some(_) => step("connected", "Phone connected", State::Ok, "connected", None),
    });
    let auto = f.wp.auto_connect.as_ref();
    let auto_misses_hfp = auto.is_some_and(|a| !a.iter().any(|x| x == "hfp_ag" || x == "hsp_ag"));
    out.push(match phone {
        _ if f.partial && !blocked => unchecked("hfp", "Phone's calls (hands-free) profile connected"),
        _ if blocked || !tel_ok => skip("hfp", "Phone's calls (hands-free) profile connected"),
        Some(p) if p.connected && !p.hfp_connected => step(
            "hfp",
            "Phone's calls (hands-free) profile connected",
            State::Fail,
            format!(
                "the phone is connected but not as a hands-free device{}",
                if auto_misses_hfp { "; also bluez5.auto-connect lists no hfp_ag, so PipeWire never asks for it" } else { "" }
            ),
            Some(format!(
                "on the phone: Settings > Connections > Bluetooth > gear icon next to this computer > turn on Calls (Samsung One UI; \"Phone calls\" on Pixel/stock Android). Or from here: bluetoothctl connect {} {HFP_AG_UUID}{}",
                p.address,
                if auto_misses_hfp {
                    format!(". To reconnect it automatically add hfp_ag to bluez5.auto-connect (e.g. [ a2dp_sink a2dp_source hfp_ag ]) in the rule that sets it ({})", f.wp.files.join(", "))
                } else {
                    String::new()
                }
            )),
        ),
        Some(p) if p.hfp_connected => step(
            "hfp",
            "Phone's calls (hands-free) profile connected",
            if auto_misses_hfp { State::Warn } else { State::Ok },
            if auto_misses_hfp {
                "connected now, but bluez5.auto-connect lists no hfp_ag: after a reconnect only music (A2DP) may come back".to_owned()
            } else {
                "PipeWire telephony has the phone as an audio gateway".to_owned()
            },
            auto_misses_hfp.then(|| format!("add hfp_ag to bluez5.auto-connect (e.g. [ a2dp_sink a2dp_source hfp_ag ]) in {}, then systemctl --user restart wireplumber", f.wp.files.join(", "))),
        ),
        _ => skip("hfp", "Phone's calls (hands-free) profile connected"),
    });
    out.push(match (phone, f.audio.as_ref()) {
        _ if f.partial => unchecked("card", "Phone's audio card in PipeWire (audio-gateway)"),
        (Some(p), Some(_)) if p.hfp_connected => match (&p.card, p.card_has_gateway_profile, p.card_profile.as_deref()) {
            (None, _, _) => step("card", "Phone's audio card in PipeWire (audio-gateway)", State::Fail, "PipeWire shows no card for the phone", Some("systemctl --user restart wireplumber; then: pactl list cards short | grep bluez".into())),
            (Some(c), false, _) => step("card", "Phone's audio card in PipeWire (audio-gateway)", State::Fail, format!("{c} offers no audio-gateway profile"), Some(format!("bluetoothctl disconnect {a} && bluetoothctl connect {a}", a = p.address))),
            (Some(c), true, Some("audio-gateway")) => step("card", "Phone's audio card in PipeWire (audio-gateway)", State::Ok, format!("{c}: audio-gateway"), None),
            (Some(c), true, prof) => step(
                "card",
                "Phone's audio card in PipeWire (audio-gateway)",
                State::Warn,
                format!("{c} is in profile {}: no call audio until it is audio-gateway (GeekBridge switches it for calls answered here and switches it back afterwards)", prof.unwrap_or("none")),
                Some(format!("pactl set-card-profile {c} audio-gateway")),
            ),
        },
        _ => skip("card", "Phone's audio card in PipeWire (audio-gateway)"),
    });

    // 11. local devices
    out.push(match f.audio.as_ref() {
        None if f.partial => unchecked("devices", "Headphones and microphone for calls"),
        None => step("devices", "Headphones and microphone for calls", State::Fail, "the sound server could not be read", Some("systemctl --user status pipewire pipewire-pulse wireplumber".into())),
        Some(a) if !a.pipewire => step("devices", "Headphones and microphone for calls", State::Warn, format!("{}: not PipeWire; calls go to the default devices and GeekBridge cannot route or mute them", a.server_name), None),
        Some(a) => {
            let show = |d: &Option<(String, String, bool)>| match d {
                Some((_, desc, true)) => format!("{desc} (your pick)"),
                Some((_, desc, false)) => format!("{desc} (system default)"),
                None => "none".to_owned(),
            };
            let detail = format!("output: {}; microphone: {}{}", show(&a.output), show(&a.input), if a.input_muted { " (muted)" } else { "" });
            if a.output.is_none() || a.input.is_none() {
                step("devices", "Headphones and microphone for calls", State::Fail, detail, Some("connect headphones and a microphone, then pick them: geekbridge-ctl call audio-devices".into()))
            } else if !a.missing_pick.is_empty() || a.input_muted {
                step(
                    "devices",
                    "Headphones and microphone for calls",
                    State::Warn,
                    if a.missing_pick.is_empty() { detail } else { format!("{detail}; not connected: {}", a.missing_pick.join(", ")) },
                    Some(if a.input_muted { "unmute the microphone (wpctl set-mute @DEFAULT_AUDIO_SOURCE@ 0) or pick another: geekbridge-ctl call audio-devices --input NAME".into() } else { "geekbridge-ctl call audio-devices (list and pick)".into() }),
                )
            } else {
                step("devices", "Headphones and microphone for calls", State::Ok, detail, None)
            }
        }
    });

    // 12. during a call
    out.push(match phone {
        Some(p) if p.voice_streams => step(
            "voice",
            "Call voice link open now",
            State::Ok,
            "the phone is sending a call's audio here",
            None,
        ),
        _ => step(
            "voice",
            "Call voice link open now",
            State::Skip,
            "no call audio right now (checked again during a call: geekbridge-ctl calls)",
            None,
        ),
    });
    out
}

/// GeekBridge's own WirePlumber fragment. It sorts after the usual `NN-name.conf` and
/// `bluetooth-*.conf` files, so its rule is applied last and wins; no user file is edited.
pub const WP_DROPIN: &str = "zz-geekbridge-call-audio.conf";

/// The WirePlumber fragment that fixes what the checklist found in the configuration (missing
/// `hfp_hf` role, a non-native backend, telephony off, an auto-connect list without `hfp_ag`),
/// keeping every other value the user set. `None` when nothing needs fixing.
pub fn wp_dropin(cfg: &WpConfig) -> Option<String> {
    let mut props = Vec::new();
    if let Some(roles) = &cfg.roles
        && !roles.iter().any(|r| r == "hfp_hf")
    {
        let mut r = roles.clone();
        r.push("hfp_hf".into());
        props.push(format!("    bluez5.roles = [ {} ]", r.join(" ")));
    }
    if cfg
        .backend
        .as_deref()
        .is_some_and(|b| !matches!(b, "native" | "any"))
    {
        props.push("    bluez5.hfphsp-backend = \"native\"".into());
    }
    if cfg.telephony == Some(false) {
        props.push("    bluez5.telephony-dbus-service = true".into());
    }
    let auto = cfg
        .auto_connect
        .as_ref()
        .filter(|a| !a.iter().any(|x| x == "hfp_ag"))
        .map(|a| {
            let mut a = a.clone();
            a.push("hfp_ag".into());
            a
        });
    if props.is_empty() && auto.is_none() {
        return None;
    }
    let mut out = String::from(
        "## Written by GeekBridge (\"Set up call audio\") so a phone's calls can play on this computer.\n## Delete this file to undo it, then: systemctl --user restart wireplumber\n",
    );
    if !props.is_empty() {
        out.push_str(&format!(
            "monitor.bluez.properties = {{\n{}\n}}\n",
            props.join("\n")
        ));
    }
    if let Some(a) = auto {
        out.push_str(&format!(
            "## Also reconnect a phone's calls (hands-free) profile, not only music.\nmonitor.bluez.rules = [\n  {{\n    matches = [ {{ device.name = \"~bluez_card.*\" }} ]\n    actions = {{ update-props = {{ bluez5.auto-connect = [ {} ] }} }}\n  }}\n]\n",
            a.join(" ")
        ));
    }
    Some(out)
}

/// Write [`wp_dropin`] into `<config_home>/wireplumber/wireplumber.conf.d/`. An earlier copy of
/// GeekBridge's own file is kept as `.bak`; user files are never touched. Returns the file
/// written, or `None` when nothing needed fixing. WirePlumber must be restarted afterwards.
///
/// A WirePlumber that cannot parse a fragment does not start, and with it goes all the user's
/// sound. So the text is checked before it is put in place: by [`check_spa_json`], and by
/// PipeWire's own parser (`spa-json-dump`, PipeWire 1.x) when it is installed. A text that fails
/// either is not written (`InvalidData`).
pub fn write_wp_dropin(config_home: &Path, cfg: &WpConfig) -> std::io::Result<Option<PathBuf>> {
    let Some(text) = wp_dropin(cfg) else {
        return Ok(None);
    };
    check_spa_json(&text).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "the WirePlumber fragment GeekBridge made does not parse ({e}); nothing was written"
            ),
        )
    })?;
    let dir = config_home.join("wireplumber/wireplumber.conf.d");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(WP_DROPIN);
    // The temporary name does not end in `.conf`, so WirePlumber never reads it.
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, &text)?;
    if let Some(Err(e)) = spa_json_dump(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "PipeWire's parser rejects the WirePlumber fragment ({e}); nothing was written"
            ),
        ));
    }
    let bak = path.with_extension("conf.bak");
    if path.exists() {
        std::fs::copy(&path, &bak)?;
    } else if bak.exists() {
        // A backup from an older, since deleted copy must not come back on an undo.
        std::fs::remove_file(&bak)?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(Some(path))
}

/// Take GeekBridge's fragment back: the previous copy if there was one (`.bak`), else none.
pub fn undo_wp_dropin(config_home: &Path) -> std::io::Result<()> {
    let path = config_home
        .join("wireplumber/wireplumber.conf.d")
        .join(WP_DROPIN);
    let bak = path.with_extension("conf.bak");
    if bak.exists() {
        std::fs::rename(&bak, &path)
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// What [`apply_wp_dropin`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WpFix {
    /// The configuration needs no change; nothing was written.
    NotNeeded,
    /// Written, WirePlumber restarted and still running.
    Applied(PathBuf),
    /// Written but WirePlumber was not restarted (`why`); it takes effect at the next restart.
    NotRestarted { path: PathBuf, why: String },
    /// WirePlumber did not keep running with it (`why`): the fragment was taken back and
    /// WirePlumber restarted again with the previous configuration.
    RolledBack { path: PathBuf, why: String },
}

/// Write the fragment, restart WirePlumber and make sure it still runs; if it does not, take the
/// fragment back and restart once more, so a GeekBridge change can never leave the user without
/// sound. `restart` restarts WirePlumber; `healthy` says whether it runs after the restart.
pub fn apply_wp_dropin(
    config_home: &Path,
    cfg: &WpConfig,
    restart: &mut dyn FnMut() -> Result<(), String>,
    healthy: &mut dyn FnMut() -> Result<(), String>,
) -> std::io::Result<WpFix> {
    let Some(path) = write_wp_dropin(config_home, cfg)? else {
        return Ok(WpFix::NotNeeded);
    };
    if let Err(why) = restart() {
        return Ok(WpFix::NotRestarted { path, why });
    }
    match healthy() {
        Ok(()) => Ok(WpFix::Applied(path)),
        Err(why) => {
            undo_wp_dropin(config_home)?;
            let again = restart();
            Ok(WpFix::RolledBack {
                path,
                why: match again {
                    Ok(()) => why,
                    Err(e) => format!(
                        "{why}; restarting it again also failed ({e}): systemctl --user restart wireplumber"
                    ),
                },
            })
        }
    }
}

/// PipeWire's own SPA-JSON parser on `file`, if installed: `None` = not available.
fn spa_json_dump(file: &Path) -> Option<Result<(), String>> {
    let out = std::process::Command::new("spa-json-dump")
        .arg(file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .ok()?;
    Some(if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    })
}

/// A strict-enough check of WirePlumber's SPA-JSON (the relaxed JSON of its `.conf` files):
/// balanced `{}`/`[]`, closed strings, objects made of key/value pairs, `#` comments. It accepts
/// everything GeekBridge writes and what WirePlumber's own files use; it is a guard against
/// writing something broken, not a full parser.
pub fn check_spa_json(text: &str) -> Result<(), String> {
    struct P<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn line(&self) -> usize {
            1 + self.b[..self.i.min(self.b.len())]
                .iter()
                .filter(|c| **c == b'\n')
                .count()
        }
        fn err(&self, what: &str) -> String {
            format!("{what} on line {}", self.line())
        }
        /// Whitespace, commas, `=`/`:` separators and comments.
        fn skip(&mut self) {
            while let Some(&c) = self.b.get(self.i) {
                match c {
                    b' ' | b'\t' | b'\r' | b'\n' | b',' | b'=' | b':' => self.i += 1,
                    b'#' => {
                        while self.b.get(self.i).is_some_and(|c| *c != b'\n') {
                            self.i += 1;
                        }
                    }
                    _ => break,
                }
            }
        }
        fn string(&mut self) -> Result<(), String> {
            self.i += 1;
            loop {
                match self.b.get(self.i) {
                    None | Some(b'\n') => return Err(self.err("unterminated string")),
                    Some(b'\\') => self.i += 2,
                    Some(b'"') => {
                        self.i += 1;
                        return Ok(());
                    }
                    Some(_) => self.i += 1,
                }
            }
        }
        fn bare(&mut self) {
            while self
                .b
                .get(self.i)
                .is_some_and(|c| !c.is_ascii_whitespace() && !b"{}[]=:,\"#".contains(c))
            {
                self.i += 1;
            }
        }
        fn scalar(&mut self) -> Result<(), String> {
            match self.b.get(self.i) {
                Some(b'"') => self.string(),
                Some(c) if b"{}[]".contains(c) => Err(self.err("a key or value expected")),
                Some(_) => {
                    self.bare();
                    Ok(())
                }
                None => Err(self.err("a value expected")),
            }
        }
        fn value(&mut self, depth: usize) -> Result<(), String> {
            if depth > 64 {
                return Err(self.err("nested too deeply"));
            }
            self.skip();
            match self.b.get(self.i) {
                Some(b'{') => {
                    self.i += 1;
                    self.members(Some(b'}'), depth + 1)
                }
                Some(b'[') => {
                    self.i += 1;
                    loop {
                        self.skip();
                        match self.b.get(self.i) {
                            None => return Err(self.err("unclosed [")),
                            Some(b']') => {
                                self.i += 1;
                                return Ok(());
                            }
                            Some(b'}') => return Err(self.err("} where ] was expected")),
                            Some(_) => self.value(depth + 1)?,
                        }
                    }
                }
                Some(b'}' | b']') => Err(self.err("a value expected")),
                _ => self.scalar(),
            }
        }
        /// Key/value pairs up to `close` (`None`: the end of the text).
        fn members(&mut self, close: Option<u8>, depth: usize) -> Result<(), String> {
            loop {
                self.skip();
                match (self.b.get(self.i).copied(), close) {
                    (None, None) => return Ok(()),
                    (None, Some(_)) => return Err(self.err("unclosed {")),
                    (Some(c), Some(x)) if c == x => {
                        self.i += 1;
                        return Ok(());
                    }
                    (Some(b'}' | b']'), _) => return Err(self.err("unexpected closing bracket")),
                    _ => {
                        self.scalar()?;
                        self.skip();
                        if self.b.get(self.i).is_none_or(|c| b"}]".contains(c)) {
                            return Err(self.err("a key without a value"));
                        }
                        self.value(depth)?;
                    }
                }
            }
        }
    }
    let mut p = P {
        b: text.as_bytes(),
        i: 0,
    };
    p.members(None, 0)
}

/// True when nothing failed (warnings allowed).
pub fn ready(steps: &[Step]) -> bool {
    !steps.iter().any(|s| s.state == State::Fail)
}

/// The first failing step, else the first warning.
pub fn first_problem(steps: &[Step]) -> Option<&Step> {
    steps
        .iter()
        .find(|s| s.state == State::Fail)
        .or_else(|| steps.iter().find(|s| s.state == State::Warn))
}

/// Is the missing adapter the problem? Then every place that shows calls shows
/// [`bridge_core::ipc::NO_BLUETOOTH_WARNING`].
pub fn no_bluetooth(steps: &[Step]) -> bool {
    steps
        .iter()
        .any(|s| s.id == "adapter" && s.state == State::Fail)
}

/// One line: ready, the no-Bluetooth warning, or the first open step.
pub fn headline(steps: &[Step]) -> String {
    if no_bluetooth(steps) {
        return bridge_core::ipc::NO_BLUETOOTH_WARNING.to_owned();
    }
    match first_problem(steps) {
        None => "Ready: answer a call on this computer and you talk through its headphones and microphone.".to_owned(),
        Some(p) => format!("{}: {}", p.title, p.detail),
    }
}

/// The stable form the apps read (`Response::HearCalls`).
pub fn to_ipc(
    steps: &[Step],
    setup: bridge_core::ipc::CallAudioSetupState,
    offer_setup: bool,
) -> bridge_core::ipc::HearCalls {
    bridge_core::ipc::HearCalls {
        ready: ready(steps),
        no_bluetooth: no_bluetooth(steps),
        headline: headline(steps),
        steps: steps
            .iter()
            .map(|s| bridge_core::ipc::HearCallsStep {
                id: s.id.to_owned(),
                title: s.title.clone(),
                state: match s.state {
                    State::Ok => "ok",
                    State::Warn => "warn",
                    State::Fail => "fail",
                    State::Skip => "skip",
                }
                .to_owned(),
                detail: s.detail.clone(),
                fix: s.fix.clone(),
            })
            .collect(),
        setup,
        offer_setup,
    }
}

pub fn render(steps: &[Step]) -> String {
    to_ipc(steps, Default::default(), false).render()
}

// ---------------------------------------------------------------------- parsing helpers

/// Where the kernel lists Bluetooth controllers. `GEEKBRIDGE_BT_SYSFS` overrides it for the
/// call-audio sandbox (scripts/e2e/call-audio-sandbox.sh), which has no kernel controller.
pub fn bt_sysfs() -> PathBuf {
    std::env::var_os("GEEKBRIDGE_BT_SYSFS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/bluetooth"))
}

/// Controllers (`hciN`) under a sysfs class directory.
pub fn adapters_in(class_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(class_dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.strip_prefix("hci")
                        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
                        .then_some(n)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Bluetooth entries under `/sys/class/rfkill`.
pub fn rfkill_in(class_dir: &Path) -> Vec<Rfkill> {
    let read = |p: PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_owned());
    std::fs::read_dir(class_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| read(e.path().join("type")).is_ok_and(|t| t == "bluetooth"))
                .map(|e| Rfkill {
                    soft: read(e.path().join("soft")).is_ok_and(|v| v == "1"),
                    hard: read(e.path().join("hard")).is_ok_and(|v| v == "1"),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `Powered: yes|no` from `bluetoothctl show`.
pub fn parse_powered(show: &str) -> Option<bool> {
    show.lines().find_map(|l| {
        let v = l.trim().strip_prefix("Powered:")?.trim();
        Some(v == "yes")
    })
}

/// `bluetoothctl info ADDR` → (name, paired, trusted, connected, hfp gateway).
pub fn parse_info(info: &str) -> (String, bool, bool, bool, bool) {
    let mut r = (String::new(), false, false, false, false);
    for l in info.lines() {
        let l = l.trim();
        let yes = |k: &str| l.strip_prefix(k).is_some_and(|v| v.trim() == "yes");
        if let Some(n) = l.strip_prefix("Name:") {
            r.0 = n.trim().to_owned();
        }
        r.1 |= yes("Paired:");
        r.2 |= yes("Trusted:");
        r.3 |= yes("Connected:");
        r.4 |= l.to_ascii_lowercase().starts_with("uuid:")
            && l.to_ascii_lowercase().contains("0000111f-");
    }
    r
}

fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| {
            // SPA-JSON comments start with '#' outside a string.
            let mut in_str = false;
            let mut end = l.len();
            for (i, c) in l.char_indices() {
                match c {
                    '"' => in_str = !in_str,
                    '#' if !in_str => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            &l[..end]
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The value of every `key = value` / `key: value` in SPA-JSON text, in order.
fn values_of(text: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(pos) = rest.find(key) {
        let before_ok = rest[..pos]
            .chars()
            .last()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | ',' | '"'));
        let after = &rest[pos + key.len()..];
        rest = after;
        if !before_ok {
            continue;
        }
        let after = after.trim_start_matches('"').trim_start();
        let Some(after) = after.strip_prefix(['=', ':']) else {
            continue;
        };
        let after = after.trim_start();
        let value = if let Some(arr) = after.strip_prefix('[') {
            arr.split(']').next().unwrap_or_default().to_owned()
        } else {
            after
                .split(|c: char| c.is_whitespace() || c == ',' || c == '}')
                .next()
                .unwrap_or_default()
                .to_owned()
        };
        out.push(value);
    }
    out
}

fn list(v: &str) -> Vec<String> {
    v.split(|c: char| c.is_whitespace() || c == ',')
        .map(|s| s.trim_matches('"'))
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Fold one WirePlumber config file into `cfg` (later files win).
pub fn read_wp_text(cfg: &mut WpConfig, name: &str, text: &str) {
    let t = strip_comments(text);
    let mut touched = false;
    if let Some(v) = values_of(&t, "bluez5.roles").pop() {
        cfg.roles = Some(list(&v));
        touched = true;
    }
    if let Some(v) = values_of(&t, "bluez5.hfphsp-backend").pop() {
        cfg.backend = Some(v.trim_matches('"').to_owned());
        touched = true;
    }
    if let Some(v) = values_of(&t, "bluez5.auto-connect").pop() {
        cfg.auto_connect = Some(list(&v));
        touched = true;
    }
    if let Some(v) = values_of(&t, "bluez5.telephony-dbus-service").pop() {
        cfg.telephony = Some(v.trim_matches('"') == "true");
        touched = true;
    }
    if let Some(v) = values_of(&t, "bluez5.enable-msbc").pop() {
        cfg.msbc = Some(v.trim_matches('"') == "true");
        touched = true;
    }
    if touched {
        cfg.files.push(name.to_owned());
    }
}

/// WirePlumber's configuration as it loads it: the main file (user override first), then the
/// `wireplumber.conf.d` fragments of the system, `/etc` and the user, each sorted by name.
pub fn read_wp_config(home_config: Option<&Path>, etc: &Path, share: &Path) -> WpConfig {
    let mut cfg = WpConfig::default();
    let mut files: Vec<PathBuf> = Vec::new();
    let main = home_config
        .map(|h| h.join("wireplumber/wireplumber.conf"))
        .filter(|p| p.exists())
        .or_else(|| Some(etc.join("wireplumber/wireplumber.conf")).filter(|p| p.exists()))
        .unwrap_or_else(|| share.join("wireplumber/wireplumber.conf"));
    files.push(main);
    let dirs = [
        Some(share.join("wireplumber/wireplumber.conf.d")),
        Some(etc.join("wireplumber/wireplumber.conf.d")),
        home_config.map(|h| h.join("wireplumber/wireplumber.conf.d")),
    ];
    for d in dirs.into_iter().flatten() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(&d)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "conf"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        files.extend(v);
    }
    for f in files {
        if let Ok(text) = std::fs::read_to_string(&f) {
            read_wp_text(&mut cfg, &f.display().to_string(), &text);
        }
    }
    cfg
}

#[cfg(target_os = "linux")]
mod host {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use crate::audio::{AudioServer, route};

    fn run(cmd: &str, args: &[&str]) -> Option<(bool, String)> {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let end = Instant::now() + Duration::from_secs(3);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Err(_) => return None,
                Ok(None) if Instant::now() >= end => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        };
        let mut s = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take()?, &mut s).ok()?;
        Some((status.success(), s))
    }

    fn active(user: bool, unit: &str) -> Option<bool> {
        let args: Vec<&str> = if user {
            vec!["--user", "is-active", unit]
        } else {
            vec!["is-active", unit]
        };
        // `is-active` prints the state even when it exits non-zero; no output means systemd
        // could not be asked (no user manager, a container, the sandbox): unknown.
        run("systemctl", &args).and_then(|(_, out)| {
            let s = out.trim();
            (!s.is_empty()).then(|| s == "active")
        })
    }

    /// Everything that needs no D-Bus library and no sound-server client: sysfs, systemd,
    /// `bluetoothctl` and the WirePlumber files. What `geekbridge-ctl` reads when the service is
    /// not running; the telephony, card and device steps are then marked as not checked.
    pub fn gather_basic() -> Facts {
        let mut f = Facts {
            partial: true,
            adapters: adapters_in(&bt_sysfs()),
            btusb_loaded: Path::new("/sys/module/btusb").exists(),
            rfkill: rfkill_in(Path::new("/sys/class/rfkill")),
            service_active: active(false, "bluetooth.service"),
            wireplumber_running: active(true, "wireplumber.service"),
            ..Facts::default()
        };
        let home_config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        f.wp = read_wp_config(
            home_config.as_deref(),
            Path::new("/etc"),
            Path::new("/usr/share"),
        );

        let btctl = |args: &[&str]| run("bluetoothctl", args).map(|(_, o)| o);
        let show = btctl(&["show"]);
        f.bluetoothctl = show.is_some();
        f.powered = show.as_deref().and_then(parse_powered);
        if f.bluetoothctl && !f.adapters.is_empty() {
            let list = btctl(&["devices", "Paired"]).unwrap_or_default();
            for addr in crate::readiness::parse_devices(&list) {
                let info = btctl(&["info", &addr]).unwrap_or_default();
                let (name, paired, trusted, connected, gw) = parse_info(&info);
                f.phones.push(PhoneFacts {
                    name,
                    address: addr.to_ascii_uppercase(),
                    paired,
                    trusted,
                    connected,
                    hfp_gateway: gw,
                    ..PhoneFacts::default()
                });
            }
        }
        f
    }

    /// The whole host: [`gather_basic`] plus PipeWire telephony and the sound server.
    /// `state_dir` holds the user's device pick. Read-only.
    pub fn gather(state_dir: &Path) -> Facts {
        let mut f = gather_basic();
        f.partial = false;
        f.remembered = crate::setup::load_remembered(state_dir).map(|r| r.address);

        // Telephony: which phones have an audio gateway object.
        let mut registered: Vec<String> = Vec::new();
        match crate::pipewire::PipeWireBackend::connect(crate::pipewire::PipeWireConfig::default())
        {
            Ok(b) => match crate::CallBackend::snapshot(&b) {
                Ok(s) => {
                    f.telephony_present =
                        Some(matches!(s.service, crate::ServiceStatus::Present { .. }));
                    registered = s
                        .phones
                        .iter()
                        .map(|p| p.device.address.expose().to_ascii_uppercase())
                        .collect();
                }
                Err(_) => f.telephony_present = None,
            },
            Err(_) => f.telephony_present = None,
        }

        let snap = crate::audio::connect_platform()
            .ok()
            .and_then(|mut s| s.snapshot().ok());

        for p in &mut f.phones {
            p.hfp_connected = registered.contains(&p.address);
            {
                if let (Some(s), Some(a)) = (&snap, crate::audio::BtAddress::parse(&p.address)) {
                    let v = route::phone_voice(s, a);
                    if let Some(c) = &v.card {
                        p.card = Some(c.name.clone());
                        p.card_profile = c.active_profile.clone();
                        p.card_has_gateway_profile = c.gateway_profile().is_some();
                    }
                    p.voice_streams = v.streams_present();
                }
            }
        }

        if let Some(s) = &snap {
            let choice =
                crate::audio::prefs::load_choice(&state_dir.join(crate::audio::prefs::CHOICE_FILE));
            let mut a = AudioFacts {
                server_name: s.identity.server_name.clone(),
                pipewire: s.identity.is_pipewire(),
                ..AudioFacts::default()
            };
            for (role, pick) in [
                (route::Role::Output, choice.output.as_deref()),
                (route::Role::Input, choice.input.as_deref()),
            ] {
                let r = route::resolve(s, role, pick);
                if let Some(route::Resolved::Fallback { wanted, .. }) = &r {
                    a.missing_pick.push(wanted.clone());
                }
                let entry = r.map(|r| {
                    let chosen = matches!(r, route::Resolved::Chosen { .. });
                    let name = r.name().to_owned();
                    (name.clone(), route::describe(s, role, &name), chosen)
                });
                match role {
                    route::Role::Output => a.output = entry,
                    route::Role::Input => {
                        a.input_muted = entry.as_ref().is_some_and(|(n, _, _)| {
                            s.sources.iter().any(|x| &x.name == n && x.mute)
                        });
                        a.input = entry;
                    }
                }
            }
            f.audio = Some(a);
        }
        f
    }
}
#[cfg(target_os = "linux")]
pub use host::{gather, gather_basic};

#[cfg(test)]
#[path = "hear_tests.rs"]
mod tests;
