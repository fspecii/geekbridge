// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl doctor`: "why does it not work?" in plain words, with the command that fixes it.
//!
//! Unlike `diagnose` (a redacted bundle for bug reports) this output is for the person at the
//! keyboard, so it may name their phones and print the exact fix commands. Everything here is a
//! pure function of [`Inputs`]; the ctl gathers the inputs (service replies, environment, firewall
//! files) and prints the result, so each rule is unit-tested without a service or a firewall.

use bridge_core::ipc::{CallingStatus, ClipboardStatus, LinkPhase, Status};
use bridge_core::phone_view::{DataDevice, DataState};
use std::fmt::Write as _;

/// First and last port GeekBridge may listen on (the KDE Connect range).
pub const PORT_LO: u16 = 1716;
pub const PORT_HI: u16 = 1764;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub level: Level,
    pub title: String,
    pub detail: String,
    /// What to do about it; a command where one exists.
    pub fix: Option<String>,
}

fn check(level: Level, title: &str, detail: impl Into<String>, fix: Option<&str>) -> Check {
    Check {
        level,
        title: title.into(),
        detail: detail.into(),
        fix: fix.map(str::to_owned),
    }
}

/// The graphical-session variables of the shell running `doctor`.
#[derive(Clone, Debug, Default)]
pub struct SessionEnv {
    pub wayland_display: bool,
    pub display: bool,
    pub xdg_runtime_dir: bool,
    pub hyprland_signature: bool,
    pub desktop: String,
}

pub use bridge_core::firewall::{Firewall, firewall_blocks, ufw_allows};

pub struct Inputs {
    /// `Err` carries the text of the failed call.
    pub status: Result<Status, String>,
    pub clipboard: Option<ClipboardStatus>,
    pub calling: Option<CallingStatus>,
    /// Per-phone feature states (`MessagesView.devices`).
    pub data: Vec<DataDevice>,
    pub session: SessionEnv,
    pub firewall: Firewall,
    /// A service-less `doctor` still says how to start it; `true` when a unit file is installed.
    pub systemd_unit: bool,
}

fn firewall_check(fw: &Firewall) -> Check {
    let title = "Firewall";
    let fix_ufw =
        "geekbridge-ctl firewall   (prints the exact sudo ufw lines for TCP and UDP 1716-1764)";
    match fw.ufw_enabled {
        Some(true) => match &fw.ufw_rules {
            Some(rules) => {
                let (tcp, udp) = (ufw_allows(rules, "tcp"), ufw_allows(rules, "udp"));
                if tcp && udp {
                    check(
                        Level::Ok,
                        title,
                        "ufw is on and allows TCP and UDP 1716-1764.",
                        None,
                    )
                } else {
                    let missing = match (tcp, udp) {
                        (false, false) => "TCP and UDP",
                        (false, true) => "TCP",
                        _ => "UDP",
                    };
                    check(
                        Level::Fail,
                        title,
                        format!(
                            "ufw is on and has no rule for {missing} 1716-1764, so the phone cannot find or reach this computer (pairing times out silently)."
                        ),
                        Some(fix_ufw),
                    )
                }
            }
            None => check(
                Level::Warn,
                title,
                "ufw is on, but its rules are root-only so this check cannot see whether 1716-1764 is open.",
                Some(
                    "sudo ufw status | grep -c 1716   (0 means no rule yet: run geekbridge-ctl firewall)",
                ),
            ),
        },
        _ if fw.firewalld_active => check(
            Level::Warn,
            title,
            "firewalld is running; this check cannot see its rules without root.",
            Some(
                "sudo firewall-cmd --permanent --add-port=1716-1764/tcp --add-port=1716-1764/udp && sudo firewall-cmd --reload",
            ),
        ),
        Some(false) => check(
            Level::Ok,
            title,
            "ufw is off; nothing here blocks the phone.",
            None,
        ),
        None => check(
            Level::Ok,
            title,
            "No ufw or firewalld found. If another firewall is active, allow TCP and UDP 1716-1764 from your LAN.",
            None,
        ),
    }
}

fn session_check(s: &SessionEnv) -> Check {
    session_check_on(s, cfg!(all(unix, not(target_os = "macos"))))
}

/// `x11_or_wayland`: this OS has Wayland/X11 sessions at all. Windows and macOS have neither
/// `WAYLAND_DISPLAY` nor `DISPLAY`, so the variables say nothing there.
fn session_check_on(s: &SessionEnv, x11_or_wayland: bool) -> Check {
    let title = "Desktop session";
    if !x11_or_wayland {
        return check(
            Level::Ok,
            title,
            "Native desktop session (no Wayland/X11 variables needed on this system).",
            None,
        );
    }
    if !s.wayland_display && !s.display {
        return check(
            Level::Warn,
            title,
            "This shell has neither WAYLAND_DISPLAY nor DISPLAY: windows, the clipboard bridge, notifications and the hotkey will not work from here.",
            Some(
                "run this from a terminal inside your desktop session; if the service runs under systemd --user: systemctl --user import-environment WAYLAND_DISPLAY DISPLAY HYPRLAND_INSTANCE_SIGNATURE && systemctl --user restart geekbridge-service",
            ),
        );
    }
    if !s.xdg_runtime_dir {
        return check(
            Level::Warn,
            title,
            "XDG_RUNTIME_DIR is not set, so Wayland sockets and the D-Bus session bus cannot be found.",
            Some(
                "log in through your display manager or session manager (uwsm on Omarchy) instead of starting the compositor by hand",
            ),
        );
    }
    if s.desktop.to_ascii_lowercase().contains("hyprland") && !s.hyprland_signature {
        return check(
            Level::Warn,
            title,
            "Running under Hyprland but HYPRLAND_INSTANCE_SIGNATURE is not set, so `geekbridge-ctl hotkey` cannot talk to it.",
            Some("run it from a terminal started inside Hyprland"),
        );
    }
    let kind = if s.wayland_display { "Wayland" } else { "X11" };
    check(Level::Ok, title, format!("{kind} session found."), None)
}

