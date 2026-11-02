// SPDX-License-Identifier: GPL-3.0-or-later
//! The modem abstraction behind the simulated phone, plus an in-memory fake and an `adb`-driven
//! Android-emulator implementation. Portable: nothing here is platform specific.
//!
//! Two roles are separate on purpose:
//! * [`Modem`] is the *phone's* view: what the simulated handset's call stack is doing and the
//!   local actions (answer / hang up / dial) the Linux side may ask for.
//! * [`RemoteParty`] is the *network's* view: someone else calls the phone, accepts the phone's
//!   outgoing call, or hangs up. On the Android emulator these are `adb emu gsm ...` console
//!   commands.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Call state of the (single-call) modem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModemState {
    Idle,
    Ringing { number: String },
    Dialing { number: String },
    Active { number: String },
}

impl ModemState {
    /// The remote number (possibly empty if the platform did not report one).
    pub fn number(&self) -> Option<&str> {
        match self {
            ModemState::Idle => None,
            ModemState::Ringing { number }
            | ModemState::Dialing { number }
            | ModemState::Active { number } => Some(number),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModemError {
    /// `adb` could not be run, timed out, or reported a failure.
    #[error("adb: {0}")]
    Adb(String),
    /// Output from the platform could not be understood.
    #[error("parse: {0}")]
    Parse(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("invalid number: {0}")]
    InvalidNumber(String),
    /// The action is not valid in the modem's current state.
    #[error("invalid state: {0}")]
    State(String),
}

/// The phone side of a call.
pub trait Modem: Send + Sync {
    fn state(&self) -> Result<ModemState, ModemError>;
    /// Answer a ringing call.
    fn answer(&self) -> Result<(), ModemError>;
    /// End the current call (reject while ringing, cancel while dialing, hang up while active).
    fn hangup(&self) -> Result<(), ModemError>;
    /// Start an outgoing call.
    fn dial(&self, number: &str) -> Result<(), ModemError>;
}

/// The far end of a call (the cellular network side).
pub trait RemoteParty: Send + Sync {
    /// `number` calls the phone.
    fn ring(&self, number: &str) -> Result<(), ModemError>;
    /// The remote party picks up the phone's outgoing call to `number`.
    fn remote_accept(&self, number: &str) -> Result<(), ModemError>;
    /// The remote party hangs up (or cancels) its call with `number`.
    fn remote_hangup(&self, number: &str) -> Result<(), ModemError>;
}

/// Maximum accepted length of a phone number.
pub const MAX_NUMBER_LEN: usize = 32;

/// Digits and `+ * #` only, 1..=32 characters. This is the only gate between caller-supplied
/// text and the `adb` command line (and, for `adb shell`, the device shell), so it is strict.
pub fn validate_number(n: &str) -> Result<(), ModemError> {
    if n.is_empty() || n.len() > MAX_NUMBER_LEN {
        return Err(ModemError::InvalidNumber(format!(
            "must be 1-{MAX_NUMBER_LEN} characters, got {}",
            n.len()
        )));
    }
    if !n
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'+' | b'*' | b'#'))
    {
        return Err(ModemError::InvalidNumber(
            "only digits and + * # are allowed".into(),
        ));
    }
    Ok(())
}

/// Replace every run of 5+ number-like characters (`0-9 + * #`) with `<number>` so error text
/// and logs never carry phone numbers.
pub fn redact_numbers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 5 {
            out.push_str("<number>");
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in s.chars() {
        if c.is_ascii_digit() || matches!(c, '+' | '*' | '#') {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------------ FakeModem

/// In-memory single-call state machine implementing both [`Modem`] and [`RemoteParty`]. Cloning
/// shares the state, so a test can hold one handle while the simulator holds another.
///
/// Transitions: `ring` Idle->Ringing; `answer` Ringing->Active; `hangup` Ringing|Dialing|Active
/// ->Idle; `dial` Idle->Dialing; `remote_accept` Dialing->Active; `remote_hangup`
/// Ringing|Dialing|Active->Idle. Anything else is [`ModemError::State`]. `remote_*` calls must
/// name the number of the current call.
#[derive(Clone, Default)]
pub struct FakeModem {
    inner: Arc<Mutex<FakeInner>>,
}

#[derive(Default)]
struct FakeInner {
    state: Option<ModemState>,
    actions: Vec<&'static str>,
}

impl FakeInner {
    fn state(&self) -> ModemState {
        self.state.clone().unwrap_or(ModemState::Idle)
    }
}

impl FakeModem {
    pub fn new() -> Self {
        Self::default()
    }

    /// Names of the actions applied so far (numbers are deliberately not recorded).
    pub fn actions(&self) -> Vec<&'static str> {
        lock(&self.inner).actions.clone()
    }

    fn apply(
        &self,
        action: &'static str,
        f: impl FnOnce(ModemState) -> Result<ModemState, ModemError>,
    ) -> Result<(), ModemError> {
        let mut g = lock(&self.inner);
        let next = f(g.state())?;
        g.state = Some(next);
        g.actions.push(action);
        Ok(())
    }
}

fn wrong(action: &str, from: &ModemState) -> ModemError {
    let from = match from {
        ModemState::Idle => "idle",
        ModemState::Ringing { .. } => "ringing",
        ModemState::Dialing { .. } => "dialing",
        ModemState::Active { .. } => "active",
    };
    ModemError::State(format!("cannot {action} while {from}"))
}

fn mismatch(action: &str) -> ModemError {
    ModemError::State(format!("{action}: number does not match the current call"))
}

impl Modem for FakeModem {
    fn state(&self) -> Result<ModemState, ModemError> {
        Ok(lock(&self.inner).state())
    }

    fn answer(&self) -> Result<(), ModemError> {
        self.apply("answer", |s| match s {
            ModemState::Ringing { number } => Ok(ModemState::Active { number }),
            other => Err(wrong("answer", &other)),
        })
    }

    fn hangup(&self) -> Result<(), ModemError> {
        self.apply("hangup", |s| match s {
            ModemState::Idle => Err(wrong("hang up", &s)),
            _ => Ok(ModemState::Idle),
        })
    }

    fn dial(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        self.apply("dial", |s| match s {
            ModemState::Idle => Ok(ModemState::Dialing {
                number: number.to_string(),
            }),
            other => Err(wrong("dial", &other)),
        })
    }
}

impl RemoteParty for FakeModem {
    fn ring(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        self.apply("ring", |s| match s {
            ModemState::Idle => Ok(ModemState::Ringing {
                number: number.to_string(),
            }),
            other => Err(wrong("ring", &other)),
        })
    }

    fn remote_accept(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        self.apply("remote_accept", |s| match s {
            ModemState::Dialing { number: n } if n == number => {
                Ok(ModemState::Active { number: n })
            }
            ModemState::Dialing { .. } => Err(mismatch("remote_accept")),
            other => Err(wrong("remote_accept", &other)),
        })
    }

    fn remote_hangup(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        self.apply("remote_hangup", |s| match s {
            ModemState::Idle => Err(wrong("remote_hangup", &s)),
            other if other.number() == Some(number) => Ok(ModemState::Idle),
            _ => Err(mismatch("remote_hangup")),
        })
    }
}

// ------------------------------------------------------------------------------ registry parsing

/// Value text following `key=` in `line`, where `key` must start at a word boundary (so
/// `mCallState` does not match `mRingingCallState`). Bracketed lists are returned whole.
fn values_of<'a>(line: &'a str, key: &str) -> Option<Vec<&'a str>> {
    let needle = format!("{key}=");
    let mut from = 0;
    while let Some(i) = line[from..].find(&needle) {
        let at = from + i;
        let boundary = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if boundary {
            let rest = &line[at + needle.len()..];
            return Some(if let Some(list) = rest.strip_prefix('[') {
                let end = list.find(']').unwrap_or(list.len());
                list[..end].split(',').map(str::trim).collect()
            } else {
                let end = rest
                    .find(|c: char| c.is_whitespace() || c == ',' || c == '}')
                    .unwrap_or(rest.len());
                vec![&rest[..end]]
            });
        }
        from = at + needle.len();
    }
    None
}

/// One phone's (subscription's) call fields from `telephony.registry`.
#[derive(Default)]
struct RegistryEntry {
    /// `mCallState`: 0 idle, 1 ringing, 2 offhook.
    call_state: u8,
    /// Precise call states (`PreciseCallState` constants: 1 active, 2 holding, 3 dialing,
    /// 4 alerting, 5 incoming, 6 waiting, 7 disconnected, 8 disconnecting, 0 none, -1 invalid).
    ringing: Option<i32>,
    foreground: Option<i32>,
    background: Option<i32>,
    number: Option<String>,
}

/// The integer right after `label` (for example `Foreground call state:`) in `line`.
fn int_after(line: &str, label: &str) -> Option<i32> {
    let rest = line[line.find(label)? + label.len()..].trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

impl RegistryEntry {
    fn state(&self) -> ModemState {
        let number = self.number.clone().unwrap_or_default();
        let (ring, fg, bg) = (
            self.ringing.unwrap_or(0),
            self.foreground.unwrap_or(0),
            self.background.unwrap_or(0),
        );
        let precise = self.foreground.is_some();
        match self.call_state {
            0 => ModemState::Idle,
            // Ringing, unless the ringing slot is being torn down.
            1 if precise && ring == 8 => ModemState::Idle,
            // A waiting call (another call is up): single-call modem, so show the call in
            // progress, never the waiting one.
            1 if precise && (ring == 6 || matches!(fg, 1 | 3 | 4) || bg == 2) => {
                foreground_state(fg, bg, String::new())
            }
            1 => ModemState::Ringing { number },
            // Offhook without precise fields cannot be told apart: report it as active.
            _ if !precise => ModemState::Active { number },
            // Answer in flight: still the incoming call until the foreground call is active.
            _ if ring == 5 && fg <= 0 => ModemState::Ringing { number },
            _ => foreground_state(fg, bg, number),
        }
    }
}

impl RegistryEntry {
    /// Offhook, incoming call accepted, foreground call not yet active.
    fn answer_in_flight(&self) -> bool {
        self.call_state == 2
            && self.foreground.is_some()
            && self.ringing == Some(5)
            && self.foreground.unwrap_or(0) <= 0
    }
}

/// Offhook: map the foreground/background precise states. Alerting (remote is ringing) is
/// reported as `Dialing`, the nearest state a [`ModemState`] has.
fn foreground_state(fg: i32, bg: i32, number: String) -> ModemState {
    match fg {
        7 | 8 => ModemState::Idle,
        3 | 4 => ModemState::Dialing { number },
        1 => ModemState::Active { number },
        _ if bg == 2 => ModemState::Active { number },
        // Offhook with no foreground state yet: an outgoing call is being set up.
        _ => ModemState::Dialing { number },
    }
}

fn rank(s: &ModemState) -> u8 {
    match s {
        ModemState::Idle => 0,
        ModemState::Ringing { .. } => 1,
        ModemState::Dialing { .. } => 2,
        ModemState::Active { .. } => 3,
    }
}

/// Parse `dumpsys telephony.registry` (or its `grep`ped call lines).
///
/// Per phone/subscription this reads `mCallState` (0 idle, 1 ringing, 2 offhook), the precise
/// ringing/foreground/background states (`mPreciseCallState=Ringing call state: R, Foreground
/// call state: F, ...`, or the separate `m*CallState=` keys) and `mCallIncomingNumber`. Offhook
/// is split into dialing (`F` 3 or 4, or nothing yet) and active (`F` 1, or a held call) using
/// the precise fields; without them offhook is reported as `Active`. With several phones the
/// most advanced one wins (active, then dialing, then ringing): a second incoming call cannot be
/// represented by a single-call modem. `F` 7/8 (disconnecting) is idle. The number is only
/// reported by Android while ringing and is dropped (empty) unless it is a plausible number.
pub fn parse_registry_call_state(dumpsys_output: &str) -> Result<ModemState, ModemError> {
    parse_registry_detail(dumpsys_output).map(|(s, _)| s)
}

/// [`parse_registry_call_state`] plus whether an answer is in flight (the incoming call was
/// accepted but the foreground call is not active yet; reported as `Ringing`).
fn parse_registry_detail(dumpsys_output: &str) -> Result<(ModemState, bool), ModemError> {
    let mut entries: Vec<RegistryEntry> = Vec::new();
    let mut group = 0;
    for line in dumpsys_output.lines() {
        let line = line.trim();
        if let Some(vals) = values_of(line, "mCallState") {
            group = entries.len();
            for v in vals {
                let n: u8 = v
                    .parse()
                    .map_err(|_| ModemError::Parse(format!("bad mCallState value {v:?}")))?;
                if n > 2 {
                    return Err(ModemError::Parse(format!("mCallState out of range: {n}")));
                }
                entries.push(RegistryEntry {
                    call_state: n,
                    ..RegistryEntry::default()
                });
            }
        }
        let Some(last) = entries.last_mut() else {
            continue;
        };
        if line.contains("mPreciseCallState=") {
            last.ringing = int_after(line, "Ringing call state:");
            last.foreground = int_after(line, "Foreground call state:");
            last.background = int_after(line, "Background call state:");
        }
        for (key, slot) in [
            ("mRingingCallState", &mut last.ringing),
            ("mForegroundCallState", &mut last.foreground),
            ("mBackgroundCallState", &mut last.background),
        ] {
            if slot.is_none()
                && let Some(v) = values_of(line, key).and_then(|v| v.first()?.parse().ok())
            {
                *slot = Some(v);
            }
        }
        if let Some(vals) = values_of(line, "mCallIncomingNumber") {
            for (i, v) in vals.into_iter().enumerate() {
                if let Some(e) = entries.get_mut(group + i)
                    && e.number.is_none()
                {
                    e.number = Some(if validate_number(v).is_ok() {
                        v.to_string()
                    } else {
                        String::new()
                    });
                }
            }
        }
    }
    if entries.is_empty() {
        return Err(ModemError::Parse(
            "no mCallState= line in telephony.registry output".into(),
        ));
    }
    // Entries that carry precise fields are authoritative over bare repeats of mCallState.
    if entries.iter().any(|e| e.foreground.is_some()) {
        entries.retain(|e| e.foreground.is_some());
    }
    Ok(entries
        .iter()
        .map(|e| (e.state(), e.answer_in_flight()))
        .max_by_key(|(s, _)| rank(s))
        .unwrap_or((ModemState::Idle, false)))
}

// ------------------------------------------------------------------------------------ AdbModem

const ADB_TIMEOUT: Duration = Duration::from_secs(10);
const ADB_OUTPUT_CAP: usize = 4 << 20;

/// Seconds after `dial` during which an idle registry is not taken as "the call ended": Telecom
/// and the modem need about a second to show the new call.
const DIAL_GRACE: Duration = Duration::from_secs(10);

/// Host-side command that returns only the call lines of the registry dump (fast, small).
const REGISTRY_CMD: &str = "dumpsys telephony.registry | grep -E '^ +(mCallState|mRingingCallState|mForegroundCallState|mBackgroundCallState|mPreciseCallState|mCallIncomingNumber)='";

/// Drives an Android emulator (or device) with `adb`. Every command is an argv vector given to
/// [`Command`] (never a shell on the host); numbers are validated first and, because `adb shell`
/// hands one string to the device shell, quoted there as well.
///
/// Measured emulator behaviour this relies on is described in `docs/emulated-calls.md`:
/// the registry's precise call states tell dialing from active; the console cannot answer or hang
/// up a call the guest placed (the modem simulator answers outbound calls by itself after about
/// two seconds), so [`RemoteParty::remote_accept`] / [`RemoteParty::remote_hangup`] report
/// [`ModemError::Unsupported`] for outbound calls rather than pretending.
pub struct AdbModem {
    adb: PathBuf,
    serial: String,
    server_port: Option<u16>,
    /// The call we know about; the registry reports a number only while ringing.
    track: Mutex<Track>,
    timing: KeyTiming,
}

/// How hard [`AdbModem::answer`] / [`AdbModem::hangup`] try. Telecom silently drops a key sent
/// within about two seconds of a call appearing (measured), so a key is only trusted once the
/// registry shows the effect. The overall cap plus one 10 s adb timeout stays under the 30 s
/// D-Bus method timeout.
#[derive(Clone, Copy)]
struct KeyTiming {
    attempts: u32,
    poll: Duration,
    /// How long the first attempt waits for an effect; each retry waits `step` longer.
    first_window: Duration,
    step: Duration,
    overall: Duration,
}

impl Default for KeyTiming {
    fn default() -> Self {
        Self {
            attempts: 3,
            poll: Duration::from_millis(250),
            first_window: Duration::from_millis(2500),
            step: Duration::from_millis(1000),
            overall: Duration::from_secs(15),
        }
    }
}

#[derive(Default)]
struct Track {
    number: String,
    /// The guest placed the call (via [`Modem::dial`]).
    outbound: bool,
    /// Offhook has been observed since `dial`.
    seen_offhook: bool,
    dialed_at: Option<Instant>,
}

impl AdbModem {
    pub fn new(adb: PathBuf, serial: String, server_port: Option<u16>) -> Self {
        Self {
            adb,
            serial,
            server_port,
            track: Mutex::new(Track::default()),
            timing: KeyTiming::default(),
        }
    }

    /// Run `adb [-P port] -s serial <args...>` and return stdout. Kills the child after 10 s;
    /// keeps at most 4 MiB of output per stream.
    fn run(&self, args: &[&str]) -> Result<String, ModemError> {
        let mut cmd = Command::new(&self.adb);
        if let Some(p) = self.server_port {
            cmd.arg("-P").arg(p.to_string());
        }
        cmd.arg("-s").arg(&self.serial).args(args);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // ETXTBSY ("Text file busy") is transient when another thread has just written the
        // executable (a forked child briefly inherits that write handle); retry a few times.
        let mut attempts = 0;
        let mut child = loop {
            match cmd.spawn() {
                Ok(c) => break c,
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 5 => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(20 * attempts));
                }
                Err(e) => {
                    return Err(ModemError::Adb(format!(
                        "cannot run {}: {e}",
                        self.adb.display()
                    )));
                }
            }
        };
        fn capture(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = (&mut r).take(ADB_OUTPUT_CAP as u64).read_to_end(&mut buf);
                // Keep draining so the child never blocks on a full pipe.
                let _ = std::io::copy(&mut r, &mut std::io::sink());
                buf
            })
        }
        let out = capture(child.stdout.take().expect("piped stdout"));
        let err = capture(child.stderr.take().expect("piped stderr"));
        let deadline = Instant::now() + ADB_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ModemError::Adb(format!(
                        "adb {} timed out after {}s",
                        args.first().copied().unwrap_or(""),
                        ADB_TIMEOUT.as_secs()
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(ModemError::Adb(format!("waiting for adb: {e}"))),
            }
        };
        let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
        let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
        if !status.success() {
            let first = stderr
                .lines()
                .chain(stdout.lines())
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output");
            return Err(ModemError::Adb(format!(
                "adb {} failed ({status}): {}",
                args.first().copied().unwrap_or(""),
                redact_numbers(first.trim())
            )));
        }
        Ok(stdout)
    }

    /// `adb emu <console command>`: the console prints `OK` or `KO: reason` and adb exits 0
    /// either way. Returns the (redacted) `KO` reason, if any.
    fn emu(&self, words: &[&str]) -> Result<Option<String>, ModemError> {
        let mut args = vec!["emu"];
        args.extend_from_slice(words);
        let out = self.run(&args)?;
        Ok(out
            .lines()
            .find(|l| l.trim_start().starts_with("KO"))
            .map(|l| redact_numbers(l.trim())))
    }

    fn keyevent(&self, key: &str) -> Result<(), ModemError> {
        self.run(&["shell", "input", "keyevent", key]).map(|_| ())
    }
}

