//! Redacted rendering of snapshots and command results for the CLI and logs.
//!
//! Phone numbers, contact names, Bluetooth addresses and device interface paths never appear in
//! the default output: devices and calls are identified by short fingerprints (which the CLI also
//! accepts as selectors). [`Redactor::reveal`] exists for a developer inspecting their own device
//! and must be chosen explicitly.
use crate::engine::*;
use crate::model::*;
use crate::readiness::*;
use bridge_core::calls::Sensitive;
use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Redactor {
    reveal: bool,
}
impl Redactor {
    pub fn redacted() -> Self {
        Self { reveal: false }
    }
    pub fn reveal() -> Self {
        Self { reveal: true }
    }
    pub fn is_reveal(&self) -> bool {
        self.reveal
    }

    /// Keep the last two characters of a number.
    pub fn number(&self, s: &Sensitive) -> String {
        if self.reveal {
            return s.expose().to_owned();
        }
        let tail: String = {
            let chars: Vec<char> = s.expose().chars().collect();
            chars[chars.len().saturating_sub(2)..].iter().collect()
        };
        format!("***{tail}")
    }
    /// Keep the first character of a name.
    pub fn name(&self, s: &Sensitive) -> String {
        if self.reveal {
            return s.expose().to_owned();
        }
        match s.expose().chars().next() {
            Some(c) => format!("{c}***"),
            None => String::new(),
        }
    }
    /// Keep the last octet of a Bluetooth address.
    pub fn address(&self, s: &Sensitive) -> String {
        if self.reveal {
            return s.expose().to_owned();
        }
        let last: String = s
            .expose()
            .chars()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("**:**:**:**:**:{last}")
    }
    pub fn device(&self, id: &TransportDeviceId) -> String {
        if self.reveal {
            id.as_str().to_owned()
        } else {
            format!("#{}", id.fingerprint())
        }
    }
}

fn val<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}
fn step<T: Serialize>(s: &Step<T>) -> Value {
    match s {
        Ok(v) => val(v),
        Err(e) => json!({ "error": val(e) }),
    }
}
fn opt<T: Serialize>(v: &Option<T>) -> Value {
    v.as_ref().map_or(Value::Null, val)
}

pub fn environment_json(env: &Step<EnvironmentFacts>) -> Value {
    match env {
        Err(e) => json!({ "error": val(e) }),
        Ok(env) => json!({
            "os": {
                "platform": val(&env.os.platform),
                "build": env.os.build,
                "windows_11": env.os.is_windows_11(),
            },
            "contract": val(&env.contract),
            "package_identity": match &env.package {
                PackageIdentity::Present { family_name } =>
                    json!({ "state": "present", "family_name": family_name }),
                PackageIdentity::Absent => json!({ "state": "absent" }),
                PackageIdentity::Unknown { code } => json!({ "state": "unknown", "code": code }),
            },
            "phone_calls_privacy": val(&env.privacy),
        }),
    }
}

pub fn call_json(call: &CallView, r: &Redactor) -> Value {
    let c = &call.record;
    json!({
        "call": call.reference.fingerprint(),
        "status": val(&c.status),
        "direction": val(&c.direction),
        "audio_device": val(&c.audio_device),
        "muted": c.muted,
        "hold_supported": c.hold_supported,
        "phase": call.phase.map(|p| val(&p)),
        "number": c.number.as_ref().map(|n| r.number(n)),
        "name": c.name.as_ref().map(|n| r.name(n)),
        "core_id": if r.is_reveal() { json!(call.core_id.as_str()) } else { Value::Null },
    })
}

pub fn device_json(index: usize, d: &DeviceView, r: &Redactor) -> Value {
    let (transport, registered, routing, ringing) = match &d.info {
        Ok(i) => (
            step(&i.transport),
            step(&i.registered),
            step(&i.audio_routing),
            step(&i.in_band_ringing),
        ),
        Err(e) => {
            let err = json!({ "error": val(e) });
            (err.clone(), err.clone(), err.clone(), err)
        }
    };
    json!({
        "index": index + 1,
        "device": d.id.fingerprint(),
        "device_id": if r.is_reveal() { json!(d.id.as_str()) } else { Value::Null },
        "epoch": d.epoch,
        "selected": d.selected,
        "name": d.name.as_ref().map(|n| r.name(n)),
        "address": d.address.as_ref().map(|a| r.address(a)),
        "enabled": d.enabled,
        "transport": transport,
        "registered": registered,
        "access": val(&d.access),
        "connect_attempt": val(&d.connect),
        "audio_routing": routing,
        "in_band_ringing": ringing,
        "line": d.line.as_ref().map(|l| json!({
            "line": crate::model::fingerprint(l.id.as_str()),
            "can_dial": l.can_dial,
            "ambiguous": d.line_ambiguous,
        })),
        "calls": d.calls.iter().map(|c| call_json(c, r)).collect::<Vec<_>>(),
        "calls_error": opt(&d.calls_error),
        "readiness": {
            "control_ready": d.readiness.control_ready(),
            "dial_ready": d.readiness.dial_blocker().is_none(),
            "audio": val(&d.readiness.audio),
            "blockers": val(&d.readiness.blockers),
        },
    })
}

