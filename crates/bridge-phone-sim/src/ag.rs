//! D-Bus objects of the simulated `org.pipewire.Telephony` service: the object manager at the
//! root, and per phone (`ag1`) the `AudioGateway1`, `AudioGatewayTransport1` and per-phone
//! `ObjectManager` interfaces; `Call1` lives on `ag1/call<N>`.
//!
//! Method handlers never hold [`Shared::inner`] while the modem works (the modem may spawn
//! `adb`): they check the cached state, release the lock, run the modem command on a helper
//! thread and map its error. The *effect* of a command (a call changing state) is observed by
//! the next [`crate::Sim::tick`], exactly as a real phone's effect would arrive later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::modem::{Modem, ModemError, redact_numbers, validate_number};

pub(crate) const SERVICE: &str = "org.pipewire.Telephony";
pub(crate) const MANAGER: &str = "/org/pipewire/Telephony";
pub(crate) const AG_PATH: &str = "/org/pipewire/Telephony/ag1";
pub(crate) const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
pub(crate) const AG: &str = "org.pipewire.Telephony.AudioGateway1";
pub(crate) const TRANSPORT: &str = "org.pipewire.Telephony.AudioGatewayTransport1";
pub(crate) const CALL: &str = "org.pipewire.Telephony.Call1";

/// CVSD, the only codec a simulated narrowband link would negotiate.
pub(crate) const CODEC_CVSD: u8 = 1;

pub(crate) type Props = HashMap<String, OwnedValue>;
pub(crate) type Ifaces = HashMap<String, Props>;
pub(crate) type Managed = HashMap<OwnedObjectPath, Ifaces>;

pub(crate) fn call_path(id: u32) -> String {
    format!("{AG_PATH}/call{id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallModel {
    pub id: u32,
    /// One of `incoming dialing active` (the modem is single-call; `held`, `waiting` and
    /// `alerting` are never produced).
    pub state: &'static str,
    pub line: String,
}

pub(crate) struct Inner {
    pub address: String,
    pub call: Option<CallModel>,
    pub transport: &'static str,
    pub codec: u8,
    pub next_call_id: u32,
    /// A modem command from a D-Bus method is running.
    pub in_flight: bool,
}

pub(crate) struct Shared {
    pub modem: Arc<dyn Modem>,
    pub inner: Mutex<Inner>,
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.pipewire.Telephony.Error")]
pub(crate) enum PwError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    NotSupported(String),
    InvalidFormat(String),
    InvalidState(String),
    InProgress(String),
}

pub(crate) fn map_modem(e: ModemError) -> PwError {
    match e {
        ModemError::InvalidNumber(_) => PwError::InvalidFormat("Invalid phone number".into()),
        ModemError::State(m) => PwError::InvalidState(redact_numbers(&m)),
        ModemError::Unsupported(m) => PwError::NotSupported(redact_numbers(&m)),
        ModemError::Adb(m) | ModemError::Parse(m) => {
            PwError::Failed(format!("Modem command failed: {}", redact_numbers(&m)))
        }
    }
}

fn unsupported(what: &str) -> PwError {
    PwError::NotSupported(format!(
        "{what} is not supported: the simulated modem handles a single call"
    ))
}

fn invalid_state(msg: &str) -> PwError {
    PwError::InvalidState(msg.into())
}

/// Clears `Inner::in_flight` when a modem command finishes.
struct InFlight(Arc<Shared>);
impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&self.0.inner).in_flight = false;
    }
}

impl Shared {
    /// Run one modem command off the D-Bus executor thread. Only one may run at a time
    /// (`InProgress` otherwise, like PipeWire's "Command already in progress").
    async fn run_modem<F>(self: &Arc<Self>, f: F) -> Result<(), PwError>
    where
        F: FnOnce(&dyn Modem) -> Result<(), ModemError> + Send + 'static,
    {
        {
            let mut g = lock(&self.inner);
            if g.in_flight {
                return Err(PwError::InProgress("Command already in progress".into()));
            }
            g.in_flight = true;
        }
        let guard = InFlight(self.clone());
        let modem = self.modem.clone();
        let res = blocking::unblock(move || f(&*modem)).await;
        drop(guard);
        res.map_err(map_modem)
    }

    fn call_state(&self, id: u32) -> Option<&'static str> {
        lock(&self.inner)
            .call
            .as_ref()
            .filter(|c| c.id == id)
            .map(|c| c.state)
    }
}

fn v<T: Into<Value<'static>>>(x: T) -> OwnedValue {
    OwnedValue::try_from(x.into()).expect("value without file descriptors")
}

pub(crate) fn call_props(c: &CallModel) -> Props {
    HashMap::from([
        ("LineIdentification".to_string(), v(c.line.clone())),
        ("IncomingLine".to_string(), v(String::new())),
        ("Name".to_string(), v(String::new())),
        ("Multiparty".to_string(), v(false)),
        ("State".to_string(), v(c.state.to_string())),
    ])
}

pub(crate) fn transport_props(state: &str, codec: u8) -> Props {
    HashMap::from([
        ("State".to_string(), v(state.to_string())),
        ("Codec".to_string(), v(codec)),
        ("RejectSCO".to_string(), v(false)),
    ])
}

pub(crate) fn changed(entries: Vec<(&str, OwnedValue)>) -> Props {
    entries
        .into_iter()
        .map(|(k, val)| (k.to_string(), val))
        .collect()
}

pub(crate) fn str_value(s: &str) -> OwnedValue {
    v(s.to_string())
}

pub(crate) fn u8_value(x: u8) -> OwnedValue {
    v(x)
}