impl AdbModem {
    /// Press `key` and wait until the registry shows that `reacted` holds for the observed
    /// state (`(state, answer_in_flight)`), re-sending the key only while the state is still
    /// the one seen before the first press. Never returns `Ok` without an observed effect.
    fn press_until(
        &self,
        key: &str,
        what: &str,
        before: &ModemState,
        start: Instant,
        reacted: impl Fn(&ModemState, bool) -> Result<bool, ModemError>,
    ) -> Result<(), ModemError> {
        let t = self.timing;
        let out_of_time =
            || ModemError::Adb(format!("{what}: no effect within {}s", t.overall.as_secs()));
        let same = |a: &ModemState| std::mem::discriminant(a) == std::mem::discriminant(before);
        let mut window = t.first_window;
        for _attempt in 1..=t.attempts {
            if start.elapsed() >= t.overall {
                return Err(out_of_time());
            }
            self.keyevent(key)?;
            let attempt_end = Instant::now() + window;
            loop {
                std::thread::sleep(t.poll);
                if start.elapsed() >= t.overall {
                    return Err(out_of_time());
                }
                let (now, in_flight) = self.observe()?;
                if reacted(&now, in_flight)? {
                    return Ok(());
                }
                if !same(&now) {
                    // Something changed but not what we wanted: do not press again, just wait.
                    continue;
                }
                if Instant::now() >= attempt_end {
                    break;
                }
            }
            window += t.step;
        }
        Err(ModemError::Adb(format!(
            "{what}: no effect after {} attempts",
            t.attempts
        )))
    }

