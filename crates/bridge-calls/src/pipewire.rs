//! Linux adapter: PipeWire's `org.pipewire.Telephony` D-Bus service (zbus, blocking).
//!
//! Object layout (`telephony.c`, master; interface set confirmed against the installed
//! libspa-bluez5 1.6.9 XML):
//!
//! ```text
//! /org/pipewire/Telephony            ObjectManager.GetManagedObjects -> a{oa{sa{sv}}}  (phones)
//! /org/pipewire/Telephony/ag<N>      AudioGateway1, AudioGatewayTransport1, ObjectManager (calls)
//! /org/pipewire/Telephony/ag<N>/call<M>   Call1
//! ```
//!
//! Every method call is addressed to the service's *unique* bus name captured in the handle, so a
//! restarted service cannot receive a request meant for its predecessor, and no call can
//! D-Bus-activate anything: absence is detected with `GetNameOwner` first.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::Duration;

use async_io::Timer;
use bridge_core::calls::{CallId, Sensitive};
use futures_lite::{StreamExt, future};
use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};
use zbus::{MatchRule, MessageStream};

use crate::backend::CallBackend;
use crate::declared::{
    DeclaredApi, DeclaredInterface, IFACE_AG, IFACE_CALL, IFACE_OBJECT_MANAGER, IFACE_TRANSPORT,
    MANAGER_PATH, SERVICE, derive_capabilities,
};
use crate::error::{BackendError, ControlError, RemoteErrorKind};
use crate::guard;
use crate::model::{
    Action, AudioCodecEvidence, CallFingerprint, CallObservation, CallSnapshot, DeviceRef,
    PhoneSnapshot, ServiceStatus, Snapshot, TransportSnapshot, TransportState,
};

type Props = HashMap<String, OwnedValue>;
type Managed = HashMap<OwnedObjectPath, HashMap<String, Props>>;

/// Where to find the bus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BusTarget {
    /// `DBUS_SESSION_BUS_ADDRESS` (PipeWire's default for the telephony service).
    #[default]
    Session,
    /// The system bus (`bluez5.telephony.use-system-bus`).
    System,
    /// An explicit address (a private bus, as the tests use).
    Address(String),
}

impl BusTarget {
    pub fn connect(&self, method_timeout: Duration) -> zbus::Result<Connection> {
        let builder = match self {
            Self::Session => zbus::blocking::connection::Builder::session()?,
            Self::System => zbus::blocking::connection::Builder::system()?,
            Self::Address(a) => zbus::blocking::connection::Builder::address(a.as_str())?,
        };
        builder.method_timeout(method_timeout).build()
    }
}

#[derive(Clone, Debug)]
pub struct PipeWireConfig {
    pub bus: BusTarget,
    /// Bound on one method call. `Dial`, `Answer` and `Hangup` replies wait for the phone's AT
    /// response, so this must be generous; on expiry the outcome is reported as unknown.
    pub method_timeout: Duration,
}

impl Default for PipeWireConfig {
    fn default() -> Self {
        Self {
            bus: BusTarget::Session,
            method_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Default)]
struct ApiCache {
    owner: String,
    phone: Option<(Option<DeclaredInterface>, Option<DeclaredInterface>, bool)>,
    call: Option<DeclaredInterface>,
}

/// Bus access shared by the backend and the probe: reads, introspection, one control call.
pub(crate) struct Client {
    conn: Connection,
    cache: Mutex<ApiCache>,
}

fn bus_err(e: impl std::fmt::Display) -> BackendError {
    BackendError::Bus(e.to_string())
}

fn is_gone(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::MethodError(name, _, _)
        if matches!(name.as_str(),
            "org.freedesktop.DBus.Error.ServiceUnknown" | "org.freedesktop.DBus.Error.NameHasNoOwner"))
        || matches!(e, zbus::Error::FDO(f) if matches!(**f,
            zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_)))
}

fn is_unknown_object(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::MethodError(name, _, _)
        if name.as_str() == "org.freedesktop.DBus.Error.UnknownObject")
        || matches!(e, zbus::Error::FDO(f) if matches!(**f, zbus::fdo::Error::UnknownObject(_)))
}

fn is_timeout(e: &zbus::Error) -> bool {
    matches!(e, zbus::Error::InputOutput(io) if io.kind() == std::io::ErrorKind::TimedOut)
}

