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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

pub const HFP_AG_UUID: &str = crate::hear::HFP_AG_UUID;
const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const AGENT_MANAGER: &str = "org.bluez.AgentManager1";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const AGENT_PATH: &str = "/org/geekbridge/BluetoothAgent";
/// bluetoothd gives every agent request 60 s (`REQUEST_TIMEOUT` in BlueZ's src/agent.c) and then
/// cancels it; the code has to be confirmed within that, whatever GeekBridge waits.
pub const BLUEZ_AGENT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long one `Pair()` may take: paging the phone, the user comparing the codes (at most
/// [`BLUEZ_AGENT_TIMEOUT`]) and the service discovery that follows.
const PAIR_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// After pairing, how long BlueZ may take to learn the phone's services (its UUIDs).
const SERVICES_WAIT: Duration = Duration::from_secs(10);
/// With a name given and no device by that name, when to list the phones that are nearby.
const HINT_AFTER: Duration = Duration::from_secs(20);

type Ifaces = HashMap<String, HashMap<String, OwnedValue>>;
type Managed = HashMap<OwnedObjectPath, Ifaces>;

/// Where BlueZ is. `None` = the system bus (`DBUS_SYSTEM_BUS_ADDRESS` is honoured).
#[derive(Clone, Debug, Default)]
pub struct Bus(pub Option<String>);

/// Is the machine's own (system-bus) Bluetooth off limits to this process? True when
/// `GEEKBRIDGE_NO_SYSTEM_BLUETOOTH` is set (and not `0`), and also whenever the process runs
/// sandboxed (`GEEKBRIDGE_STATE_DIR` or `GEEKBRIDGE_CONTROL_PORT` overridden), because a sandbox
/// still sees the real BlueZ on the system bus. `GEEKBRIDGE_ALLOW_SYSTEM_BLUETOOTH=1` lifts the
/// sandbox rule on purpose (not the explicit `NO_SYSTEM_BLUETOOTH`).
pub fn system_bluetooth_blocked_with(get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty() && v != "0");
    if set("GEEKBRIDGE_NO_SYSTEM_BLUETOOTH") {
        return true;
    }
    let sandboxed = get("GEEKBRIDGE_STATE_DIR").is_some_and(|v| !v.is_empty())
        || get("GEEKBRIDGE_CONTROL_PORT").is_some_and(|v| !v.is_empty());
    sandboxed && get("GEEKBRIDGE_ALLOW_SYSTEM_BLUETOOTH").as_deref() != Some("1")
}

pub fn system_bluetooth_blocked() -> bool {
    system_bluetooth_blocked_with(|k| std::env::var(k).ok())
}

const BLOCKED: &str = "this process is a sandbox or test (GEEKBRIDGE_NO_SYSTEM_BLUETOOTH, GEEKBRIDGE_STATE_DIR or GEEKBRIDGE_CONTROL_PORT is set), so it does not touch the computer's real Bluetooth adapter; set GEEKBRIDGE_ALLOW_SYSTEM_BLUETOOTH=1 to allow it on purpose";

impl Bus {
    fn builder(&self) -> Result<zbus::blocking::connection::Builder<'static>, String> {
        if self.0.is_none() && system_bluetooth_blocked() {
            return Err(BLOCKED.into());
        }
        match &self.0 {
            Some(a) => {
                zbus::blocking::connection::Builder::address(a.as_str()).map_err(|e| e.to_string())
            }
            None => zbus::blocking::connection::Builder::system().map_err(|e| e.to_string()),
        }
    }
    fn connect(&self, timeout: Duration) -> Result<Connection, String> {
        self.builder()?
            .method_timeout(timeout)
            .build()
            .map_err(|e| format!("cannot reach BlueZ (is bluetooth.service running?): {e}"))
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub bus: Bus,
    /// The phone's Bluetooth name, or part of it (case-insensitive). `None` = the first phone
    /// that shows up (or that pairs from its side). A phone that pairs from its own side during
    /// the setup is taken whatever its name: the user compared the code, which is what binds it.
    pub phone_name: Option<String>,
    pub discoverable_for: Duration,
    /// Overall limit for finding and pairing.
    pub timeout: Duration,
    /// How long the user has to confirm the code. BlueZ itself gives up after
    /// [`BLUEZ_AGENT_TIMEOUT`], so this is kept a little below that.
    pub confirm_timeout: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bus: Bus::default(),
            phone_name: None,
            discoverable_for: Duration::from_secs(120),
            timeout: Duration::from_secs(150),
            confirm_timeout: Duration::from_secs(55),
        }
    }
}

