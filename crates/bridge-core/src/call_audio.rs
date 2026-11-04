// SPDX-License-Identifier: GPL-3.0-or-later
//! "Call audio on this computer": the guided setup checklist, one model for every platform.
//!
//! To hear and speak a phone call through the computer's headphones and microphone the voice has to
//! travel over Bluetooth hands-free (HFP) between phone and computer: Android gives apps no call audio
//! (`docs/call-audio-options.md`). Whether that works is a chain of small facts (adapter, radio
//! unblocked, powered, phone paired, hands-free connected, headphones and microphone chosen) and each
//! link fails for its own reason with its own one-line fix. This module turns raw facts into that
//! ordered list. It performs no I/O: collectors in `bridge-service` read the host and the windows
//! draw the result, so every parser and every decision here is tested with canned data.
//!
//! Nothing here runs a command. A [`Fix`] may carry a command to copy and paste; it is never run, and
//! none of them uses `sudo` unless the fix really needs it (the user decides).
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where the long explanation lives.
pub const DOC: &str = "docs/call-audio-options.md";

/// How long the "Test call audio" microphone meter listens.
pub const TEST_SECONDS: u32 = 5;
/// Meter readings per second (each reading is the loudest sample in that slice, 0..=100).
pub const METER_HZ: u32 = 10;
/// Peak level (0..=100) at or above which the microphone counts as having heard something.
pub const HEARD_THRESHOLD: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    Adapter,
    Unblocked,
    Powered,
    Paired,
    Hfp,
    Headset,
}

impl StepId {
    pub fn key(self) -> &'static str {
        match self {
            Self::Adapter => "callaudio.step.adapter",
            Self::Unblocked => "callaudio.step.unblocked",
            Self::Powered => "callaudio.step.powered",
            Self::Paired => "callaudio.step.paired",
            Self::Hfp => "callaudio.step.hfp",
            Self::Headset => "callaudio.step.headset",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Ok,
    /// Needs the user; the step carries a [`Fix`].
    Todo,
    /// An earlier step has to be done first.
    Waiting,
    /// This computer cannot tell (reading failed, or the OS does not expose it).
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    NoAdapter,
    HardBlocked,
    Unblock,
    StartBluetooth,
    PowerOn,
    Pair,
    Connect,
    ChooseDevices,
    WindowsAdapter,
    WindowsPair,
    WindowsPhoneCalls,
    WindowsSound,
}

impl FixKind {
    pub const ALL: [FixKind; 12] = [
        Self::NoAdapter,
        Self::HardBlocked,
        Self::Unblock,
        Self::StartBluetooth,
        Self::PowerOn,
        Self::Pair,
        Self::Connect,
        Self::ChooseDevices,
        Self::WindowsAdapter,
        Self::WindowsPair,
        Self::WindowsPhoneCalls,
        Self::WindowsSound,
    ];
    /// Catalogue key of the one-line fix.
    pub fn key(self) -> &'static str {
        match self {
            Self::NoAdapter => "callaudio.fix.no_adapter",
            Self::HardBlocked => "callaudio.fix.hard_blocked",
            Self::Unblock => "callaudio.fix.unblock",
            Self::StartBluetooth => "callaudio.fix.start_bluetooth",
            Self::PowerOn => "callaudio.fix.power_on",
            Self::Pair => "callaudio.fix.pair",
            Self::Connect => "callaudio.fix.connect",
            Self::ChooseDevices => "callaudio.fix.choose_devices",
            Self::WindowsAdapter => "callaudio.fix.windows_adapter",
            Self::WindowsPair => "callaudio.fix.windows_pair",
            Self::WindowsPhoneCalls => "callaudio.fix.windows_phone_calls",
            Self::WindowsSound => "callaudio.fix.windows_sound",
        }
    }
}

/// A one-line fix: text from the catalogue by `kind`, plus an optional command to copy. The command
/// is data for the clipboard; nothing in GeekBridge runs it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub kind: FixKind,
    #[serde(default)]
    pub command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub id: StepId,
    pub state: StepState,
    #[serde(default)]
    pub fix: Option<Fix>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Everything is in place: the voice can come to this computer.
    Ready,
    /// No Bluetooth adapter: answer here, talk on the phone, or add a USB adapter.
    NoBluetooth,
    /// There are steps left; see the list.
    Todo,
    /// macOS: not available (see the guidance); never green.
    MacFallback,
    /// A platform this build knows nothing about.
    Unsupported,
}

/// A headphone/speaker (output) or microphone (input) the audio server offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// The audio server's name for it (what a selection is stored as).
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub bluetooth: bool,
    #[serde(default)]
    pub default: bool,
}

/// What the user chose; `None` follows the system default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub input: Option<String>,
}

