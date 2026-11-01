//! Private `dbus-daemon` plus a mock `org.pipewire.Telephony` service that records every method
//! call it receives. No Bluetooth, no phone: the mock only answers the declared D-Bus API.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

pub const SERVICE: &str = "org.pipewire.Telephony";
pub const MANAGER: &str = "/org/pipewire/Telephony";
pub const AG: &str = "org.pipewire.Telephony.AudioGateway1";
pub const TRANSPORT: &str = "org.pipewire.Telephony.AudioGatewayTransport1";
pub const CALL: &str = "org.pipewire.Telephony.Call1";

type Props = HashMap<String, OwnedValue>;
type Ifaces = HashMap<String, Props>;
type Managed = HashMap<OwnedObjectPath, Ifaces>;

// ------------------------------------------------------------------ private bus

pub struct PrivateBus {
    child: Child,
    pub address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    /// `None` (with a loud message) if no `dbus-daemon` is installed, unless
    /// `GEEKBRIDGE_REQUIRE_DBUS=1`, in which case it panics.
    pub fn start() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bus");
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                socket.display()
            ),
        )
        .unwrap();
        let spawned = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .arg("--nofork")
            .arg("--print-address=1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                if std::env::var("GEEKBRIDGE_REQUIRE_DBUS").as_deref() == Ok("1") {
                    panic!("dbus-daemon required but unavailable: {e}");
                }
                eprintln!(
                    "SKIPPED: dbus-daemon not available ({e}); set GEEKBRIDGE_REQUIRE_DBUS=1 to fail instead"
                );
                return None;
            }
        };
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_string();
        assert!(address.starts_with("unix:"), "bad bus address {address:?}");
        Some(Self {
            child,
            address,
            _dir: dir,
        })
    }

    pub fn connect(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ------------------------------------------------------------------ mock model

#[derive(Clone, Debug)]
pub struct CallModel {
    pub state: String,
    pub line: String,
    pub incoming_line: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct PhoneModel {
    pub address: String,
    pub transport: Option<(String, u8)>,
    pub calls: BTreeMap<u32, CallModel>,
}

/// One received method call, exactly as the wire carried it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    pub path: String,
    pub interface: &'static str,
    pub member: &'static str,
    /// Arguments rendered as the single string argument, or empty.
    pub arg: String,
}

#[derive(Default)]
pub struct Shared {
    pub phones: Mutex<BTreeMap<u32, PhoneModel>>,
    pub log: Mutex<Vec<Invocation>>,
    /// Make the next method call fail with this telephony error (`Failed`, `InvalidState`, ...).
    pub fail_next: Mutex<Option<&'static str>>,
    /// Make the next method call take this long before replying.
    pub delay_next: Mutex<Option<Duration>>,
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.pipewire.Telephony.Error")]
pub enum PwError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    NotSupported(String),
    InvalidFormat(String),
    InvalidState(String),
    InProgress(String),
    #[zbus(name = "CME")]
    Cme(String),
}

impl Shared {
    fn record(
        &self,
        path: &str,
        interface: &'static str,
        member: &'static str,
        arg: &str,
    ) -> Result<(), PwError> {
        self.log.lock().unwrap().push(Invocation {
            path: path.into(),
            interface,
            member,
            arg: arg.into(),
        });
        let delay = self.delay_next.lock().unwrap().take();
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
        match self.fail_next.lock().unwrap().take() {
            None => Ok(()),
            Some("Failed") => Err(PwError::Failed("Method call failed".into())),
            Some("NotSupported") => Err(PwError::NotSupported(
                "Method is not supported on this Audio Gateway".into(),
            )),
            Some("InvalidFormat") => Err(PwError::InvalidFormat(
                "Invalid phone number or tones".into(),
            )),
            Some("InvalidState") => Err(PwError::InvalidState(
                "The current state does not allow this method call".into(),
            )),
            Some("InProgress") => Err(PwError::InProgress("Command already in progress".into())),
            Some("CME") => Err(PwError::Cme("SIM not inserted".into())),
            Some(other) => panic!("unknown injected error {other}"),
        }
    }
    pub fn log(&self) -> Vec<Invocation> {
        self.log.lock().unwrap().clone()
    }
}

fn v<T: Into<Value<'static>>>(x: T) -> OwnedValue {
    OwnedValue::try_from(x.into()).unwrap()
}
fn call_props(c: &CallModel) -> Props {
    HashMap::from([
        ("LineIdentification".to_string(), v(c.line.clone())),
        ("IncomingLine".to_string(), v(c.incoming_line.clone())),
        ("Name".to_string(), v(c.name.clone())),
        ("Multiparty".to_string(), v(false)),
        ("State".to_string(), v(c.state.clone())),
    ])
}
fn phone_ifaces(p: &PhoneModel) -> Ifaces {
    let mut m = Ifaces::new();
    m.insert(
        AG.into(),
        HashMap::from([
            ("Address".to_string(), v(p.address.clone())),
            ("SpeakerVolume".to_string(), v(9u8)),
            ("MicrophoneVolume".to_string(), v(7u8)),
        ]),
    );
    if let Some((state, codec)) = &p.transport {
        m.insert(
            TRANSPORT.into(),
            HashMap::from([
                ("State".to_string(), v(state.clone())),
                ("Codec".to_string(), v(*codec)),
                ("RejectSCO".to_string(), v(false)),
            ]),
        );
    }
    m
}