/// What the setup reports while it runs.
#[derive(Debug)]
pub enum Event {
    /// A step started or finished (one line for the user).
    Step(String),
    /// Show `passkey` and ask the user whether the phone shows the same; answer through `reply`.
    /// An empty `passkey` is a pairing without a code to compare ("allow this device?").
    Confirm {
        device: String,
        passkey: String,
        reply: mpsc::Sender<bool>,
    },
    /// The code shown by the last [`Event::Confirm`] is no longer asked for (the phone
    /// cancelled, BlueZ timed out, or nobody answered): stop showing it.
    Withdrawn,
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
    crate::private_file::replace(
        &dir.join(REMEMBERED_FILE),
        &serde_json::to_vec_pretty(r).map_err(std::io::Error::other)?,
    )
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
fn get_u32(i: &HashMap<String, OwnedValue>, k: &str) -> Option<u32> {
    i.get(k)
        .and_then(|v| u32::try_from(v.try_clone().ok()?).ok())
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
    /// `public` or `random` (an LE-only advertisement; never the phone's hands-free side).
    pub address_type: String,
    pub name: String,
    pub icon: String,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub services_resolved: bool,
    pub hfp_gateway: bool,
}

/// Format characters that draw nothing or reorder text (bidi controls, zero-width, joiners, BOM).
fn is_invisible_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// A Bluetooth name as it may be shown: every device in range chooses its own, so escape sequences, line breaks,
/// direction overrides and zero-width characters are dropped and the length is bounded.
pub fn clean_remote_name(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() && !is_invisible_format(*c))
        .take(48)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// `Name (AA:BB:CC:DD:EE:FF)` from a BlueZ device object path (`.../dev_AA_BB_CC_DD_EE_FF`): the address is the part
/// the user can compare with the phone's own Bluetooth address, and the part a lookalike name cannot fake.
pub fn device_label(path: &str, name: &str) -> String {
    let addr = path
        .rsplit('/')
        .next()
        .and_then(|l| l.strip_prefix("dev_"))
        .map(|a| a.replace('_', ":"))
        .filter(|a| a.len() == 17 && a.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'));
    match addr {
        Some(a) => format!("{name} ({a})"),
        None => name.to_owned(),
    }
}

/// The profiles of a phone's call audio: hands-free, headset, A2DP and AVRCP.
const AUDIO_SERVICES: [&str; 8] = [
    "0000111e", "0000111f", "00001108", "00001112", "0000110a", "0000110b", "0000110c", "0000110e",
];

/// May BlueZ connect `uuid` for `device`? Only for a device the user confirmed during this setup, and only the
/// profiles that carry call audio. Anything else that connects while the agent is registered is refused.
fn service_allowed(confirmed: &HashSet<String>, device: &str, uuid: &str) -> bool {
    confirmed.contains(device)
        && uuid
            .get(..8)
            .is_some_and(|p| AUDIO_SERVICES.iter().any(|a| a.eq_ignore_ascii_case(p)))
}

impl DeviceInfo {
    fn from(path: &str, d: &HashMap<String, OwnedValue>) -> Self {
        Self {
            path: path.to_owned(),
            address: get_str(d, "Address")
                .unwrap_or_default()
                .to_ascii_uppercase(),
            address_type: get_str(d, "AddressType").unwrap_or_default(),
            name: clean_remote_name(
                &get_str(d, "Alias")
                    .or_else(|| get_str(d, "Name"))
                    .unwrap_or_default(),
            ),
            icon: get_str(d, "Icon").unwrap_or_default(),
            paired: get_bool(d, "Paired"),
            trusted: get_bool(d, "Trusted"),
            connected: get_bool(d, "Connected"),
            services_resolved: get_bool(d, "ServicesResolved"),
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
            _ => self.phone_like(),
        }
    }
    fn phone_like(&self) -> bool {
        self.icon == "phone" || self.hfp_gateway
    }
    /// Something GeekBridge may start a pairing with: a Classic (BR/EDR) device. Phones also
    /// advertise over LE from changing random addresses; those objects never carry hands-free.
    fn pairable_from_here(&self) -> bool {
        self.address_type != "random"
    }
}

/// The adapter as found at the start, and the modes the setup changes (put back at the end).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterInfo {
    pub path: String,
    pub address: String,
    /// The name phones show for this computer.
    pub alias: String,
    pub powered: bool,
    pub pairable: bool,
    pub discoverable: bool,
    pub discoverable_timeout: u32,
}

/// Every adapter, the one to use first: a powered one, then by path (`hci0` before `hci1`).
fn adapters(m: &Managed) -> Vec<AdapterInfo> {
    let mut v: Vec<AdapterInfo> = m
        .iter()
        .filter_map(|(p, i)| {
            let a = i.get(ADAPTER)?;
            Some(AdapterInfo {
                path: p.to_string(),
                address: get_str(a, "Address").unwrap_or_default(),
                alias: get_str(a, "Alias")
                    .or_else(|| get_str(a, "Name"))
                    .unwrap_or_default(),
                powered: get_bool(a, "Powered"),
                pairable: get_bool(a, "Pairable"),
                discoverable: get_bool(a, "Discoverable"),
                discoverable_timeout: get_u32(a, "DiscoverableTimeout").unwrap_or(180),
            })
        })
        .collect();
    v.sort_by(|a, b| b.powered.cmp(&a.powered).then_with(|| a.path.cmp(&b.path)));
    v
}

/// The devices BlueZ knows under `adapter` (a phone seen by another adapter is not reachable
/// through this one).
fn devices_on(m: &Managed, adapter: &str) -> Vec<DeviceInfo> {
    let prefix = format!("{adapter}/");
    let mut v: Vec<DeviceInfo> = m
        .iter()
        .filter(|(p, _)| p.as_str().starts_with(&prefix))
        .filter_map(|(p, i)| i.get(DEVICE).map(|d| DeviceInfo::from(p.as_str(), d)))
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
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
    fn adapter(&self) -> Result<AdapterInfo, String> {
        adapters(&self.managed()?).into_iter().next().ok_or_else(|| {
            "no Bluetooth adapter: plug in a USB Bluetooth adapter (geekbridge-ctl calling says what else is missing)".to_owned()
        })
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
    fn device(&self, path: &str) -> Result<Option<DeviceInfo>, String> {
        let m = self.managed()?;
        Ok(m.iter()
            .find(|(p, _)| p.as_str() == path)
            .and_then(|(p, i)| i.get(DEVICE).map(|d| DeviceInfo::from(p.as_str(), d))))
    }
}

/// BlueZ's "already done" errors are success for us.
fn benign(e: &str) -> bool {
    e.contains("AlreadyConnected") || e.contains("AlreadyExists") || e.contains("InProgress")
}

/// A failed `Pair()` in words, with BlueZ's own error kept for the logs.
pub fn pair_error(e: &str) -> String {
    let hint = if e.contains("AuthenticationCanceled") || e.contains("AuthenticationTimeout") {
        "the code was not confirmed in time on both screens"
    } else if e.contains("AuthenticationRejected") {
        "the code was declined"
    } else if e.contains("AuthenticationFailed") {
        "the phone refused; if it still lists this computer from an earlier pairing, remove it there (gear icon > Unpair) and try again"
    } else if e.contains("ConnectionAttemptFailed")
        || e.contains("page-timeout")
        || e.contains("Host is down")
    {
        "the phone did not answer; keep its Bluetooth screen open and bring it closer"
    } else {
        ""
    };
    if hint.is_empty() {
        e.to_owned()
    } else {
        format!("{hint} ({e})")
    }
}

/// A failed hands-free connection in words.
pub fn hfp_error(e: &str) -> String {
    if e.contains("profile-unavailable")
        || e.contains("NotAvailable")
        || e.contains("ProfileUnavailable")
    {
        format!(
            "this computer offers no hands-free profile to the phone ({e}): WirePlumber has to run with the hfp_hf role and the native backend; run geekbridge-ctl calling"
        )
    } else if e.contains("page-timeout") || e.contains("Host is down") {
        format!("the phone did not answer ({e}): is its Bluetooth on and is it near?")
    } else {
        format!(
            "the phone did not accept the calls (hands-free) connection ({e}). On the phone: Bluetooth > this computer (gear icon) > turn on Calls / Phone calls"
        )
    }
}

// ------------------------------------------------------------------ the pairing agent

/// What the agent needs from the user, handed to the setup loop.
enum Ask {
    Confirm {
        device: String,
        passkey: Option<u32>,
        reply: mpsc::Sender<bool>,
    },
    Withdrawn,
}

struct Agent {
    asks: mpsc::Sender<Ask>,
    timeout: Duration,
    names: Arc<Mutex<HashMap<String, String>>>,
    /// Bumped by `Cancel()`; a waiting request sees it change and gives up.
    cancels: Arc<AtomicU64>,
    /// Device paths whose pairing the user confirmed during this setup: the only ones whose services are authorized.
    confirmed: Arc<Mutex<HashSet<String>>>,
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.bluez.Error")]
enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

impl Agent {
    async fn name_of(&self, conn: &zbus::Connection, device: &str) -> String {
        let known = self
            .names
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device)
            .cloned();
        if let Some(n) = known.filter(|n| !n.is_empty()) {
            return device_label(device, &n);
        }
        // A phone that starts the pairing itself can be new to BlueZ: ask for its name.
        let r = conn
            .call_method(Some(BLUEZ), device, Some(PROPS), "Get", &(DEVICE, "Alias"))
            .await;
        r.ok()
            .and_then(|m| m.body().deserialize::<OwnedValue>().ok())
            .and_then(|v| String::try_from(v).ok())
            .map(|n| clean_remote_name(&n))
            .filter(|n| !n.is_empty())
            .map(|n| device_label(device, &n))
            .unwrap_or_else(|| device_label(device, "a Bluetooth device"))
    }

    fn confirm(&self, device: &str) {
        self.confirmed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(device.to_owned());
    }

    /// Ask the user and wait for the answer without blocking the connection, so BlueZ's
    /// `Cancel()` is still delivered while the code is on screen.
    async fn ask(&self, device: String, passkey: Option<u32>) -> Result<(), AgentError> {
        let generation = self.cancels.load(Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.asks
            .send(Ask::Confirm {
                device,
                passkey,
                reply: tx,
            })
            .map_err(|_| AgentError::Canceled("setup ended".into()))?;
        let end = Instant::now() + self.timeout;
        let r = loop {
            match rx.try_recv() {
                Ok(true) => break Ok(()),
                Ok(false) => break Err(AgentError::Rejected("the codes did not match".into())),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err(AgentError::Canceled("setup ended".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if self.cancels.load(Ordering::SeqCst) != generation {
                // Cancel() already told the setup.
                return Err(AgentError::Canceled("cancelled by BlueZ".into()));
            }
            if Instant::now() >= end {
                break Err(AgentError::Canceled(
                    "nobody confirmed the code in time".into(),
                ));
            }
            async_io::Timer::after(Duration::from_millis(50)).await;
        };
        if r.is_err() {
            let _ = self.asks.send(Ask::Withdrawn);
        }
        r
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}
    fn request_pin_code(&self, _device: OwnedObjectPath) -> Result<String, AgentError> {
        Err(AgentError::Rejected(
            "legacy PIN pairing is not supported".into(),
        ))
    }
    fn display_pin_code(&self, _device: OwnedObjectPath, _pin: String) {}
    fn request_passkey(&self, _device: OwnedObjectPath) -> Result<u32, AgentError> {
        Err(AgentError::Rejected(
            "typing a passkey is not supported".into(),
        ))
    }
    fn display_passkey(&self, _device: OwnedObjectPath, _passkey: u32, _entered: u16) {}
    async fn request_confirmation(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> Result<(), AgentError> {
        let name = self.name_of(conn, device.as_str()).await;
        self.ask(name, Some(passkey)).await?;
        self.confirm(device.as_str());
        Ok(())
    }
    /// An incoming pairing without a code to compare ("just works"). Phones compare codes, so
    /// this is unusual: the user is asked instead of letting anything nearby pair.
    async fn request_authorization(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        device: OwnedObjectPath,
    ) -> Result<(), AgentError> {
        let name = self.name_of(conn, device.as_str()).await;
        self.ask(name, None).await?;
        self.confirm(device.as_str());
        Ok(())
    }
    fn authorize_service(&self, device: OwnedObjectPath, uuid: String) -> Result<(), AgentError> {
        // Registered only while the user runs setup, and the phone connects its profiles right after pairing, before
        // it is trusted: so the device the user just confirmed gets its call-audio profiles. Any other device that
        // connects in this window (it is discoverable) gets nothing.
        let confirmed = self.confirmed.lock().unwrap_or_else(|e| e.into_inner());
        if service_allowed(&confirmed, device.as_str(), &uuid) {
            Ok(())
        } else {
            Err(AgentError::Rejected(
                "this device was not confirmed in the setup".into(),
            ))
        }
    }
    fn cancel(&self) {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        let _ = self.asks.send(Ask::Withdrawn);
    }
}

fn forward(ask: Ask, events: &mut impl FnMut(Event)) {
    match ask {
        Ask::Confirm {
            device,
            passkey,
            reply,
        } => events(Event::Confirm {
            device,
            passkey: passkey.map(|p| format!("{p:06}")).unwrap_or_default(),
            reply,
        }),
        Ask::Withdrawn => events(Event::Withdrawn),
    }
}

/// An already paired phone that offers hands-free and has the wanted name (any phone when no
/// name is given). A paired phone by another name is not guessed at: the user may be setting up
/// a second phone. A phone whose Bluetooth name differs from the name GeekBridge knows it by
/// (Android reports the model number, e.g. "SM-F946B" for a "Galaxy Z Fold5") is found by
/// what the user does on it during the setup instead: pairing or connecting from the phone.
fn already_paired<'a>(devices: &'a [DeviceInfo], name: Option<&str>) -> Option<&'a DeviceInfo> {
    devices
        .iter()
        .find(|d| d.paired && d.hfp_gateway && d.matches(name))
}

// ------------------------------------------------------------------ setup

/// Run the whole setup. `cancel` stops it between steps (and cancels a pairing in progress).
/// Blocking (up to `opts.timeout`).
pub fn run(
    opts: &Options,
    mut events: impl FnMut(Event),
    cancel: &AtomicBool,
) -> Result<Remembered, String> {
    let bz = Bluez {
        conn: opts.bus.connect(Duration::from_secs(90))?,
    };
    let ad = bz.adapter()?;
    let adapter = ad.path.clone();
    events(Event::Step(format!(
        "Using Bluetooth adapter {} ({}); phones list this computer as \"{}\"",
        adapter.rsplit('/').next().unwrap_or(&adapter),
        ad.address,
        ad.alias
    )));
    if !ad.powered {
        events(Event::Step("Turning Bluetooth on".into()));
        bz.set(&adapter, ADAPTER, "Powered", Value::from(true))
            .map_err(|e| format!("could not turn Bluetooth on ({e}); try: rfkill unblock bluetooth; bluetoothctl power on"))?;
    }
    // Whatever way this function ends (an error, a panic), the adapter gets its old modes back.
    let mut guard = AdapterGuard {
        bz: &bz,
        adapter: adapter.clone(),
        discoverable: ad.discoverable,
        timeout: ad.discoverable_timeout,
        pairable: ad.pairable,
        power_off: !ad.powered,
        done: false,
    };
    // If any of the three changes is refused, put back what was already changed: the computer's Bluetooth
    // settings must not stay altered by a setup that never got going.
    let make_visible = || -> Result<(), String> {
        bz.set(&adapter, ADAPTER, "Pairable", Value::from(true))?;
        bz.set(
            &adapter,
            ADAPTER,
            "DiscoverableTimeout",
            Value::from(opts.discoverable_for.as_secs() as u32),
        )?;
        bz.set(&adapter, ADAPTER, "Discoverable", Value::from(true))
    };
    if let Err(e) = make_visible() {
        let _ = bz.set(
            &adapter,
            ADAPTER,
            "Discoverable",
            Value::from(ad.discoverable),
        );
        let _ = bz.set(
            &adapter,
            ADAPTER,
            "DiscoverableTimeout",
            Value::from(ad.discoverable_timeout),
        );
        let _ = bz.set(&adapter, ADAPTER, "Pairable", Value::from(ad.pairable));
        return Err(format!(
            "could not make this computer visible over Bluetooth ({e}); check that Bluetooth is on and not blocked (rfkill list)"
        ));
    }
    events(Event::Step(format!(
        "This computer is visible as a Bluetooth device for {} s",
        opts.discoverable_for.as_secs()
    )));

    // The agent lives on its own connection. `Pair()` is sent from that connection too, so
    // BlueZ uses this agent for it even when another one is the default.
    let names = Arc::new(Mutex::new(HashMap::new()));
    let (ctx, crx) = mpsc::channel();
    let agent_conn = opts
        .bus
        .builder()?
        .method_timeout(PAIR_CALL_TIMEOUT)
        .serve_at(
            AGENT_PATH,
            Agent {
                asks: ctx,
                timeout: opts.confirm_timeout.min(BLUEZ_AGENT_TIMEOUT),
                names: names.clone(),
                cancels: Arc::new(AtomicU64::new(0)),
                confirmed: Arc::new(Mutex::new(HashSet::new())),
            },
        )
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    let agent_path = ObjectPath::try_from(AGENT_PATH).map_err(|e| e.to_string())?;
    let restore = |bz: &Bluez| {
        let _ = bz.call(&adapter, ADAPTER, "StopDiscovery");
        let _ = bz.set(
            &adapter,
            ADAPTER,
            "Discoverable",
            Value::from(ad.discoverable),
        );
        let _ = bz.set(
            &adapter,
            ADAPTER,
            "DiscoverableTimeout",
            Value::from(ad.discoverable_timeout),
        );
        let _ = bz.set(&adapter, ADAPTER, "Pairable", Value::from(ad.pairable));
        let _ = agent_conn.call_method(
            Some(BLUEZ),
            "/org/bluez",
            Some(AGENT_MANAGER),
            "UnregisterAgent",
            &(&agent_path,),
        );
    };
    if let Err(e) = agent_conn.call_method(
        Some(BLUEZ),
        "/org/bluez",
        Some(AGENT_MANAGER),
        "RegisterAgent",
        &(&agent_path, "DisplayYesNo"),
    ) {
        restore(&bz);
        return Err(format!("could not register the pairing agent: {e}"));
    }
    if let Err(e) = agent_conn.call_method(
        Some(BLUEZ),
        "/org/bluez",
        Some(AGENT_MANAGER),
        "RequestDefaultAgent",
        &(&agent_path,),
    ) {
        events(Event::Step(format!(
            "Another Bluetooth pairing helper stays in charge ({e}); if the phone starts the pairing, confirm the code in that helper's window"
        )));
    }

    let result = (|| -> Result<DeviceInfo, String> {
        let m = bz.managed()?;
        let devices = devices_on(&m, &adapter);
        let paired_before: HashSet<String> = devices
            .iter()
            .filter(|d| d.paired)
            .map(|d| d.path.clone())
            .collect();
        let connected_before: HashSet<String> = devices
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.path.clone())
            .collect();
        // Already paired (an earlier setup, or paired by hand): only connect.
        if let Some(d) = already_paired(&devices, opts.phone_name.as_deref()) {
            events(Event::Step(format!("{} is already paired", d.name)));
            return Ok(d.clone());
        }
        events(Event::Step(match &opts.phone_name {
            Some(n) => format!(
                "Looking for \"{n}\". On the phone open Settings > Connections > Bluetooth (Samsung) or Settings > Connected devices > Pair new device, keep that screen open and tap \"{}\" when it shows up",
                ad.alias
            ),
            None => format!(
                "Looking for the phone. On the phone open its Bluetooth settings, keep that screen open and tap \"{}\" when it shows up",
                ad.alias
            ),
        }));
        // Hands-free is Classic Bluetooth: do not scan for LE advertisements (a phone's LE
        // side shows up as extra devices with random addresses).
        let filter: HashMap<&str, Value<'_>> = HashMap::from([("Transport", Value::from("bredr"))]);
        let _ = bz.conn.call_method(
            Some(BLUEZ),
            adapter.as_str(),
            Some(ADAPTER),
            "SetDiscoveryFilter",
            &(filter,),
        );
        let _ = bz.call(&adapter, ADAPTER, "StartDiscovery");
        let started = Instant::now();
        let end = started + opts.timeout;
        let mut tried: Vec<String> = Vec::new();
        let mut hinted = false;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err("setup cancelled".into());
            }
            // A pairing code from the agent (pairing started by either side).
            while let Ok(a) = crx.try_recv() {
                forward(a, &mut events);
            }
            let m = bz.managed()?;
            if !m.keys().any(|p| p.as_str() == adapter) {
                return Err("the Bluetooth adapter went away during the setup (unplugged?); plug it back in and start again".into());
            }
            let devices = devices_on(&m, &adapter);
            {
                let mut n = names.lock().unwrap_or_else(|e| e.into_inner());
                for d in &devices {
                    n.insert(d.path.clone(), d.name.clone());
                }
            }
            // Whatever its name: a phone paired during this setup (the phone started it and the
            // user compared the code), or an already paired one the user connected from the phone
            // meanwhile; else a phone by the wanted name that got paired.
            if let Some(d) = devices.iter().find(|d| {
                d.paired
                    && d.hfp_gateway
                    && (!paired_before.contains(&d.path)
                        || (d.connected && !connected_before.contains(&d.path))
                        || d.matches(opts.phone_name.as_deref()))
            }) {
                return Ok(d.clone());
            }
            if let Some(d) = devices.iter().find(|d| {
                !d.paired
                    && d.pairable_from_here()
                    && !tried.contains(&d.path)
                    && d.matches(opts.phone_name.as_deref())
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
                let conn = agent_conn.clone();
                let pairing = std::thread::spawn(move || {
                    conn.call_method(Some(BLUEZ), path.as_str(), Some(DEVICE), "Pair", &())
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                });
                let mut cancel_sent = false;
                while !pairing.is_finished() {
                    if cancel.load(Ordering::SeqCst) && !cancel_sent {
                        cancel_sent = true;
                        let _ = bz.call(&d.path, DEVICE, "CancelPairing");
                    }
                    if let Ok(a) = crx.recv_timeout(Duration::from_millis(100)) {
                        forward(a, &mut events);
                    }
                }
                let r = pairing
                    .join()
                    .unwrap_or_else(|_| Err("pairing failed".into()));
                while let Ok(a) = crx.try_recv() {
                    forward(a, &mut events);
                }
                if cancel.load(Ordering::SeqCst) {
                    return Err("setup cancelled".into());
                }
                match r {
                    Ok(()) => {}
                    Err(e) if benign(&e) => {}
                    Err(e) => {
                        events(Event::Step(format!(
                            "Pairing with {} failed: {}",
                            d.name,
                            pair_error(&e)
                        )));
                        let _ = bz.call(&adapter, ADAPTER, "StartDiscovery");
                        continue;
                    }
                }
                if let Some(x) = bz.device(&d.path)?.filter(|x| x.paired) {
                    return Ok(x);
                }
            }
            if !hinted
                && let Some(n) = &opts.phone_name
                && started.elapsed() >= HINT_AFTER
            {
                let near: Vec<String> = devices
                    .iter()
                    .filter(|d| !d.paired && d.phone_like() && !d.name.is_empty())
                    .map(|d| format!("\"{}\"", d.name))
                    .collect();
                if !near.is_empty() {
                    hinted = true;
                    events(Event::Step(format!(
                        "No device called \"{n}\" so far; phones nearby: {}. If one of them is yours, tap \"{}\" in that phone's Bluetooth list to pair from the phone",
                        near.join(", "),
                        ad.alias
                    )));
                }
            }
            if Instant::now() >= end {
                return Err(match &opts.phone_name {
                    Some(n) => format!(
                        "no phone called \"{n}\" showed up or paired within {} s; is its Bluetooth on and its Bluetooth screen open? Tap \"{}\" there to pair from the phone (try again, or run without a name)",
                        opts.timeout.as_secs(),
                        ad.alias
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
            restore(&bz);
            return Err(e);
        }
    };

    // Trust it while the agent is still registered: right after pairing the phone connects its
    // profiles, and BlueZ asks the agent to authorize each one for a device it does not trust.
    events(Event::Step(format!(
        "Paired with {} ({}); marking it trusted so it reconnects by itself",
        device.name, device.address
    )));
    let trusted = if device.trusted {
        Ok(())
    } else {
        bz.set(&device.path, DEVICE, "Trusted", Value::from(true))
    };
    restore(&bz);
    trusted?;

    // BlueZ may still be reading the phone's services right after pairing.
    let end = Instant::now() + SERVICES_WAIT;
    let mut device = device;
    while !device.hfp_gateway && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(300));
        if let Some(d) = bz.device(&device.path)? {
            device = d;
        }
    }
    if !device.hfp_gateway {
        events(Event::Step(format!(
            "{} does not list the hands-free (calls) service yet; trying anyway",
            device.name
        )));
    }
    events(Event::Step(
        "Connecting the phone's calls (hands-free) profile".into(),
    ));
    guard.done = true;
    connect_hfp(&bz, &device.path)?;
    Ok(Remembered {
        address: device.address,
        name: device.name,
        geekbridge_name: opts.phone_name.clone(),
    })
}

/// Puts the adapter's Discoverable/DiscoverableTimeout/Pairable (read before the setup changed
/// them) back when dropped, and Powered off again if the setup turned it on and did not get as
/// far as pairing. Setting a value that is already there is harmless.
struct AdapterGuard<'a> {
    bz: &'a Bluez,
    adapter: String,
    discoverable: bool,
    timeout: u32,
    pairable: bool,
    power_off: bool,
    /// Set once the phone is paired: Bluetooth stays on then.
    done: bool,
}

impl Drop for AdapterGuard<'_> {
    fn drop(&mut self) {
        let a = &self.adapter;
        let _ = self
            .bz
            .set(a, ADAPTER, "Discoverable", Value::from(self.discoverable));
        let _ = self
            .bz
            .set(a, ADAPTER, "DiscoverableTimeout", Value::from(self.timeout));
        let _ = self
            .bz
            .set(a, ADAPTER, "Pairable", Value::from(self.pairable));
        if self.power_off && !self.done {
            let _ = self.bz.set(a, ADAPTER, "Powered", Value::from(false));
        }
    }
}

/// Connect the phone's hands-free profile only (not every profile: `Connect()` would also pull
/// the phone's media audio here); bring the whole link up only if that is refused.
fn connect_hfp(bz: &Bluez, path: &str) -> Result<(), String> {
    match bz.connect_profile(path, HFP_AG_UUID) {
        Ok(()) => return Ok(()),
        Err(e) if benign(&e) => return Ok(()),
        Err(_) => {}
    }
    // Connect() can fail on one profile and still bring up others; HFP is asked for again.
    let _ = bz.call(path, DEVICE, "Connect");
    match bz.connect_profile(path, HFP_AG_UUID) {
        Ok(()) => Ok(()),
        Err(e) if benign(&e) => Ok(()),
        Err(e) => Err(hfp_error(&e)),
    }
}

/// Connect a remembered phone's hands-free profile (it came back in range, or GeekBridge's link
/// to it came up). Quietly does nothing if it is already connected. Blocking (BlueZ may take a
/// few seconds to page an absent phone).
pub fn reconnect(bus: &Bus, address: &str) -> Result<bool, String> {
    let bz = Bluez {
        conn: bus.connect(Duration::from_secs(20))?,
    };
    let m = bz.managed()?;
    let Some(ad) = adapters(&m).into_iter().next() else {
        return Err("no Bluetooth adapter".into());
    };
    if !ad.powered {
        return Ok(false);
    }
    let Some(d) = adapters(&m)
        .iter()
        .flat_map(|a| devices_on(&m, &a.path))
        .find(|d| d.address.eq_ignore_ascii_case(address) && d.paired)
    else {
        return Err(
            "the remembered phone is no longer paired; run the call-audio setup again".into(),
        );
    };
    connect_hfp(&bz, &d.path).map(|()| true)
}

/// Unpair `address` from this computer (BlueZ `RemoveDevice`). A phone that is already gone is
/// success. Blocking.
pub fn forget(bus: &Bus, address: &str) -> Result<(), String> {
    let bz = Bluez {
        conn: bus.connect(Duration::from_secs(20))?,
    };
    let m = bz.managed()?;
    for a in adapters(&m) {
        let Some(d) = devices_on(&m, &a.path)
            .into_iter()
            .find(|d| d.address.eq_ignore_ascii_case(address))
        else {
            continue;
        };
        bz.conn
            .call_method(
                Some(BLUEZ),
                a.path.as_str(),
                Some(ADAPTER),
                "RemoveDevice",
                &(ObjectPath::try_from(d.path.as_str()).map_err(|e| e.to_string())?,),
            )
            .map_err(|e| format!("could not unpair the phone: {e}"))?;
    }
    Ok(())
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
            address_type: "public".into(),
            name: name.into(),
            icon: icon.into(),
            paired: false,
            trusted: false,
            connected: false,
            services_resolved: false,
            hfp_gateway: gw,
        }
    }

    fn paired(path: &str, name: &str, connected: bool) -> DeviceInfo {
        DeviceInfo {
            path: path.into(),
            paired: true,
            connected,
            ..dev(name, "phone", true)
        }
    }

    #[test]
    fn an_already_paired_phone_is_only_taken_by_its_name() {
        let fold = paired("/org/bluez/hci0/dev_1", "Galaxy Z Fold5", true);
        let pixel = paired("/org/bluez/hci0/dev_2", "Pixel 6", false);
        let ds = vec![fold.clone(), pixel.clone()];
        assert_eq!(already_paired(&ds, Some("fold5")), Some(&fold));
        assert_eq!(already_paired(&ds, Some("pixel")), Some(&pixel));
        // GeekBridge's name for the Fold ("SM-F946B", Build.MODEL) matches nothing: no guess,
        // even with one phone connected (it may be the other phone).
        assert_eq!(already_paired(&ds, Some("SM-F946B")), None);
        assert_eq!(
            already_paired(std::slice::from_ref(&fold), Some("SM-F946B")),
            None
        );
        // Something paired that offers no hands-free is never it.
        let speaker = DeviceInfo {
            hfp_gateway: false,
            ..paired("/org/bluez/hci0/dev_3", "Speaker", true)
        };
        assert_eq!(already_paired(&[speaker], None), None);
    }

    #[test]
    fn le_advertisements_with_random_addresses_are_never_paired_from_here() {
        let le = DeviceInfo {
            address_type: "random".into(),
            ..dev("Galaxy Z Fold5", "", false)
        };
        assert!(!le.pairable_from_here());
        assert!(dev("Galaxy Z Fold5", "phone", false).pairable_from_here());
        // BlueZ before AddressType existed: assume Classic.
        assert!(
            DeviceInfo {
                address_type: String::new(),
                ..le
            }
            .pairable_from_here()
        );
    }

    #[test]
    fn a_powered_adapter_is_used_before_an_unpowered_one() {
        let mut m: Managed = HashMap::new();
        let ad = |powered: bool| {
            let mut i: HashMap<String, OwnedValue> = HashMap::new();
            i.insert("Powered".into(), OwnedValue::from(powered));
            i.insert(
                "Alias".into(),
                OwnedValue::try_from(Value::from("omarchy")).unwrap(),
            );
            i.insert("DiscoverableTimeout".into(), OwnedValue::from(180u32));
            let mut ifs: Ifaces = HashMap::new();
            ifs.insert(ADAPTER.into(), i);
            ifs
        };
        m.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0").unwrap(),
            ad(false),
        );
        m.insert(
            OwnedObjectPath::try_from("/org/bluez/hci1").unwrap(),
            ad(true),
        );
        let v = adapters(&m);
        assert_eq!(v[0].path, "/org/bluez/hci1");
        assert_eq!(v[0].alias, "omarchy");
        assert_eq!(v[0].discoverable_timeout, 180);
        assert_eq!(v[1].path, "/org/bluez/hci0");
        // Devices are taken from the chosen adapter only.
        let mut d: HashMap<String, OwnedValue> = HashMap::new();
        d.insert(
            "Address".into(),
            OwnedValue::try_from(Value::from("aa:bb:cc:00:11:22")).unwrap(),
        );
        let mut ifs: Ifaces = HashMap::new();
        ifs.insert(DEVICE.into(), d);
        m.insert(
            OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_00_11_22").unwrap(),
            ifs,
        );
        assert!(devices_on(&m, "/org/bluez/hci1").is_empty());
        assert_eq!(
            devices_on(&m, "/org/bluez/hci0")[0].address,
            "AA:BB:CC:00:11:22"
        );
    }

    #[test]
    fn bluez_errors_become_sentences_that_keep_the_raw_error() {
        let e = pair_error("org.bluez.Error.AuthenticationCanceled: Authentication Canceled");
        assert!(e.starts_with("the code was not confirmed in time"), "{e}");
        assert!(e.contains("org.bluez.Error.AuthenticationCanceled"), "{e}");
        assert!(pair_error("org.bluez.Error.AuthenticationFailed").contains("Unpair"));
        assert!(
            pair_error("org.bluez.Error.Failed: br-connection-page-timeout")
                .contains("did not answer")
        );
        assert_eq!(
            pair_error("org.bluez.Error.Failed: odd"),
            "org.bluez.Error.Failed: odd"
        );
        assert!(
            hfp_error("org.bluez.Error.NotAvailable: br-connection-profile-unavailable")
                .contains("hfp_hf")
        );
        assert!(hfp_error("org.bluez.Error.Failed: Protocol not available").contains("Calls"));
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

    /// Security review 7 (S7-10): any device in range chooses the name it shows, with no pairing needed. The name
    /// reaches the terminal and the setup window, so it must carry nothing that moves the cursor or reorders text.
    #[test]
    fn a_remote_device_name_is_cleaned_before_it_is_shown() {
        let mut d: HashMap<String, OwnedValue> = HashMap::new();
        d.insert(
            "Alias".into(),
            OwnedValue::try_from(Value::from("Ana\u{1b}[2J\u{202E}'s Galaxy\n\u{200B}")).unwrap(),
        );
        assert_eq!(
            DeviceInfo::from("/org/bluez/hci0/dev_AA", &d).name,
            "Ana[2J's Galaxy"
        );
        let long = clean_remote_name(&"x".repeat(500));
        assert_eq!(long.chars().count(), 48);
        assert_eq!(
            device_label("/org/bluez/hci0/dev_AA_BB_CC_00_11_22", "Pixel"),
            "Pixel (AA:BB:CC:00:11:22)"
        );
        assert_eq!(device_label("/weird", "Pixel"), "Pixel");
    }

    /// S7-11: while the agent is registered it must not wave through services for devices the user did not confirm.
    #[test]
    fn services_are_authorized_only_for_the_device_the_user_confirmed_and_only_audio_ones() {
        let confirmed: HashSet<String> = ["/org/bluez/hci0/dev_AA".to_owned()].into();
        assert!(service_allowed(
            &confirmed,
            "/org/bluez/hci0/dev_AA",
            HFP_AG_UUID
        ));
        assert!(service_allowed(
            &confirmed,
            "/org/bluez/hci0/dev_AA",
            "0000110B-0000-1000-8000-00805F9B34FB"
        ));
        // another device, even asking for hands-free
        assert!(!service_allowed(
            &confirmed,
            "/org/bluez/hci0/dev_BB",
            HFP_AG_UUID
        ));
        // the confirmed device asking for something that is not about call audio (phone book, files, keyboard)
        assert!(!service_allowed(
            &confirmed,
            "/org/bluez/hci0/dev_AA",
            "0000112f-0000-1000-8000-00805f9b34fb"
        ));
        assert!(!service_allowed(
            &confirmed,
            "/org/bluez/hci0/dev_AA",
            "00001124-0000-1000-8000-00805f9b34fb"
        ));
        assert!(!service_allowed(
            &HashSet::new(),
            "/org/bluez/hci0/dev_AA",
            HFP_AG_UUID
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_remembered_phone_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let r = Remembered {
            address: "AA:BB:CC:00:11:22".into(),
            name: "Pixel".into(),
            geekbridge_name: None,
        };
        save_remembered(d.path(), &r).unwrap();
        let mode = std::fs::metadata(d.path().join(REMEMBERED_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
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

#[cfg(test)]
mod sandbox_guard_tests {
    use super::system_bluetooth_blocked_with as blocked;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn a_plain_run_may_use_the_system_bluetooth() {
        assert!(!blocked(env(&[])));
        assert!(!blocked(env(&[("GEEKBRIDGE_NO_SYSTEM_BLUETOOTH", "0")])));
        assert!(!blocked(env(&[("GEEKBRIDGE_STATE_DIR", "")])));
    }

    #[test]
    fn a_sandbox_or_the_explicit_switch_blocks_it() {
        assert!(blocked(env(&[("GEEKBRIDGE_NO_SYSTEM_BLUETOOTH", "1")])));
        assert!(blocked(env(&[("GEEKBRIDGE_STATE_DIR", "/tmp/x")])));
        assert!(blocked(env(&[("GEEKBRIDGE_CONTROL_PORT", "9")])));
    }

    #[test]
    fn allow_lifts_only_the_sandbox_rule() {
        let allow = ("GEEKBRIDGE_ALLOW_SYSTEM_BLUETOOTH", "1");
        assert!(!blocked(env(&[("GEEKBRIDGE_STATE_DIR", "/tmp/x"), allow])));
        assert!(blocked(env(&[
            ("GEEKBRIDGE_NO_SYSTEM_BLUETOOTH", "1"),
            allow
        ])));
    }
}
