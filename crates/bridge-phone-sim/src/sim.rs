//! The simulated service: owns the bus connection and reconciles the modem's call state into
//! `org.pipewire.Telephony` objects and signals.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::connection::Builder;

use crate::ag::{
    AG_PATH, AgManagerObj, AgObj, CALL, CODEC_CVSD, CallModel, CallObj, Inner, MANAGER, PROPERTIES,
    SERVICE, Shared, TRANSPORT, TransportObj, call_path, changed, lock, str_value, u8_value,
};
use crate::modem::{Modem, ModemError, ModemState, redact_numbers};

#[derive(Clone, Debug)]
pub struct SimConfig {
    /// Bus to serve on. `None` is the session bus.
    pub bus_address: Option<String>,
    /// Bluetooth address reported by `AudioGateway1.Address` (not a real device).
    pub phone_address: String,
    /// How often [`Sim::run_until`] polls the modem.
    pub poll_interval: Duration,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            bus_address: None,
            phone_address: "02:00:00:00:00:01".to_string(),
            poll_interval: Duration::from_millis(300),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("bus error: {0}")]
    Bus(#[from] zbus::Error),
    #[error("invalid D-Bus value: {0}")]
    Value(#[from] zbus::zvariant::Error),
    /// A real PipeWire/WirePlumber telephony service (or another simulator) owns the name.
    #[error(
        "the bus name {SERVICE} is already owned by another service (a real PipeWire/WirePlumber telephony service, or another simulator); \
         start the simulator on a private bus with --bus ADDRESS"
    )]
    NameTaken,
    #[error("modem error: {0}")]
    Modem(#[from] ModemError),
}

/// What reconciliation decided to change, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    AddCall {
        id: u32,
        state: &'static str,
        line: String,
    },
    SetCallState {
        id: u32,
        state: &'static str,
    },
    SetCallLine {
        id: u32,
        line: String,
    },
    /// Set the call `disconnected`, then remove the object.
    RemoveCall {
        id: u32,
    },
    Transport {
        state: &'static str,
        codec: u8,
    },
}

/// Pure reconciliation of the modem state into service changes. Call ids are taken from
/// `next_id` upward and never reused.
pub(crate) fn plan(
    call: Option<&CallModel>,
    transport: (&str, u8),
    next_id: u32,
    modem: &ModemState,
) -> Vec<Change> {
    let want: Option<(&'static str, &str)> = match modem {
        ModemState::Idle => None,
        ModemState::Ringing { number } => Some(("incoming", number)),
        ModemState::Dialing { number } => Some(("dialing", number)),
        ModemState::Active { number } => Some(("active", number)),
    };
    let mut out = Vec::new();
    match (call, want) {
        (None, None) => {}
        (Some(c), None) => out.push(Change::RemoveCall { id: c.id }),
        (None, Some((state, line))) => {
            out.push(Change::AddCall {
                id: next_id,
                state,
                line: line.to_string(),
            });
        }
        (Some(c), Some((state, line))) => {
            let progresses = state == "active" && matches!(c.state, "incoming" | "dialing");
            let same_party = line.is_empty() || c.line.is_empty() || c.line == line;
            if (c.state == state || progresses) && same_party {
                if c.state != state {
                    out.push(Change::SetCallState { id: c.id, state });
                }
                if c.line.is_empty() && !line.is_empty() {
                    out.push(Change::SetCallLine {
                        id: c.id,
                        line: line.to_string(),
                    });
                }
            } else {
                // A different call (or an impossible transition): never morph one into another.
                out.push(Change::RemoveCall { id: c.id });
                out.push(Change::AddCall {
                    id: next_id,
                    state,
                    line: line.to_string(),
                });
            }
        }
    }
    let target = if matches!(modem, ModemState::Active { .. }) {
        ("active", CODEC_CVSD)
    } else {
        ("idle", 0)
    };
    if (transport.0, transport.1) != target {
        out.push(Change::Transport {
            state: target.0,
            codec: target.1,
        });
    }
    out
}

pub struct Sim {
    conn: Connection,
    shared: Arc<Shared>,
    tick_lock: Mutex<()>,
    interval: Duration,
}