    /// One registry read, folded into the tracked call number. Returns the state and whether an
    /// answer is in flight.
    fn observe(&self) -> Result<(ModemState, bool), ModemError> {
        let dump = self.run(&["shell", REGISTRY_CMD])?;
        let (parsed, in_flight) = parse_registry_detail(&dump)?;
        let dialing = matches!(parsed, ModemState::Dialing { .. });
        let mut t = lock(&self.track);
        let state = match parsed {
            ModemState::Idle => {
                let dial_pending = t.outbound
                    && !t.seen_offhook
                    && t.dialed_at.is_some_and(|at| at.elapsed() < DIAL_GRACE);
                if !dial_pending {
                    *t = Track::default();
                }
                ModemState::Idle
            }
            ModemState::Ringing { number } => {
                if !number.is_empty() {
                    t.number = number;
                    t.outbound = false;
                }
                ModemState::Ringing {
                    number: t.number.clone(),
                }
            }
            ModemState::Dialing { number } | ModemState::Active { number } => {
                if !number.is_empty() {
                    t.number = number;
                }
                t.seen_offhook = true;
                let number = t.number.clone();
                if dialing {
                    ModemState::Dialing { number }
                } else {
                    ModemState::Active { number }
                }
            }
        };
        Ok((state, in_flight))
    }
}