fn service_down(err: &str, systemd_unit: bool) -> Check {
    let fix = if systemd_unit {
        "systemctl --user start geekbridge-service   (then: geekbridge-ctl status)"
    } else {
        "start it: geekbridge-service   (from a source checkout: scripts/run-desktop.sh)"
    };
    check(
        Level::Fail,
        "Service",
        format!(
            "geekbridge-service is not answering, so no phone can connect and the apps show no computer: {}",
            crate::diagnose::scrub(err)
        ),
        Some(fix),
    )
}

/// The service's reason for a phone without a link, with the firewall fact only the ctl can add.
pub fn why_for(
    status: &Status,
    device_id: &str,
    fw_blocks: bool,
) -> Option<bridge_core::why_disconnected::WhyDisconnected> {
    let mut w = status
        .disconnected
        .iter()
        .find(|w| w.device_id == device_id)?
        .clone();
    bridge_core::why_disconnected::with_firewall_blocking(&mut w, fw_blocks);
    Some(w)
}

fn phones_checks(status: &Status, fw_blocks: bool) -> Vec<Check> {
    let title = "Phones";
    if status.paired.is_empty() {
        return vec![check(
            Level::Warn,
            title,
            "No phone is paired yet.",
            Some(
                "open GeekBridge on the computer and scan its QR code from the phone (Home > Pair a computer); or: geekbridge-ctl pair-qr",
            ),
        )];
    }
    let mut out = Vec::new();
    for p in &status.paired {
        let link = status.links.iter().find(|l| l.device_id == p.device_id);
        let connected = link.is_some_and(|l| l.phase == LinkPhase::Connected);
        let reconnecting = status.reconnecting.contains(&p.device_id);
        let why = why_for(status, &p.device_id, fw_blocks);
        out.push(if connected {
            check(Level::Ok, title, format!("{} is connected.", p.name), None)
        } else if let Some(w) = why {
            check(
                Level::Warn,
                title,
                format!(
                    "{} is not connected: {}",
                    p.name,
                    bridge_core::why_disconnected::one_line(&w)
                ),
                Some(&w.fix),
            )
        } else if reconnecting {
            check(
                Level::Warn,
                title,
                format!("{} is paired; this computer is trying to reach it again.", p.name),
                Some("open the GeekBridge app on the phone (same Wi-Fi, Stay connected on); still stuck: geekbridge-ctl test-connection"),
            )
        } else {
            check(
                Level::Warn,
                title,
                format!("{} is paired but not connected.", p.name),
                Some("open the GeekBridge app on the phone on the same Wi-Fi; if the computer does not appear there, UDP 1716 broadcasts are blocked (firewall, guest Wi-Fi or VPN): scan the QR code instead, or geekbridge-ctl test-connection"),
            )
        });
    }
    out
}

fn listener_check(status: &Status) -> Check {
    let title = "Listener";
    match status.inbound_listener {
        None => check(
            Level::Fail,
            title,
            "The service is not listening for phones (all of TCP 1716-1764 may be taken by other programs).",
            Some(
                "quit KDE Connect / GSConnect, or see which program owns the ports: ss -tlnp | grep -E ':17(1[6-9]|[2-5][0-9]|6[0-4])\\b'",
            ),
        ),
        Some(addr) => check(
            Level::Ok,
            title,
            status
                .port_note()
                .unwrap_or_else(|| format!("Listening on TCP {}.", addr.port())),
            None,
        ),
    }
}

fn capability_checks(i: &Inputs, status: &Status) -> Vec<Check> {
    let mut out = Vec::new();
    match &i.clipboard {
        Some(c) if !c.available => out.push(check(
            Level::Warn,
            "Clipboard",
            format!(
                "Clipboard sync cannot work ({}); backend: {}.",
                c.problem.as_deref().unwrap_or("no reason given"),
                c.backend
            ),
            Some("install wl-clipboard (Wayland, needs the data-control protocol; not stock GNOME) or xclip (X11), then restart the service"),
        )),
        Some(c) => out.push(check(Level::Ok, "Clipboard", format!("Backend {} works.", c.backend), None)),
        None => {}
    }
    if let Some(c) = &i.calling {
        out.push(calls_check(c));
    }
    for p in &status.paired {
        let Some(d) = i.data.iter().find(|d| d.device_id == p.device_id) else {
            continue;
        };
        out.push(messages_check(d, &p.name));
    }
    out
}

/// The Bluetooth side of calls in plain words. Having no Bluetooth controller is not a fault: the
/// phone link still answers, declines, hangs up and dials; only the voice stays on the phone.
pub fn calls_check(c: &CallingStatus) -> Check {
    let no_controller =
        c.verdict == "no_bluetooth_controller" || c.bluetooth_controllers == Some(0);
    if no_controller {
        return check(
            Level::Ok,
            "Calls",
            "No Bluetooth controller on this computer. Call control works over the phone link (answer, decline, hang up, dial), but the call audio stays on the phone: you will not hear the other person here.",
            Some(
                "to hear calls on this computer add a USB Bluetooth adapter and pair the phone as a hands-free device (docs/call-audio-options.md); otherwise nothing to do",
            ),
        );
    }
    let level = if c.backend == "none" || c.read_only {
        Level::Warn
    } else {
        Level::Ok
    };
    check(level, "Calls", crate::diagnose::scrub(&c.verdict), None)
}