impl Selection {
    /// Parse the small state file. Anything unreadable is "nothing chosen"; names with control
    /// characters or an absurd length are dropped.
    pub fn parse(text: &str) -> Self {
        let clean = |s: Option<String>| s.filter(|s| device_name_ok(s));
        match serde_json::from_str::<Selection>(text) {
            Ok(s) => Selection {
                output: clean(s.output),
                input: clean(s.input),
            },
            Err(_) => Selection::default(),
        }
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

/// A server device name safe to store and to pass as an argument: printable ASCII-ish, bounded.
pub fn device_name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 200 && !s.starts_with('-') && s.chars().all(|c| !c.is_control())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestPhase {
    #[default]
    Idle,
    /// Playing the tone.
    Tone,
    /// Listening to the microphone.
    Listening,
    Done,
}

/// The "Test call audio" run, as the window shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestProgress {
    pub phase: TestPhase,
    /// Meter readings so far (0..=100), newest last, one per `1/METER_HZ` second.
    #[serde(default)]
    pub levels: Vec<u8>,
    /// The tone could be started on the chosen output; `None` until tried.
    #[serde(default)]
    pub tone_played: Option<bool>,
    /// The microphone could be opened; `None` until tried.
    #[serde(default)]
    pub mic_opened: Option<bool>,
}

impl TestProgress {
    pub fn peak(&self) -> u8 {
        self.levels.iter().copied().max().unwrap_or(0)
    }
    pub fn heard(&self) -> bool {
        self.peak() >= HEARD_THRESHOLD
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    /// `linux`, `windows`, `macos` or the raw OS name.
    pub platform: String,
    pub verdict: Verdict,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub outputs: Vec<Device>,
    #[serde(default)]
    pub inputs: Vec<Device>,
    /// The output/input that will be used (the saved choice, else the system default, else the first).
    #[serde(default)]
    pub chosen_output: Option<String>,
    #[serde(default)]
    pub chosen_input: Option<String>,
    /// "Test call audio" can run on this computer right now.
    pub can_test: bool,
    #[serde(default)]
    pub test: TestProgress,
}

impl Setup {
    pub fn done_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|s| s.state == StepState::Ok)
            .count()
    }
    /// The first step that needs the user: what to show first.
    pub fn next_fix(&self) -> Option<&Step> {
        self.steps
            .iter()
            .find(|s| s.state == StepState::Todo && s.fix.is_some())
    }
}

// ---------------------------------------------------------------------------------------------
// Linux
// ---------------------------------------------------------------------------------------------

/// Software and hardware block state of the Bluetooth radios (`/sys/class/rfkill`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rfkill {
    pub soft: bool,
    pub hard: bool,
}

/// One rfkill entry: its `type` file and its `soft` / `hard` files.
pub fn parse_rfkill(kind: &str, soft: &str, hard: &str) -> Option<Rfkill> {
    (kind.trim() == "bluetooth").then(|| Rfkill {
        soft: soft.trim() == "1",
        hard: hard.trim() == "1",
    })
}

/// Everything the Linux checklist needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinuxFacts {
    pub adapters: usize,
    /// Worst case over the Bluetooth radios, `None` when rfkill lists none.
    pub rfkill: Option<Rfkill>,
    /// `Powered` from `bluetoothctl show`; `None` if `bluetoothctl` could not talk to bluetoothd.
    pub powered: Option<bool>,
    /// Paired devices that offer the hands-free audio gateway service (phones). `None` = unreadable.
    pub paired_phones: Option<Vec<String>>,
    /// Of those, the ones connected right now.
    pub connected_phones: usize,
    /// Phones the telephony backend registered (always means connected).
    pub registered_phones: usize,
    pub outputs: Vec<Device>,
    pub inputs: Vec<Device>,
    pub selection: Selection,
    pub audio_server_ok: bool,
}

/// `Powered: yes` from `bluetoothctl show` (the first controller). `None` if there is no such line.
pub fn parse_powered(show: &str) -> Option<bool> {
    show.lines().find_map(|l| {
        let l = l.trim();
        let v = l.strip_prefix("Powered:")?.trim();
        Some(v == "yes")
    })
}

/// `Connected: yes` from `bluetoothctl info <addr>`.
pub fn parse_connected(info: &str) -> bool {
    info.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("Connected: yes"))
}

/// Devices from `pactl --format=json list sinks|sources`. Monitors of outputs are not microphones.
/// The default (by name) is flagged. Malformed input gives an empty list.
pub fn parse_pactl_devices(json: &str, default_name: Option<&str>, sources: bool) -> Vec<Device> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|d| {
            let name = d.get("name")?.as_str()?;
            if !device_name_ok(name) {
                return None;
            }
            if sources
                && (name.ends_with(".monitor")
                    || d.get("monitor_of_sink").is_some_and(|m| !m.is_null()))
            {
                return None;
            }
            let props = d.get("properties");
            let prop = |k: &str| props.and_then(|p| p.get(k)).and_then(Value::as_str);
            let bluetooth = prop("device.api") == Some("bluez5") || name.starts_with("bluez_");
            let label = d
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(name);
            let label: String = label.chars().filter(|c| !c.is_control()).take(80).collect();
            Some(Device {
                id: name.to_owned(),
                label,
                bluetooth,
                default: default_name == Some(name),
            })
        })
        .collect()
}