impl Modem for AdbModem {
    fn state(&self) -> Result<ModemState, ModemError> {
        self.observe().map(|(s, _)| s)
    }

    /// Only valid while ringing (`KEYCODE_CALL` in any other state opens the dialer). Returns
    /// `Ok` once the registry shows the call accepted (answer in flight, or active).
    fn answer(&self) -> Result<(), ModemError> {
        let start = Instant::now();
        let (before, in_flight) = self.observe()?;
        if !matches!(before, ModemState::Ringing { .. }) || in_flight {
            return Err(ModemError::State(if in_flight {
                "the call is already being answered".into()
            } else {
                "cannot answer: no call is ringing".into()
            }));
        }
        self.press_until(
            "KEYCODE_CALL",
            "answer",
            &before,
            start,
            |now, in_flight| match now {
                ModemState::Active { .. } => Ok(true),
                ModemState::Ringing { .. } => Ok(in_flight),
                ModemState::Idle => Err(ModemError::State(
                    "the call ended before it could be answered".into(),
                )),
                ModemState::Dialing { .. } => Ok(false),
            },
        )
    }

    /// Reject, cancel or end the call; `Ok` once the registry is idle.
    fn hangup(&self) -> Result<(), ModemError> {
        let start = Instant::now();
        let (before, _) = self.observe()?;
        if before == ModemState::Idle {
            return Err(ModemError::State("cannot hang up: there is no call".into()));
        }
        self.press_until("KEYCODE_ENDCALL", "hangup", &before, start, |now, _| {
            Ok(*now == ModemState::Idle)
        })
    }