fn prop_str(p: &Props, key: &str) -> Option<String> {
    p.get(key)?.downcast_ref::<&str>().ok().map(str::to_owned)
}
fn prop_u8(p: &Props, key: &str) -> Option<u8> {
    p.get(key)?.downcast_ref::<u8>().ok()
}
fn prop_bool(p: &Props, key: &str) -> Option<bool> {
    p.get(key)?.downcast_ref::<bool>().ok()
}
fn sensitive_nonempty(p: &Props, key: &str) -> Option<Sensitive> {
    prop_str(p, key)
        .filter(|s| !s.is_empty())
        .map(Sensitive::new)
}

/// Parse introspection XML into per-interface method/property signatures.
pub(crate) fn parse_introspection(
    xml: &str,
) -> Result<BTreeMap<String, DeclaredInterface>, BackendError> {
    let node =
        zbus_xml::Node::from_reader(xml.as_bytes()).map_err(|e| BackendError::Malformed {
            context: "Introspect".into(),
            detail: e.to_string(),
        })?;
    let mut out = BTreeMap::new();
    for iface in node.interfaces() {
        let mut d = DeclaredInterface::default();
        for m in iface.methods() {
            let sig: String = m
                .args()
                .iter()
                .filter(|a| a.direction() != Some(zbus_xml::ArgDirection::Out))
                .map(|a| a.ty().to_string())
                .collect();
            d.methods.insert(m.name().to_string(), sig);
        }
        for p in iface.properties() {
            d.properties
                .insert(p.name().to_string(), p.ty().to_string());
        }
        out.insert(iface.name().to_string(), d);
    }
    Ok(out)
}

impl Client {
    pub(crate) fn new(conn: Connection) -> Self {
        Self {
            conn,
            cache: Mutex::new(ApiCache::default()),
        }
    }

    /// Unique name of the telephony service, or `None` if nothing owns the name. Never starts it.
    pub(crate) fn owner(&self) -> Result<Option<String>, BackendError> {
        let dbus = DBusProxy::new(&self.conn).map_err(bus_err)?;
        let name = zbus::names::BusName::try_from(SERVICE).map_err(bus_err)?;
        match dbus.get_name_owner(name) {
            Ok(o) => Ok(Some(o.to_string())),
            Err(zbus::fdo::Error::NameHasNoOwner(_)) => Ok(None),
            Err(e) => Err(bus_err(e)),
        }
    }

    fn managed(&self, owner: &str, path: &str) -> Result<Managed, zbus::Error> {
        let reply = self.conn.call_method(
            Some(owner),
            path,
            Some(IFACE_OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        )?;
        reply.body().deserialize::<Managed>()
    }

    fn introspect(&self, owner: &str, path: &str) -> Result<String, zbus::Error> {
        let reply = self.conn.call_method(
            Some(owner),
            path,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )?;
        reply.body().deserialize::<String>()
    }

    /// Declared API for this service process, introspected once per owner and cached.
    fn declared_api(
        &self,
        owner: &str,
        phone_path: &str,
        first_call_path: Option<&str>,
    ) -> Result<DeclaredApi, BackendError> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.owner != owner {
            *cache = ApiCache {
                owner: owner.to_string(),
                ..Default::default()
            };
        }
        if cache.phone.is_none() {
            let xml = self.introspect(owner, phone_path).map_err(bus_err)?;
            let mut ifaces = parse_introspection(&xml)?;
            cache.phone = Some((
                ifaces.remove(IFACE_AG),
                ifaces.remove(IFACE_TRANSPORT),
                ifaces.contains_key(IFACE_OBJECT_MANAGER),
            ));
        }
        if cache.call.is_none()
            && let Some(call_path) = first_call_path
        {
            let xml = self.introspect(owner, call_path).map_err(bus_err)?;
            cache.call = parse_introspection(&xml)?.remove(IFACE_CALL);
        }
        let (audio_gateway, transport, object_manager) = cache.phone.clone().unwrap_or_default();
        Ok(DeclaredApi {
            audio_gateway,
            transport,
            call: cache.call.clone(),
            object_manager,
        })
    }