// ------------------------------------------------------------------ mock objects

struct ManagerObj(Arc<Shared>);
#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl ManagerObj {
    fn get_managed_objects(&self) -> Managed {
        self.0
            .phones
            .lock()
            .unwrap()
            .iter()
            .map(|(n, p)| {
                (
                    OwnedObjectPath::try_from(format!("{MANAGER}/ag{n}")).unwrap(),
                    phone_ifaces(p),
                )
            })
            .collect()
    }
}

struct AgManagerObj(u32, Arc<Shared>);
#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl AgManagerObj {
    fn get_managed_objects(&self) -> Managed {
        let phones = self.1.phones.lock().unwrap();
        phones
            .get(&self.0)
            .map(|p| {
                p.calls
                    .iter()
                    .map(|(m, c)| {
                        (
                            OwnedObjectPath::try_from(format!("{MANAGER}/ag{}/call{m}", self.0))
                                .unwrap(),
                            Ifaces::from([(CALL.to_string(), call_props(c))]),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

struct AgObj(u32, Arc<Shared>);
impl AgObj {
    fn path(&self) -> String {
        format!("{MANAGER}/ag{}", self.0)
    }
    fn with<T>(&self, f: impl FnOnce(&PhoneModel) -> T) -> T {
        f(&self.1.phones.lock().unwrap()[&self.0])
    }
}
#[zbus::interface(name = "org.pipewire.Telephony.AudioGateway1")]
impl AgObj {
    fn dial(&self, number: String) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "Dial", &number)
    }
    fn swap_calls(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "SwapCalls", "")
    }
    fn release_and_answer(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "ReleaseAndAnswer", "")
    }
    fn release_and_swap(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "ReleaseAndSwap", "")
    }
    fn hold_and_answer(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "HoldAndAnswer", "")
    }
    fn hangup_all(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "HangupAll", "")
    }
    fn create_multiparty(&self) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "CreateMultiparty", "")
    }
    fn send_tones(&self, tones: String) -> Result<(), PwError> {
        self.1.record(&self.path(), AG, "SendTones", &tones)
    }
    #[zbus(property)]
    fn address(&self) -> String {
        self.with(|p| p.address.clone())
    }
    #[zbus(property)]
    fn speaker_volume(&self) -> u8 {
        9
    }
    #[zbus(property)]
    fn microphone_volume(&self) -> u8 {
        7
    }
}

struct TransportObj(u32, Arc<Shared>);
#[zbus::interface(name = "org.pipewire.Telephony.AudioGatewayTransport1")]
impl TransportObj {
    fn activate(&self) -> Result<(), PwError> {
        self.1.record(
            &format!("{MANAGER}/ag{}", self.0),
            TRANSPORT,
            "Activate",
            "",
        )
    }
    #[zbus(property)]
    fn state(&self) -> String {
        self.1.phones.lock().unwrap()[&self.0]
            .transport
            .clone()
            .map(|t| t.0)
            .unwrap_or_default()
    }
    #[zbus(property)]
    fn codec(&self) -> u8 {
        self.1.phones.lock().unwrap()[&self.0]
            .transport
            .clone()
            .map(|t| t.1)
            .unwrap_or(0)
    }
    #[zbus(property, name = "RejectSCO")]
    fn reject_sco(&self) -> bool {
        false
    }
}

struct CallObj(u32, u32, Arc<Shared>);
impl CallObj {
    fn path(&self) -> String {
        format!("{MANAGER}/ag{}/call{}", self.0, self.1)
    }
    fn model(&self) -> CallModel {
        self.2.phones.lock().unwrap()[&self.0].calls[&self.1].clone()
    }
}
#[zbus::interface(name = "org.pipewire.Telephony.Call1")]
impl CallObj {
    fn answer(&self) -> Result<(), PwError> {
        self.2.record(&self.path(), CALL, "Answer", "")
    }
    fn hangup(&self) -> Result<(), PwError> {
        self.2.record(&self.path(), CALL, "Hangup", "")
    }
    #[zbus(property)]
    fn line_identification(&self) -> String {
        self.model().line
    }
    #[zbus(property)]
    fn incoming_line(&self) -> String {
        self.model().incoming_line
    }
    #[zbus(property)]
    fn name(&self) -> String {
        self.model().name
    }
    #[zbus(property)]
    fn multiparty(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn state(&self) -> String {
        self.model().state
    }
}

// ------------------------------------------------------------------ mock service

pub struct MockOptions {
    /// Register `AudioGatewayTransport1` (older/other builds might not).
    pub with_transport: bool,
}
impl Default for MockOptions {
    fn default() -> Self {
        Self {
            with_transport: true,
        }
    }
}

pub struct MockService {
    pub conn: Connection,
    pub shared: Arc<Shared>,
    opts: MockOptions,
}

impl MockService {
    pub fn start(bus: &PrivateBus) -> Self {
        Self::start_with(bus, MockOptions::default())
    }
    pub fn start_with(bus: &PrivateBus, opts: MockOptions) -> Self {
        let shared = Arc::new(Shared::default());
        let conn = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(MANAGER, ManagerObj(shared.clone()))
            .unwrap()
            .build()
            .unwrap();
        Self { conn, shared, opts }
    }