    fn dial(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        // `#` and `*` are percent-encoded for the URI; the whole URI is single-quoted for the
        // device shell (validated characters cannot contain a quote).
        let uri = number.replace('#', "%23").replace('*', "%2A");
        let cmd = format!("am start -a android.intent.action.CALL -d 'tel:{uri}'");
        let out = self.run(&["shell", &cmd])?;
        if let Some(bad) = out
            .lines()
            .find(|l| l.contains("Error") || l.contains("Exception"))
        {
            return Err(ModemError::Adb(format!(
                "am start CALL refused: {}",
                redact_numbers(bad.trim())
            )));
        }
        *lock(&self.track) = Track {
            number: number.to_string(),
            outbound: true,
            seen_offhook: false,
            dialed_at: Some(Instant::now()),
        };
        Ok(())
    }
}

impl RemoteParty for AdbModem {
    fn ring(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        match self.emu(&["gsm", "call", number])? {
            None => Ok(()),
            Some(ko) => Err(ModemError::Adb(format!(
                "emulator console refused gsm call: {ko}"
            ))),
        }
    }

    /// The emulator's modem answers outbound calls by itself and `gsm accept` is refused for
    /// them (measured), so this normally reports [`ModemError::Unsupported`].
    fn remote_accept(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        match self.emu(&["gsm", "accept", number])? {
            None => Ok(()),
            Some(ko) => Err(ModemError::Unsupported(format!(
                "the emulator console refused gsm accept ({ko}); its modem answers outbound \
                 calls by itself after about two seconds"
            ))),
        }
    }