/// Pick the device a test or a call will use: the saved one if it still exists, else the system
/// default, else the first that is not Bluetooth, else the first.
pub fn choose(devices: &[Device], saved: Option<&str>) -> Option<String> {
    saved
        .and_then(|s| devices.iter().find(|d| d.id == s))
        .or_else(|| devices.iter().find(|d| d.default))
        .or_else(|| devices.iter().find(|d| !d.bluetooth))
        .or_else(|| devices.first())
        .map(|d| d.id.clone())
}

fn step(id: StepId, state: StepState, fix: Option<Fix>) -> Step {
    Step { id, state, fix }
}
fn todo(id: StepId, kind: FixKind, command: Option<&str>) -> Step {
    step(
        id,
        StepState::Todo,
        Some(Fix {
            kind,
            command: command.map(str::to_owned),
        }),
    )
}
fn ok(id: StepId) -> Step {
    step(id, StepState::Ok, None)
}
fn waiting(id: StepId) -> Step {
    step(id, StepState::Waiting, None)
}

/// The Linux checklist. Each step waits for the one before it; the headphones step is independent
/// of Bluetooth so the local test works before an adapter is bought.
pub fn build_linux(f: &LinuxFacts) -> Setup {
    let have_adapter = f.adapters > 0;
    let mut steps = Vec::with_capacity(6);
    steps.push(if have_adapter {
        ok(StepId::Adapter)
    } else {
        todo(StepId::Adapter, FixKind::NoAdapter, None)
    });
    let unblocked = match f.rfkill {
        _ if !have_adapter => waiting(StepId::Unblocked),
        Some(r) if r.hard => todo(StepId::Unblocked, FixKind::HardBlocked, None),
        Some(r) if r.soft => todo(
            StepId::Unblocked,
            FixKind::Unblock,
            Some("rfkill unblock bluetooth"),
        ),
        _ => ok(StepId::Unblocked),
    };
    let unblocked_ok = unblocked.state == StepState::Ok;
    steps.push(unblocked);
    let powered = if !unblocked_ok {
        waiting(StepId::Powered)
    } else {
        match f.powered {
            Some(true) => ok(StepId::Powered),
            Some(false) => todo(
                StepId::Powered,
                FixKind::PowerOn,
                Some("bluetoothctl power on"),
            ),
            None => todo(
                StepId::Powered,
                FixKind::StartBluetooth,
                Some("sudo systemctl enable --now bluetooth.service"),
            ),
        }
    };
    let powered_ok = powered.state == StepState::Ok;
    steps.push(powered);
    let paired_list = f.paired_phones.as_deref();
    let paired = if !powered_ok {
        waiting(StepId::Paired)
    } else if f.registered_phones > 0 || paired_list.is_some_and(|p| !p.is_empty()) {
        ok(StepId::Paired)
    } else if paired_list.is_none() {
        step(StepId::Paired, StepState::Unknown, None)
    } else {
        todo(StepId::Paired, FixKind::Pair, Some("bluetoothctl scan on"))
    };
    let paired_ok = paired.state == StepState::Ok;
    steps.push(paired);
    let hfp = if !paired_ok {
        waiting(StepId::Hfp)
    } else if f.registered_phones > 0 || f.connected_phones > 0 {
        ok(StepId::Hfp)
    } else {
        let cmd = paired_list
            .and_then(|p| p.first())
            .map(|a| format!("bluetoothctl connect {a}"));
        todo(StepId::Hfp, FixKind::Connect, cmd.as_deref())
    };
    let hfp_ok = hfp.state == StepState::Ok;
    steps.push(hfp);

    let chosen_output = choose(&f.outputs, f.selection.output.as_deref());
    let chosen_input = choose(&f.inputs, f.selection.input.as_deref());
    let headset = if !f.audio_server_ok {
        step(StepId::Headset, StepState::Unknown, None)
    } else if chosen_output.is_some() && chosen_input.is_some() {
        ok(StepId::Headset)
    } else {
        todo(
            StepId::Headset,
            FixKind::ChooseDevices,
            Some("pactl list short sinks; pactl list short sources"),
        )
    };
    let headset_ok = headset.state == StepState::Ok;
    steps.push(headset);

    let verdict = if !have_adapter {
        Verdict::NoBluetooth
    } else if hfp_ok && headset_ok {
        Verdict::Ready
    } else {
        Verdict::Todo
    };
    Setup {
        platform: "linux".into(),
        verdict,
        steps,
        can_test: f.audio_server_ok && (chosen_output.is_some() || chosen_input.is_some()),
        outputs: f.outputs.clone(),
        inputs: f.inputs.clone(),
        chosen_output,
        chosen_input,
        test: TestProgress::default(),
    }
}

