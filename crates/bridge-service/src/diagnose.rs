//! `geekbridge-ctl diagnose`: a support bundle that is safe to paste into a bug report.
//!
//! The report carries versions, capability states, counters and the last error *kinds*. It never
//! carries the control token, certificates or fingerprints, device names, IP addresses, phone
//! numbers or clipboard/file content. Devices are shown as `device-1`, `device-2`, ... in the
//! order the service lists them, so two lines about the same phone can still be matched.

use bridge_core::ipc::{
    CallingStatus, ClipboardStatus, EventKind, EventPage, LinkPhase, Status, UpdateStatus,
};
use std::fmt::Write as _;

/// Everything `diagnose` reads from the service. Each part is optional: a failed call is reported
/// as a note instead of aborting the whole bundle.
pub struct Inputs {
    pub status: Status,
    pub events: Option<EventPage>,
    pub calling: Option<CallingStatus>,
    pub clipboard: Option<ClipboardStatus>,
    pub update: Option<UpdateStatus>,
    pub notes: Vec<String>,
}

/// Replaces anything that looks like an IPv4/IPv6 address, a long hex string (fingerprints, ids)
/// or a phone-number-like digit run in a service-generated message, and bounds its length.
pub fn scrub(text: &str) -> String {
    let mut out = String::new();
    for word in text.split_whitespace().take(40) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&scrub_word(word));
    }
    out.chars().take(160).collect()
}

fn scrub_word(word: &str) -> String {
    let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != ':' && c != '.');
    let hex_run = core.chars().filter(char::is_ascii_hexdigit).count();
    let digits = core.chars().filter(char::is_ascii_digit).count();
    let looks_ip = (core.matches('.').count() == 3 && digits >= 4)
        || (core.matches(':').count() >= 2 && hex_run >= 4);
    let looks_secret = core.len() >= 16 && hex_run * 10 >= core.len() * 9;
    let looks_number = digits >= 6;
    if looks_ip || looks_secret || looks_number {
        word.replace(core, "<redacted>")
    } else {
        word.to_owned()
    }
}

fn alias(status: &Status, device_id: &str) -> String {
    status
        .paired
        .iter()
        .map(|d| d.device_id.as_str())
        .chain(status.links.iter().map(|l| l.device_id.as_str()))
        .fold(Vec::<&str>::new(), |mut seen, id| {
            if !seen.contains(&id) {
                seen.push(id);
            }
            seen
        })
        .iter()
        .position(|id| *id == device_id)
        .map_or_else(|| "device-?".into(), |i| format!("device-{}", i + 1))
}

fn event_label(kind: &EventKind) -> String {
    let json = serde_json::to_value(kind).unwrap_or_default();
    let name = json
        .get("kind")
        .and_then(|k| k.as_str())
        .unwrap_or("unknown")
        .to_owned();
    match json.get("reason").and_then(|r| r.as_str()) {
        Some(reason) => format!("{name} ({})", scrub(reason)),
        None => name,
    }
}

fn is_error(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::ConnectFailed { .. }
            | EventKind::PairFailed { .. }
            | EventKind::LinkLost { .. }
            | EventKind::PacketRejected
    )
}