impl Sim {
    /// Connect, serve the objects, then claim `org.pipewire.Telephony` with `DoNotQueue`.
    /// Fails with [`SimError::NameTaken`] if anyone already owns the name.
    pub fn start(cfg: SimConfig, modem: Arc<dyn Modem>) -> Result<Sim, SimError> {
        let shared = Arc::new(Shared {
            modem,
            inner: Mutex::new(Inner {
                address: cfg.phone_address.clone(),
                call: None,
                transport: "idle",
                codec: 0,
                next_call_id: 1,
                in_flight: false,
            }),
        });
        let builder = match &cfg.bus_address {
            Some(a) => Builder::address(a.as_str())?,
            None => Builder::session()?,
        };
        let conn = builder
            .serve_at(MANAGER, crate::ag::ManagerObj(shared.clone()))?
            .serve_at(AG_PATH, AgObj(shared.clone()))?
            .serve_at(AG_PATH, TransportObj(shared.clone()))?
            .serve_at(AG_PATH, AgManagerObj(shared.clone()))?
            .build()?;
        match conn.request_name_with_flags(SERVICE, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        {
            Ok(_) => {}
            Err(zbus::Error::NameTaken) => return Err(SimError::NameTaken),
            Err(e) => return Err(e.into()),
        }
        Ok(Sim {
            conn,
            shared,
            tick_lock: Mutex::new(()),
            interval: cfg.poll_interval,
        })
    }

    /// The bus unique name (`:1.N`) of the simulator's connection.
    pub fn unique_name(&self) -> String {
        self.conn
            .unique_name()
            .map(|n| n.to_string())
            .unwrap_or_default()
    }

    /// Read the modem once and bring the D-Bus objects in line with it, emitting
    /// `InterfacesAdded` / `PropertiesChanged` / `InterfacesRemoved` as needed. Ticks are
    /// serialised; method handlers can run concurrently.
    pub fn tick(&self) -> Result<(), SimError> {
        let _serial = lock(&self.tick_lock);
        // No internal lock is held while the modem (possibly `adb`) is queried.
        let desired = self.shared.modem.state()?;
        let (call, transport, codec, next_id) = {
            let g = lock(&self.shared.inner);
            (g.call.clone(), g.transport, g.codec, g.next_call_id)
        };
        for change in plan(call.as_ref(), (transport, codec), next_id, &desired) {
            self.apply(change)?;
        }
        Ok(())
    }

    /// Tick every poll interval until `stop` is set. Modem errors are reported once per distinct
    /// message on stderr (numbers redacted) and do not stop the loop.
    pub fn run_until(&self, stop: &AtomicBool) {
        let mut last_err: Option<String> = None;
        while !stop.load(Ordering::Relaxed) {
            match self.tick() {
                Ok(()) => {
                    if last_err.take().is_some() {
                        eprintln!("geekbridge-phone-sim: modem reachable again");
                    }
                }
                Err(e) => {
                    let msg = redact_numbers(&e.to_string());
                    if last_err.as_deref() != Some(msg.as_str()) {
                        eprintln!("geekbridge-phone-sim: {msg}");
                        last_err = Some(msg);
                    }
                }
            }
            let deadline = Instant::now() + self.interval;
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20).min(self.interval));
            }
        }
    }

    // ------------------------------------------------------------------------------ applying

    fn apply(&self, change: Change) -> Result<(), SimError> {
        let server = self.conn.object_server();
        match change {
            Change::AddCall { id, state, line } => {
                let model = CallModel { id, state, line };
                // State first, then the object: zbus emits `InterfacesAdded` from the phone's
                // ObjectManager when the object is registered and reads the properties at once.
                {
                    let mut g = lock(&self.shared.inner);
                    g.call = Some(model);
                    g.next_call_id = id + 1;
                }
                server.at(
                    call_path(id).as_str(),
                    CallObj {
                        id,
                        shared: self.shared.clone(),
                    },
                )?;
            }
            Change::SetCallState { id, state } => {
                if let Some(c) = lock(&self.shared.inner)
                    .call
                    .as_mut()
                    .filter(|c| c.id == id)
                {
                    c.state = state;
                }
                self.emit_changed(
                    &call_path(id),
                    CALL,
                    changed(vec![("State", str_value(state))]),
                )?;
            }
            Change::SetCallLine { id, line } => {
                if let Some(c) = lock(&self.shared.inner)
                    .call
                    .as_mut()
                    .filter(|c| c.id == id)
                {
                    c.line = line.clone();
                }
                self.emit_changed(
                    &call_path(id),
                    CALL,
                    changed(vec![("LineIdentification", str_value(&line))]),
                )?;
            }
            Change::RemoveCall { id } => {
                if let Some(c) = lock(&self.shared.inner)
                    .call
                    .as_mut()
                    .filter(|c| c.id == id)
                {
                    c.state = "disconnected";
                }
                self.emit_changed(
                    &call_path(id),
                    CALL,
                    changed(vec![("State", str_value("disconnected"))]),
                )?;
                lock(&self.shared.inner).call = None;
                // zbus emits `InterfacesRemoved` from the ObjectManager on removal.
                let _ = server.remove::<CallObj, _>(call_path(id).as_str());
            }
            Change::Transport { state, codec } => {
                {
                    let mut g = lock(&self.shared.inner);
                    g.transport = state;
                    g.codec = codec;
                }
                self.emit_changed(
                    AG_PATH,
                    TRANSPORT,
                    changed(vec![
                        ("State", str_value(state)),
                        ("Codec", u8_value(codec)),
                    ]),
                )?;
            }
        }
        Ok(())
    }

    fn emit_changed(
        &self,
        path: &str,
        iface: &str,
        props: crate::ag::Props,
    ) -> Result<(), SimError> {
        self.conn.emit_signal(
            None::<&str>,
            path,
            PROPERTIES,
            "PropertiesChanged",
            &(iface, props, Vec::<String>::new()),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