// ---------------------------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------------------------

/// One row of `Get-PnpDevice | Select Status, FriendlyName, InstanceId | ConvertTo-Json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PnpDevice {
    pub status: String,
    pub name: String,
    pub instance_id: String,
}

/// Parse PowerShell's `ConvertTo-Json`: a single object when one row matched, an array for several,
/// nothing at all for none. A UTF-8 byte-order mark (Windows PowerShell prints one) is ignored.
pub fn parse_pnp_json(text: &str) -> Vec<PnpDevice> {
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Vec::new();
    }
    let rows: Vec<Value> = match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(a)) => a,
        Ok(v @ Value::Object(_)) => vec![v],
        _ => return Vec::new(),
    };
    rows.iter()
        .filter_map(|r| {
            let s = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_owned);
            Some(PnpDevice {
                status: s("Status").unwrap_or_default(),
                name: s("FriendlyName").unwrap_or_default(),
                instance_id: s("InstanceId")?,
            })
        })
        .collect()
}

/// Service class of a hands-free audio gateway (a phone), as it appears in a `BTHENUM` instance id.
const HFP_AG_GUID: &str = "0000111F-0000-1000-8000-00805F9B34FB";

/// `BTHENUM\{0000111F-...}_VID&...` is the "Hands-Free AG" node Windows creates for a paired phone
/// that offers hands-free. Its Status is `OK` while that profile is connected. Unverified on a
/// real Windows install: the instance-id shape is Microsoft's, the status semantics are not
/// documented, so this is reported as best-effort.
pub fn is_hfp_gateway_node(d: &PnpDevice) -> bool {
    let id = d.instance_id.to_ascii_uppercase();
    id.starts_with("BTHENUM\\") && id.contains(HFP_AG_GUID)
}

/// The Bluetooth radio itself (a USB or PCI device), not a paired device or the enumerators.
pub fn is_bluetooth_radio(d: &PnpDevice) -> bool {
    let id = d.instance_id.to_ascii_uppercase();
    let n = d.name.to_ascii_lowercase();
    (id.starts_with("USB\\") || id.starts_with("PCI\\") || id.starts_with("ACPI\\"))
        && !n.contains("enumerator")
        && !n.contains("hub")
}

/// Audio endpoints from `Get-PnpDevice -Class AudioEndpoint`: instance ids carry `{0.0.0.` for
/// playback and `{0.0.1.` for capture. Names read like `Speakers (Realtek Audio)`.
pub fn windows_endpoints(devs: &[PnpDevice]) -> (Vec<Device>, Vec<Device>) {
    let mut out = Vec::new();
    let mut inp = Vec::new();
    for d in devs.iter().filter(|d| d.status.eq_ignore_ascii_case("OK")) {
        let id = d.instance_id.to_ascii_uppercase();
        let dev = Device {
            id: d.instance_id.clone(),
            label: d
                .name
                .chars()
                .filter(|c| !c.is_control())
                .take(80)
                .collect(),
            bluetooth: d.name.to_ascii_lowercase().contains("hands-free"),
            default: false,
        };
        if id.contains("{0.0.0.") {
            out.push(dev);
        } else if id.contains("{0.0.1.") {
            inp.push(dev);
        }
    }
    (out, inp)
}

/// Checklist for Windows from the Bluetooth class devices and the audio endpoints. Windows routes
/// call audio to the default communications device itself, so the headphones step is only
/// "check Sound settings" and there is no local test.
pub fn build_windows(bluetooth: Option<&[PnpDevice]>, endpoints: &[PnpDevice]) -> Setup {
    let (outputs, inputs) = windows_endpoints(endpoints);
    let Some(bt) = bluetooth else {
        return Setup {
            platform: "windows".into(),
            verdict: Verdict::Todo,
            steps: vec![
                step(StepId::Adapter, StepState::Unknown, None),
                step(StepId::Paired, StepState::Unknown, None),
                step(StepId::Hfp, StepState::Unknown, None),
                step(StepId::Headset, StepState::Unknown, None),
            ],
            outputs,
            inputs,
            chosen_output: None,
            chosen_input: None,
            can_test: false,
            test: TestProgress::default(),
        };
    };
    let adapter_ok = bt.iter().any(is_bluetooth_radio);
    let radio_works = bt
        .iter()
        .any(|d| is_bluetooth_radio(d) && d.status.eq_ignore_ascii_case("OK"));
    let nodes: Vec<&PnpDevice> = bt.iter().filter(|d| is_hfp_gateway_node(d)).collect();
    let mut steps = Vec::new();
    steps.push(if radio_works {
        ok(StepId::Adapter)
    } else if adapter_ok {
        todo(StepId::Adapter, FixKind::WindowsAdapter, None)
    } else {
        todo(StepId::Adapter, FixKind::NoAdapter, None)
    });
    let paired = if !radio_works {
        waiting(StepId::Paired)
    } else if nodes.is_empty() {
        todo(
            StepId::Paired,
            FixKind::WindowsPair,
            Some("start ms-settings:bluetooth"),
        )
    } else {
        ok(StepId::Paired)
    };
    let paired_ok = paired.state == StepState::Ok;
    steps.push(paired);
    let hfp = if !paired_ok {
        waiting(StepId::Hfp)
    } else if nodes.iter().any(|d| d.status.eq_ignore_ascii_case("OK")) {
        ok(StepId::Hfp)
    } else {
        todo(
            StepId::Hfp,
            FixKind::WindowsPhoneCalls,
            Some("start ms-settings:bluetooth"),
        )
    };
    let hfp_ok = hfp.state == StepState::Ok;
    steps.push(hfp);
    // Windows picks the default communications device; the OS exposes no cheap read of it here.
    steps.push(Step {
        id: StepId::Headset,
        state: StepState::Unknown,
        fix: Some(Fix {
            kind: FixKind::WindowsSound,
            command: Some("start ms-settings:sound".into()),
        }),
    });
    let verdict = if !adapter_ok {
        Verdict::NoBluetooth
    } else if hfp_ok {
        Verdict::Ready
    } else {
        Verdict::Todo
    };
    Setup {
        platform: "windows".into(),
        verdict,
        steps,
        outputs,
        inputs,
        chosen_output: None,
        chosen_input: None,
        can_test: false,
        test: TestProgress::default(),
    }
}