    /// Build one phone from the manager's entry. `Ok(None)` if it vanished while being read.
    fn read_phone(
        &self,
        owner: &str,
        path: &str,
        ifaces: &HashMap<String, Props>,
        warnings: &mut Vec<String>,
    ) -> Result<Option<PhoneSnapshot>, BackendError> {
        let Some(ag) = ifaces.get(IFACE_AG) else {
            return Ok(None);
        };
        let Some(address) = prop_str(ag, "Address").filter(|a| !a.is_empty()) else {
            warnings.push(format!("{path}: AudioGateway1.Address missing; ignored"));
            return Ok(None);
        };
        let transport = ifaces.get(IFACE_TRANSPORT).map(|t| {
            let raw_state = prop_str(t, "State").unwrap_or_default();
            let codec_id = prop_u8(t, "Codec").unwrap_or(0);
            TransportSnapshot {
                state: TransportState::from_dbus(&raw_state),
                raw_state,
                codec_id,
                codec_evidence: AudioCodecEvidence::from_id(codec_id),
                reject_sco: prop_bool(t, "RejectSCO"),
            }
        });

        let call_objects = match self.managed(owner, path) {
            Ok(m) => m,
            Err(e) if is_unknown_object(&e) || is_gone(&e) => {
                warnings.push(format!("{path} vanished while being read"));
                return Ok(None);
            }
            Err(e) => return Err(bus_err(e)),
        };
        let mut calls = Vec::new();
        let mut call_paths: Vec<_> = call_objects.iter().collect();
        call_paths.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        for (call_path, call_ifaces) in call_paths {
            let Some(c) = call_ifaces.get(IFACE_CALL) else {
                continue;
            };
            let Some(id) = call_path
                .as_str()
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let Some(raw_state) = prop_str(c, "State") else {
                warnings.push(format!(
                    "{}: Call1.State missing; ignored",
                    call_path.as_str()
                ));
                continue;
            };
            calls.push(CallSnapshot {
                id: CallId::new(id),
                path: call_path.as_str().to_string(),
                observation: CallObservation::from_dbus(&raw_state),
                raw_state,
                fingerprint: CallFingerprint {
                    line_identification: sensitive_nonempty(c, "LineIdentification"),
                    incoming_line: sensitive_nonempty(c, "IncomingLine"),
                    name: sensitive_nonempty(c, "Name"),
                },
                multiparty: prop_bool(c, "Multiparty").unwrap_or(false),
            });
        }

        let declared_api =
            self.declared_api(owner, path, calls.first().map(|c| c.path.as_str()))?;
        let (capabilities, evidence) = derive_capabilities(&declared_api, transport.as_ref());
        Ok(Some(PhoneSnapshot {
            device: DeviceRef {
                owner: owner.to_string(),
                path: path.to_string(),
                address: Sensitive::new(address),
            },
            speaker_volume: prop_u8(ag, "SpeakerVolume"),
            microphone_volume: prop_u8(ag, "MicrophoneVolume"),
            transport,
            calls,
            declared_api,
            capabilities,
            evidence,
        }))
    }

    /// Read the service, optionally restricted to one phone path (used for fresh revalidation).
    pub(crate) fn snapshot_filtered(&self, only: Option<&str>) -> Result<Snapshot, BackendError> {
        let Some(owner) = self.owner()? else {
            return Ok(Snapshot::absent());
        };
        let managed = match self.managed(&owner, MANAGER_PATH) {
            Ok(m) => m,
            Err(e) if is_gone(&e) => return Ok(Snapshot::absent()),
            Err(e) => return Err(bus_err(e)),
        };
        let mut warnings = Vec::new();
        let mut phones = Vec::new();
        let mut entries: Vec<_> = managed.iter().collect();
        entries.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        for (path, ifaces) in entries {
            if only.is_some_and(|p| p != path.as_str()) || !ifaces.contains_key(IFACE_AG) {
                continue;
            }
            if let Some(p) = self.read_phone(&owner, path.as_str(), ifaces, &mut warnings)? {
                phones.push(p);
            }
        }
        Ok(Snapshot {
            service: ServiceStatus::Present { owner },
            phones,
            warnings,
        })
    }

    /// Revalidate against fresh state, then send exactly one method call.
    pub(crate) fn invoke(&self, action: &Action) -> Result<(), ControlError> {
        let handle = action.device();
        let owner = self
            .owner()
            .map_err(|e| ControlError::Bus(e.to_string()))?
            .ok_or(ControlError::ServiceGone)?;
        if owner != handle.owner {
            return Err(ControlError::ServiceRestarted);
        }
        let snapshot = self
            .snapshot_filtered(Some(&handle.path))
            .map_err(|e| ControlError::Bus(e.to_string()))?;
        let phone = match snapshot.service {
            ServiceStatus::Absent => return Err(ControlError::ServiceGone),
            ServiceStatus::Present { .. } => snapshot
                .phones
                .into_iter()
                .next()
                .ok_or(ControlError::NoSuchDevice)?,
        };
        let plan = guard::plan(action, &phone)?;

        let result = match &plan.arg {
            Some(arg) => self.conn.call_method(
                Some(owner.as_str()),
                plan.path.as_str(),
                Some(plan.spec.interface),
                plan.spec.member,
                &(arg.as_str(),),
            ),
            None => self.conn.call_method(
                Some(owner.as_str()),
                plan.path.as_str(),
                Some(plan.spec.interface),
                plan.spec.member,
                &(),
            ),
        };
        match result {
            Ok(_) => Ok(()),
            Err(e) => Err(map_call_error(e, action.call().is_some())),
        }
    }
}