    pub fn unique_name(&self) -> String {
        self.conn.unique_name().unwrap().to_string()
    }

    fn emit(
        &self,
        path: &str,
        iface: &str,
        member: &str,
        body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
    ) {
        self.conn
            .emit_signal(None::<&str>, path, iface, member, body)
            .unwrap();
    }

    pub fn add_phone(&self, n: u32, address: &str, transport: Option<(&str, u8)>) {
        let transport = transport
            .filter(|_| self.opts.with_transport)
            .map(|(s, c)| (s.to_string(), c));
        let model = PhoneModel {
            address: address.into(),
            transport,
            calls: BTreeMap::new(),
        };
        let ifaces = phone_ifaces(&model);
        self.shared.phones.lock().unwrap().insert(n, model);
        let path = format!("{MANAGER}/ag{n}");
        let server = self.conn.object_server();
        server
            .at(path.as_str(), AgObj(n, self.shared.clone()))
            .unwrap();
        server
            .at(path.as_str(), AgManagerObj(n, self.shared.clone()))
            .unwrap();
        if self.opts.with_transport {
            server
                .at(path.as_str(), TransportObj(n, self.shared.clone()))
                .unwrap();
        }
        self.emit(
            MANAGER,
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesAdded",
            &(ObjectPath::try_from(path.as_str()).unwrap(), ifaces),
        );
    }

    pub fn remove_phone(&self, n: u32) {
        let path = format!("{MANAGER}/ag{n}");
        let calls: Vec<u32> = self.shared.phones.lock().unwrap()[&n]
            .calls
            .keys()
            .copied()
            .collect();
        for m in calls {
            self.remove_call(n, m);
        }
        let server = self.conn.object_server();
        let _ = server.remove::<AgObj, _>(path.as_str());
        let _ = server.remove::<AgManagerObj, _>(path.as_str());
        let _ = server.remove::<TransportObj, _>(path.as_str());
        self.shared.phones.lock().unwrap().remove(&n);
        self.emit(
            MANAGER,
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesRemoved",
            &(
                ObjectPath::try_from(path.as_str()).unwrap(),
                vec![AG, TRANSPORT],
            ),
        );
    }

    pub fn add_call(&self, n: u32, m: u32, state: &str, line: &str, name: &str) {
        let c = CallModel {
            state: state.into(),
            line: line.into(),
            incoming_line: String::new(),
            name: name.into(),
        };
        let props = call_props(&c);
        self.shared
            .phones
            .lock()
            .unwrap()
            .get_mut(&n)
            .unwrap()
            .calls
            .insert(m, c);
        let path = format!("{MANAGER}/ag{n}/call{m}");
        self.conn
            .object_server()
            .at(path.as_str(), CallObj(n, m, self.shared.clone()))
            .unwrap();
        self.emit(
            &format!("{MANAGER}/ag{n}"),
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesAdded",
            &(
                ObjectPath::try_from(path.as_str()).unwrap(),
                Ifaces::from([(CALL.to_string(), props)]),
            ),
        );
    }

    pub fn set_call_state(&self, n: u32, m: u32, state: &str) {
        self.shared
            .phones
            .lock()
            .unwrap()
            .get_mut(&n)
            .unwrap()
            .calls
            .get_mut(&m)
            .unwrap()
            .state = state.into();
        self.emit(
            &format!("{MANAGER}/ag{n}/call{m}"),
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                CALL,
                HashMap::from([("State".to_string(), v(state.to_string()))]),
                Vec::<String>::new(),
            ),
        );
    }

    pub fn remove_call(&self, n: u32, m: u32) {
        let path = format!("{MANAGER}/ag{n}/call{m}");
        let _ = self
            .conn
            .object_server()
            .remove::<CallObj, _>(path.as_str());
        self.shared
            .phones
            .lock()
            .unwrap()
            .get_mut(&n)
            .unwrap()
            .calls
            .remove(&m);
        self.emit(
            &format!("{MANAGER}/ag{n}"),
            "org.freedesktop.DBus.ObjectManager",
            "InterfacesRemoved",
            &(ObjectPath::try_from(path.as_str()).unwrap(), vec![CALL]),
        );
    }

    pub fn set_transport(&self, n: u32, state: &str, codec: u8) {
        if let Some(p) = self.shared.phones.lock().unwrap().get_mut(&n) {
            p.transport = Some((state.into(), codec));
        }
        self.emit(
            &format!("{MANAGER}/ag{n}"),
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            &(
                TRANSPORT,
                HashMap::from([("State".to_string(), v(state.to_string()))]),
                Vec::<String>::new(),
            ),
        );
    }

    pub fn log(&self) -> Vec<Invocation> {
        self.shared.log()
    }
}