pub fn render(i: &Inputs, host: &str) -> String {
    let s = &i.status;
    let mut o = String::new();
    let _ = writeln!(
        o,
        "GeekBridge support report (redacted: no names, addresses, keys or content)"
    );
    let _ = writeln!(o, "\n== Versions ==");
    let _ = writeln!(o, "geekbridge-ctl: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        o,
        "service:        {} (ipc {})",
        scrub(&s.service_version),
        s.ipc_version
    );
    let _ = writeln!(o, "host:           {host}");
    let _ = writeln!(
        o,
        "inbound listener: {}",
        if s.inbound_listener.is_some() {
            "listening"
        } else {
            "NOT listening"
        }
    );

    let _ = writeln!(o, "\n== Paired devices ({}) ==", s.paired.len());
    for d in &s.paired {
        let consents: Vec<String> = d
            .consents
            .iter()
            .map(|c| format!("{:?}={}", c.feature, if c.enabled { "on" } else { "off" }))
            .collect();
        let link = s.links.iter().find(|l| l.device_id == d.device_id);
        let _ = writeln!(
            o,
            "{}: type {}, link {}, consents [{}]",
            alias(s, &d.device_id),
            scrub(&d.device_type),
            link.map_or("none".into(), |l| format!("{:?}", l.phase).to_lowercase()),
            consents.join(", ")
        );
    }
    let _ = writeln!(o, "pending pairing challenges: {}", s.challenges.len());

    let _ = writeln!(o, "\n== Links and perf counters ==");
    for l in &s.links {
        let _ = writeln!(
            o,
            "{}: {}, negotiated packet types {}, pings {}, rejected packets {}, up since unix_ms {}",
            alias(s, &l.device_id),
            if l.phase == LinkPhase::Connected {
                "connected"
            } else {
                "not connected"
            },
            l.negotiated.len(),
            l.pings_received,
            l.rejected_packets,
            l.since_unix_ms
        );
    }
    let _ = writeln!(o, "events recorded: {}", s.event_seq);

    let _ = writeln!(o, "\n== Capabilities ==");
    match &i.calling {
        Some(c) => {
            let _ = writeln!(
                o,
                "calling: {} / {} - {} (phones {}, calls {})",
                scrub(&c.platform),
                scrub(&c.backend),
                scrub(&c.verdict),
                c.phones,
                c.calls
            );
        }
        None => {
            let _ = writeln!(o, "calling: unavailable");
        }
    }
    match &i.clipboard {
        Some(c) => {
            let _ = writeln!(
                o,
                "clipboard: backend {}, available {}, observe {}, watching {}, window {} s, hotkey {} ({}), image limit {} MiB{}",
                scrub(&c.backend),
                c.available,
                scrub(&c.observe),
                c.watching,
                c.window_ms / 1000,
                scrub(&c.hotkey),
                scrub(&c.hotkey_binding),
                c.max_image_bytes / (1024 * 1024),
                c.problem
                    .as_ref()
                    .map_or(String::new(), |p| format!(", problem: {}", scrub(p)))
            );
            if let Some(p) = &c.last_pull {
                let _ = writeln!(
                    o,
                    "clipboard last pull: {} {}",
                    scrub(&p.state),
                    p.reason.as_deref().map_or(String::new(), scrub)
                );
            }
            for d in &c.devices {
                let _ = writeln!(
                    o,
                    "clipboard {}: {}, {}, negotiated {}, sent {}, received {}, skipped {}{}",
                    alias(s, &d.device_id),
                    if d.enabled { "enabled" } else { "off" },
                    if d.connected { "connected" } else { "offline" },
                    d.negotiated,
                    d.sent,
                    d.received,
                    d.skipped,
                    d.last_skip
                        .as_ref()
                        .map_or(String::new(), |r| format!(", last skip {}", scrub(r)))
                );
            }
        }
        None => {
            let _ = writeln!(o, "clipboard: unavailable");
        }
    }
    match &i.update {
        Some(u) => {
            let _ = writeln!(
                o,
                "updates: {:?}, auto-check {}, current {}, latest {}, downloadable {}",
                u.state,
                u.enabled,
                scrub(&u.current_version),
                u.latest
                    .as_ref()
                    .map_or("unknown".into(), |l| scrub(&l.version)),
                u.downloadable
            );
        }
        None => {
            let _ = writeln!(o, "updates: unavailable");
        }
    }

    let _ = writeln!(o, "\n== Last errors ==");
    let mut errors = 0;
    if let Some(page) = &i.events {
        for e in page
            .events
            .iter()
            .rev()
            .filter(|e| is_error(&e.kind))
            .take(10)
        {
            errors += 1;
            let who = e.device_id.as_deref().map_or("-".into(), |d| alias(s, d));
            let _ = writeln!(
                o,
                "event {} at unix_ms {} {}: {}",
                e.seq,
                e.at_unix_ms,
                who,
                event_label(&e.kind)
            );
        }
        if page.gap {
            let _ = writeln!(o, "(older events were discarded)");
        }
    }
    if let Some(u) = &i.update
        && let Some(e) = &u.error
    {
        errors += 1;
        let _ = writeln!(o, "update: {}", scrub(e));
    }
    if errors == 0 {
        let _ = writeln!(o, "none");
    }
    if !i.notes.is_empty() {
        let _ = writeln!(o, "\n== Notes ==");
        for n in &i.notes {
            let _ = writeln!(o, "{}", scrub(n));
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_hides_addresses_keys_and_numbers() {
        let s = scrub(
            "connect to 192.168.1.20:1716 failed for +491701234567 cert AABBCCDDEEFF00112233445566778899",
        );
        assert!(!s.contains("192.168"), "{s}");
        assert!(!s.contains("1701234567"), "{s}");
        assert!(!s.contains("AABBCC"), "{s}");
        assert!(s.contains("failed"));
    }

    #[test]
    fn scrub_keeps_plain_reasons() {
        assert_eq!(scrub("timed out"), "timed out");
    }

    #[test]
    fn scrub_is_bounded() {
        assert!(scrub(&"word ".repeat(500)).chars().count() <= 160);
    }
}