/// macOS: never a checklist. Whether a Mac can be a phone's hands-free unit is unverified (the API
/// is declared, no Android phone has been tried), so the window offers the fallbacks instead.
pub fn mac_fallback() -> Setup {
    Setup {
        platform: "macos".into(),
        verdict: Verdict::MacFallback,
        steps: Vec::new(),
        outputs: Vec::new(),
        inputs: Vec::new(),
        chosen_output: None,
        chosen_input: None,
        can_test: false,
        test: TestProgress::default(),
    }
}

pub fn unsupported(platform: &str) -> Setup {
    Setup {
        verdict: Verdict::Unsupported,
        ..Setup {
            platform: platform.chars().take(32).collect(),
            ..mac_fallback()
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The local test: tone and level meter
// ---------------------------------------------------------------------------------------------

/// Sample rate of the test tone and of the meter capture.
pub const TEST_RATE: u32 = 16_000;

/// A short WAV (mono, 16-bit, 16 kHz): two rising notes at a gentle level, with fades so it
/// does not click. About 1.2 s. Played on the chosen output by the service.
pub fn tone_wav() -> Vec<u8> {
    let rate = TEST_RATE as usize;
    let notes: [(f32, usize); 2] = [(660.0, rate * 6 / 10), (880.0, rate * 6 / 10)];
    let mut pcm: Vec<i16> = Vec::new();
    for (hz, len) in notes {
        let fade = rate / 100 * 3;
        for i in 0..len {
            let env = (i.min(len - 1 - i) as f32 / fade as f32).min(1.0);
            let s = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin();
            pcm.push((s * env * 0.25 * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + pcm.len() * 2);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&TEST_RATE.to_le_bytes());
    w.extend_from_slice(&(TEST_RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

/// Loudest sample of a block of little-endian 16-bit mono PCM, scaled to 0..=100 with a mild
/// square-root curve so ordinary speech (about -30 dBFS) lands visibly mid-meter.
pub fn level_of(pcm: &[u8]) -> u8 {
    let peak = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c).unsigned_abs())
        .max()
        .unwrap_or(0);
    let x = f32::from(peak) / 32768.0;
    ((x.sqrt() * 100.0).round() as u32).min(100) as u8
}

/// Bytes of PCM in one meter slice.
pub const fn slice_bytes() -> usize {
    (TEST_RATE / METER_HZ) as usize * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, bt: bool, def: bool) -> Device {
        Device {
            id: id.into(),
            label: id.into(),
            bluetooth: bt,
            default: def,
        }
    }

    fn base() -> LinuxFacts {
        LinuxFacts {
            adapters: 1,
            rfkill: Some(Rfkill::default()),
            powered: Some(true),
            paired_phones: Some(vec!["AA:BB:CC:DD:EE:01".into()]),
            connected_phones: 1,
            registered_phones: 0,
            outputs: vec![dev("out1", false, true)],
            inputs: vec![dev("in1", false, true)],
            selection: Selection::default(),
            audio_server_ok: true,
        }
    }

    fn states(s: &Setup) -> Vec<StepState> {
        s.steps.iter().map(|x| x.state).collect()
    }

    #[test]
    fn everything_in_place_is_ready_and_testable() {
        let s = build_linux(&base());
        assert_eq!(s.verdict, Verdict::Ready);
        assert!(s.steps.iter().all(|x| x.state == StepState::Ok));
        assert!(s.can_test);
        assert_eq!(s.chosen_output.as_deref(), Some("out1"));
        assert_eq!(s.done_count(), 6);
        assert!(s.next_fix().is_none());
    }

    #[test]
    fn no_adapter_is_plain_and_waits_the_bluetooth_steps_but_not_the_headset() {
        let s = build_linux(&LinuxFacts {
            adapters: 0,
            rfkill: None,
            powered: None,
            paired_phones: None,
            ..base()
        });
        assert_eq!(s.verdict, Verdict::NoBluetooth);
        assert_eq!(
            states(&s),
            [
                StepState::Todo,
                StepState::Waiting,
                StepState::Waiting,
                StepState::Waiting,
                StepState::Waiting,
                StepState::Ok
            ]
        );
        assert_eq!(s.steps[0].fix.as_ref().unwrap().kind, FixKind::NoAdapter);
        assert!(
            s.can_test,
            "the headset test works before an adapter exists"
        );
    }

    #[test]
    fn soft_block_gets_the_unblock_command_and_hard_block_none() {
        let mut f = base();
        f.rfkill = Some(Rfkill {
            soft: true,
            hard: false,
        });
        let s = build_linux(&f);
        let fix = s.steps[1].fix.clone().unwrap();
        assert_eq!(fix.kind, FixKind::Unblock);
        assert_eq!(fix.command.as_deref(), Some("rfkill unblock bluetooth"));
        assert_eq!(s.steps[2].state, StepState::Waiting);
        f.rfkill = Some(Rfkill {
            soft: true,
            hard: true,
        });
        let fix = build_linux(&f).steps[1].fix.clone().unwrap();
        assert_eq!(fix.kind, FixKind::HardBlocked);
        assert_eq!(fix.command, None);
    }

    #[test]
    fn unpowered_and_daemon_down_are_told_apart() {
        let mut f = base();
        f.powered = Some(false);
        let fix = build_linux(&f).steps[2].fix.clone().unwrap();
        assert_eq!(fix.kind, FixKind::PowerOn);
        assert_eq!(fix.command.as_deref(), Some("bluetoothctl power on"));
        f.powered = None;
        let fix = build_linux(&f).steps[2].fix.clone().unwrap();
        assert_eq!(fix.kind, FixKind::StartBluetooth);
    }

    #[test]
    fn paired_but_not_connected_offers_the_connect_command_for_that_phone() {
        let mut f = base();
        f.connected_phones = 0;
        let s = build_linux(&f);
        assert_eq!(s.verdict, Verdict::Todo);
        assert_eq!(s.steps[3].state, StepState::Ok);
        let fix = s.steps[4].fix.clone().unwrap();
        assert_eq!(fix.kind, FixKind::Connect);
        assert_eq!(
            fix.command.as_deref(),
            Some("bluetoothctl connect AA:BB:CC:DD:EE:01")
        );
        assert_eq!(s.next_fix().unwrap().id, StepId::Hfp);
    }

    #[test]
    fn no_paired_phone_and_unreadable_pairing() {
        let mut f = base();
        f.paired_phones = Some(vec![]);
        f.connected_phones = 0;
        let s = build_linux(&f);
        assert_eq!(s.steps[3].fix.as_ref().unwrap().kind, FixKind::Pair);
        assert_eq!(s.steps[4].state, StepState::Waiting);
        f.paired_phones = None;
        assert_eq!(build_linux(&f).steps[3].state, StepState::Unknown);
    }

    #[test]
    fn a_registered_phone_counts_as_paired_and_connected() {
        let mut f = base();
        f.paired_phones = None;
        f.connected_phones = 0;
        f.registered_phones = 1;
        assert_eq!(build_linux(&f).verdict, Verdict::Ready);
    }

    #[test]
    fn missing_devices_ask_to_choose_and_the_audio_server_down_is_unknown() {
        let mut f = base();
        f.inputs.clear();
        let s = build_linux(&f);
        assert_eq!(
            s.steps[5].fix.as_ref().unwrap().kind,
            FixKind::ChooseDevices
        );
        assert!(s.can_test, "the tone alone can still be tested");
        assert_eq!(s.verdict, Verdict::Todo);
        f.audio_server_ok = false;
        let s = build_linux(&f);
        assert_eq!(s.steps[5].state, StepState::Unknown);
        assert!(!s.can_test);
    }

    #[test]
    fn saved_choice_wins_until_the_device_is_gone() {
        let devs = vec![
            dev("a", false, true),
            dev("b", false, false),
            dev("bt", true, false),
        ];
        assert_eq!(choose(&devs, Some("b")).as_deref(), Some("b"));
        assert_eq!(choose(&devs, Some("gone")).as_deref(), Some("a"));
        assert_eq!(choose(&devs, None).as_deref(), Some("a"));
        let only_bt = vec![dev("bt", true, false), dev("x", false, false)];
        assert_eq!(choose(&only_bt, None).as_deref(), Some("x"));
        assert_eq!(choose(&[], None), None);
    }

    #[test]
    fn selection_file_survives_garbage() {
        let s = Selection {
            output: Some("alsa_output.x".into()),
            input: None,
        };
        assert_eq!(Selection::parse(&s.to_json()), s);
        assert_eq!(Selection::parse("not json"), Selection::default());
        assert_eq!(
            Selection::parse(r#"{"output":"-rf","input":"a\nb"}"#),
            Selection::default(),
            "option-looking and multi-line names are dropped"
        );
    }

    #[test]
    fn bluetoothctl_and_rfkill_text() {
        assert_eq!(
            parse_powered(
                "Controller AA:BB (public)\n\tName: x\n\tPowered: yes\n\tPairable: yes\n"
            ),
            Some(true)
        );
        assert_eq!(parse_powered("\tPowered: no\n"), Some(false));
        assert_eq!(parse_powered("No default controller available\n"), None);
        assert!(parse_connected("Device AA\n\tConnected: yes\n"));
        assert!(!parse_connected("Device AA\n\tConnected: no\n"));
        assert_eq!(
            parse_rfkill("bluetooth\n", "1\n", "0\n"),
            Some(Rfkill {
                soft: true,
                hard: false
            })
        );
        assert_eq!(parse_rfkill("wlan\n", "1\n", "1\n"), None);
    }

    const SINKS: &str = r#"[
      {"index":1,"name":"alsa_output.usb-Headset.analog-stereo","description":"Headset Analog Stereo",
       "monitor_source":"alsa_output.usb-Headset.analog-stereo.monitor","properties":{"device.bus":"usb"}},
      {"index":2,"name":"bluez_output.AA_BB.1","description":"Phone","properties":{"device.api":"bluez5"}}
    ]"#;
    const SOURCES: &str = r#"[
      {"index":1,"name":"alsa_output.usb-Headset.analog-stereo.monitor","description":"Monitor of Headset","monitor_of_sink":1},
      {"index":2,"name":"alsa_input.usb-Headset.mono-fallback","description":"Headset Mono","monitor_of_sink":null}
    ]"#;

    #[test]
    fn pactl_json_lists_real_devices_only() {
        let sinks =
            parse_pactl_devices(SINKS, Some("alsa_output.usb-Headset.analog-stereo"), false);
        assert_eq!(sinks.len(), 2);
        assert!(sinks[0].default && !sinks[0].bluetooth);
        assert_eq!(sinks[0].label, "Headset Analog Stereo");
        assert!(sinks[1].bluetooth && !sinks[1].default);
        let src = parse_pactl_devices(SOURCES, None, true);
        assert_eq!(src.len(), 1, "an output's monitor is not a microphone");
        assert_eq!(src[0].id, "alsa_input.usb-Headset.mono-fallback");
        assert!(parse_pactl_devices("garbage", None, false).is_empty());
        assert!(parse_pactl_devices("{}", None, false).is_empty());
    }

    const BT: &str = r#"[
      {"Status":"OK","FriendlyName":"Intel(R) Wireless Bluetooth(R)","InstanceId":"USB\\VID_8087&PID_0033\\5&1"},
      {"Status":"OK","FriendlyName":"Microsoft Bluetooth Enumerator","InstanceId":"BTH\\MS_BTHBRB\\6&2"},
      {"Status":"OK","FriendlyName":"Pixel 9 Hands-Free AG","InstanceId":"BTHENUM\\{0000111f-0000-1000-8000-00805f9b34fb}_VID&0001_PID&0002\\7&3"},
      {"Status":"OK","FriendlyName":"Pixel 9","InstanceId":"BTHENUM\\DEV_AABBCCDDEEFF\\7&4"}
    ]"#;
    const ENDPOINTS: &str = r#"[
      {"Status":"OK","FriendlyName":"Speakers (Realtek Audio)","InstanceId":"SWD\\MMDEVAPI\\{0.0.0.00000000}.{aaa}"},
      {"Status":"OK","FriendlyName":"Microphone (Realtek Audio)","InstanceId":"SWD\\MMDEVAPI\\{0.0.1.00000000}.{bbb}"},
      {"Status":"Unknown","FriendlyName":"Old headset","InstanceId":"SWD\\MMDEVAPI\\{0.0.0.00000000}.{ccc}"}
    ]"#;

    #[test]
    fn pnp_json_accepts_one_object_many_or_none_and_a_bom() {
        assert_eq!(parse_pnp_json("").len(), 0);
        assert_eq!(parse_pnp_json("\u{feff}  ").len(), 0);
        assert_eq!(parse_pnp_json("nonsense").len(), 0);
        let one = r#"{"Status":"OK","FriendlyName":"x","InstanceId":"USB\\a"}"#;
        assert_eq!(parse_pnp_json(one).len(), 1);
        assert_eq!(parse_pnp_json(&format!("\u{feff}{one}")).len(), 1);
        assert_eq!(parse_pnp_json(BT).len(), 4);
        // A row without an instance id is useless and skipped.
        assert_eq!(parse_pnp_json(r#"[{"Status":"OK"}]"#).len(), 0);
    }

    #[test]
    fn windows_ready_when_the_phone_hands_free_node_is_ok() {
        let bt = parse_pnp_json(BT);
        let s = build_windows(Some(&bt), &parse_pnp_json(ENDPOINTS));
        assert_eq!(s.verdict, Verdict::Ready);
        assert_eq!(s.outputs.len(), 1, "only devices that are OK are listed");
        assert_eq!(s.inputs.len(), 1);
        assert!(!s.can_test, "Windows owns routing; no local test");
        assert_eq!(s.steps.last().unwrap().state, StepState::Unknown);
        assert_eq!(
            s.steps.last().unwrap().fix.as_ref().unwrap().kind,
            FixKind::WindowsSound
        );
    }

    #[test]
    fn windows_paired_but_phone_calls_off_and_no_adapter_and_unreadable() {
        let mut bt = parse_pnp_json(BT);
        bt[2].status = "Error".into();
        let s = build_windows(Some(&bt), &[]);
        assert_eq!(s.verdict, Verdict::Todo);
        assert_eq!(
            s.steps[2].fix.as_ref().unwrap().kind,
            FixKind::WindowsPhoneCalls
        );
        let none = build_windows(Some(&[]), &[]);
        assert_eq!(none.verdict, Verdict::NoBluetooth);
        assert_eq!(none.steps[0].fix.as_ref().unwrap().kind, FixKind::NoAdapter);
        let nopair: Vec<_> = parse_pnp_json(BT).into_iter().take(2).collect();
        assert_eq!(
            build_windows(Some(&nopair), &[]).steps[1]
                .fix
                .as_ref()
                .unwrap()
                .kind,
            FixKind::WindowsPair
        );
        let unread = build_windows(None, &[]);
        assert!(unread.steps.iter().all(|s| s.state == StepState::Unknown));
    }

    #[test]
    fn mac_is_never_green_and_has_no_steps() {
        let m = mac_fallback();
        assert_eq!(m.verdict, Verdict::MacFallback);
        assert!(m.steps.is_empty() && !m.can_test);
        assert_eq!(unsupported("freebsd").verdict, Verdict::Unsupported);
    }

    #[test]
    fn tone_is_a_valid_short_wav() {
        let w = tone_wav();
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        let data = u32::from_le_bytes(w[40..44].try_into().unwrap()) as usize;
        assert_eq!(w.len(), 44 + data);
        let secs = data as f32 / 2.0 / TEST_RATE as f32;
        assert!((1.0..1.5).contains(&secs), "{secs}");
        // Gentle: peak well below full scale, so no one is startled through a headset.
        assert!(level_of(&w[44..]) < 60);
        assert!(level_of(&w[44..]) > 20, "but clearly audible");
    }

    #[test]
    fn meter_levels() {
        assert_eq!(level_of(&[]), 0);
        assert_eq!(level_of(&[0; 64]), 0);
        let loud: Vec<u8> = i16::MAX.to_le_bytes().to_vec();
        assert_eq!(level_of(&loud), 100);
        let speech = 1000i16.to_le_bytes(); // about -30 dBFS
        let l = level_of(&speech);
        assert!((10..30).contains(&l), "{l}");
        assert!(l >= HEARD_THRESHOLD);
        let hiss = 20i16.to_le_bytes();
        assert!(
            level_of(&hiss) < HEARD_THRESHOLD,
            "room noise does not count as heard"
        );
        assert_eq!(slice_bytes(), 3200);
    }

    #[test]
    fn progress_reports_the_peak() {
        let mut t = TestProgress::default();
        assert!(!t.heard());
        t.levels = vec![1, 2, 30, 4];
        assert_eq!(t.peak(), 30);
        assert!(t.heard());
    }

    #[test]
    fn every_fix_and_step_has_a_catalogue_line() {
        let en = include_str!("../../bridge-i18n/i18n/en.properties");
        let has = |k: &str| en.lines().any(|l| l.starts_with(&format!("{k} =")));
        for f in FixKind::ALL {
            assert!(has(f.key()), "{}", f.key());
        }
        for s in [
            StepId::Adapter,
            StepId::Unblocked,
            StepId::Powered,
            StepId::Paired,
            StepId::Hfp,
            StepId::Headset,
        ] {
            assert!(has(s.key()), "{}", s.key());
        }
    }

    #[test]
    fn no_command_runs_sudo_except_the_one_the_user_must_copy() {
        // The only fix that mentions sudo is the daemon start, and it is text to copy, never run.
        let mut f = base();
        f.powered = None;
        let s = build_linux(&f);
        let sudo: Vec<_> = s
            .steps
            .iter()
            .filter_map(|s| s.fix.as_ref()?.command.as_deref())
            .filter(|c| c.contains("sudo"))
            .collect();
        assert_eq!(sudo, ["sudo systemctl enable --now bluetooth.service"]);
    }
}
