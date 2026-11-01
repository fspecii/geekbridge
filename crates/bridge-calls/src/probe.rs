//! Read-only inventory: which call backends, controllers, phones and calls exist right now, and
//! what each declares. Produces structured evidence; never calls a control method.
//!
//! The only bus operations it performs are `GetNameOwner`/`NameHasOwner`,
//! `GetConnectionUnixProcessID`, `GetManagedObjects`, `Introspect`, and (through the shared
//! snapshot code) property reads. Owner checks always precede calls so that probing can never
//! D-Bus-activate `bluetoothd` or any other service.

use bridge_core::calls::{CallFeature, Support, UnsupportedReason};
use serde::Serialize;

use crate::declared::{self, DeclaredApi, FeatureEvidence, MethodSpec};
use crate::model::{AudioCodecEvidence, TransportState};

pub const REPORT_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Default)]
pub enum BusChoice {
    /// The bus the OS provides (`DBUS_SESSION_BUS_ADDRESS` / system socket).
    #[default]
    Default,
    /// An explicit D-Bus address (private test bus).
    Address(String),
    Skip,
}

#[derive(Clone, Debug, Default)]
pub struct ProbeOptions {
    pub session: BusChoice,
    pub system: BusChoice,
    /// Print real numbers, names and Bluetooth addresses instead of masked values.
    pub show_private: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// This build has no call adapter for the host OS.
    PlatformUnavailable,
    /// The telephony service is not on any probed bus.
    NoTelephonyService,
    /// BlueZ is running but exposes zero Bluetooth adapters.
    NoBluetoothController,
    /// Service and controller exist; no phone is connected as an audio gateway.
    NoPhone,
    /// At least one phone is registered as an audio gateway. Says nothing about whether calls
    /// work: see `hardware_verified`.
    PhoneRegistered,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeSummary {
    pub verdict: Verdict,
    pub telephony_service_present: bool,
    /// `None` when BlueZ itself could not be queried.
    pub bluetooth_controllers: Option<usize>,
    pub phones: usize,
    pub calls: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct CallProbe {
    pub path: String,
    pub state: String,
    pub direction: String,
    pub number: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransportProbe {
    pub state: TransportState,
    pub raw_state: String,
    pub codec_id: u8,
    pub codec: AudioCodecEvidence,
}

#[derive(Clone, Debug, Serialize)]
pub struct PhoneProbe {
    pub path: String,
    pub address: String,
    pub transport: Option<TransportProbe>,
    pub calls: Vec<CallProbe>,
    pub declared_api: DeclaredApi,
    pub capabilities: Vec<FeatureEvidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TelephonyProbe {
    pub service: &'static str,
    pub owner: Option<String>,
    pub owner_process: Option<String>,
    /// Interfaces on `/org/pipewire/Telephony` per live introspection.
    pub manager_interfaces: Vec<String>,
    pub phones: Vec<PhoneProbe>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BusProbe {
    /// `session`, `system`, or `address`.
    pub bus: String,
    pub connected: bool,
    pub error: Option<String>,
    pub telephony: Option<TelephonyProbe>,
    /// Whether anything owns `org.ofono` here. Presence only; oFono is not used.
    pub ofono_name_owned: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterProbe {
    pub path: String,
    pub address: Option<String>,
    pub powered: Option<bool>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BluezDeviceProbe {
    pub path: String,
    pub address: Option<String>,
    pub connected: Option<bool>,
    pub paired: Option<bool>,
    /// Advertises the HFP Audio Gateway service class (0x111f): a phone-like device.
    pub hfp_audio_gateway: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BluezProbe {
    pub service_present: bool,
    pub owner_process: Option<String>,
    pub adapters: Vec<AdapterProbe>,
    pub devices: Vec<BluezDeviceProbe>,
    /// Objects exposing the experimental `org.bluez.Telephony1` (needs bluetoothd `-E`).
    pub telephony1_objects: usize,
    pub call1_objects: usize,
    pub error: Option<String>,
}

/// What the PipeWire API declares in its sources, independent of any connected phone.
#[derive(Clone, Debug, Serialize)]
pub struct StaticApiReference {
    /// `documentation`: not observed on this machine's bus.
    pub evidence_kind: &'static str,
    pub methods: Vec<MethodSpec>,
    pub sources: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProbeReport {
    pub schema: u32,
    pub tool: &'static str,
    /// Always true: the probe has no code path that invokes a control method.
    pub read_only: bool,
    pub platform: String,
    pub backend: &'static str,
    /// Numbers, names and Bluetooth addresses are masked.
    pub redacted: bool,
    pub buses: Vec<BusProbe>,
    pub bluez: Option<BluezProbe>,
    pub summary: ProbeSummary,
    /// Per-feature support derived from live introspection of the first phone, or an explicit
    /// "no phone" table. Declared support only.
    pub capability_evidence: Vec<FeatureEvidence>,
    /// Features exercised on real hardware and recorded. The probe cannot produce entries; it is
    /// empty until a hardware run is recorded elsewhere.
    pub hardware_verified: Vec<CallFeature>,
    pub static_api: StaticApiReference,
    /// Plain-language findings, including every "no X" the probe established.
    pub statements: Vec<String>,
}

pub fn static_api_reference() -> StaticApiReference {
    StaticApiReference {
        evidence_kind: "documentation",
        methods: vec![
            declared::DIAL,
            declared::SWAP_CALLS,
            declared::RELEASE_AND_ANSWER,
            declared::RELEASE_AND_SWAP,
            declared::HOLD_AND_ANSWER,
            declared::HANGUP_ALL,
            declared::CREATE_MULTIPARTY,
            declared::SEND_TONES,
            declared::ACTIVATE,
            declared::ANSWER,
            declared::HANGUP,
        ],
        sources: vec![
            "PipeWire spa/plugins/bluez5/telephony.c (master, read 2026-11-01)",
            "PipeWire spa/plugins/bluez5/backend-native.c (master, read 2026-11-01)",
            "XML compiled into installed libspa-bluez5.so (pipewire 1.6.9)",
        ],
    }
}

/// Features table when no phone exists: nothing can be introspected, so nothing is declared.
pub fn no_phone_evidence() -> Vec<FeatureEvidence> {
    CallFeature::ALL
        .into_iter()
        .map(|feature| match feature {
            CallFeature::MuteRinger => FeatureEvidence {
                feature,
                support: Support::Unsupported {
                    reason: UnsupportedReason::NotInProtocol,
                },
                basis: "HFP has no ringer-mute procedure in this API".into(),
            },
            CallFeature::MuteMicrophone => FeatureEvidence {
                feature,
                support: Support::Unsupported {
                    reason: UnsupportedReason::NotImplemented,
                },
                basis: "microphone mute is a host audio-graph operation, not part of this API"
                    .into(),
            },
            _ => FeatureEvidence {
                feature,
                support: Support::Unknown,
                basis: "no phone is connected: there is no object to introspect".into(),
            },
        })
        .collect()
}

pub fn mask_address(a: &str) -> String {
    let mut parts: Vec<&str> = a.split(':').collect();
    if parts.len() == 6 {
        for p in &mut parts[3..] {
            *p = "xx";
        }
        parts.join(":")
    } else {
        "<redacted>".into()
    }
}

pub fn mask_number(n: &str) -> String {
    let tail: String = n
        .chars()
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("***{tail}")
}

/// Compute the verdict and the plain-language statements from collected facts.
pub fn summarize(
    platform_available: bool,
    buses: &[BusProbe],
    bluez: Option<&BluezProbe>,
) -> (ProbeSummary, Vec<String>) {
    let mut statements = Vec::new();
    let phones: usize = buses
        .iter()
        .filter_map(|b| b.telephony.as_ref())
        .map(|t| t.phones.len())
        .sum();
    let calls: usize = buses
        .iter()
        .filter_map(|b| b.telephony.as_ref())
        .flat_map(|t| &t.phones)
        .map(|p| p.calls.len())
        .sum();
    let service = buses
        .iter()
        .filter_map(|b| b.telephony.as_ref())
        .any(|t| t.owner.is_some());
    let controllers = bluez
        .filter(|b| b.service_present && b.error.is_none())
        .map(|b| b.adapters.len());

    if !platform_available {
        statements.push(
            "No call backend exists for this operating system in this build; calling is unavailable here.".into(),
        );
    } else {
        if !service {
            statements.push(format!(
                "No {} service is on any probed bus: no telephony backend.",
                declared::SERVICE
            ));
        }
        match (bluez, controllers) {
            (Some(b), _) if !b.service_present => statements.push(
                "org.bluez is not on the system bus (bluetoothd not running): no Bluetooth controller can be reported.".into(),
            ),
            (_, Some(0)) => statements.push(
                "No Bluetooth controller: org.bluez is running but exposes zero org.bluez.Adapter1 objects.".into(),
            ),
            (Some(b), None) if b.error.is_some() => statements.push(format!(
                "Bluetooth controllers could not be enumerated: {}",
                b.error.as_deref().unwrap_or("unknown error")
            )),
            (None, _) => statements.push("BlueZ was not probed (system bus skipped).".into()),
            _ => {}
        }
        if service && phones == 0 {
            statements.push(
                "No phone: the telephony service exposes zero org.pipewire.Telephony.AudioGateway1 objects (no phone is connected as an HFP audio gateway).".into(),
            );
        }
        if phones > 0 {
            statements.push(format!(
                "{phones} phone object(s) registered, {calls} call object(s). Registration shows the HFP service-level connection came up per PipeWire's source; it does not show that calls or audio work."
            ));
        }
    }
    statements.push(
        "No live calling claim: no feature is hardware-verified. All support above is 'declared' (introspection/source) or 'unknown'.".into(),
    );

    let verdict = if !platform_available {
        Verdict::PlatformUnavailable
    } else if phones > 0 {
        Verdict::PhoneRegistered
    } else if controllers == Some(0) {
        Verdict::NoBluetoothController
    } else if !service {
        Verdict::NoTelephonyService
    } else {
        Verdict::NoPhone
    };
    (
        ProbeSummary {
            verdict,
            telephony_service_present: service,
            bluetooth_controllers: controllers,
            phones,
            calls,
        },
        statements,
    )
}

/// Human-readable rendering of a report.
pub fn render_text(r: &ProbeReport) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(s, "geekbridge-call-probe (read-only) on {}", r.platform);
    let _ = writeln!(
        s,
        "backend: {}   verdict: {:?}",
        r.backend, r.summary.verdict
    );
    for st in &r.statements {
        let _ = writeln!(s, "  - {st}");
    }
    for b in &r.buses {
        let _ = writeln!(
            s,
            "bus {}: connected={} {}",
            b.bus,
            b.connected,
            b.error.as_deref().unwrap_or("")
        );
        if let Some(t) = &b.telephony {
            let _ = writeln!(
                s,
                "  {}: owner={} process={} phones={}",
                t.service,
                t.owner.as_deref().unwrap_or("none"),
                t.owner_process.as_deref().unwrap_or("-"),
                t.phones.len()
            );
            for p in &t.phones {
                let _ = writeln!(
                    s,
                    "  phone {} addr={} calls={}",
                    p.path,
                    p.address,
                    p.calls.len()
                );
            }
        }
    }
    if let Some(b) = &r.bluez {
        let _ = writeln!(
            s,
            "bluez: present={} adapters={} devices={} telephony1={} call1={}",
            b.service_present,
            b.adapters.len(),
            b.devices.len(),
            b.telephony1_objects,
            b.call1_objects
        );
    }
    let _ = writeln!(s, "capability evidence (declared != verified):");
    for e in &r.capability_evidence {
        let _ = writeln!(s, "  {:?}: {:?} - {}", e.feature, e.support, e.basis);
    }
    let _ = writeln!(s, "hardware verified: {:?}", r.hardware_verified);
    s
}

/// Report for a platform without an adapter. Contacts no bus and runs no command.
pub fn unavailable_report(platform: &str, reason: &str) -> ProbeReport {
    let (summary, mut statements) = summarize(false, &[], None);
    statements.insert(0, reason.to_string());
    ProbeReport {
        schema: REPORT_SCHEMA,
        tool: "geekbridge-call-probe",
        read_only: true,
        platform: platform.to_string(),
        backend: "unavailable",
        redacted: true,
        buses: Vec::new(),
        bluez: None,
        summary,
        capability_evidence: CallFeature::ALL
            .into_iter()
            .map(|feature| FeatureEvidence {
                feature,
                support: Support::Unsupported {
                    reason: UnsupportedReason::NotImplemented,
                },
                basis: "no call backend is implemented for this platform".into(),
            })
            .collect(),
        hardware_verified: Vec::new(),
        static_api: static_api_reference(),
        statements,
    }
}

/// Probe the host. On non-Linux platforms this is [`unavailable_report`].
pub fn run(options: &ProbeOptions) -> ProbeReport {
    #[cfg(target_os = "linux")]
    {
        crate::probe_linux::run(options)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = options;
        let b = crate::backend::UnavailableBackend::for_this_platform();
        unavailable_report(b.platform(), b.reason())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus(owner: Option<&str>, phones: usize) -> BusProbe {
        BusProbe {
            bus: "session".into(),
            connected: true,
            error: None,
            ofono_name_owned: None,
            telephony: Some(TelephonyProbe {
                service: declared::SERVICE,
                owner: owner.map(str::to_string),
                owner_process: None,
                manager_interfaces: vec![],
                phones: (0..phones)
                    .map(|i| PhoneProbe {
                        path: format!("/org/pipewire/Telephony/ag{}", i + 1),
                        address: "AA:BB:CC:xx:xx:xx".into(),
                        transport: None,
                        calls: vec![],
                        declared_api: DeclaredApi::default(),
                        capabilities: vec![],
                    })
                    .collect(),
                warnings: vec![],
            }),
        }
    }
    fn bluez(adapters: usize) -> BluezProbe {
        BluezProbe {
            service_present: true,
            owner_process: None,
            adapters: (0..adapters)
                .map(|i| AdapterProbe {
                    path: format!("/org/bluez/hci{i}"),
                    address: None,
                    powered: None,
                })
                .collect(),
            devices: vec![],
            telephony1_objects: 0,
            call1_objects: 0,
            error: None,
        }
    }

    #[test]
    fn states_no_controller_and_no_phone_explicitly() {
        let (s, st) = summarize(true, &[bus(Some(":1.5"), 0)], Some(&bluez(0)));
        assert_eq!(s.verdict, Verdict::NoBluetoothController);
        assert_eq!(s.bluetooth_controllers, Some(0));
        assert!(
            st.iter().any(|x| x.starts_with("No Bluetooth controller")),
            "{st:?}"
        );
        assert!(st.iter().any(|x| x.starts_with("No phone")), "{st:?}");
        assert!(st.iter().any(|x| x.contains("No live calling claim")));
    }

    #[test]
    fn no_phone_with_a_controller() {
        let (s, st) = summarize(true, &[bus(Some(":1.5"), 0)], Some(&bluez(1)));
        assert_eq!(s.verdict, Verdict::NoPhone);
        assert!(st.iter().any(|x| x.starts_with("No phone")));
        assert!(!st.iter().any(|x| x.starts_with("No Bluetooth controller")));
    }

    #[test]
    fn missing_service_is_reported() {
        let (s, st) = summarize(true, &[bus(None, 0)], Some(&bluez(1)));
        assert_eq!(s.verdict, Verdict::NoTelephonyService);
        assert!(st.iter().any(|x| x.contains("no telephony backend")));
    }

    #[test]
    fn registered_phone_is_not_a_calling_claim() {
        let (s, st) = summarize(true, &[bus(Some(":1.5"), 1)], Some(&bluez(1)));
        assert_eq!(s.verdict, Verdict::PhoneRegistered);
        assert!(
            st.iter()
                .any(|x| x.contains("does not show that calls or audio work"))
        );
        assert!(st.iter().any(|x| x.contains("No live calling claim")));
    }

    #[test]
    fn unavailable_platform_report_is_explicit() {
        let r = unavailable_report("windows", "no adapter yet");
        assert_eq!(r.summary.verdict, Verdict::PlatformUnavailable);
        assert!(r.read_only && r.hardware_verified.is_empty());
        assert!(
            r.capability_evidence
                .iter()
                .all(|e| e.support != Support::Verified)
        );
        assert!(r.buses.is_empty());
        assert!(
            r.capability_evidence
                .iter()
                .all(|e| matches!(e.support, Support::Unsupported { .. }))
        );
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("platform_unavailable"));
    }

    #[test]
    fn masking() {
        assert_eq!(mask_address("AA:BB:CC:DD:EE:FF"), "AA:BB:CC:xx:xx:xx");
        assert_eq!(mask_address("garbage"), "<redacted>");
        assert_eq!(mask_number("+15550142"), "***42");
        assert_eq!(mask_number("7"), "***7");
    }

    #[test]
    fn no_phone_table_never_claims_support() {
        for e in no_phone_evidence() {
            assert!(
                matches!(e.support, Support::Unknown | Support::Unsupported { .. }),
                "{e:?}"
            );
        }
    }
}
