// SPDX-License-Identifier: GPL-3.0-or-later
//! "Set up call audio": pair the phone with this computer as a hands-free device, entirely over
//! BlueZ's D-Bus API, with no `bluetoothctl` session and no sudo.
//!
//! [`run`] does, in order: find the adapter; power it on; make it pairable and discoverable for
//! a while (so the phone can also start the pairing from its own Bluetooth screen); register a
//! pairing agent (`DisplayYesNo`) that hands the six-digit code to the caller to show and
//! confirm; look for the phone (by the name GeekBridge knows it by) and pair; mark it trusted
//! (so it reconnects by itself); connect it and its hands-free profile; then make the adapter
//! invisible again and drop the agent. [`reconnect`] is the later, silent part: connect a
//! remembered phone's hands-free profile when it comes back.
//!
//! Matching the Bluetooth device to the GeekBridge phone: Android hides its own Bluetooth
//! address from apps (since Android 6 `BluetoothAdapter.getAddress()` returns a constant), so the
//! phone cannot simply tell the computer. The phone is found by its Bluetooth name (by default
//! the same device name the GeekBridge link reports), and the user confirms the same six-digit
//! code on both screens, which is what binds them. The address learned here is remembered.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

pub const HFP_AG_UUID: &str = crate::hear::HFP_AG_UUID;
const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const AGENT_PATH: &str = "/org/geekbridge/BluetoothAgent";

type Ifaces = HashMap<String, HashMap<String, OwnedValue>>;
type Managed = HashMap<OwnedObjectPath, Ifaces>;

/// Where BlueZ is. `None` = the system bus (`DBUS_SYSTEM_BUS_ADDRESS` is honoured).
#[derive(Clone, Debug, Default)]
pub struct Bus(pub Option<String>);

impl Bus {
    fn connect(&self, timeout: Duration) -> Result<Connection, String> {
        let b = match &self.0 {
            Some(a) => zbus::blocking::connection::Builder::address(a.as_str())
                .map_err(|e| e.to_string())?,
            None => zbus::blocking::connection::Builder::system().map_err(|e| e.to_string())?,
        };
        b.method_timeout(timeout)
            .build()
            .map_err(|e| format!("cannot reach BlueZ (is bluetooth.service running?): {e}"))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub bus: Bus,
    /// The phone's Bluetooth name, or part of it (case-insensitive). `None` = the first phone
    /// that shows up (or that pairs from its side).
    pub phone_name: Option<String>,
    pub discoverable_for: Duration,
    /// Overall limit for finding and pairing.
    pub timeout: Duration,
    /// How long the user has to confirm the code.
    pub confirm_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bus: Bus::default(),
            phone_name: None,
            discoverable_for: Duration::from_secs(120),
            timeout: Duration::from_secs(150),
            confirm_timeout: Duration::from_secs(60),
        }
    }
}

/// What the setup reports while it runs.
#[derive(Debug)]
pub enum Event {
    /// A step started or finished (one line for the user).
    Step(String),
    /// Show `passkey` and ask the user whether the phone shows the same; answer through `reply`.
    Confirm {
        device: String,
        passkey: String,
        reply: mpsc::Sender<bool>,
    },
}

/// The phone that was set up; remember it for [`reconnect`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    pub name: String,
    /// The GeekBridge phone name it was matched to, if any.
    #[serde(default)]
    pub geekbridge_name: Option<String>,
}

pub const REMEMBERED_FILE: &str = "call-audio-phone.json";

pub fn load_remembered(dir: &std::path::Path) -> Option<Remembered> {
    let b = std::fs::read(dir.join(REMEMBERED_FILE)).ok()?;
    serde_json::from_slice(&b).ok()
}

pub fn save_remembered(dir: &std::path::Path, r: &Remembered) -> std::io::Result<()> {
    let path = dir.join(REMEMBERED_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(tmp, path)
}

fn get_str(i: &HashMap<String, OwnedValue>, k: &str) -> Option<String> {
    i.get(k)
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
}
fn get_bool(i: &HashMap<String, OwnedValue>, k: &str) -> bool {
    i.get(k)
        .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
        .unwrap_or(false)
}
fn get_uuids(i: &HashMap<String, OwnedValue>) -> Vec<String> {
    i.get("UUIDs")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
}

/// One device as BlueZ shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub path: String,
    pub address: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub hfp_gateway: bool,
}