pub fn snapshot_json(s: &Snapshot, r: &Redactor) -> Value {
    json!({
        "seq": s.seq,
        "environment": environment_json(&s.env),
        "environment_blockers": val(&s.env_blockers),
        "devices": s.devices.iter().enumerate().map(|(i, d)| device_json(i, d, r)).collect::<Vec<_>>(),
        "transports_error": opt(&s.transports_error),
        "lines_error": opt(&s.lines_error),
        "truncated": s.truncated,
    })
}

pub fn command_report_json(c: &CommandReport, r: &Redactor) -> Value {
    json!({
        "action": c.action,
        "outcome": val(&c.outcome),
        "steps_done": c.steps_done,
        "dialed_call": c.dialed.as_ref().map(|d| d.fingerprint()),
        "audio_route_step": c.route.as_ref().map(val),
        "call_after": c.after.as_ref().map(|a| call_json(a, r)),
    })
}

/// One-line English for a blocker, for the text report.
pub fn describe_blocker(b: &Blocker) -> String {
    match b {
        Blocker::NotWindows => "This host is not Windows; the phone-line APIs do not exist here.".into(),
        Blocker::EnvironmentUnreadable { kind } => {
            format!("The environment could not be read ({kind:?}).")
        }
        Blocker::UnsupportedOs { build } => format!(
            "Windows 11 (build 22000 or later) is required; this host reports build {}. Windows 10 is unsupported.",
            build.map_or("unknown".into(), |b| b.to_string())
        ),
        Blocker::ContractMissing { required_major } => format!(
            "Windows.ApplicationModel.Calls.CallsPhoneContract version {required_major} is not present."
        ),
        Blocker::ContractUnknown => "Could not determine which phone-call API contract versions exist.".into(),
        Blocker::PackageIdentityMissing => "The process has no package identity; register the sparse package (packaging/windows) and run the registered executable.".into(),
        Blocker::PackageIdentityUnknown => "Package identity could not be determined.".into(),
        Blocker::PrivacyBlocked { access } => format!(
            "Phone calls access is blocked ({access:?}); check Settings > Privacy & security > Phone calls and the manifest capability."
        ),
        Blocker::EnumerationFailed { kind } => format!("Enumerating phone transports failed ({kind:?})."),
        Blocker::NoPairedTransport => "No Bluetooth phone is available as a phone-line transport; pair the phone in Windows Bluetooth settings first.".into(),
        Blocker::DeviceDisabled => "Windows reports this transport as disabled.".into(),
        Blocker::DeviceUnreadable { kind } => format!(
            "The transport could not be opened ({kind:?}); access may be denied, or another hands-free stack owns the phone."
        ),
        Blocker::AccessNotGranted { access } => format!(
            "Transport access has not been granted in this session ({access:?}); run the explicit request-access or register action."
        ),
        Blocker::NotRegistered => "This app is not registered for the phone's transport; run the explicit register action.".into(),
        Blocker::LinesUnreadable { kind } => format!("Reading phone lines failed ({kind:?})."),
        Blocker::NoLine => "Registered, but Windows shows no phone line: the phone is not connected yet (run the explicit connect action) or the link is coming up.".into(),
        Blocker::LineNotDialable => "The phone line cannot dial right now.".into(),
        Blocker::LineDialabilityUnknown => "Whether the line can dial could not be read.".into(),
    }
}

pub fn describe_audio(a: AudioReadiness) -> &'static str {
    match a {
        AudioReadiness::NotApplicable => "not applicable until control is ready",
        AudioReadiness::Routable => {
            "Windows reports the call audio can be routed to this computer (not measured)"
        }
        AudioReadiness::Blocked => {
            "BLOCKED: Windows reports it cannot route call audio to this computer (is a Bluetooth headset connected?)"
        }
        AudioReadiness::Unreported => "Windows did not report audio routing",
    }
}