    /// `gsm cancel` ends an inbound call. For a call the guest placed it prints `OK` and does
    /// nothing (measured), so that case is reported as unsupported instead of sending it.
    fn remote_hangup(&self, number: &str) -> Result<(), ModemError> {
        validate_number(number)?;
        if lock(&self.track).outbound {
            return Err(ModemError::Unsupported(
                "the emulator console cannot hang up a call the guest placed (gsm cancel is \
                 accepted but has no effect); end it locally with Modem::hangup"
                    .into(),
            ));
        }
        match self.emu(&["gsm", "cancel", number])? {
            None => Ok(()),
            Some(ko) => Err(ModemError::Adb(format!(
                "emulator console refused gsm cancel: {ko}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(s: &str) -> ModemState {
        ModemState::Ringing { number: s.into() }
    }

    fn act(s: &str) -> ModemState {
        ModemState::Active { number: s.into() }
    }

    #[test]
    fn validate_number_accepts_dialable() {
        for ok in ["1", "+15550100", "*123#", "0049 ".trim(), &"9".repeat(32)] {
            validate_number(ok).unwrap_or_else(|e| panic!("{ok:?}: {e}"));
        }
    }

    #[test]
    fn validate_number_rejects_everything_else() {
        let long = "1".repeat(33);
        for bad in [
            "",
            &long,
            "12 34",
            "1;2",
            "$(id)",
            "`id`",
            "1\n2",
            "a",
            "A",
            "1,2",
            "'1'",
            "1|2",
            "1&2",
            "-1",
            "+1\u{0663}",
            "1\0",
            "tel:1",
        ] {
            assert!(
                matches!(validate_number(bad), Err(ModemError::InvalidNumber(_))),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn redact_hides_numbers_only() {
        assert_eq!(
            redact_numbers("KO: no call from +15550100 (code 12)"),
            "KO: no call from <number> (code 12)"
        );
        assert_eq!(redact_numbers("exit 1234"), "exit 1234");
        assert_eq!(redact_numbers("tel:*12#"), "tel:*12#");
    }

    const IDLE: &str = "\
TELEPHONY REGISTRY (telephony.registry):
  mCallState=0
  mCallIncomingNumber=
  mServiceState=0
  mRingingCallState=0
  mForegroundCallState=0
";

    #[test]
    fn registry_idle() {
        assert_eq!(parse_registry_call_state(IDLE).unwrap(), ModemState::Idle);
    }

    #[test]
    fn registry_ringing_with_number() {
        let out = IDLE
            .replace("mCallState=0", "mCallState=1")
            .replace("mCallIncomingNumber=", "mCallIncomingNumber=+15550100");
        assert_eq!(parse_registry_call_state(&out).unwrap(), num("+15550100"));
    }

    #[test]
    fn registry_ringing_without_number() {
        let out = IDLE.replace("mCallState=0", "mCallState=1");
        assert_eq!(parse_registry_call_state(&out).unwrap(), num(""));
        let out = out.replace("mCallIncomingNumber=", "mCallIncomingNumber=null");
        assert_eq!(parse_registry_call_state(&out).unwrap(), num(""));
        let out = out.replace("mCallIncomingNumber=null", "mCallIncomingNumber=Unknown");
        assert_eq!(parse_registry_call_state(&out).unwrap(), num(""));
    }

    #[test]
    fn registry_offhook() {
        // Without the precise fields offhook cannot be told from dialing: reported as active.
        assert_eq!(
            parse_registry_call_state("  mCallState=2\n  mCallIncomingNumber=\n").unwrap(),
            ModemState::Active { number: "".into() }
        );
        // With them, a foreground state of 0 means an outgoing call is still being set up.
        let out = IDLE.replace("mCallState=0", "mCallState=2");
        assert_eq!(
            parse_registry_call_state(&out).unwrap(),
            ModemState::Dialing { number: "".into() }
        );
    }

    #[test]
    fn registry_ignores_lookalike_keys() {
        // mRingingCallState / mForegroundCallState must not be mistaken for mCallState.
        let out = "  mRingingCallState=1\n  mForegroundCallState=2\n  mCallState=0\n";
        assert_eq!(parse_registry_call_state(out).unwrap(), ModemState::Idle);
    }

    #[test]
    fn registry_multiple_subscriptions() {
        // One block per phone; the second SIM is ringing.
        let two = "\
Phone Id=0
  mCallState=0
  mCallIncomingNumber=
Phone Id=1
  mCallState=1
  mCallIncomingNumber=5550111
";
        assert_eq!(parse_registry_call_state(two).unwrap(), num("5550111"));
        // Offhook on one phone outranks ringing on the other; the number follows the offhook one.
        let both = "\
Phone Id=0
  mCallState=2
  mCallIncomingNumber=5550122
Phone Id=1
  mCallState=1
  mCallIncomingNumber=5550111
";
        assert_eq!(
            parse_registry_call_state(both).unwrap(),
            ModemState::Active {
                number: "5550122".into()
            }
        );
    }

    #[test]
    fn registry_bracketed_lists() {
        let out = "  mCallState=[0, 1]\n  mCallIncomingNumber=[, 5550111]\n";
        assert_eq!(parse_registry_call_state(out).unwrap(), num("5550111"));
        let out = "  mCallState=[0, 0]\n  mCallIncomingNumber=[, ]\n";
        assert_eq!(parse_registry_call_state(out).unwrap(), ModemState::Idle);
    }

    #[test]
    fn registry_same_line_keys() {
        let out = "Record{mCallState=1 mCallIncomingNumber=5550100}";
        assert_eq!(parse_registry_call_state(out).unwrap(), num("5550100"));
    }

    #[test]
    fn registry_errors() {
        assert!(matches!(
            parse_registry_call_state("Can't find service: telephony.registry"),
            Err(ModemError::Parse(_))
        ));
        assert!(matches!(
            parse_registry_call_state("mCallState=7"),
            Err(ModemError::Parse(_))
        ));
        assert!(matches!(
            parse_registry_call_state("mCallState=x"),
            Err(ModemError::Parse(_))
        ));
    }

    /// Registry lines as the emulator prints them (see docs/emulated-calls.md).
    fn reg(call_state: u8, ring: i32, fg: i32, bg: i32, number: &str) -> String {
        format!(
            "  Phone Id=0\n    mCallState={call_state}\n    mRingingCallState={ring}\n    \
             mForegroundCallState={fg}\n    mBackgroundCallState={bg}\n    \
             mPreciseCallState=Ringing call state: {ring}, Foreground call state: {fg}, \
             Background call state: {bg}, Disconnect cause: -1, Precise disconnect cause: -1\n    \
             mCallIncomingNumber={number}\n"
        )
    }

    #[test]
    fn registry_precise_states_follow_the_measured_table() {
        let dial = |n: &str| ModemState::Dialing { number: n.into() };
        let n = "";
        // Idle, including the stale FG=7 that stays after every hangup.
        assert_eq!(
            parse_registry_call_state(&reg(0, 0, 0, 0, n)).unwrap(),
            ModemState::Idle
        );
        assert_eq!(
            parse_registry_call_state(&reg(0, 0, 7, 0, n)).unwrap(),
            ModemState::Idle
        );
        // Ringing (5 = incoming) with the caller number.
        assert_eq!(
            parse_registry_call_state(&reg(1, 5, 0, 0, "5559001")).unwrap(),
            num("5559001")
        );
        // Answer in flight stays "ringing", number already cleared by Android.
        assert_eq!(
            parse_registry_call_state(&reg(2, 5, 0, 0, n)).unwrap(),
            num("")
        );
        // Dial in flight (offhook, no foreground yet), dialing, alerting.
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 0, 0, n)).unwrap(),
            dial("")
        );
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 3, 0, n)).unwrap(),
            dial("")
        );
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 4, 0, n)).unwrap(),
            dial("")
        );
        // Active.
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 1, 0, n)).unwrap(),
            act("")
        );
        // Held call with nothing in the foreground still counts as a call.
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 0, 2, n)).unwrap(),
            act("")
        );
        // Hangup in progress is idle; so is a ringing slot being torn down.
        assert_eq!(
            parse_registry_call_state(&reg(2, 0, 8, 0, n)).unwrap(),
            ModemState::Idle
        );
        assert_eq!(
            parse_registry_call_state(&reg(1, 8, 0, 0, n)).unwrap(),
            ModemState::Idle
        );
        // A waiting call during an active call shows the active call, not the waiting one.
        assert_eq!(
            parse_registry_call_state(&reg(1, 6, 1, 0, "5559002")).unwrap(),
            act("")
        );
    }

    #[test]
    fn registry_precise_entries_beat_bare_repeats() {
        // A later listener copy of mCallState without precise fields must not turn dialing
        // into active.
        let out = format!("{}  mCallState=2\n", reg(2, 0, 3, 0, ""));
        assert!(matches!(
            parse_registry_call_state(&out).unwrap(),
            ModemState::Dialing { .. }
        ));
    }

    #[test]
    #[cfg(unix)]
    fn adb_modem_tracks_the_number_and_phase_across_polls() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-adb");
        let regfile = dir.path().join("registry.txt");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in *telephony.registry*) cat '{}';; *) echo OK;; esac\n",
                regfile.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let m = AdbModem::new(script, "emulator-5584".into(), None);
        let set = |text: String| std::fs::write(&regfile, text).unwrap();

        // Inbound: the number is only visible while ringing, and is remembered afterwards.
        set(reg(1, 5, 0, 0, "5559001"));
        assert_eq!(m.state().unwrap(), num("5559001"));
        set(reg(2, 5, 0, 0, ""));
        assert_eq!(m.state().unwrap(), num("5559001"));
        set(reg(2, 0, 1, 0, ""));
        assert_eq!(m.state().unwrap(), act("5559001"));
        // An inbound call can be cancelled from the console.
        m.remote_hangup("5559001").unwrap();
        set(reg(0, 0, 7, 0, ""));
        assert_eq!(m.state().unwrap(), ModemState::Idle);

        // Outbound: right after dial the registry is still idle; that is not "call ended".
        m.dial("5559201").unwrap();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
        set(reg(2, 0, 3, 0, ""));
        assert_eq!(
            m.state().unwrap(),
            ModemState::Dialing {
                number: "5559201".into()
            }
        );
        // The console cannot end a guest-placed call: honest Unsupported, nothing sent.
        assert!(matches!(
            m.remote_hangup("5559201"),
            Err(ModemError::Unsupported(_))
        ));
        set(reg(2, 0, 1, 0, ""));
        assert_eq!(m.state().unwrap(), act("5559201"));
        // Once offhook has been seen, idle really means the call is over.
        set(reg(0, 0, 7, 0, ""));
        assert_eq!(m.state().unwrap(), ModemState::Idle);
        assert!(m.remote_hangup("5559201").is_ok());
    }

    fn rp(m: &FakeModem) -> &dyn RemoteParty {
        m
    }

    #[test]
    fn fake_incoming_answer_hangup() {
        let m = FakeModem::new();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
        rp(&m).ring("5550100").unwrap();
        assert_eq!(m.state().unwrap(), num("5550100"));
        m.answer().unwrap();
        assert_eq!(
            m.state().unwrap(),
            ModemState::Active {
                number: "5550100".into()
            }
        );
        m.hangup().unwrap();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
        assert_eq!(m.actions(), ["ring", "answer", "hangup"]);
    }

    #[test]
    fn fake_reject_and_remote_hangup() {
        let m = FakeModem::new();
        rp(&m).ring("5550100").unwrap();
        m.hangup().unwrap();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
        rp(&m).ring("5550100").unwrap();
        rp(&m).remote_hangup("5550100").unwrap();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
    }

    #[test]
    fn fake_outgoing() {
        let m = FakeModem::new();
        m.dial("5550101").unwrap();
        assert_eq!(
            m.state().unwrap(),
            ModemState::Dialing {
                number: "5550101".into()
            }
        );
        assert!(matches!(
            rp(&m).remote_accept("5550999"),
            Err(ModemError::State(_))
        ));
        rp(&m).remote_accept("5550101").unwrap();
        assert_eq!(
            m.state().unwrap(),
            ModemState::Active {
                number: "5550101".into()
            }
        );
        rp(&m).remote_hangup("5550101").unwrap();
        assert_eq!(m.state().unwrap(), ModemState::Idle);
    }

    #[test]
    fn fake_wrong_state_errors() {
        let m = FakeModem::new();
        assert!(matches!(m.answer(), Err(ModemError::State(_))));
        assert!(matches!(m.hangup(), Err(ModemError::State(_))));
        assert!(matches!(
            rp(&m).remote_accept("5550100"),
            Err(ModemError::State(_))
        ));
        assert!(matches!(
            rp(&m).remote_hangup("5550100"),
            Err(ModemError::State(_))
        ));
        rp(&m).ring("5550100").unwrap();
        assert!(matches!(rp(&m).ring("5550100"), Err(ModemError::State(_))));
        assert!(matches!(m.dial("5550101"), Err(ModemError::State(_))));
        assert!(matches!(
            rp(&m).remote_accept("5550100"),
            Err(ModemError::State(_))
        ));
        m.answer().unwrap();
        assert!(matches!(m.answer(), Err(ModemError::State(_))));
        // A failed action leaves the state untouched and is not recorded.
        assert_eq!(m.actions(), ["ring", "answer"]);
    }

    #[test]
    fn fake_validates_numbers_and_shares_state_across_clones() {
        let m = FakeModem::new();
        assert!(matches!(
            m.dial("1;reboot"),
            Err(ModemError::InvalidNumber(_))
        ));
        assert!(matches!(rp(&m).ring(""), Err(ModemError::InvalidNumber(_))));
        let other = m.clone();
        other.dial("5550100").unwrap();
        assert!(matches!(m.state().unwrap(), ModemState::Dialing { .. }));
    }

    #[test]
    fn adb_modem_reports_missing_binary() {
        let m = AdbModem::new(
            PathBuf::from("/nonexistent/geekbridge-test-adb"),
            "emulator-5584".into(),
            None,
        );
        assert!(matches!(m.state(), Err(ModemError::Adb(_))));
        // Number validation happens before anything is spawned.
        assert!(matches!(
            m.dial("1; rm -rf /"),
            Err(ModemError::InvalidNumber(_))
        ));
        assert!(matches!(m.ring("$(id)"), Err(ModemError::InvalidNumber(_))));
    }

    #[test]
    #[cfg(unix)]
    fn adb_modem_builds_argv_without_a_shell() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("argv.log");
        let script = dir.path().join("fake-adb");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\nprintf -- '--\\n' >> '{}'\n\
                 case \"$*\" in *telephony.registry*) printf '  mCallState=1\\n  mCallIncomingNumber=5550100\\n';; esac\n\
                 case \"$*\" in *' 5550999'*) echo 'KO: busy';; *emu*) echo OK;; esac\n",
                log.display(),
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let m = AdbModem::new(script, "emulator-5584".into(), Some(16037));
        assert_eq!(m.state().unwrap(), num("5550100"));
        m.keyevent("KEYCODE_CALL").unwrap();
        m.keyevent("KEYCODE_ENDCALL").unwrap();
        m.ring("5550100").unwrap();
        m.remote_accept("5550100").unwrap();
        m.remote_hangup("5550100").unwrap();
        assert!(matches!(m.ring("5550999"), Err(ModemError::Adb(_))));
        assert!(matches!(
            m.remote_accept("5550999"),
            Err(ModemError::Unsupported(_))
        ));
        m.dial("*123#").unwrap();
        let logged = std::fs::read_to_string(&log).unwrap();
        let calls: Vec<Vec<&str>> = logged
            .split("--\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().collect())
            .collect();
        let pre = ["-P", "16037", "-s", "emulator-5584"];
        assert!(calls.iter().all(|c| c[..4] == pre), "{calls:?}");
        assert_eq!(calls[0][4..], ["shell", REGISTRY_CMD]);
        assert_eq!(
            calls[1][4..],
            ["shell", "input", "keyevent", "KEYCODE_CALL"]
        );
        assert_eq!(
            calls[2][4..],
            ["shell", "input", "keyevent", "KEYCODE_ENDCALL"]
        );
        assert_eq!(calls[3][4..], ["emu", "gsm", "call", "5550100"]);
        assert_eq!(calls[4][4..], ["emu", "gsm", "accept", "5550100"]);
        assert_eq!(calls[5][4..], ["emu", "gsm", "cancel", "5550100"]);
        assert_eq!(
            calls[8][4..],
            [
                "shell",
                "am start -a android.intent.action.CALL -d 'tel:%2A123%23'"
            ]
        );
    }
    /// A stub `adb` whose registry output switches from `before` to `after` once `needed` key
    /// events have been received (`needed` = 0 reacts immediately, a huge value never).
    #[cfg(unix)]
    struct KeyStub {
        dir: tempfile::TempDir,
        modem: AdbModem,
    }

    #[cfg(unix)]
    impl KeyStub {
        fn new(needed: u32, before: String, after: String) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let d = dir.path().display().to_string();
            std::fs::write(dir.path().join("before.txt"), before).unwrap();
            std::fs::write(dir.path().join("after.txt"), after).unwrap();
            let script = dir.path().join("fake-adb");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nD='{d}'\nn=$(cat \"$D/n\" 2>/dev/null || echo 0)\n\
                     case \"$*\" in\n\
                     *KEYCODE_*) echo $((n+1)) > \"$D/n\"; echo \"$*\" >> \"$D/keys\";;\n\
                     *telephony.registry*) if [ \"$n\" -ge {needed} ]; then cat \"$D/after.txt\"; else cat \"$D/before.txt\"; fi;;\n\
                     esac\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut modem = AdbModem::new(script, "emulator-5584".into(), None);
            modem.timing = KeyTiming {
                attempts: 3,
                poll: Duration::from_millis(5),
                first_window: Duration::from_millis(80),
                step: Duration::from_millis(20),
                overall: Duration::from_secs(5),
            };
            Self { dir, modem }
        }

        fn keys(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("keys"))
                .unwrap_or_default()
                .lines()
                .map(|l| l.rsplit(' ').next().unwrap().to_string())
                .collect()
        }
    }

    #[test]
    #[cfg(unix)]
    fn answer_resends_once_when_the_first_key_is_ignored() {
        // Telecom not ready: the first KEYCODE_CALL does nothing, the second works.
        let s = KeyStub::new(2, reg(1, 5, 0, 0, "5559001"), reg(2, 0, 1, 0, ""));
        s.modem.answer().unwrap();
        assert_eq!(s.keys(), ["KEYCODE_CALL", "KEYCODE_CALL"]);
        assert_eq!(s.modem.state().unwrap(), act("5559001"));
    }

    #[test]
    #[cfg(unix)]
    fn answer_sends_one_key_when_it_works_and_never_when_in_flight() {
        let s = KeyStub::new(1, reg(1, 5, 0, 0, "5559001"), reg(2, 0, 1, 0, ""));
        s.modem.answer().unwrap();
        assert_eq!(s.keys(), ["KEYCODE_CALL"]);
        // Accepted but not yet active (answer in flight) counts as taken effect: no resend.
        let s = KeyStub::new(1, reg(1, 5, 0, 0, "5559001"), reg(2, 5, 0, 0, ""));
        s.modem.answer().unwrap();
        assert_eq!(s.keys(), ["KEYCODE_CALL"]);
        // And answering while an answer is in flight sends nothing.
        assert!(matches!(s.modem.answer(), Err(ModemError::State(_))));
        assert_eq!(s.keys().len(), 1);
    }

    #[test]
    #[cfg(unix)]
    fn answer_that_never_takes_effect_is_an_error_after_three_attempts() {
        let s = KeyStub::new(99, reg(1, 5, 0, 0, "5559001"), reg(2, 0, 1, 0, ""));
        let err = s.modem.answer().unwrap_err();
        assert!(matches!(err, ModemError::Adb(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("no effect after 3 attempts"), "{msg}");
        assert!(!msg.contains("5559001"), "number leaked: {msg}");
        assert_eq!(s.keys(), ["KEYCODE_CALL"; 3]);
    }

    #[test]
    #[cfg(unix)]
    fn answer_requires_ringing_and_never_sends_call_otherwise() {
        for state in [
            reg(0, 0, 0, 0, ""),
            reg(2, 0, 1, 0, ""),
            reg(2, 0, 3, 0, ""),
        ] {
            let s = KeyStub::new(0, state.clone(), state);
            assert!(matches!(s.modem.answer(), Err(ModemError::State(_))));
            assert!(s.keys().is_empty(), "KEYCODE_CALL would open the dialer");
        }
    }

    #[test]
    #[cfg(unix)]
    fn answer_reports_a_caller_who_gave_up() {
        // The call disappears after the key: not an answer.
        let s = KeyStub::new(1, reg(1, 5, 0, 0, "5559001"), reg(0, 0, 7, 0, ""));
        assert!(matches!(s.modem.answer(), Err(ModemError::State(_))));
    }

    #[test]
    #[cfg(unix)]
    fn hangup_resends_only_while_unchanged_and_requires_a_call() {
        let s = KeyStub::new(2, reg(2, 0, 1, 0, ""), reg(0, 0, 7, 0, ""));
        s.modem.hangup().unwrap();
        assert_eq!(s.keys(), ["KEYCODE_ENDCALL", "KEYCODE_ENDCALL"]);

        // Reject while ringing.
        let s = KeyStub::new(1, reg(1, 5, 0, 0, "5559001"), reg(0, 0, 8, 0, ""));
        s.modem.hangup().unwrap();
        assert_eq!(s.keys(), ["KEYCODE_ENDCALL"]);

        let s = KeyStub::new(99, reg(2, 0, 1, 0, ""), reg(0, 0, 7, 0, ""));
        let err = s.modem.hangup().unwrap_err();
        assert!(
            err.to_string().contains("no effect after 3 attempts"),
            "{err}"
        );
        assert_eq!(s.keys().len(), 3);

        let s = KeyStub::new(0, reg(0, 0, 7, 0, ""), reg(0, 0, 7, 0, ""));
        assert!(matches!(s.modem.hangup(), Err(ModemError::State(_))));
        assert!(s.keys().is_empty());
    }

    #[test]
    fn adb_error_text_never_carries_numbers() {
        // Everything surfaced to D-Bus goes through redact_numbers.
        let e = ModemError::Adb("adb shell failed (exit status: 1): no such call 5559001".into());
        assert!(!redact_numbers(&e.to_string()).contains("5559001"));
        assert_eq!(
            redact_numbers("answer: no effect after 3 attempts"),
            "answer: no effect after 3 attempts"
        );
    }
}