impl DeviceInfo {
    fn from(path: &str, d: &HashMap<String, OwnedValue>) -> Self {
        Self {
            path: path.to_owned(),
            address: get_str(d, "Address")
                .unwrap_or_default()
                .to_ascii_uppercase(),
            name: get_str(d, "Alias")
                .or_else(|| get_str(d, "Name"))
                .unwrap_or_default(),
            icon: get_str(d, "Icon").unwrap_or_default(),
            paired: get_bool(d, "Paired"),
            trusted: get_bool(d, "Trusted"),
            connected: get_bool(d, "Connected"),
            hfp_gateway: get_uuids(d)
                .iter()
                .any(|u| u.eq_ignore_ascii_case(HFP_AG_UUID)),
        }
    }
    /// Does this device look like the phone we are after?
    pub fn matches(&self, name: Option<&str>) -> bool {
        match name {
            Some(n) if !n.trim().is_empty() => {
                self.name.to_lowercase().contains(&n.trim().to_lowercase())
            }
            _ => self.icon == "phone" || self.hfp_gateway,
        }
    }
}

struct Bluez {
    conn: Connection,
}

impl Bluez {
    fn managed(&self) -> Result<Managed, String> {
        let r = self
            .conn
            .call_method(
                Some(BLUEZ),
                "/",
                Some("org.freedesktop.DBus.ObjectManager"),
                "GetManagedObjects",
                &(),
            )
            .map_err(|e| format!("BlueZ is not answering: {e}"))?;
        r.body().deserialize::<Managed>().map_err(|e| e.to_string())
    }
    fn adapter(&self) -> Result<(String, bool), String> {
        let m = self.managed()?;
        let mut adapters: Vec<(&OwnedObjectPath, &Ifaces)> =
            m.iter().filter(|(_, i)| i.contains_key(ADAPTER)).collect();
        adapters.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let (p, i) = adapters.first().ok_or(
            "no Bluetooth adapter: plug in a USB Bluetooth adapter (geekbridge-ctl calling says what else is missing)",
        )?;
        Ok((p.to_string(), get_bool(&i[ADAPTER], "Powered")))
    }
    fn devices(&self) -> Result<Vec<DeviceInfo>, String> {
        Ok(self
            .managed()?
            .iter()
            .filter_map(|(p, i)| i.get(DEVICE).map(|d| DeviceInfo::from(p.as_str(), d)))
            .collect())
    }
    fn set(&self, path: &str, iface: &str, name: &str, v: Value<'_>) -> Result<(), String> {
        self.conn
            .call_method(Some(BLUEZ), path, Some(PROPS), "Set", &(iface, name, v))
            .map(|_| ())
            .map_err(|e| format!("setting {name}: {e}"))
    }
    fn call(&self, path: &str, iface: &str, method: &str) -> Result<(), String> {
        self.conn
            .call_method(Some(BLUEZ), path, Some(iface), method, &())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    fn connect_profile(&self, path: &str, uuid: &str) -> Result<(), String> {
        self.conn
            .call_method(Some(BLUEZ), path, Some(DEVICE), "ConnectProfile", &(uuid,))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

/// BlueZ's "already done" errors are success for us.
fn benign(e: &str) -> bool {
    e.contains("AlreadyConnected") || e.contains("AlreadyExists") || e.contains("InProgress")
}

// ------------------------------------------------------------------ the pairing agent

struct Agent {
    confirm: mpsc::Sender<(String, u32, mpsc::Sender<bool>)>,
    timeout: Duration,
    names: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}
    fn request_pin_code(&self, _device: ObjectPath<'_>) -> Result<String, AgentError> {
        Err(AgentError::Rejected(
            "legacy PIN pairing is not supported".into(),
        ))
    }
    fn display_pin_code(&self, _device: ObjectPath<'_>, _pin: String) {}
    fn request_passkey(&self, _device: ObjectPath<'_>) -> Result<u32, AgentError> {
        Err(AgentError::Rejected(
            "typing a passkey is not supported".into(),
        ))
    }
    fn display_passkey(&self, _device: ObjectPath<'_>, _passkey: u32, _entered: u16) {}
    fn request_confirmation(&self, device: ObjectPath<'_>, passkey: u32) -> Result<(), AgentError> {
        let name = self
            .names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device.as_str())
            .cloned()
            .unwrap_or_else(|| "a Bluetooth device".into());
        let (tx, rx) = mpsc::channel();
        self.confirm
            .send((name, passkey, tx))
            .map_err(|_| AgentError::Canceled("setup ended".into()))?;
        match rx.recv_timeout(self.timeout) {
            Ok(true) => Ok(()),
            Ok(false) => Err(AgentError::Rejected("the codes did not match".into())),
            Err(_) => Err(AgentError::Canceled(
                "nobody confirmed the code in time".into(),
            )),
        }
    }
    fn request_authorization(&self, _device: ObjectPath<'_>) -> Result<(), AgentError> {
        // Only registered while the user runs setup.
        Ok(())
    }
    fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: String) -> Result<(), AgentError> {
        Ok(())
    }
    fn cancel(&self) {}
}

// ------------------------------------------------------------------ setup

/// Run the whole setup. `cancel` stops it between steps. Blocking (up to `opts.timeout`).
pub fn run(
    opts: &Options,
    mut events: impl FnMut(Event),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Remembered, String> {
    use std::sync::atomic::Ordering;
    let bz = Bluez {
        conn: opts.bus.connect(Duration::from_secs(90))?,
    };
    let (adapter, powered) = bz.adapter()?;
    if !powered {
        events(Event::Step("Turning Bluetooth on".into()));
        bz.set(&adapter, ADAPTER, "Powered", Value::from(true))
            .map_err(|e| format!("could not turn Bluetooth on ({e}); try: rfkill unblock bluetooth; bluetoothctl power on"))?;
    }
    events(Event::Step(format!(
        "This computer is visible as a Bluetooth device for {} s",
        opts.discoverable_for.as_secs()
    )));
    bz.set(&adapter, ADAPTER, "Pairable", Value::from(true))?;
    bz.set(
        &adapter,
        ADAPTER,
        "DiscoverableTimeout",
        Value::from(opts.discoverable_for.as_secs() as u32),
    )?;
    bz.set(&adapter, ADAPTER, "Discoverable", Value::from(true))?;

    // The agent lives on its own connection: it blocks while the user compares the codes.
    let names = std::sync::Arc::new(std::sync::Mutex::new(HashMap::new()));
    let (ctx, crx) = mpsc::channel();
    let agent_conn = {
        let b = match &opts.bus.0 {
            Some(a) => zbus::blocking::connection::Builder::address(a.as_str())
                .map_err(|e| e.to_string())?,
            None => zbus::blocking::connection::Builder::system().map_err(|e| e.to_string())?,
        };
        b.serve_at(
            AGENT_PATH,
            Agent {
                confirm: ctx,
                timeout: opts.confirm_timeout,
                names: names.clone(),
            },
        )
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?
    };
    let agent_path = ObjectPath::try_from(AGENT_PATH).map_err(|e| e.to_string())?;
    agent_conn
        .call_method(
            Some(BLUEZ),
            "/org/bluez",
            Some("org.bluez.AgentManager1"),
            "RegisterAgent",
            &(&agent_path, "DisplayYesNo"),
        )
        .map_err(|e| format!("could not register the pairing agent: {e}"))?;
    let _ = agent_conn.call_method(
        Some(BLUEZ),
        "/org/bluez",
        Some("org.bluez.AgentManager1"),
        "RequestDefaultAgent",
        &(&agent_path,),
    );
    let cleanup = |bz: &Bluez| {
        let _ = bz.call(&adapter, ADAPTER, "StopDiscovery");
        let _ = bz.set(&adapter, ADAPTER, "Discoverable", Value::from(false));
        let _ = agent_conn.call_method(
            Some(BLUEZ),
            "/org/bluez",
            Some("org.bluez.AgentManager1"),
            "UnregisterAgent",
            &(&agent_path,),
        );
    };

    let result = (|| -> Result<DeviceInfo, String> {
        // Already paired (an earlier setup, or paired by hand): only connect.
        if let Some(d) = bz
            .devices()?
            .into_iter()
            .find(|d| d.paired && d.hfp_gateway && d.matches(opts.phone_name.as_deref()))
        {
            events(Event::Step(format!("{} is already paired", d.name)));
            return Ok(d);
        }
        events(Event::Step(match &opts.phone_name {
            Some(n) => format!(
                "Looking for \"{n}\". On the phone open Settings > Connections > Bluetooth (Samsung) or Settings > Connected devices > Pair new device, and keep that screen open"
            ),
            None => "Looking for the phone. On the phone open its Bluetooth settings and keep that screen open".into(),
        }));
        let _ = bz.call(&adapter, ADAPTER, "StartDiscovery");
        let end = Instant::now() + opts.timeout;
        let mut tried: Vec<String> = Vec::new();
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("setup cancelled".into());
            }
            // A pairing code from the agent (pairing started by either side).
            while let Ok((name, passkey, reply)) = crx.try_recv() {
                events(Event::Confirm {
                    device: name,
                    passkey: format!("{passkey:06}"),
                    reply,
                });
            }
            let devices = bz.devices()?;
            {
                let mut n = names.lock().unwrap_or_else(|e| e.into_inner());
                for d in &devices {
                    n.insert(d.path.clone(), d.name.clone());
                }
            }
            // The phone paired from its side.
            if let Some(d) = devices
                .iter()
                .find(|d| d.paired && d.matches(opts.phone_name.as_deref()) && d.hfp_gateway)
            {
                return Ok(d.clone());
            }
            if let Some(d) = devices.iter().find(|d| {
                !d.paired && !tried.contains(&d.path) && d.matches(opts.phone_name.as_deref())
            }) {
                tried.push(d.path.clone());
                events(Event::Step(format!(
                    "Found {} ({}); pairing: compare the code on both screens",
                    d.name, d.address
                )));
                let _ = bz.call(&adapter, ADAPTER, "StopDiscovery");
                // Pair() returns when BlueZ is done, i.e. after the agent's confirmation. Pump
                // the confirmation to the caller from a helper thread meanwhile.
                let path = d.path.clone();
                let conn = bz.conn.clone();
                let pairing = std::thread::spawn(move || {
                    conn.call_method(Some(BLUEZ), path.as_str(), Some(DEVICE), "Pair", &())
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
                while !pairing.is_finished() {
                    if let Ok((name, passkey, reply)) = crx.recv_timeout(Duration::from_millis(100))
                    {
                        events(Event::Confirm {
                            device: name,
                            passkey: format!("{passkey:06}"),
                            reply,
                        });
                    }
                }
                match pairing
                    .join()
                    .unwrap_or_else(|_| Err("pairing failed".into()))
                {
                    Ok(()) => {}
                    Err(e) if benign(&e) => {}
                    Err(e) => {
                        events(Event::Step(format!("Pairing with {} failed: {e}", d.name)));
                        let _ = bz.call(&adapter, ADAPTER, "StartDiscovery");
                        continue;
                    }
                }
                let fresh = bz.devices()?;
                if let Some(x) = fresh.into_iter().find(|x| x.path == d.path && x.paired) {
                    return Ok(x);
                }
            }
            if Instant::now() >= end {
                return Err(match &opts.phone_name {
                    Some(n) => format!(
                        "no phone called \"{n}\" showed up or paired within {} s; is its Bluetooth on and its Bluetooth screen open? (try again, or run without a name)",
                        opts.timeout.as_secs()
                    ),
                    None => format!(
                        "no phone showed up or paired within {} s",
                        opts.timeout.as_secs()
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    })();
    let device = match result {
        Ok(d) => d,
        Err(e) => {
            cleanup(&bz);
            return Err(e);
        }
    };
    cleanup(&bz);

    events(Event::Step(format!(
        "Paired with {}; marking it trusted so it reconnects by itself",
        device.name
    )));
    bz.set(&device.path, DEVICE, "Trusted", Value::from(true))?;
    events(Event::Step(
        "Connecting the phone's calls (hands-free) profile".into(),
    ));
    connect_hfp(&bz, &device.path)?;
    Ok(Remembered {
        address: device.address,
        name: device.name,
        geekbridge_name: opts.phone_name.clone(),
    })
}

fn connect_hfp(bz: &Bluez, path: &str) -> Result<(), String> {
    match bz.call(path, DEVICE, "Connect") {
        Ok(()) => {}
        Err(e) if benign(&e) => {}
        // Connect() can fail on one profile and still bring up others; HFP is asked for below.
        Err(_) => {}
    }
    match bz.connect_profile(path, HFP_AG_UUID) {
        Ok(()) => Ok(()),
        Err(e) if benign(&e) => Ok(()),
        Err(e) => Err(format!(
            "the phone did not accept the calls (hands-free) connection ({e}). On the phone: Bluetooth > this computer (gear icon) > turn on Calls / Phone calls"
        )),
    }
}

/// Connect a remembered phone's hands-free profile (it came back in range, or GeekBridge's link
/// to it came up). Quietly does nothing if it is already connected. Blocking (BlueZ may take a
/// few seconds to page an absent phone).
pub fn reconnect(bus: &Bus, address: &str) -> Result<bool, String> {
    let bz = Bluez {
        conn: bus.connect(Duration::from_secs(20))?,
    };
    let (_adapter, powered) = bz.adapter()?;
    if !powered {
        return Ok(false);
    }
    let Some(d) = bz
        .devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(address))
    else {
        return Err(
            "the remembered phone is no longer paired; run the call-audio setup again".into(),
        );
    };
    if d.connected {
        // Connected (maybe only for music): make sure the calls profile is up too.
        return bz
            .connect_profile(&d.path, HFP_AG_UUID)
            .map(|()| true)
            .or_else(|e| if benign(&e) { Ok(true) } else { Err(e) });
    }
    connect_hfp(&bz, &d.path).map(|()| true)
}

/// Unpair `address` from this computer (BlueZ `RemoveDevice`). A phone that is already gone is
/// success. Blocking.
pub fn forget(bus: &Bus, address: &str) -> Result<(), String> {
    let bz = Bluez {
        conn: bus.connect(Duration::from_secs(20))?,
    };
    let (adapter, _) = bz.adapter()?;
    let Some(d) = bz
        .devices()?
        .into_iter()
        .find(|d| d.address.eq_ignore_ascii_case(address))
    else {
        return Ok(());
    };
    bz.conn
        .call_method(
            Some(BLUEZ),
            adapter.as_str(),
            Some(ADAPTER),
            "RemoveDevice",
            &(ObjectPath::try_from(d.path.as_str()).map_err(|e| e.to_string())?,),
        )
        .map(|_| ())
        .map_err(|e| format!("could not unpair the phone: {e}"))
}

/// Delete the remembered phone. Missing file = success.
pub fn clear_remembered(dir: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(dir.join(REMEMBERED_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Is any Bluetooth adapter known to BlueZ? (`Ok(false)` = BlueZ runs, no adapter.)
pub fn adapter_present(bus: &Bus) -> Result<bool, String> {
    let bz = Bluez {
        conn: bus.connect(Duration::from_secs(5))?,
    };
    Ok(bz.adapter().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str, icon: &str, gw: bool) -> DeviceInfo {
        DeviceInfo {
            path: "/x".into(),
            address: "AA:BB:CC:00:11:22".into(),
            name: name.into(),
            icon: icon.into(),
            paired: false,
            trusted: false,
            connected: false,
            hfp_gateway: gw,
        }
    }

    #[test]
    fn a_phone_is_matched_by_name_or_else_by_being_a_phone() {
        let fold = dev("Galaxy Z Fold5", "phone", true);
        assert!(fold.matches(Some("galaxy z fold5")));
        assert!(fold.matches(Some("Fold5")));
        assert!(!fold.matches(Some("Pixel")));
        assert!(fold.matches(None));
        assert!(fold.matches(Some("  ")));
        assert!(!dev("Speaker", "audio-card", false).matches(None));
        assert!(dev("unnamed", "", true).matches(None));
    }

    #[test]
    fn already_done_errors_are_success() {
        assert!(benign("org.bluez.Error.AlreadyConnected"));
        assert!(benign("org.bluez.Error.AlreadyExists: Already Exists"));
        assert!(!benign(
            "org.bluez.Error.Failed: br-connection-page-timeout"
        ));
    }

    #[test]
    fn remembered_phone_round_trips() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(load_remembered(d.path()), None);
        let r = Remembered {
            address: "AA:BB:CC:00:11:22".into(),
            name: "Galaxy Z Fold5".into(),
            geekbridge_name: Some("Fold".into()),
        };
        save_remembered(d.path(), &r).unwrap();
        assert_eq!(load_remembered(d.path()), Some(r));
        clear_remembered(d.path()).unwrap();
        assert_eq!(load_remembered(d.path()), None);
        clear_remembered(d.path()).unwrap();
    }
}