/// Human-readable multi-line report.
pub fn snapshot_text(s: &Snapshot, r: &Redactor) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "GeekBridge Windows calling diagnosis (read-only), refresh #{}",
        s.seq
    );
    match &s.env {
        Err(e) => {
            let _ = writeln!(out, "environment: unreadable: {e}");
        }
        Ok(env) => {
            let _ = writeln!(
                out,
                "environment: {:?}, build {}, contract v5={} v6={} v7={}, package identity {}, phone-calls privacy {:?}",
                env.os.platform,
                env.os.build.map_or("?".into(), |b| b.to_string()),
                tri(env.contract.v5),
                tri(env.contract.v6),
                tri(env.contract.v7),
                match &env.package {
                    PackageIdentity::Present { family_name } => format!("present ({family_name})"),
                    PackageIdentity::Absent => "absent".into(),
                    PackageIdentity::Unknown { code } => format!("unknown ({code})"),
                },
                env.privacy,
            );
        }
    }
    for b in &s.env_blockers {
        let _ = writeln!(out, "  blocked: {}", describe_blocker(b));
    }
    if let Some(e) = &s.transports_error {
        let _ = writeln!(out, "phone transports: unreadable: {e}");
    }
    if let Some(e) = &s.lines_error {
        let _ = writeln!(out, "phone lines: unreadable: {e}");
    }
    if s.devices.is_empty() && s.env_blockers.is_empty() && s.transports_error.is_none() {
        let _ = writeln!(out, "{}", describe_blocker(&Blocker::NoPairedTransport));
    }
    for (i, d) in s.devices.iter().enumerate() {
        let _ = writeln!(
            out,
            "device {} [{}]{} \"{}\" address {}",
            i + 1,
            r.device(&d.id),
            if d.selected { " (selected)" } else { "" },
            d.name.as_ref().map_or(String::new(), |n| r.name(n)),
            d.address.as_ref().map_or("?".into(), |a| r.address(a)),
        );
        if let Ok(info) = &d.info {
            let _ = writeln!(
                out,
                "  registered: {}  access (this session): {:?}  connect attempt: {:?}",
                step_text(&info.registered),
                d.access,
                d.connect
            );
            let _ = writeln!(
                out,
                "  audio routing: {}  in-band ringing: {}",
                step_text(&info.audio_routing),
                step_text(&info.in_band_ringing)
            );
        }
        match &d.line {
            Some(l) => {
                let _ = writeln!(
                    out,
                    "  line: visible, can_dial {}{}",
                    l.can_dial.map_or("?".into(), |b| b.to_string()),
                    if d.line_ambiguous {
                        " (several lines matched)"
                    } else {
                        ""
                    }
                );
            }
            None => {
                let _ = writeln!(out, "  line: not visible");
            }
        }
        if let Some(e) = &d.calls_error {
            let _ = writeln!(out, "  calls: unreadable: {e}");
        }
        for c in &d.calls {
            let _ = writeln!(
                out,
                "  call [{}] {:?} {:?} audio {:?} muted {} number {}",
                c.reference.fingerprint(),
                c.record.status,
                c.record.direction,
                c.record.audio_device,
                c.record.muted.map_or("?".into(), |m| m.to_string()),
                c.record.number.as_ref().map_or("?".into(), |n| r.number(n)),
            );
        }
        for b in &d.readiness.blockers {
            let _ = writeln!(out, "  blocked: {}", describe_blocker(b));
        }
        let _ = writeln!(
            out,
            "  control ready: {}  dial ready: {}  audio: {}",
            d.readiness.control_ready(),
            d.readiness.dial_blocker().is_none(),
            describe_audio(d.readiness.audio)
        );
    }
    out
}

fn tri(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "?",
    }
}
fn step_text<T: std::fmt::Debug>(s: &Step<T>) -> String {
    match s {
        Ok(v) => format!("{v:?}"),
        Err(e) => format!("unreadable ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_numbers_names_and_addresses() {
        let r = Redactor::redacted();
        assert_eq!(r.number(&Sensitive::new("+15550123")), "***23");
        assert_eq!(r.name(&Sensitive::new("Alice")), "A***");
        assert_eq!(
            r.address(&Sensitive::new("aa:bb:cc:dd:ee:ff")),
            "**:**:**:**:**:ff"
        );
        let id = TransportDeviceId::new("\\\\?\\BTHENUM#secret").unwrap();
        assert!(!r.device(&id).contains("secret"));
        assert_eq!(
            Redactor::reveal().number(&Sensitive::new("+15550123")),
            "+15550123"
        );
    }

    #[test]
    fn default_report_never_contains_private_values() {
        use crate::testing::*;
        let mock = MockPlatform::connected();
        mock.set_calls(vec![call("call-1", NativeCallStatus::Talking)]);
        let mut engine = Engine::new(mock, EngineConfig::default());
        let snap = engine.refresh();
        let text = snapshot_text(&snap, &Redactor::redacted());
        let json = snapshot_json(&snap, &Redactor::redacted()).to_string();
        for secret in [
            "+15550100",
            "Test Caller",
            "Test Phone",
            "aa:bb:cc:dd:ee",
            "BTHENUM",
        ] {
            assert!(!text.contains(secret), "text leaked {secret}");
            assert!(!json.contains(secret), "json leaked {secret}");
        }
        let revealed = snapshot_json(&snap, &Redactor::reveal()).to_string();
        assert!(revealed.contains("+15550100"));
    }
}