fn map_call_error(e: zbus::Error, call_target: bool) -> ControlError {
    if is_timeout(&e) {
        return ControlError::Timeout;
    }
    if is_gone(&e) {
        return ControlError::ServiceGone;
    }
    if is_unknown_object(&e) {
        // The object disappeared between revalidation and the call.
        return if call_target {
            ControlError::NoSuchCall
        } else {
            ControlError::NoSuchDevice
        };
    }
    let method_error = match &e {
        zbus::Error::MethodError(name, desc, _) => Some((name.to_string(), desc.clone())),
        zbus::Error::FDO(f) => match &**f {
            zbus::fdo::Error::ZBus(zbus::Error::MethodError(name, desc, _)) => {
                Some((name.to_string(), desc.clone()))
            }
            other => Some((
                "org.freedesktop.DBus.Error.Failed".to_string(),
                Some(other.to_string()),
            )),
        },
        _ => None,
    };
    match method_error {
        Some((name, desc)) => ControlError::Remote {
            kind: RemoteErrorKind::from_dbus_name(&name),
            name,
            message: desc.unwrap_or_default(),
        },
        None => ControlError::Bus(e.to_string()),
    }
}

struct Watch {
    signals: MessageStream,
    owner_changes: MessageStream,
}

/// PipeWire telephony over D-Bus, as a [`CallBackend`].
pub struct PipeWireBackend {
    client: Client,
    watch: Mutex<Watch>,
}

impl PipeWireBackend {
    pub fn connect(config: PipeWireConfig) -> Result<Self, BackendError> {
        let conn = config.bus.connect(config.method_timeout).map_err(bus_err)?;
        Self::from_connection(conn)
    }

    /// Use an existing connection (tests, embedding).
    pub fn from_connection(conn: Connection) -> Result<Self, BackendError> {
        let aconn = conn.inner().clone();
        let signal_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE)
            .map_err(bus_err)?
            .path_namespace(MANAGER_PATH)
            .map_err(bus_err)?
            .build();
        let owner_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")
            .map_err(bus_err)?
            .interface("org.freedesktop.DBus")
            .map_err(bus_err)?
            .member("NameOwnerChanged")
            .map_err(bus_err)?
            .add_arg(SERVICE)
            .map_err(bus_err)?
            .build();
        let (signals, owner_changes) = async_io::block_on(async {
            let a = MessageStream::for_match_rule(signal_rule, &aconn, Some(256)).await?;
            let b = MessageStream::for_match_rule(owner_rule, &aconn, Some(16)).await?;
            Ok::<_, zbus::Error>((a, b))
        })
        .map_err(bus_err)?;
        Ok(Self {
            client: Client::new(conn),
            watch: Mutex::new(Watch {
                signals,
                owner_changes,
            }),
        })
    }
}