/// Why Messages is or is not usable for one phone, and the way out.
pub fn messages_check(d: &DataDevice, name: &str) -> Check {
    let title = "Messages";
    match d.sms {
        DataState::Ready => check(Level::Ok, title, format!("{name}: Messages work."), None),
        DataState::Waiting => check(
            Level::Ok,
            title,
            format!("{name}: waiting for the phone's first answer."),
            None,
        ),
        DataState::Offline => check(
            Level::Warn,
            title,
            format!("{name}: Messages is unavailable because the phone is not connected."),
            Some("connect the phone first (see Phones above)"),
        ),
        DataState::ConsentOff => check(
            Level::Warn,
            title,
            format!("{name}: Messages is off for this phone (it is opt-in, nothing is requested)."),
            Some(&format!(
                "click Allow in the Messages app, or: geekbridge-ctl consent {} sms on",
                d.device_id
            )),
        ),
        DataState::NotOffered => check(
            Level::Warn,
            title,
            format!("{name}: allowed here, but the phone does not offer Messages."),
            Some(
                "on the phone: GeekBridge > Share with computer > Messages, and allow the SMS permission (Android 13+ sideloaded: Settings > Apps > GeekBridge > three dots > Allow restricted settings)",
            ),
        ),
    }
}

pub fn run(i: &Inputs) -> Vec<Check> {
    let mut out = Vec::new();
    match &i.status {
        Err(e) => out.push(service_down(e, i.systemd_unit)),
        Ok(s) => {
            out.push(check(
                Level::Ok,
                "Service",
                format!(
                    "geekbridge-service {} answers as \"{}\".",
                    crate::diagnose::scrub(&s.service_version),
                    s.device_name
                ),
                None,
            ));
            out.push(listener_check(s));
        }
    }
    out.push(firewall_check(&i.firewall));
    out.push(session_check(&i.session));
    if let Ok(s) = &i.status {
        out.extend(phones_checks(s, firewall_blocks(&i.firewall)));
        out.extend(capability_checks(i, s));
    }
    out
}

/// Number of failed checks, for the exit status.
pub fn failures(checks: &[Check]) -> usize {
    checks.iter().filter(|c| c.level == Level::Fail).count()
}