fn ag_ifaces(i: &Inner) -> Ifaces {
    Ifaces::from([
        (
            AG.to_string(),
            HashMap::from([
                ("Address".to_string(), v(i.address.clone())),
                ("SpeakerVolume".to_string(), v(9u8)),
                ("MicrophoneVolume".to_string(), v(7u8)),
            ]),
        ),
        (TRANSPORT.to_string(), transport_props(i.transport, i.codec)),
    ])
}

fn opath(s: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(s.to_string()).expect("valid object path")
}

// ------------------------------------------------------------------------------------ objects

pub(crate) struct ManagerObj(pub Arc<Shared>);
#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl ManagerObj {
    fn get_managed_objects(&self) -> Managed {
        Managed::from([(opath(AG_PATH), ag_ifaces(&lock(&self.0.inner)))])
    }
}

/// `ObjectManager` on the phone object, listing its calls.
pub(crate) struct AgManagerObj(pub Arc<Shared>);
#[zbus::interface(name = "org.freedesktop.DBus.ObjectManager")]
impl AgManagerObj {
    fn get_managed_objects(&self) -> Managed {
        lock(&self.0.inner)
            .call
            .iter()
            .map(|c| {
                (
                    opath(&call_path(c.id)),
                    Ifaces::from([(CALL.to_string(), call_props(c))]),
                )
            })
            .collect()
    }
}

pub(crate) struct AgObj(pub Arc<Shared>);
#[zbus::interface(name = "org.pipewire.Telephony.AudioGateway1")]
impl AgObj {
    async fn dial(&self, number: String) -> Result<(), PwError> {
        validate_number(&number).map_err(map_modem)?;
        if let Some(c) = &lock(&self.0.inner).call {
            return Err(if c.state == "dialing" {
                PwError::InProgress("A call is already being dialed".into())
            } else {
                invalid_state("A call already exists; the simulated modem handles a single call")
            });
        }
        self.0.run_modem(move |m| m.dial(&number)).await
    }

    async fn hangup_all(&self) -> Result<(), PwError> {
        if lock(&self.0.inner).call.is_none() {
            return Err(invalid_state("There is no call to hang up"));
        }
        self.0.run_modem(|m| m.hangup()).await
    }

    fn swap_calls(&self) -> Result<(), PwError> {
        Err(unsupported("SwapCalls"))
    }
    fn release_and_answer(&self) -> Result<(), PwError> {
        Err(unsupported("ReleaseAndAnswer"))
    }
    fn release_and_swap(&self) -> Result<(), PwError> {
        Err(unsupported("ReleaseAndSwap"))
    }
    fn hold_and_answer(&self) -> Result<(), PwError> {
        Err(unsupported("HoldAndAnswer"))
    }
    fn create_multiparty(&self) -> Result<(), PwError> {
        Err(unsupported("CreateMultiparty"))
    }
    fn send_tones(&self, _tones: String) -> Result<(), PwError> {
        Err(unsupported("SendTones"))
    }

    #[zbus(property)]
    fn address(&self) -> String {
        lock(&self.0.inner).address.clone()
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

pub(crate) struct TransportObj(pub Arc<Shared>);
#[zbus::interface(name = "org.pipewire.Telephony.AudioGatewayTransport1")]
impl TransportObj {
    /// There is no audio link to open. This only honours the state contract: the transport
    /// follows the call (it is `active` exactly while a call is), so `Activate` succeeds while a
    /// call is active and is `InvalidState` otherwise. Nothing is connected or routed.
    fn activate(&self) -> Result<(), PwError> {
        let g = lock(&self.0.inner);
        match &g.call {
            Some(c) if c.state == "active" => Ok(()),
            _ => Err(invalid_state("No active call to carry audio")),
        }
    }
    #[zbus(property)]
    fn state(&self) -> String {
        lock(&self.0.inner).transport.to_string()
    }
    #[zbus(property)]
    fn codec(&self) -> u8 {
        lock(&self.0.inner).codec
    }
    #[zbus(property, name = "RejectSCO")]
    fn reject_sco(&self) -> bool {
        false
    }
}

pub(crate) struct CallObj {
    pub id: u32,
    pub shared: Arc<Shared>,
}

impl CallObj {
    fn model(&self) -> Option<CallModel> {
        lock(&self.shared.inner)
            .call
            .clone()
            .filter(|c| c.id == self.id)
    }
}

#[zbus::interface(name = "org.pipewire.Telephony.Call1")]
impl CallObj {
    /// Valid only for `incoming` (a `waiting` call would need HoldAndAnswer/ReleaseAndAnswer).
    async fn answer(&self) -> Result<(), PwError> {
        match self.shared.call_state(self.id) {
            Some("incoming") => self.shared.run_modem(|m| m.answer()).await,
            Some(_) => Err(invalid_state("Only an incoming call can be answered")),
            None => Err(invalid_state("The call no longer exists")),
        }
    }

    /// Reject (incoming), cancel (dialing/alerting) or end (active) the call.
    async fn hangup(&self) -> Result<(), PwError> {
        match self.shared.call_state(self.id) {
            Some("incoming" | "active" | "dialing" | "alerting") => {
                self.shared.run_modem(|m| m.hangup()).await
            }
            Some(_) => Err(invalid_state("The call cannot be hung up in this state")),
            None => Err(invalid_state("The call no longer exists")),
        }
    }

    #[zbus(property)]
    fn line_identification(&self) -> String {
        self.model().map(|c| c.line).unwrap_or_default()
    }
    #[zbus(property)]
    fn incoming_line(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn name(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn multiparty(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn state(&self) -> String {
        self.model()
            .map(|c| c.state.to_string())
            .unwrap_or_else(|| "disconnected".to_string())
    }
}