impl CallBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "pipewire_telephony_dbus"
    }

    fn snapshot(&self) -> Result<Snapshot, BackendError> {
        self.client.snapshot_filtered(None)
    }

    fn invoke(&self, action: &Action) -> Result<(), ControlError> {
        self.client.invoke(action)
    }

    fn wait_for_change(&self, timeout: Duration) -> Result<bool, BackendError> {
        let mut watch = self.watch.lock().unwrap_or_else(|e| e.into_inner());
        let Watch {
            signals,
            owner_changes,
        } = &mut *watch;
        let woke = async_io::block_on(async {
            let arrived = async {
                let m = future::or(signals.next(), owner_changes.next()).await;
                m.is_some()
            };
            let expired = async {
                Timer::after(timeout).await;
                false
            };
            if !future::or(arrived, expired).await {
                return false;
            }
            // Coalesce a burst into one wake-up.
            while future::poll_once(future::or(signals.next(), owner_changes.next()))
                .await
                .flatten()
                .is_some()
            {}
            true
        });
        Ok(woke)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The XML compiled into the installed libspa-bluez5.so (pipewire 1.6.9), extracted with
    /// `strings`, prefixed by the D-Bus introspection DOCTYPE exactly as `telephony.c` emits it.
    const INSTALLED_AG_XML: &str = include_str!("../tests/fixtures/pw169-audiogateway.xml");
    const INSTALLED_CALL_XML: &str = include_str!("../tests/fixtures/pw169-call.xml");
    const LIVE_MANAGER_XML: &str = include_str!("../tests/fixtures/pw169-manager-live.xml");

    #[test]
    fn parses_the_installed_audio_gateway_declaration() {
        let ifaces = parse_introspection(INSTALLED_AG_XML).unwrap();
        let ag = &ifaces[IFACE_AG];
        assert_eq!(ag.methods["Dial"], "s");
        assert_eq!(ag.methods["SendTones"], "s");
        for m in [
            "SwapCalls",
            "ReleaseAndAnswer",
            "ReleaseAndSwap",
            "HoldAndAnswer",
            "HangupAll",
            "CreateMultiparty",
        ] {
            assert_eq!(ag.methods[m], "", "{m}");
        }
        assert_eq!(ag.properties["Address"], "s");
        assert_eq!(ag.properties["SpeakerVolume"], "y");
        let t = &ifaces[IFACE_TRANSPORT];
        assert_eq!(t.methods["Activate"], "");
        assert_eq!(t.properties["State"], "s");
        assert_eq!(t.properties["Codec"], "y");
        assert_eq!(t.properties["RejectSCO"], "b");
        assert!(ifaces.contains_key(IFACE_OBJECT_MANAGER));
        // The oFono-compat interface is present in the XML but this crate never reads it.
        assert!(ifaces.contains_key("org.ofono.VoiceCallManager"));
    }

    #[test]
    fn parses_the_installed_call_declaration() {
        let ifaces = parse_introspection(INSTALLED_CALL_XML).unwrap();
        let c = &ifaces[IFACE_CALL];
        assert_eq!(c.methods["Answer"], "");
        assert_eq!(c.methods["Hangup"], "");
        for p in ["LineIdentification", "IncomingLine", "Name", "State"] {
            assert_eq!(c.properties[p], "s", "{p}");
        }
        assert_eq!(c.properties["Multiparty"], "b");
    }

    #[test]
    fn installed_declaration_yields_declared_never_verified() {
        let mut ag = parse_introspection(INSTALLED_AG_XML).unwrap();
        let mut call = parse_introspection(INSTALLED_CALL_XML).unwrap();
        let api = DeclaredApi {
            audio_gateway: ag.remove(IFACE_AG),
            transport: ag.remove(IFACE_TRANSPORT),
            call: call.remove(IFACE_CALL),
            object_manager: ag.contains_key(IFACE_OBJECT_MANAGER),
        };
        for spec in [
            crate::declared::DIAL,
            crate::declared::SEND_TONES,
            crate::declared::SWAP_CALLS,
            crate::declared::HANGUP_ALL,
            crate::declared::ACTIVATE,
            crate::declared::ANSWER,
            crate::declared::HANGUP,
        ] {
            assert_eq!(
                api.declaration(&spec),
                crate::declared::MethodDeclaration::Declared,
                "{spec:?}"
            );
        }
        let (caps, _) = derive_capabilities(&api, None);
        for f in bridge_core::calls::CallFeature::ALL {
            assert_ne!(caps.support(f), bridge_core::calls::Support::Verified);
        }
    }

    #[test]
    fn live_manager_object_has_no_phone_interfaces() {
        // Captured from the development machine: no phone connected, so the manager object
        // exports only the oFono Manager, ObjectManager and Introspectable.
        let ifaces = parse_introspection(LIVE_MANAGER_XML).unwrap();
        assert!(ifaces.contains_key("org.ofono.Manager"));
        assert!(ifaces.contains_key(IFACE_OBJECT_MANAGER));
        assert!(!ifaces.contains_key(IFACE_AG));
    }

    #[test]
    fn malformed_xml_is_an_error_not_an_empty_api() {
        assert!(matches!(
            parse_introspection("<node><interface name='x'>"),
            Err(BackendError::Malformed { .. })
        ));
    }

    #[test]
    fn error_name_mapping() {
        assert_eq!(
            RemoteErrorKind::from_dbus_name("org.pipewire.Telephony.Error.InvalidState"),
            RemoteErrorKind::InvalidState
        );
        assert_eq!(
            RemoteErrorKind::from_dbus_name("org.pipewire.Telephony.Error.CME"),
            RemoteErrorKind::Cme
        );
        assert_eq!(
            RemoteErrorKind::from_dbus_name("org.freedesktop.DBus.Error.Failed"),
            RemoteErrorKind::Other
        );
    }
}