pub fn render(checks: &[Check]) -> String {
    let mut o = String::new();
    for c in checks {
        let tag = match c.level {
            Level::Ok => " ok ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        let _ = writeln!(o, "[{tag}] {}: {}", c.title, c.detail);
        if let Some(fix) = &c.fix {
            let _ = writeln!(o, "       fix: {fix}");
        }
    }
    let (fails, warns) = (
        failures(checks),
        checks.iter().filter(|c| c.level == Level::Warn).count(),
    );
    let _ = match (fails, warns) {
        (0, 0) => writeln!(o, "\nAll good."),
        _ => writeln!(
            o,
            "\n{fails} problem(s), {warns} warning(s). Still stuck? geekbridge-ctl test-connection, then geekbridge-ctl report."
        ),
    };
    o
}

/// A next step for a failed control-port call, appended to the plain error by the ctl.
pub fn hint_for_error(message: &str) -> Option<&'static str> {
    if message.contains("cannot reach GeekBridge service control port") {
        Some(
            "Is geekbridge-service running? Start it (systemctl --user start geekbridge-service, or scripts/run-desktop.sh), then run `geekbridge-ctl doctor`. A service started with another --state-dir or --control-port needs the same flags here.",
        )
    } else if message.contains("rejected the control token")
        || message.contains("no control token yet")
    {
        Some(
            "The ctl and the service use different state directories. Pass the service's --state-dir, or run `geekbridge-ctl doctor` for a full check.",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::ipc::{LinkStatus, PairedDevice};

    const BOTH: &str = "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT\n-A ufw-user-input -p udp --dport 1716:1764 -j ACCEPT\n";

    #[test]
    fn native_desktops_do_not_need_wayland_or_x11_variables() {
        let none = SessionEnv::default();
        assert_eq!(session_check_on(&none, false).level, Level::Ok);
        assert_eq!(session_check_on(&none, true).level, Level::Warn);
    }

    fn status() -> Status {
        serde_json::from_value(serde_json::json!({
            "ipc_version": 1, "service_version": "0.1.0", "device_id": "me", "device_name": "pc",
            "own_cert_sha256": "", "paired": [], "links": [], "challenges": [], "event_seq": 0,
            "inbound_listener": "0.0.0.0:1716"
        }))
        .unwrap()
    }

    fn phone(status: &mut Status, connected: bool) {
        status.paired.push(PairedDevice {
            device_id: "p1".into(),
            name: "Pixel".into(),
            device_type: "phone".into(),
            cert_sha256: String::new(),
            paired_at_unix: 0,
            consents: vec![],
        });
        if connected {
            let mut l: LinkStatus = serde_json::from_value(serde_json::json!({
                "device_id": "p1", "name": "Pixel", "phase": "connected", "role": null,
                "connection_id": 1, "peer_ip": null, "peer_cert_sha256": null, "negotiated": [],
                "since_unix_ms": 0, "pings_received": 0, "last_ping_unix_ms": null, "rejected_packets": 0
            }))
            .unwrap();
            l.phase = LinkPhase::Connected;
            status.links.push(l);
        }
    }

    fn inputs(status: Result<Status, String>) -> Inputs {
        Inputs {
            status,
            clipboard: None,
            calling: None,
            data: vec![],
            session: SessionEnv {
                wayland_display: true,
                xdg_runtime_dir: true,
                ..SessionEnv::default()
            },
            firewall: Firewall::default(),
            systemd_unit: false,
        }
    }

    fn find<'a>(c: &'a [Check], title: &str) -> Vec<&'a Check> {
        c.iter().filter(|c| c.title == title).collect()
    }

    #[test]
    fn ufw_rules_must_cover_the_whole_range_per_protocol() {
        assert!(ufw_allows(BOTH, "tcp") && ufw_allows(BOTH, "udp"));
        let src = "-A ufw-user-input -s 192.168.1.0/24 -p tcp --dport 1716:1764 -j ACCEPT";
        assert!(ufw_allows(src, "tcp") && !ufw_allows(src, "udp"));
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp --dport 1716 -j ACCEPT",
            "tcp"
        ));
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp --dport 22 -j ACCEPT",
            "tcp"
        ));
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp --dport 1716:1764 -j DROP",
            "tcp"
        ));
        assert!(!ufw_allows("", "tcp"));
    }

    #[test]
    fn active_ufw_without_a_rule_fails_and_names_the_command() {
        let fw = Firewall {
            ufw_enabled: Some(true),
            ufw_rules: Some("-A ufw-user-input -p tcp --dport 22 -j ACCEPT".into()),
            ..Firewall::default()
        };
        let c = firewall_check(&fw);
        assert_eq!(c.level, Level::Fail);
        assert!(
            c.detail.contains("TCP and UDP") && c.fix.unwrap().contains("geekbridge-ctl firewall")
        );
    }

    #[test]
    fn firewall_states() {
        let on = |rules: Option<&str>| Firewall {
            ufw_enabled: Some(true),
            ufw_rules: rules.map(Into::into),
            ..Firewall::default()
        };
        assert_eq!(firewall_check(&on(Some(BOTH))).level, Level::Ok);
        // Unreadable rules must not be reported as "closed" or as "open".
        assert_eq!(firewall_check(&on(None)).level, Level::Warn);
        assert_eq!(firewall_check(&Firewall::default()).level, Level::Ok);
        let fwd = Firewall {
            firewalld_active: true,
            ..Firewall::default()
        };
        assert_eq!(firewall_check(&fwd).level, Level::Warn);
        let udp_only = on(Some("-A ufw-user-input -p udp --dport 1716:1764 -j ACCEPT"));
        assert!(firewall_check(&udp_only).detail.contains("for TCP 1716"));
    }

    #[test]
    fn service_down_stops_at_one_actionable_failure() {
        let mut i = inputs(Err(
            "cannot reach GeekBridge service control port: refused".into()
        ));
        let checks = run(&i);
        let svc = find(&checks, "Service");
        assert_eq!(svc[0].level, Level::Fail);
        assert!(
            svc[0]
                .fix
                .as_ref()
                .unwrap()
                .contains("scripts/run-desktop.sh")
        );
        assert!(find(&checks, "Phones").is_empty());
        i.systemd_unit = true;
        assert!(
            run(&i)[0]
                .fix
                .as_ref()
                .unwrap()
                .contains("systemctl --user start")
        );
        assert!(failures(&checks) >= 1);
    }

    #[test]
    fn no_listener_is_a_failure() {
        let mut s = status();
        s.inbound_listener = None;
        assert_eq!(listener_check(&s).level, Level::Fail);
        assert_eq!(listener_check(&status()).level, Level::Ok);
    }

    #[test]
    fn phone_states() {
        let c = phones_checks(&status(), false);
        assert_eq!(c[0].level, Level::Warn);
        assert!(c[0].detail.contains("No phone is paired"));
        let mut s = status();
        phone(&mut s, false);
        let c = phones_checks(&s, false);
        assert!(c[0].detail.contains("Pixel is paired but not connected"));
        assert!(c[0].fix.as_ref().unwrap().contains("UDP 1716"));
        let mut s = status();
        phone(&mut s, true);
        assert_eq!(phones_checks(&s, false)[0].level, Level::Ok);
    }

    #[test]
    fn a_disconnected_phone_gets_the_services_reason_and_the_firewall_can_refine_it() {
        use bridge_core::why_disconnected::{ReasonCode, WhyDisconnected};
        let mut s = status();
        phone(&mut s, false);
        s.disconnected.push(WhyDisconnected {
            device_id: "p1".into(),
            code: ReasonCode::GaveUp,
            sentence: ReasonCode::GaveUp.sentence().into(),
            fix: ReasonCode::GaveUp.fix().into(),
            last_seen_ago_s: Some(3),
            tries: 12,
        });
        let c = phones_checks(&s, false);
        assert!(
            c[0].detail
                .contains("Pixel is not connected: The phone is nearby")
        );
        assert!(c[0].fix.as_ref().unwrap().contains("firewall"));
        let c = phones_checks(&s, true);
        assert!(c[0].detail.contains("firewall is blocking"));
        assert!(
            c[0].fix
                .as_ref()
                .unwrap()
                .contains("geekbridge-ctl firewall")
        );
    }

    #[test]
    fn only_readable_rules_without_the_ports_count_as_blocking() {
        let on = |rules: Option<&str>| Firewall {
            ufw_enabled: Some(true),
            ufw_rules: rules.map(str::to_owned),
            firewalld_active: false,
        };
        assert!(firewall_blocks(&on(Some(""))));
        assert!(!firewall_blocks(&on(Some(BOTH))));
        assert!(!firewall_blocks(&on(None)));
        assert!(!firewall_blocks(&Firewall::default()));
    }

    #[test]
    fn messages_off_says_how_to_turn_it_on() {
        let d = |sms| DataDevice {
            device_id: "p1".into(),
            sms,
            ..DataDevice::default()
        };
        let off = messages_check(&d(DataState::ConsentOff), "Pixel");
        assert_eq!(off.level, Level::Warn);
        assert!(
            off.fix
                .unwrap()
                .contains("geekbridge-ctl consent p1 sms on")
        );
        assert!(
            messages_check(&d(DataState::NotOffered), "Pixel")
                .fix
                .unwrap()
                .contains("Allow restricted settings")
        );
        assert!(
            messages_check(&d(DataState::Offline), "Pixel")
                .detail
                .contains("not connected")
        );
        assert_eq!(
            messages_check(&d(DataState::Ready), "Pixel").level,
            Level::Ok
        );
    }

    #[test]
    fn capabilities_report_messages_for_paired_phones_only() {
        let mut s = status();
        phone(&mut s, true);
        let mut i = inputs(Ok(s));
        i.data = vec![
            DataDevice {
                device_id: "p1".into(),
                sms: DataState::ConsentOff,
                ..DataDevice::default()
            },
            DataDevice {
                device_id: "other".into(),
                ..DataDevice::default()
            },
        ];
        let checks = run(&i);
        assert_eq!(find(&checks, "Messages").len(), 1);
    }

    #[test]
    fn session_without_display_warns_and_hyprland_needs_its_signature() {
        let none = SessionEnv::default();
        assert!(
            session_check_on(&none, true)
                .detail
                .contains("neither WAYLAND_DISPLAY nor DISPLAY")
        );
        let hypr = SessionEnv {
            wayland_display: true,
            xdg_runtime_dir: true,
            desktop: "Hyprland".into(),
            ..SessionEnv::default()
        };
        assert!(
            session_check_on(&hypr, true)
                .detail
                .contains("HYPRLAND_INSTANCE_SIGNATURE")
        );
        let ok = SessionEnv {
            hyprland_signature: true,
            ..hypr
        };
        assert_eq!(session_check_on(&ok, true).level, Level::Ok);
    }

    #[test]
    fn render_lists_fixes_and_a_summary() {
        let text = render(&run(&inputs(Err("refused".into()))));
        assert!(text.contains("[FAIL] Service") && text.contains("fix:"));
        assert!(text.contains("problem(s)"));
        let good = render(&[check(Level::Ok, "x", "fine", None)]);
        assert!(good.contains("All good."));
    }

    #[test]
    fn error_hints() {
        assert!(
            hint_for_error("cannot reach GeekBridge service control port: refused")
                .unwrap()
                .contains("doctor")
        );
        assert!(hint_for_error("GeekBridge service rejected the control token").is_some());
        assert!(hint_for_error("unknown feature").is_none());
    }

    #[test]
    fn no_bluetooth_controller_says_control_works_and_audio_stays_on_the_phone() {
        let mut c = CallingStatus {
            platform: "linux".into(),
            backend: "pipewire".into(),
            verdict: "no_bluetooth_controller".into(),
            telephony_service_present: true,
            bluetooth_controllers: Some(0),
            phones: 0,
            calls: 0,
            read_only: true,
        };
        let k = calls_check(&c);
        assert_eq!(k.level, Level::Ok, "not a fault");
        assert!(k.detail.contains("control works") && k.detail.contains("stays on the phone"));
        assert!(k.fix.as_deref().is_some_and(|f| f.contains("USB")));
        c.verdict = "no_phone".into();
        c.bluetooth_controllers = Some(1);
        assert!(!calls_check(&c).detail.contains("stays on the phone"));
    }

    // ---- test-gaps-4: every branch of the firewall, session, phone and capability rules

    fn ufw(enabled: Option<bool>, rules: Option<&str>, firewalld: bool) -> Firewall {
        Firewall {
            ufw_enabled: enabled,
            ufw_rules: rules.map(str::to_owned),
            firewalld_active: firewalld,
        }
    }

    #[test]
    fn ufw_port_ranges_must_contain_the_whole_geekbridge_range() {
        let rule = |ports: &str| format!("-A ufw-user-input -p tcp --dport {ports} -j ACCEPT");
        // Wider ranges and the exact range open it; one port short on either end does not.
        for ok in ["1716:1764", "1700:1800", "1:65535", "1716:65535", "0:1764"] {
            assert!(ufw_allows(&rule(ok), "tcp"), "{ok}");
        }
        for bad in [
            "1717:1764",
            "1716:1763",
            "1716",
            "1764",
            "1000:1715",
            "1765:2000",
            "1764:1716",
            "a:b",
            "1716:",
            ":1764",
            "1716:1764:1800",
            "99999:99999",
            "-1:1764",
            " ",
        ] {
            assert!(!ufw_allows(&rule(bad), "tcp"), "{bad}");
        }
        // Another protocol, another chain, no destination port, a trailing comment and padding.
        assert!(!ufw_allows(&rule("1716:1764"), "udp"));
        assert!(!ufw_allows(
            "-A ufw-user-output -p tcp --dport 1716:1764 -j ACCEPT",
            "tcp"
        ));
        assert!(!ufw_allows("-A ufw-user-input -p tcp -j ACCEPT", "tcp"));
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT -m comment",
            "tcp"
        ));
        assert!(ufw_allows(&format!("  {}  \r", rule("1716:1764")), "tcp"));
        // A missing value after --dport must not panic.
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp --dport -j ACCEPT",
            "tcp"
        ));
        assert!(!ufw_allows("-A ufw-user-input -p --dport -j ACCEPT", "tcp"));
        // Only one line has to match; a later DROP does not cancel an earlier ACCEPT (ufw orders them itself).
        let two = format!("{}\n{}\n", rule("22"), rule("1716:1764"));
        assert!(ufw_allows(&two, "tcp"));
    }

    #[test]
    fn every_ufw_state_gets_exactly_one_answer() {
        let tcp = "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT";
        let udp = "-A ufw-user-input -p udp --dport 1716:1764 -j ACCEPT";
        let both = format!("{tcp}\n{udp}");
        // on + readable
        assert_eq!(
            firewall_check(&ufw(Some(true), Some(&both), false)).level,
            Level::Ok
        );
        let c = firewall_check(&ufw(Some(true), Some(udp), false));
        assert_eq!(c.level, Level::Fail);
        assert!(c.detail.contains("no rule for TCP 1716"), "{}", c.detail);
        let c = firewall_check(&ufw(Some(true), Some(tcp), false));
        assert!(c.detail.contains("no rule for UDP 1716"), "{}", c.detail);
        let c = firewall_check(&ufw(Some(true), Some(""), false));
        assert!(c.detail.contains("TCP and UDP 1716"), "{}", c.detail);
        // on + unreadable: a warning with a way to look, never a failure and never an "ok"
        let c = firewall_check(&ufw(Some(true), None, false));
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.unwrap().contains("sudo ufw status"));
        // ufw on beats firewalld for the verdict when its rules are readable
        assert_eq!(
            firewall_check(&ufw(Some(true), Some(&both), true)).level,
            Level::Ok
        );
        assert_eq!(
            firewall_check(&ufw(Some(true), Some(""), true)).level,
            Level::Fail
        );
        // ufw on but unreadable: the ufw warning, not the firewalld one
        assert!(
            firewall_check(&ufw(Some(true), None, true))
                .detail
                .contains("ufw is on")
        );
        // ufw off + firewalld running: firewalld is what could block
        let c = firewall_check(&ufw(Some(false), None, true));
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.unwrap().contains("firewall-cmd"));
        // ufw off alone
        let c = firewall_check(&ufw(Some(false), None, false));
        assert_eq!(
            (c.level, c.detail.as_str()),
            (Level::Ok, "ufw is off; nothing here blocks the phone.")
        );
        // rules left behind by an off ufw are ignored
        assert_eq!(
            firewall_check(&ufw(Some(false), Some(""), false)).level,
            Level::Ok
        );
        // nothing known + firewalld
        assert_eq!(firewall_check(&ufw(None, None, true)).level, Level::Warn);
        // nothing at all
        let c = firewall_check(&ufw(None, None, false));
        assert_eq!(c.level, Level::Ok);
        assert!(c.detail.contains("No ufw or firewalld"));
        // every answer titled the same, so the output groups
        assert_eq!(c.title, "Firewall");
    }

    #[test]
    fn firewall_blocks_agrees_with_the_check_for_every_state() {
        let tcp = "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT";
        for (fw, blocks) in [
            (ufw(Some(true), Some(""), false), true),
            (ufw(Some(true), Some(tcp), false), true),
            (
                ufw(
                    Some(true),
                    Some(&format!("{tcp}\n{}", tcp.replace("tcp", "udp"))),
                    false,
                ),
                false,
            ),
            (ufw(Some(true), None, false), false),
            (ufw(Some(false), Some(""), false), false),
            (ufw(None, Some(""), false), false),
            (ufw(None, None, true), false),
        ] {
            assert_eq!(firewall_blocks(&fw), blocks, "{fw:?}");
            assert_eq!(
                firewall_check(&fw).level == Level::Fail,
                blocks,
                "a failing check is exactly a known block: {fw:?}"
            );
        }
    }

    #[test]
    fn session_check_covers_wayland_x11_runtime_dir_and_hyprland() {
        let env = |w, x, rt, sig, desktop: &str| SessionEnv {
            wayland_display: w,
            display: x,
            xdg_runtime_dir: rt,
            hyprland_signature: sig,
            desktop: desktop.into(),
        };
        let c = session_check_on(&env(true, false, true, false, "GNOME"), true);
        assert_eq!(
            (c.level, c.detail.as_str()),
            (Level::Ok, "Wayland session found.")
        );
        let c = session_check_on(&env(false, true, true, false, ""), true);
        assert_eq!(
            (c.level, c.detail.as_str()),
            (Level::Ok, "X11 session found.")
        );
        // both set: Wayland is the one named
        assert!(
            session_check_on(&env(true, true, true, false, ""), true)
                .detail
                .starts_with("Wayland")
        );
        // a display but no runtime dir
        for (w, x) in [(true, false), (false, true)] {
            let c = session_check_on(&env(w, x, false, false, ""), true);
            assert_eq!(c.level, Level::Warn);
            assert!(c.detail.contains("XDG_RUNTIME_DIR"));
        }
        // no display at all outranks a missing runtime dir
        assert!(
            session_check_on(&env(false, false, false, false, ""), true)
                .detail
                .contains("neither")
        );
        // Hyprland, any case, with or without a colon list; X11 under Hyprland (XWayland shell) warns too
        for d in ["Hyprland", "hyprland", "X-Hyprland:Other"] {
            let c = session_check_on(&env(false, true, true, false, d), true);
            assert_eq!(c.level, Level::Warn, "{d}");
        }
        assert_eq!(
            session_check_on(&env(true, false, true, true, "Hyprland"), true).level,
            Level::Ok
        );
        assert_eq!(
            session_check_on(&env(true, false, true, false, "Sway"), true).level,
            Level::Ok
        );
        // Windows and macOS: nothing in the environment can make it fail or warn
        for e in [
            SessionEnv::default(),
            env(true, true, true, true, "Hyprland"),
            env(false, false, false, false, "Hyprland"),
        ] {
            let c = session_check_on(&e, false);
            assert_eq!(c.level, Level::Ok);
            assert!(c.detail.contains("Native desktop session"));
            assert!(c.fix.is_none());
        }
    }

    #[test]
    fn the_real_session_check_uses_the_platform_it_runs_on() {
        let empty = session_check(&SessionEnv::default());
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(empty.level, Level::Warn);
        } else {
            assert_eq!(
                empty.level,
                Level::Ok,
                "Windows and macOS never warn about Wayland/X11"
            );
        }
    }

    #[test]
    fn a_reconnecting_phone_without_a_reason_says_so_and_a_reason_wins_over_it() {
        let mut s = status();
        phone(&mut s, false);
        s.reconnecting.push("p1".into());
        let c = phones_checks(&s, false);
        assert_eq!(c[0].level, Level::Warn);
        assert!(c[0].detail.contains("trying to reach it again"));
        assert!(c[0].fix.as_ref().unwrap().contains("test-connection"));
        // A reason from the service replaces the generic line.
        s.disconnected.push(bridge_core::why_disconnected::explain(
            "p1",
            &bridge_core::why_disconnected::Facts {
                computer_online: false,
                ..Default::default()
            },
        ));
        let c = phones_checks(&s, false);
        assert!(
            c[0].detail.contains("no network connection"),
            "{}",
            c[0].detail
        );
        assert!(!c[0].detail.contains("trying to reach"));
    }

    #[test]
    fn a_connected_phone_is_ok_even_when_a_stale_reason_is_left_over() {
        let mut s = status();
        phone(&mut s, true);
        s.disconnected.push(bridge_core::why_disconnected::explain(
            "p1",
            &bridge_core::why_disconnected::Facts::default(),
        ));
        let c = phones_checks(&s, true);
        assert_eq!(c.len(), 1);
        assert_eq!(
            (c[0].level, c[0].detail.as_str()),
            (Level::Ok, "Pixel is connected.")
        );
    }

    #[test]
    fn a_link_that_is_not_in_the_connected_phase_does_not_count_as_connected() {
        let mut s = status();
        phone(&mut s, true);
        s.links[0].phase = LinkPhase::Connecting;
        let c = phones_checks(&s, false);
        assert_eq!(c[0].level, Level::Warn);
        // A link for some other device is no help either.
        let mut s = status();
        phone(&mut s, true);
        s.links[0].device_id = "other".into();
        assert_eq!(phones_checks(&s, false)[0].level, Level::Warn);
    }

    #[test]
    fn every_paired_phone_gets_its_own_line_and_the_firewall_only_refines_the_unreachable_ones() {
        use bridge_core::why_disconnected::{ReasonCode, WhyDisconnected};
        let mut s = status();
        phone(&mut s, true);
        phone(&mut s, false);
        s.paired[1].device_id = "p2".into();
        s.paired[1].name = "Tablet".into();
        s.disconnected.push(WhyDisconnected {
            device_id: "p2".into(),
            code: ReasonCode::PausedByUser,
            sentence: ReasonCode::PausedByUser.sentence().into(),
            fix: ReasonCode::PausedByUser.fix().into(),
            last_seen_ago_s: None,
            tries: 0,
        });
        let c = phones_checks(&s, true);
        assert_eq!(c.len(), 2);
        assert!(c[0].detail.contains("Pixel is connected"));
        assert!(
            c[1].detail.contains("You disconnected this phone"),
            "paused is not a firewall matter"
        );
        assert_eq!(why_for(&s, "nobody", true), None);
        assert_eq!(
            why_for(&s, "p2", true).unwrap().code,
            ReasonCode::PausedByUser
        );
    }

    #[test]
    fn listener_note_for_a_fallback_port() {
        let mut s = status();
        s.inbound_listener = Some("0.0.0.0:1720".parse().unwrap());
        let c = listener_check(&s);
        assert_eq!(c.level, Level::Ok);
        assert!(
            c.detail.contains("1720") && c.detail.contains("1716"),
            "{}",
            c.detail
        );
        s.inbound_listener = Some("0.0.0.0:2000".parse().unwrap());
        assert_eq!(listener_check(&s).detail, "Listening on TCP 2000.");
        let none = {
            let mut n = status();
            n.inbound_listener = None;
            listener_check(&n)
        };
        assert!(none.fix.unwrap().contains("ss -tlnp"));
    }

    fn clip(available: bool, problem: Option<&str>) -> ClipboardStatus {
        ClipboardStatus {
            backend: "wl-clipboard".into(),
            available,
            observe: "watch".into(),
            watching: available,
            problem: problem.map(str::to_owned),
            max_text_bytes: 1,
            max_image_bytes: 1,
            window_ms: 1,
            hotkey: String::new(),
            hotkey_binding: String::new(),
            last_pull: None,
            devices: vec![],
            other_sync_app: false,
        }
    }

    #[test]
    fn clipboard_checks_say_why_and_what_to_install() {
        let mut i = inputs(Ok(status()));
        i.clipboard = Some(clip(true, None));
        let checks = run(&i);
        let c = find(&checks, "Clipboard");
        assert_eq!(
            (c[0].level, c[0].detail.as_str()),
            (Level::Ok, "Backend wl-clipboard works.")
        );
        i.clipboard = Some(clip(false, Some("no data-control")));
        let checks = run(&i);
        let c = find(&checks, "Clipboard");
        assert_eq!(c[0].level, Level::Warn);
        assert!(c[0].detail.contains("no data-control") && c[0].detail.contains("wl-clipboard"));
        assert!(c[0].fix.as_ref().unwrap().contains("xclip"));
        i.clipboard = Some(clip(false, None));
        assert!(
            find(&run(&i), "Clipboard")[0]
                .detail
                .contains("no reason given")
        );
        i.clipboard = None;
        assert!(find(&run(&i), "Clipboard").is_empty());
    }

    #[test]
    fn calls_levels_follow_the_backend_and_the_read_only_flag() {
        let calling =
            |backend: &str, verdict: &str, controllers: Option<usize>, ro: bool| CallingStatus {
                platform: "linux".into(),
                backend: backend.into(),
                verdict: verdict.into(),
                telephony_service_present: true,
                bluetooth_controllers: controllers,
                phones: 0,
                calls: 0,
                read_only: ro,
            };
        assert_eq!(
            calls_check(&calling("pipewire", "ready", Some(1), false)).level,
            Level::Ok
        );
        assert_eq!(
            calls_check(&calling("none", "ready", Some(1), false)).level,
            Level::Warn
        );
        assert_eq!(
            calls_check(&calling("pipewire", "ready", Some(1), true)).level,
            Level::Warn
        );
        // Unknown controller count is not "no controller"
        assert_eq!(
            calls_check(&calling("pipewire", "ready", None, false)).level,
            Level::Ok
        );
        // Zero controllers is fine whatever the backend, and it wins over read-only
        let k = calls_check(&calling("none", "something_else", Some(0), true));
        assert_eq!(k.level, Level::Ok);
        assert!(k.detail.contains("No Bluetooth controller"));
        // The verdict is scrubbed before it is printed
        let k = calls_check(&calling(
            "pipewire",
            "see /home/alice/secret",
            Some(1),
            false,
        ));
        assert!(!k.detail.contains("alice"), "{}", k.detail);
        assert_eq!(k.title, "Calls");
    }

    #[test]
    fn messages_states_waiting_and_ok_have_no_fix() {
        let d = |sms| DataDevice {
            device_id: "p1".into(),
            sms,
            ..DataDevice::default()
        };
        for st in [DataState::Ready, DataState::Waiting] {
            let c = messages_check(&d(st), "Pixel");
            assert_eq!(c.level, Level::Ok);
            assert!(c.fix.is_none());
            assert!(c.detail.starts_with("Pixel: "));
        }
        for st in [
            DataState::Offline,
            DataState::ConsentOff,
            DataState::NotOffered,
        ] {
            let c = messages_check(&d(st), "Pixel");
            assert_eq!(c.level, Level::Warn);
            assert!(c.fix.is_some());
        }
    }

    #[test]
    fn run_orders_the_checks_and_skips_phone_checks_when_the_service_is_down() {
        let mut s = status();
        phone(&mut s, true);
        let mut i = inputs(Ok(s));
        i.calling = Some(CallingStatus {
            platform: "linux".into(),
            backend: "pipewire".into(),
            verdict: "ready".into(),
            telephony_service_present: true,
            bluetooth_controllers: Some(1),
            phones: 1,
            calls: 0,
            read_only: false,
        });
        i.data = vec![DataDevice {
            device_id: "p1".into(),
            sms: DataState::Ready,
            ..DataDevice::default()
        }];
        let titles: Vec<_> = run(&i).into_iter().map(|c| c.title).collect();
        assert_eq!(
            titles,
            [
                "Service",
                "Listener",
                "Firewall",
                "Desktop session",
                "Phones",
                "Calls",
                "Messages"
            ]
        );
        // Service down: only the facts that need no service remain, and calling/clipboard replies are not trusted.
        let mut down = inputs(Err("refused".into()));
        down.calling = i.calling.clone();
        down.clipboard = Some(clip(true, None));
        let titles: Vec<_> = run(&down).into_iter().map(|c| c.title).collect();
        assert_eq!(titles, ["Service", "Firewall", "Desktop session"]);
        // The service's version is scrubbed of paths.
        let mut s = status();
        s.service_version = "0.1.0 /home/alice/bin".into();
        let c = run(&inputs(Ok(s)));
        assert!(!c[0].detail.contains("alice"), "{}", c[0].detail);
    }

    #[test]
    fn a_service_error_is_scrubbed_before_it_is_printed() {
        let c = service_down(
            "failed to read /home/alice/.local/state/geekbridge/token",
            false,
        );
        assert!(!c.detail.contains("alice"), "{}", c.detail);
        assert_eq!(c.level, Level::Fail);
    }

    #[test]
    fn render_counts_problems_and_warnings_and_indents_fixes() {
        let checks = [
            check(Level::Ok, "A", "fine", None),
            check(Level::Warn, "B", "meh", Some("do x")),
            check(Level::Fail, "C", "bad", Some("do y")),
            check(Level::Fail, "D", "worse", None),
        ];
        assert_eq!(failures(&checks), 2);
        let t = render(&checks);
        assert!(t.contains("[ ok ] A: fine\n"));
        assert!(t.contains("[warn] B: meh\n       fix: do x\n"));
        assert!(t.contains("[FAIL] C: bad\n       fix: do y\n"));
        assert!(t.contains("[FAIL] D: worse\n\n"));
        assert!(t.contains("2 problem(s), 1 warning(s)."));
        // Only warnings still is not "all good".
        assert!(
            render(&[check(Level::Warn, "B", "meh", None)]).contains("0 problem(s), 1 warning(s).")
        );
        assert!(render(&[]).contains("All good."));
        assert_eq!(failures(&[]), 0);
    }

    #[test]
    fn error_hints_cover_both_token_failures_and_ignore_everything_else() {
        assert!(
            hint_for_error("no control token yet")
                .unwrap()
                .contains("state director")
        );
        assert!(
            hint_for_error("rejected the control token")
                .unwrap()
                .contains("--state-dir")
        );
        assert!(
            hint_for_error("cannot reach GeekBridge service control port 127.0.0.1:1")
                .unwrap()
                .contains("systemctl")
        );
        assert_eq!(hint_for_error(""), None);
        assert_eq!(hint_for_error("timeout"), None);
    }
}
