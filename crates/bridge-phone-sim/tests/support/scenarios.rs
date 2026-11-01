//! Desktop-side end-to-end scenario engine, shared by the `geekbridge-call-e2e` binary and the
//! integration tests (both `#[path]`-include this file).
//!
//! The *desktop* is the real production code: `bridge_calls::Session` over
//! `bridge_calls::pipewire::PipeWireBackend`. The *phone* is a `Modem + RemoteParty` pair (the
//! in-memory `FakeModem` or an Android emulator via `AdbModem`), mirrored onto D-Bus by the
//! simulator service. Only signalling is exercised; no Bluetooth, SCO audio or AT exchange exists
//! anywhere in this path, so nothing here verifies audio or hardware behaviour.
//!
//! Every wait is a poll against a deadline (`Harness::wait`); there are no fixed sleeps. Phone
//! numbers are never printed beyond their last two digits.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use bridge_calls::pipewire::{BusTarget, PipeWireBackend, PipeWireConfig};
use bridge_calls::{ControlError, Session, SessionEvent};
use bridge_core::calls::{
    AudioLinkState, CallCommand, CallDirection, CallEvent, CallId, CallInfo, CallPhase, CallState,
    CommandRejection, EndReason, EndedCall, LinkState, Sensitive,
};
use bridge_phone_sim::{Modem, ModemState, RemoteParty, Sim, SimConfig};

/// Default number used by every scenario. A fictional 555 number.
pub const DEFAULT_NUMBER: &str = "5551234";
/// Default per-step deadline.
pub const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(15);

/// A phone that can be driven from both sides.
pub trait Phone: Modem + RemoteParty {}
impl<T: Modem + RemoteParty + ?Sized> Phone for T {}

// ------------------------------------------------------------------ redaction

/// `***34`: only the last two characters of a number survive.
pub fn redact(number: &str) -> String {
    let tail: Vec<char> = number.chars().rev().take(2).collect();
    let tail: String = tail.into_iter().rev().collect();
    format!("***{tail}")
}

/// Replace every run of six or more digits (optionally `+`-prefixed) so a number can never leak
/// through an error message.
pub fn scrub(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let start = i;
        if chars[i] == '+' && chars.get(i + 1).is_some_and(char::is_ascii_digit) {
            i += 1;
        }
        let digits_start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i - digits_start >= 6 {
            let run: String = chars[digits_start..i].iter().collect();
            out.push_str(&redact(&run));
        } else if i > start {
            out.extend(&chars[start..i]);
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

pub fn describe_modem(state: &ModemState) -> String {
    match state {
        ModemState::Idle => "Idle".to_string(),
        ModemState::Ringing { number } => format!("Ringing({})", redact(number)),
        ModemState::Dialing { number } => format!("Dialing({})", redact(number)),
        ModemState::Active { number } => format!("Active({})", redact(number)),
    }
}

pub fn describe_call_state(state: &CallState) -> String {
    let calls: Vec<String> = state
        .calls
        .values()
        .map(|c| format!("{}:{:?}/{:?}", c.id.as_str(), c.direction, c.phase))
        .collect();
    let ended: Vec<String> = state
        .recent_ended
        .iter()
        .rev()
        .take(3)
        .map(|e| format!("{}:{:?}", e.id.as_str(), e.reason))
        .collect();
    format!(
        "link={:?} calls=[{}] audio={:?} ended(newest first)=[{}]",
        state.link,
        calls.join(","),
        state.audio,
        ended.join(",")
    )
}

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// Caller-ID comparison tolerant of country-code / formatting differences.
fn same_number(a: &str, b: &str) -> bool {
    let (a, b) = (digits(a), digits(b));
    !a.is_empty() && !b.is_empty() && (a.ends_with(&b) || b.ends_with(&a))
}

// ------------------------------------------------------------------ private bus

#[derive(Debug)]
pub enum BusStartError {
    /// `dbus-daemon` is not installed.
    Missing(String),
    Failed(String),
}

impl std::fmt::Display for BusStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(e) => write!(f, "dbus-daemon not available: {e}"),
            Self::Failed(e) => write!(f, "could not start a private dbus-daemon: {e}"),
        }
    }
}

static BUS_COUNTER: AtomicU32 = AtomicU32::new(0);

/// A private session-style `dbus-daemon`, killed (and its socket directory removed) on drop.
pub struct PrivateBus {
    child: Child,
    pub address: String,
    dir: std::path::PathBuf,
}

impl PrivateBus {
    pub fn start() -> Result<Self, BusStartError> {
        let dir = std::env::temp_dir().join(format!(
            "geekbridge-e2e-{}-{}",
            std::process::id(),
            BUS_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| BusStartError::Failed(e.to_string()))?;
        let socket = dir.join("bus");
        let conf = dir.join("bus.conf");
        let cleanup = |dir: &std::path::Path| {
            let _ = std::fs::remove_dir_all(dir);
        };
        if let Err(e) = std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                socket.display()
            ),
        ) {
            cleanup(&dir);
            return Err(BusStartError::Failed(e.to_string()));
        }
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
                cleanup(&dir);
                return Err(if e.kind() == std::io::ErrorKind::NotFound {
                    BusStartError::Missing(e.to_string())
                } else {
                    BusStartError::Failed(e.to_string())
                });
            }
        };
        let mut line = String::new();
        let read = child
            .stdout
            .take()
            .map(|out| BufReader::new(out).read_line(&mut line));
        let address = line.trim().to_string();
        if !matches!(read, Some(Ok(n)) if n > 0) || !address.starts_with("unix:") {
            let _ = child.kill();
            let _ = child.wait();
            cleanup(&dir);
            return Err(BusStartError::Failed(format!(
                "unexpected dbus-daemon address line {address:?}"
            )));
        }
        Ok(Self {
            child,
            address,
            dir,
        })
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ------------------------------------------------------------------ simulator in a thread

/// The simulator service running on its own thread. Dropping (or [`SimService::stop`]) ends the
/// loop and drops the `Sim`, which releases the bus name: the "service crashed" case.
pub struct SimService {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub unique_name: String,
}

impl SimService {
    pub fn start(
        bus_address: &str,
        modem: Arc<dyn Modem>,
        poll_interval: Duration,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let cfg = SimConfig {
            bus_address: Some(bus_address.to_string()),
            poll_interval,
            ..SimConfig::default()
        };
        let flag = stop.clone();
        let join = std::thread::Builder::new()
            .name("phone-sim".into())
            .spawn(move || match Sim::start(cfg, modem) {
                Ok(sim) => {
                    let _ = tx.send(Ok(sim.unique_name().to_string()));
                    sim.run_until(&flag);
                    drop(sim);
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                }
            })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(Ok(unique_name)) => Ok(Self {
                stop,
                join: Some(join),
                unique_name,
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(format!("simulator failed to start: {e}"))
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                Err("simulator did not start within 15 s".to_string())
            }
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for SimService {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A private bus with an in-process simulator on it, plus the phone driving it.
pub struct SelfContained {
    // Field order is drop order: the simulator goes before the bus it lives on.
    pub sim: Arc<Mutex<Option<SimService>>>,
    pub bus: PrivateBus,
    pub phone: Arc<dyn Phone>,
}

impl SelfContained {
    /// `modem` is what the simulator mirrors; `phone` drives the same modem from the remote end
    /// (for `FakeModem` both are clones of one shared object).
    pub fn start(modem: Arc<dyn Modem>, phone: Arc<dyn Phone>) -> Result<Self, String> {
        Self::start_with_interval(modem, phone, Duration::from_millis(25))
    }

    /// Like [`start`](Self::start) with an explicit simulator poll interval (the real binary's
    /// default is 300 ms).
    pub fn start_with_interval(
        modem: Arc<dyn Modem>,
        phone: Arc<dyn Phone>,
        poll_interval: Duration,
    ) -> Result<Self, String> {
        let bus = PrivateBus::start().map_err(|e| e.to_string())?;
        let sim = SimService::start(&bus.address, modem, poll_interval)?;
        Ok(Self {
            sim: Arc::new(Mutex::new(Some(sim))),
            bus,
            phone,
        })
    }

    pub fn harness(&self, opts: Options) -> Harness {
        self.try_harness(opts)
            .expect("connect the desktop to the private bus")
    }

    pub fn try_harness(&self, opts: Options) -> Result<Harness, String> {
        let sim = self.sim.clone();
        let mut h = Harness::connect(
            BusTarget::Address(self.bus.address.clone()),
            self.phone.clone(),
            opts,
        )?;
        h.service_killer = Some(Box::new(move || {
            // Dropping the SimService releases the name; take it out of the shared slot.
            let taken = sim.lock().unwrap_or_else(|e| e.into_inner()).take();
            drop(taken);
        }));
        Ok(h)
    }
}

// ------------------------------------------------------------------ reporting types

#[derive(Clone, Debug)]
pub struct StepRecord {
    pub scenario: &'static str,
    pub step: String,
    pub elapsed: Duration,
    /// `None` on PASS.
    pub error: Option<String>,
    /// Extra non-failing detail (for example which refusal a guard rail produced).
    pub note: Option<String>,
}

impl StepRecord {
    pub fn passed(&self) -> bool {
        self.error.is_none()
    }

    /// `S1 [   12 ms] PASS  remote rings` (FAIL lines carry the reason).
    pub fn line(&self) -> String {
        let verdict = if self.passed() { "PASS" } else { "FAIL" };
        let mut s = format!(
            "{} [{:>6} ms] {}  {}",
            self.scenario,
            self.elapsed.as_millis(),
            verdict,
            self.step
        );
        if let Some(n) = &self.note {
            s.push_str(&format!(" ({n})"));
        }
        if let Some(e) = &self.error {
            s.push_str(&format!(": {e}"));
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    Skip,
}

#[derive(Clone, Debug)]
pub struct ScenarioReport {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    pub skip_reason: Option<String>,
    pub steps: Vec<StepRecord>,
    pub elapsed: Duration,
}

impl ScenarioReport {
    pub fn failing_step(&self) -> Option<&StepRecord> {
        self.steps.iter().find(|s| !s.passed())
    }
    pub fn summary_line(&self) -> String {
        match self.status {
            Status::Pass => format!(
                "{} [{:>6} ms] PASS  scenario: {}",
                self.id,
                self.elapsed.as_millis(),
                self.title
            ),
            Status::Fail => format!(
                "{} [{:>6} ms] FAIL  scenario: {} (failing step: {})",
                self.id,
                self.elapsed.as_millis(),
                self.title,
                self.failing_step().map_or("?", |s| s.step.as_str())
            ),
            Status::Skip => format!(
                "{} [{:>6} ms] SKIP  scenario: {} ({})",
                self.id,
                0,
                self.title,
                self.skip_reason.as_deref().unwrap_or("skipped")
            ),
        }
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_opt(s: &Option<String>) -> String {
    s.as_deref().map_or("null".to_string(), json_str)
}

/// Machine-readable report. Hand-rolled so this file stays dependency-free.
pub fn render_json(reports: &[ScenarioReport]) -> String {
    let scenarios: Vec<String> = reports
        .iter()
        .map(|r| {
            let steps: Vec<String> = r
                .steps
                .iter()
                .map(|s| {
                    format!(
                        "{{\"step\":{},\"ok\":{},\"elapsed_ms\":{},\"note\":{},\"error\":{}}}",
                        json_str(&s.step),
                        s.passed(),
                        s.elapsed.as_millis(),
                        json_opt(&s.note),
                        json_opt(&s.error)
                    )
                })
                .collect();
            let status = match r.status {
                Status::Pass => "pass",
                Status::Fail => "fail",
                Status::Skip => "skip",
            };
            format!(
                "{{\"id\":{},\"title\":{},\"status\":\"{}\",\"elapsed_ms\":{},\"skip_reason\":{},\"steps\":[{}]}}",
                json_str(r.id),
                json_str(r.title),
                status,
                r.elapsed.as_millis(),
                json_opt(&r.skip_reason),
                steps.join(",")
            )
        })
        .collect();
    let ok = reports.iter().all(|r| r.status != Status::Fail);
    format!(
        "{{\"simulated_signalling_only\":true,\"passed\":{},\"scenarios\":[{}]}}",
        ok,
        scenarios.join(",")
    )
}

// ------------------------------------------------------------------ harness

/// What the phone behind the modem can and cannot do. Chosen from the mode, never probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// The modem answers a guest-placed call by itself after a short dialing window (the Android
    /// emulator does, after about 2 s), so a remote party cannot choose when it is answered.
    pub auto_answers_outbound: bool,
    /// `RemoteParty::remote_accept` works for a call the guest placed.
    pub remote_accept_outbound: bool,
    /// `RemoteParty::remote_hangup` works for a call the guest placed.
    pub remote_hangup_outbound: bool,
    /// A local answer/hang-up sent while a call is still ringing can be silently ignored (the
    /// emulator's Telecom needs up to ~3 s after the modem registers the ring). When set, the
    /// harness re-sends the desktop command if the modem has not reacted within this time.
    pub input_retry: Option<Duration>,
}

impl Caps {
    /// The in-memory `FakeModem`: a remote party fully controls outbound calls.
    pub const FAKE: Caps = Caps {
        auto_answers_outbound: false,
        remote_accept_outbound: true,
        remote_hangup_outbound: true,
        input_retry: None,
    };
    /// The Android emulator's modem simulator, as measured in
    /// `docs/architecture.md`.
    pub const EMULATOR: Caps = Caps {
        auto_answers_outbound: true,
        remote_accept_outbound: false,
        remote_hangup_outbound: false,
        input_retry: Some(Duration::from_millis(2500)),
    };
}

/// Number used with the emulator by default: distinct from `5551234`, which other tools driving
/// the same emulator also use.
pub const EMULATOR_NUMBER: &str = "5557701";

#[derive(Clone, Debug)]
pub struct Options {
    pub step_timeout: Duration,
    pub number: String,
    pub caps: Caps,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            step_timeout: DEFAULT_STEP_TIMEOUT,
            number: DEFAULT_NUMBER.to_string(),
            caps: Caps::FAKE,
        }
    }
}

/// The step already failed and was recorded; unwinds a scenario.
#[derive(Debug)]
pub struct Failed;

type Run = Result<(), Failed>;

/// A read-only view handed to wait conditions.
pub struct World<'a> {
    pub state: Option<&'a CallState>,
    pub device: Option<&'a bridge_calls::DeviceRef>,
    /// Events observed since the scenario began.
    pub events: &'a [SessionEvent],
    pub service: &'a bridge_calls::ServiceStatus,
    pub phone: &'a dyn Phone,
}

impl World<'_> {
    pub fn modem(&self) -> Result<ModemState, String> {
        self.phone
            .state()
            .map_err(|e| format!("modem state unreadable: {e}"))
    }

    pub fn event_seen(&self, pred: impl Fn(&CallEvent) -> bool) -> bool {
        self.events.iter().any(|e| pred(&e.event))
    }

    pub fn ended(&self, id: &CallId) -> Option<&EndedCall> {
        self.state?.recent_ended.iter().rev().find(|e| &e.id == id)
    }
}

pub struct Harness {
    session: Session,
    /// Where the simulator's bus is, so a second consumer (the service call hub) can join it.
    pub bus: BusTarget,
    pub phone: Arc<dyn Phone>,
    pub step_timeout: Duration,
    pub number: String,
    pub caps: Caps,
    events: Vec<SessionEvent>,
    /// When each entry of `events` was observed.
    event_times: Vec<Instant>,
    /// Conditions see only `events[mark..]`; a scenario moves it forward between attempts.
    mark: usize,
    dial_at: Option<Instant>,
    /// The reducer replayed over every observed event, kept per phone path. It survives a phone
    /// disappearing, which the session's own state does not.
    shadow: BTreeMap<String, CallState>,
    last_path: Option<String>,
    commands: u32,
    scenario: &'static str,
    records: Vec<StepRecord>,
    sink: Box<dyn FnMut(&StepRecord)>,
    /// Present only when the harness owns the simulator (the `--fake` mode).
    pub service_killer: Option<Box<dyn FnMut()>>,
}

fn modem_err(e: impl std::fmt::Display) -> String {
    format!("modem: {e}")
}

impl Harness {
    pub fn connect(bus: BusTarget, phone: Arc<dyn Phone>, opts: Options) -> Result<Self, String> {
        let backend = PipeWireBackend::connect(PipeWireConfig {
            bus: bus.clone(),
            method_timeout: Duration::from_secs(10),
        })
        .map_err(|e| e.to_string())?;
        Ok(Self {
            session: Session::new(Box::new(backend)),
            bus,
            phone,
            step_timeout: opts.step_timeout,
            number: opts.number,
            caps: opts.caps,
            events: Vec::new(),
            event_times: Vec::new(),
            mark: 0,
            dial_at: None,
            shadow: BTreeMap::new(),
            last_path: None,
            commands: 0,
            scenario: "",
            records: Vec::new(),
            sink: Box::new(|_| {}),
            service_killer: None,
        })
    }

    /// Called with each step as soon as it completes (for live output).
    pub fn set_sink(&mut self, sink: impl FnMut(&StepRecord) + 'static) {
        self.sink = Box::new(sink);
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    fn absorb(&mut self, events: Vec<SessionEvent>) {
        if let Some((d, _, _)) = self.session.phones().next() {
            self.last_path = Some(d.path.clone());
        }
        for e in events {
            self.shadow
                .entry(e.device.path.clone())
                .or_default()
                .apply(e.event.clone());
            self.events.push(e);
            self.event_times.push(Instant::now());
        }
    }

    /// When the first event after `mark` matching `pred` was observed.
    fn event_time(&self, pred: impl Fn(&CallEvent) -> bool) -> Option<Instant> {
        self.events[self.mark..]
            .iter()
            .zip(&self.event_times[self.mark..])
            .find(|(e, _)| pred(&e.event))
            .map(|(_, t)| *t)
    }

    fn saw_phase(&self, id: &CallId, phase: CallPhase) -> bool {
        self.events[self.mark..].iter().any(|e| {
            matches!(&e.event, CallEvent::CallUpdated { id: eid, phase: p, .. } if eid == id && *p == phase)
        })
    }

    /// `Dial -> Dialing 1130 ms, Dialing -> Active 1900 ms` from observation times.
    fn timing(&self, id: &CallId) -> String {
        let at = |phase: CallPhase| {
            self.event_time(|e| {
                matches!(e, CallEvent::CallUpdated { id: eid, phase: p, .. } if eid == id && *p == phase)
            })
        };
        let ms = |a: Option<Instant>, b: Option<Instant>| {
            Some(b?.saturating_duration_since(a?).as_millis())
        };
        let dialing = at(CallPhase::Dialing).or_else(|| at(CallPhase::Alerting));
        let mut parts = Vec::new();
        if let Some(v) = ms(self.dial_at, dialing) {
            parts.push(format!("Dial sent -> session saw Dialing: {v} ms"));
        }
        if let Some(v) = ms(dialing, at(CallPhase::Active)) {
            parts.push(format!("Dialing -> Active: {v} ms"));
        }
        parts.join(", ")
    }

    pub fn refresh(&mut self) -> Result<(), String> {
        let evs = self
            .session
            .refresh()
            .map_err(|e| format!("session refresh: {e}"))?;
        self.absorb(evs);
        Ok(())
    }

    fn pump(&mut self, timeout: Duration) -> Result<(), String> {
        let evs = self
            .session
            .pump(timeout)
            .map_err(|e| format!("session pump: {e}"))?;
        self.absorb(evs);
        Ok(())
    }

    fn current_device(&self) -> Option<bridge_calls::DeviceRef> {
        self.session.phones().next().map(|(d, _, _)| d.clone())
    }

    fn world(&self) -> World<'_> {
        let device = self.session.phones().next().map(|(d, _, _)| d);
        let live = self.session.phones().next().map(|(_, s, _)| s);
        let state = live.or_else(|| self.last_path.as_ref().and_then(|p| self.shadow.get(p)));
        World {
            state,
            device,
            events: &self.events[self.mark..],
            service: self.session.service(),
            phone: self.phone.as_ref(),
        }
    }

    fn describe(&self) -> String {
        let w = self.world();
        let session = w.state.map_or("no phone".to_string(), describe_call_state);
        let modem = match w.modem() {
            Ok(s) => describe_modem(&s),
            Err(e) => e,
        };
        format!("session: {session}; modem: {modem}")
    }

    /// Poll until `cond` yields a value, an error, or the step deadline passes. The session is
    /// refreshed before the first check and pumped (bounded to 100 ms per slice, so modem-only
    /// changes are seen too) between checks.
    pub fn wait<T>(
        &mut self,
        what: &str,
        mut cond: impl FnMut(&World) -> Result<Option<T>, String>,
    ) -> Result<T, String> {
        let started = Instant::now();
        let deadline = started + self.step_timeout;
        self.refresh()?;
        loop {
            let verdict = {
                let w = self.world();
                cond(&w)
            };
            if let Some(v) = verdict? {
                return Ok(v);
            }
            let now = Instant::now();
            if now >= deadline {
                let last = self.describe();
                return Err(format!(
                    "timed out after {} ms waiting for {what}; last observed {last}",
                    started.elapsed().as_millis()
                ));
            }
            self.pump((deadline - now).min(Duration::from_millis(100)))?;
        }
    }

    /// Run one named step, record it, and forward it to the sink.
    pub fn step<T>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, Failed> {
        self.step_noted(label, |h| f(h).map(|v| (v, None)))
    }

    /// Like [`step`](Self::step), the closure may attach a note to a passing step.
    pub fn step_noted<T>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Self) -> Result<(T, Option<String>), String>,
    ) -> Result<T, Failed> {
        let started = Instant::now();
        let result = f(self);
        let elapsed = started.elapsed();
        let (value, error, note) = match result {
            Ok((v, note)) => (Some(v), None, note),
            Err(e) => (None, Some(e), None),
        };
        let rec = StepRecord {
            scenario: self.scenario,
            step: scrub(label),
            elapsed,
            error: error.as_deref().map(scrub),
            note: note.as_deref().map(scrub),
        };
        (self.sink)(&rec);
        self.records.push(rec);
        value.ok_or(Failed)
    }

    // ---- commands

    /// Send a neutral command to the (current) phone. Counted whether or not it is refused.
    pub fn exec(&mut self, command: CallCommand) -> Result<(), ControlError> {
        self.commands += 1;
        let Some(device) = self.current_device() else {
            return Err(ControlError::NoSuchDevice);
        };
        self.session.execute(&device, &command)
    }

    pub fn reject(&mut self, call: &CallId) -> Result<(), ControlError> {
        self.commands += 1;
        let Some(device) = self.current_device() else {
            return Err(ControlError::NoSuchDevice);
        };
        self.session.reject(&device, call)
    }

    pub fn commands_sent(&self) -> u32 {
        self.commands
    }

    // ---- shared conditions

    fn modem_is(&mut self, what: &str, want: fn(&ModemState) -> bool) -> Result<(), String> {
        self.wait(what, |w| Ok(want(&w.modem()?).then_some(())))
    }

    /// Wait for the modem to satisfy `want` after a desktop command. With `Caps::input_retry`
    /// the command is re-sent (up to 3 times) whenever the modem has not reacted in that time.
    /// Returns how many times it was re-sent.
    fn wait_modem_resending(
        &mut self,
        what: &str,
        resend: &dyn Fn(&mut Self) -> Result<(), String>,
        mut want: impl FnMut(&World) -> Result<bool, String>,
    ) -> Result<u32, String> {
        let total = self.step_timeout;
        let started = Instant::now();
        let mut resent = 0;
        let outcome = loop {
            let slice = match self.caps.input_retry {
                Some(r) if resent < 3 => r.min(total.saturating_sub(started.elapsed())),
                _ => total.saturating_sub(started.elapsed()),
            };
            self.step_timeout = slice.max(Duration::from_millis(50));
            let r = self.wait(what, |w| Ok(want(w)?.then_some(())));
            self.step_timeout = total;
            match r {
                Ok(()) => break Ok(resent),
                Err(e)
                    if e.starts_with("timed out after")
                        && self.caps.input_retry.is_some()
                        && resent < 3
                        && started.elapsed() + Duration::from_millis(200) < total =>
                {
                    resent += 1;
                    resend(self)?;
                }
                Err(e) if resent > 0 => {
                    break Err(format!("{e} (command re-sent {resent} time(s))"));
                }
                Err(e) => break Err(e),
            }
        };
        self.step_timeout = total;
        outcome
    }

    /// Wait for the single ringing/dialing call matching `want`; checks nothing else.
    fn wait_call(
        &mut self,
        what: &str,
        mut want: impl FnMut(&World, &CallInfo) -> bool,
    ) -> Result<CallInfo, String> {
        self.wait(what, |w| {
            let Some(st) = w.state else { return Ok(None) };
            Ok(st.calls.values().find(|c| want(w, c)).cloned())
        })
    }

    /// The call is gone from the session, a `CallEnded` event was seen for it, and the reducer
    /// recorded `reason`.
    fn wait_ended(&mut self, id: &CallId, reason: EndReason) -> Result<(), String> {
        let id = id.clone();
        self.wait(&format!("call ended with {reason:?}"), |w| {
            let Some(st) = w.state else { return Ok(None) };
            let ended_event =
                w.event_seen(|e| matches!(e, CallEvent::CallEnded { id: eid, .. } if eid == &id));
            if st.calls.contains_key(&id) || !ended_event {
                return Ok(None);
            }
            match w.ended(&id) {
                Some(e) if e.reason == reason => Ok(Some(())),
                Some(e) => Err(format!(
                    "call ended with {:?}, expected {reason:?}",
                    e.reason
                )),
                None => Ok(None),
            }
        })
    }

    /// Precondition shared by every scenario: link up, no calls, modem idle. A modem that is not
    /// idle at the start is forced idle (local hang-up plus console cancel, repeated, bounded by
    /// the step deadline) and reported as environmental: some other party started that call.
    fn reset(&mut self) -> Run {
        self.step_noted(
            "precondition: phone visible, link up, no calls, modem idle",
            |h| {
                h.refresh()?;
                let own = h.number.clone();
                let initial = h.phone.state().map_err(modem_err)?;
                let mut note = None;
                let mut kicked = None;
                if !matches!(initial, ModemState::Idle) {
                    let whose = match initial.number() {
                        Some(n) if !n.is_empty() && !same_number(n, &own) => {
                            "a call from a different number"
                        }
                        Some(n) if !n.is_empty() => "a call on this harness's number",
                        _ => "a call of unknown number",
                    };
                    note = Some(format!(
                        "ENVIRONMENT: modem was {} at scenario start ({whose}: a leftover or foreign call); forcing it idle",
                        describe_modem(&initial)
                    ));
                    let _ = h.phone.hangup();
                    let _ = h.phone.remote_hangup(&own);
                    kicked = Some(Instant::now());
                }
                let forced = note.is_some();
                h.wait("phone visible with no calls and an idle modem", |w| {
                    let Some(st) = w.state else { return Ok(None) };
                    let idle = matches!(w.modem()?, ModemState::Idle);
                    if !idle
                        && forced
                        && kicked.is_some_and(|t| t.elapsed() > Duration::from_millis(1500))
                    {
                        let _ = w.phone.hangup();
                        let _ = w.phone.remote_hangup(&own);
                        kicked = Some(Instant::now());
                    }
                    Ok((w.device.is_some()
                        && st.link == LinkState::Up
                        && st.calls.is_empty()
                        && idle)
                        .then_some(()))
                })
                .map_err(|e| {
                    if forced {
                        format!("ENVIRONMENT: could not clear a call this harness did not start ({e})")
                    } else {
                        e
                    }
                })?;
                h.mark = h.events.len();
                Ok(((), note))
            },
        )
    }

    fn ring_and_see(&mut self) -> Result<CallId, Failed> {
        let n = self.number.clone();
        self.step("remote party rings", |h| {
            h.phone.ring(&n).map_err(modem_err)
        })?;
        let n = self.number.clone();
        let ring_n = n.clone();
        let call = self.step_noted(
            "session reports CallUpdated: direction Incoming, phase Incoming",
            |h| {
                // With a flaky stimulus (the emulator sometimes drops a console ring that follows
                // a call that has just ended) a ring that never reached the modem is re-sent. A
                // modem that is ringing but a session that does not see it is a real failure.
                let total = h.step_timeout;
                let started = Instant::now();
                let mut rerings = 0u32;
                let result = loop {
                    let may_retry = h.caps.input_retry.is_some() && rerings < 2;
                    h.step_timeout = if may_retry {
                        Duration::from_secs(6).min(total.saturating_sub(started.elapsed()))
                    } else {
                        total.saturating_sub(started.elapsed())
                    }
                    .max(Duration::from_millis(50));
                    let r = h.wait("an incoming call", incoming_call);
                    h.step_timeout = total;
                    match r {
                        Err(e)
                            if may_retry
                                && e.starts_with("timed out after")
                                && started.elapsed() + Duration::from_secs(1) < total
                                && matches!(h.phone.state(), Ok(ModemState::Idle)) =>
                        {
                            rerings += 1;
                            h.phone.ring(&ring_n).map_err(modem_err)?;
                        }
                        other => break other,
                    }
                };
                h.step_timeout = total;
                let call = result?;
                let note = (rerings > 0).then(|| format!("ring re-sent {rerings} time(s): the modem never showed the first one (the emulator drops some console rings)"));
                Ok((call, note))
            },
        )?;
        let id = call.id.clone();
        self.step(
            "caller ID reached the session (last digits match)",
            |_| match &call.party.number {
                Some(got) if same_number(got.expose(), &n) => Ok(()),
                Some(got) => Err(format!(
                    "caller number {} does not match the ringing number {} (ENVIRONMENT: another party rang this phone?)",
                    redact(got.expose()),
                    redact(&n)
                )),
                None => Err("session call has no caller number".to_string()),
            },
        )?;
        Ok(id)
    }

    fn dial_and_see(&mut self) -> Result<CallId, Failed> {
        let n = self.number.clone();
        self.step("desktop Dial", |h| {
            h.dial_at = Some(Instant::now());
            h.exec(CallCommand::Dial {
                number: Sensitive::new(n),
            })
            .map_err(|e| format!("Dial refused: {e}"))
        })?;
        if self.caps.auto_answers_outbound {
            // The modem answers by itself after about two seconds, so the dialing window is
            // short. Judge it from the session's events only: no modem polling competes for adb.
            return self.step_noted(
                "session reports the outgoing call as Dialing/Alerting (polled every <=100 ms)",
                |h| {
                    let id = h.wait("an outgoing call reported as Dialing/Alerting", |w| {
                        for e in w.events {
                            if let CallEvent::CallUpdated {
                                id,
                                direction,
                                phase: CallPhase::Dialing | CallPhase::Alerting,
                                ..
                            } = &e.event
                            {
                                if *direction != CallDirection::Outgoing {
                                    return Err(format!(
                                        "call direction is {direction:?}, expected Outgoing"
                                    ));
                                }
                                return Ok(Some(id.clone()));
                            }
                        }
                        if w.event_seen(|e| {
                            matches!(
                                e,
                                CallEvent::CallUpdated {
                                    phase: CallPhase::Active,
                                    ..
                                }
                            )
                        }) {
                            return Err("dialing window missed: the first phase the session saw for the outgoing call was Active (the modem auto-answers after about 2 s; the simulator's modem polling plus adb latency is slower than the window)".to_string());
                        }
                        Ok(None)
                    })?;
                    let note = h.timing(&id);
                    Ok((id, Some(note)))
                },
            );
        }
        let call =
            self.step(
                "session shows an outgoing call (Dialing/Alerting) and the modem is Dialing",
                |h| {
                    h.wait(
                        "an outgoing call in Dialing/Alerting with a dialing modem",
                        |w| {
                            let Some(st) = w.state else { return Ok(None) };
                            let Some(c) = st.calls.values().find(|c| {
                                matches!(c.phase, CallPhase::Dialing | CallPhase::Alerting)
                            }) else {
                                return Ok(None);
                            };
                            if c.direction != CallDirection::Outgoing {
                                return Err(format!(
                                    "call direction is {:?}, expected Outgoing",
                                    c.direction
                                ));
                            }
                            Ok(matches!(w.modem()?, ModemState::Dialing { .. }).then(|| c.clone()))
                        },
                    )
                },
            )?;
        Ok(call.id)
    }

    /// Wait until the call is gone and ended, whatever the reason.
    fn wait_ended_any(&mut self, id: &CallId) -> Result<EndReason, String> {
        let id = id.clone();
        self.wait("the call to end", |w| {
            let Some(st) = w.state else { return Ok(None) };
            let ended_event =
                w.event_seen(|e| matches!(e, CallEvent::CallEnded { id: eid, .. } if eid == &id));
            if st.calls.contains_key(&id) || !ended_event {
                return Ok(None);
            }
            Ok(w.ended(&id).map(|e| e.reason))
        })
    }

    fn answer_incoming_to_active(&mut self, id: &CallId) -> Run {
        let call = id.clone();
        self.step("desktop Answer", |h| {
            h.exec(CallCommand::Answer { call })
                .map_err(|e| format!("Answer refused: {e}"))
        })?;
        let call = id.clone();
        self.step_noted("modem is Active", |h| {
            let mut idle_since: Option<Instant> = None;
            let resent = h.wait_modem_resending(
                "modem Active",
                &|h| {
                    h.exec(CallCommand::Answer { call: call.clone() })
                        .map_err(|e| format!("Answer re-send refused: {e}"))
                },
                |w| match w.modem()? {
                    ModemState::Active { .. } => Ok(true),
                    ModemState::Idle => {
                        let since = *idle_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > Duration::from_secs(1) {
                            Err("the modem is Idle after Answer, not Active: either the ringing call was cancelled by another party driving this phone (ENVIRONMENT) or Answer never reached the modem".to_string())
                        } else {
                            Ok(false)
                        }
                    }
                    _ => {
                        idle_since = None;
                        Ok(false)
                    }
                },
            )?;
            Ok(((), (resent > 0).then(|| format!("Answer re-sent {resent} time(s): the modem ignored it while the call was still starting to ring"))))
        })?;
        let call = id.clone();
        self.step_noted("session phase Active and audio link connected", |h| {
            let audio = h.wait("call Active with the audio link connected", |w| {
                let Some(st) = w.state else { return Ok(None) };
                let active = st
                    .calls
                    .get(&call)
                    .is_some_and(|c| c.phase == CallPhase::Active);
                Ok(match (active, st.audio) {
                    (true, AudioLinkState::Connected { codec }) => Some(codec),
                    _ => None,
                })
            })?;
            Ok(((), Some(format!("codec {audio:?}"))))
        })
    }

    fn hangup_to_idle(&mut self, id: &CallId, reason: EndReason) -> Run {
        let call = id.clone();
        self.step("desktop Hangup", |h| {
            h.exec(CallCommand::Hangup { call })
                .map_err(|e| format!("Hangup refused: {e}"))
        })?;
        self.step("modem is Idle", |h| {
            h.modem_is("modem Idle", |m| matches!(m, ModemState::Idle))
        })?;
        let call = id.clone();
        self.step(&format!("session shows CallEnded reason {reason:?}"), |h| {
            h.wait_ended(&call, reason)
        })
    }
}

/// Wait condition: the session reports exactly the incoming call this harness expects.
fn incoming_call(w: &World) -> Result<Option<CallInfo>, String> {
    let Some(st) = w.state else { return Ok(None) };
    let Some(c) = st.calls.values().find(|c| c.phase == CallPhase::Incoming) else {
        return Ok(None);
    };
    let id = c.id.clone();
    let seen = w.event_seen(|e| {
        matches!(e, CallEvent::CallUpdated { id: eid, direction: CallDirection::Incoming, phase: CallPhase::Incoming, .. } if eid == &id)
    });
    if !seen {
        return Ok(None);
    }
    if c.direction != CallDirection::Incoming {
        return Err(format!(
            "call direction is {:?}, expected Incoming",
            c.direction
        ));
    }
    Ok(Some(c.clone()))
}

// ------------------------------------------------------------------ scenarios

pub struct Scenario {
    pub id: &'static str,
    pub title: &'static str,
    /// Needs the harness to be able to take the simulator service away (self-contained mode).
    pub needs_service_control: bool,
    pub run: fn(&mut Harness) -> Run,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "S1",
        title: "incoming call answered, then hung up by the desktop",
        needs_service_control: false,
        run: s1_incoming_answered,
    },
    Scenario {
        id: "S2",
        title: "incoming call rejected by the desktop",
        needs_service_control: false,
        run: s2_incoming_rejected,
    },
    Scenario {
        id: "S3",
        title: "incoming call missed (remote gives up)",
        needs_service_control: false,
        run: s3_missed,
    },
    Scenario {
        id: "S4",
        title: "outgoing call placed by the desktop, answered, ended by the remote",
        needs_service_control: false,
        run: s4_outgoing,
    },
    Scenario {
        id: "S5",
        title: "outgoing call cancelled by the desktop before it is answered",
        needs_service_control: false,
        run: s5_outgoing_cancelled,
    },
    Scenario {
        id: "S6",
        title: "guard rails refuse bad commands without touching the modem",
        needs_service_control: false,
        run: s6_guard_rails,
    },
    Scenario {
        id: "S7",
        title: "simulator service lost during an active call",
        needs_service_control: true,
        run: s7_link_loss,
    },
    Scenario {
        id: "S8",
        title: "service call hub: decline, answer and hang up through the UI-facing view",
        needs_service_control: false,
        run: s8_service_hub,
    },
];

pub fn scenario_ids() -> Vec<&'static str> {
    SCENARIOS.iter().map(|s| s.id).collect()
}

fn s1_incoming_answered(h: &mut Harness) -> Run {
    h.reset()?;
    let id = h.ring_and_see()?;
    h.answer_incoming_to_active(&id)?;
    // The reducer fed with the event stream alone must agree with the session's own state.
    h.step("event stream replays to the session's state", |h| {
        h.refresh()?;
        let w = h.world();
        let live = w.state.ok_or("no live state")?;
        let path = w.device.map(|d| d.path.clone()).ok_or("no device")?;
        let shadow = h.shadow.get(&path).ok_or("no replayed state")?;
        let (a, b) = (live, shadow);
        if a.calls == b.calls && a.audio == b.audio && a.link == b.link {
            Ok(())
        } else {
            Err(format!(
                "replayed {} != live {}",
                describe_call_state(b),
                describe_call_state(a)
            ))
        }
    })?;
    h.hangup_to_idle(&id, EndReason::Completed)
}

fn s2_incoming_rejected(h: &mut Harness) -> Run {
    h.reset()?;
    let id = h.ring_and_see()?;
    let call = id.clone();
    h.step("desktop reject (Session::reject)", |h| {
        h.reject(&call).map_err(|e| format!("reject refused: {e}"))
    })?;
    let call = id.clone();
    h.step_noted("modem is Idle", |h| {
        let resent = h.wait_modem_resending(
            "modem Idle",
            &|h| {
                h.reject(&call)
                    .map_err(|e| format!("reject re-send refused: {e}"))
            },
            |w| Ok(matches!(w.modem()?, ModemState::Idle)),
        )?;
        Ok(((), (resent > 0).then(|| format!("reject re-sent {resent} time(s): the modem ignored it while the call was still starting to ring"))))
    })?;
    h.step("session shows CallEnded reason NotAnswered", |h| {
        h.wait_ended(&id, EndReason::NotAnswered)
    })
}

fn s3_missed(h: &mut Harness) -> Run {
    h.reset()?;
    let before = h.commands_sent();
    let id = h.ring_and_see()?;
    let n = h.number.clone();
    h.step("remote party hangs up while ringing", |h| {
        h.phone.remote_hangup(&n).map_err(modem_err)
    })?;
    h.step("session shows CallEnded reason NotAnswered", |h| {
        h.wait_ended(&id, EndReason::NotAnswered)
    })?;
    h.step("modem is Idle", |h| {
        h.modem_is("modem Idle", |m| matches!(m, ModemState::Idle))
    })?;
    h.step(
        "no command was sent and the call never became Active",
        |h| {
            h.refresh()?;
            if h.commands_sent() != before {
                return Err(format!(
                    "{} command(s) were sent",
                    h.commands_sent() - before
                ));
            }
            let became_active = h.events.iter().any(|e| {
                matches!(
                    &e.event,
                    CallEvent::CallUpdated {
                        phase: CallPhase::Active,
                        ..
                    }
                )
            });
            if became_active {
                return Err("an Active phase was observed".to_string());
            }
            Ok(())
        },
    )
}

fn s4_outgoing(h: &mut Harness) -> Run {
    h.reset()?;
    let id = h.dial_and_see()?;
    let caps = h.caps;
    if !caps.auto_answers_outbound {
        let n = h.number.clone();
        h.step("remote party accepts", |h| {
            h.phone.remote_accept(&n).map_err(modem_err)
        })?;
        h.step("modem is Active", |h| {
            h.modem_is("modem Active", |m| matches!(m, ModemState::Active { .. }))
        })?;
    }
    let call = id.clone();
    let label = if caps.auto_answers_outbound {
        "modem auto-answers: session phase Active and audio link connected"
    } else {
        "session phase Active and audio link connected"
    };
    h.step_noted(label, |h| {
        let codec = h.wait("outgoing call Active with the audio link connected", |w| {
            let Some(st) = w.state else { return Ok(None) };
            let active = st
                .calls
                .get(&call)
                .is_some_and(|c| c.phase == CallPhase::Active);
            Ok(match (active, st.audio) {
                (true, AudioLinkState::Connected { codec }) => Some(codec),
                _ => None,
            })
        })?;
        Ok(((), Some(format!("codec {codec:?}; {}", h.timing(&call)))))
    })?;
    if caps.auto_answers_outbound {
        h.step("modem is Active", |h| {
            h.modem_is("modem Active", |m| matches!(m, ModemState::Active { .. }))
        })?;
    }
    if caps.remote_hangup_outbound {
        let n = h.number.clone();
        h.step("remote party hangs up", |h| {
            h.phone.remote_hangup(&n).map_err(modem_err)
        })?;
        h.step("modem is Idle", |h| {
            h.modem_is("modem Idle", |m| matches!(m, ModemState::Idle))
        })?;
        h.step("session shows CallEnded reason Completed", |h| {
            h.wait_ended(&id, EndReason::Completed)
        })
    } else {
        // The modem cannot be hung up from the remote side: the desktop ends the call.
        h.hangup_to_idle(&id, EndReason::Completed)
    }
}

/// With a modem that auto-answers (the emulator) the desktop races the answer: hang up the moment
/// the session shows Dialing. If the call already went Active the attempt is reported and retried.
fn s5_outgoing_cancelled(h: &mut Harness) -> Run {
    let attempts = if h.caps.auto_answers_outbound { 3 } else { 1 };
    for attempt in 1..=attempts {
        h.reset()?;
        let id = h.dial_and_see()?;
        let call = id.clone();
        h.step_noted("desktop Hangup before the remote answers", |h| {
            let started = Instant::now();
            h.exec(CallCommand::Hangup { call })
                .map_err(|e| format!("Hangup refused: {e}"))?;
            Ok((
                (),
                Some(format!(
                    "hangup call took {} ms",
                    started.elapsed().as_millis()
                )),
            ))
        })?;
        h.step("modem is Idle", |h| {
            h.modem_is("modem Idle", |m| matches!(m, ModemState::Idle))
        })?;
        let last = attempt == attempts;
        let done = h.step_noted(
            "session shows CallEnded reason OutgoingNotConnected",
            |h| {
                let reason = h.wait_ended_any(&id)?;
                let went_active = h.saw_phase(&id, CallPhase::Active);
                let timing = h.timing(&id);
                match reason {
                    EndReason::OutgoingNotConnected => {
                        let note = (attempt > 1).then(|| format!("attempt {attempt}/{attempts}"));
                        Ok((true, note))
                    }
                    EndReason::Completed if went_active && h.caps.auto_answers_outbound => {
                        if last {
                            Err(format!(
                                "the call became Active before the hangup took effect on all {attempts} attempts; the modem auto-answers after about 2 s and this stack was too slow to win the race ({timing})"
                            ))
                        } else {
                            Ok((
                                false,
                                Some(format!(
                                    "RACE lost: the call went Active before the hangup took effect ({timing}); retrying, attempt {attempt}/{attempts}"
                                )),
                            ))
                        }
                    }
                    other => Err(format!(
                        "call ended with {other:?}, expected OutgoingNotConnected"
                    )),
                }
            },
        )?;
        if done {
            return Ok(());
        }
    }
    Ok(())
}

/// Run `command`, require a refusal that satisfies `accept`, and require the modem and the
/// session's call set to be unchanged afterwards. Returns the refusal as a note.
fn refused(
    h: &mut Harness,
    label: &str,
    command: CallCommand,
    accept: fn(&ControlError) -> bool,
) -> Run {
    h.step_noted(label, |h| {
        let modem_before = h.phone.state().map_err(modem_err)?;
        h.refresh()?;
        let calls_before = h.world().state.map(|s| s.calls.clone());
        let err = match h.exec(command) {
            Ok(()) => return Err("the command was accepted".to_string()),
            Err(e) => e,
        };
        if err.outcome_unknown() {
            return Err(format!("refusal is not definite: {err}"));
        }
        if !accept(&err) {
            return Err(format!("unexpected kind of refusal: {err:?}"));
        }
        h.refresh()?;
        let modem_after = h.phone.state().map_err(modem_err)?;
        if modem_after != modem_before {
            return Err(format!(
                "modem changed from {} to {}",
                describe_modem(&modem_before),
                describe_modem(&modem_after)
            ));
        }
        if h.world().state.map(|s| s.calls.clone()) != calls_before {
            return Err("the session's call set changed".to_string());
        }
        Ok(((), Some(format!("refused: {err}"))))
    })
}

fn is_local_refusal(e: &ControlError) -> bool {
    matches!(
        e,
        ControlError::Preflight(_)
            | ControlError::WrongState(_)
            | ControlError::InvalidArgument(_)
            | ControlError::Unsupported(_)
            | ControlError::NotDeclared { .. }
            | ControlError::NoSuchCall
    )
}

fn s6_guard_rails(h: &mut Harness) -> Run {
    h.reset()?;
    let id = h.ring_and_see()?;
    h.answer_incoming_to_active(&id)?;

    refused(
        h,
        "Answer on an active call is refused locally",
        CallCommand::Answer { call: id.clone() },
        |e| {
            matches!(
                e,
                ControlError::Preflight(CommandRejection::InvalidState)
                    | ControlError::WrongState(_)
            )
        },
    )?;
    refused(
        h,
        "Hangup of a call id that does not exist is refused locally",
        CallCommand::Hangup {
            call: CallId::new("call-does-not-exist"),
        },
        |e| {
            matches!(
                e,
                ControlError::Preflight(CommandRejection::NoSuchCall) | ControlError::NoSuchCall
            )
        },
    )?;
    for (label, number) in [
        ("Dial with letters is refused locally", "55a1234"),
        ("Dial with an empty number is refused locally", ""),
        (
            "Dial with an over-long number is refused locally",
            &"1".repeat(40),
        ),
    ] {
        refused(
            h,
            label,
            CallCommand::Dial {
                number: Sensitive::new(number),
            },
            |e| {
                matches!(
                    e,
                    ControlError::Preflight(CommandRejection::InvalidArgument)
                        | ControlError::InvalidArgument(_)
                )
            },
        )?;
    }
    refused(
        h,
        "SwapCalls (nothing held) surfaces a clear refusal",
        CallCommand::SwapCalls,
        |e| is_local_refusal(e) || matches!(e, ControlError::Remote { .. }),
    )?;
    refused(
        h,
        "MuteRinger is refused as unsupported",
        CallCommand::MuteRinger,
        is_local_refusal,
    )?;
    refused(
        h,
        "DisconnectAudio is refused as unsupported",
        CallCommand::DisconnectAudio,
        is_local_refusal,
    )?;
    let call = id.clone();
    h.step_noted(
        "Session::reject on an active call is refused locally",
        |h| {
            let before = h.phone.state().map_err(modem_err)?;
            let err = match h.reject(&call) {
                Ok(()) => return Err("reject of an active call was accepted".to_string()),
                Err(e) => e,
            };
            if !matches!(err, ControlError::WrongState(_)) {
                return Err(format!("unexpected kind of refusal: {err:?}"));
            }
            let after = h.phone.state().map_err(modem_err)?;
            if before != after {
                return Err("modem changed".to_string());
            }
            Ok(((), Some(format!("refused: {err}"))))
        },
    )?;
    h.step("the call is still Active after every refusal", |h| {
        let call = id.clone();
        h.wait("call still Active", |w| {
            let active = w
                .state
                .and_then(|s| s.calls.get(&call))
                .is_some_and(|c| c.phase == CallPhase::Active);
            let modem_active = matches!(w.modem()?, ModemState::Active { .. });
            Ok((active && modem_active).then_some(()))
        })
    })?;
    h.hangup_to_idle(&id, EndReason::Completed)
}

fn s7_link_loss(h: &mut Harness) -> Run {
    h.reset()?;
    let id = h.ring_and_see()?;
    h.answer_incoming_to_active(&id)?;
    h.step("simulator service is stopped", |h| {
        let kill = h
            .service_killer
            .as_mut()
            .ok_or("this harness cannot stop the service")?;
        kill();
        Ok(())
    })?;
    let call = id.clone();
    h.step("session emits Link Down and sees the service absent", |h| {
        h.wait("Link Down", |w| {
            let down = w.event_seen(|e| {
                matches!(
                    e,
                    CallEvent::Link {
                        state: LinkState::Down
                    }
                )
            });
            Ok((down && matches!(w.service, bridge_calls::ServiceStatus::Absent)).then_some(()))
        })
    })?;
    h.step("the active call ended with reason LinkLost", |h| {
        let w = h.world();
        let st = w.state.ok_or("no state retained")?;
        if !st.calls.is_empty() {
            return Err("calls are still shown after the link dropped".to_string());
        }
        match w.ended(&call) {
            Some(e) if e.reason == EndReason::LinkLost => Ok(()),
            Some(e) => Err(format!("ended with {:?}, expected LinkLost", e.reason)),
            None => Err("no ended-call record for the call".to_string()),
        }
    })?;
    h.step("audio link reported disconnected", |h| {
        let st = h.world().state.ok_or("no state retained")?;
        (st.audio == AudioLinkState::Disconnected && st.link == LinkState::Down)
            .then_some(())
            .ok_or_else(|| format!("state is {}", describe_call_state(st)))
    })?;
    h.step("commands after the loss are refused locally", |h| {
        match h.exec(CallCommand::Hangup { call: id.clone() }) {
            Err(ControlError::NoSuchDevice) => Ok(()),
            Err(e) => Err(format!("unexpected refusal: {e:?}")),
            Ok(()) => Err("the command was accepted".to_string()),
        }
    })
}

/// Run one scenario against the harness. A failed scenario is followed by a best-effort modem
/// hang-up so the next one starts clean.
pub fn run_scenario(h: &mut Harness, sc: &Scenario) -> ScenarioReport {
    if sc.needs_service_control && h.service_killer.is_none() {
        return ScenarioReport {
            id: sc.id,
            title: sc.title,
            status: Status::Skip,
            skip_reason: Some("needs --fake (the harness must own the simulator)".to_string()),
            steps: Vec::new(),
            elapsed: Duration::ZERO,
        };
    }
    h.scenario = sc.id;
    h.records.clear();
    h.events.clear();
    h.event_times.clear();
    h.mark = 0;
    h.commands = 0;
    let started = Instant::now();
    let outcome = (sc.run)(h);
    let elapsed = started.elapsed();
    if !sc.needs_service_control {
        let _ = h.phone.hangup();
    }
    let steps = std::mem::take(&mut h.records);
    ScenarioReport {
        id: sc.id,
        title: sc.title,
        status: if outcome.is_ok() {
            Status::Pass
        } else {
            Status::Fail
        },
        skip_reason: None,
        steps,
        elapsed,
    }
}

/// Run the scenarios named in `only` (all when empty). Scenarios that stop the service always run
/// last, because nothing can run after them.
pub fn run_selected(h: &mut Harness, only: &[String]) -> Vec<ScenarioReport> {
    let mut chosen: Vec<&Scenario> = SCENARIOS
        .iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o.eq_ignore_ascii_case(s.id)))
        .collect();
    chosen.sort_by_key(|s| s.needs_service_control);
    chosen.into_iter().map(|s| run_scenario(h, s)).collect()
}

// ------------------------------------------------------------------ S8: the service call hub

/// The same Bluetooth path the Calls window uses: `bridge_service::call_control::CallHub` over its own
/// `Session` on the simulator bus. The harness's own session keeps observing, so a disagreement
/// between the two shows up as a failed wait. Only signalling: the audio link here is the
/// simulator's declared one.
fn s8_service_hub(h: &mut Harness) -> Run {
    use bridge_core::calls_view::{ActionTarget, AudioRoute, ControlPath};
    use bridge_core::ipc::CallAction;
    use bridge_service::call_control::{CallHub, linux::LinuxHfp};

    h.reset()?;
    let hub = h.step(
        "call hub starts on the simulator bus (Bluetooth hands-free source)",
        |h| {
            let backend = PipeWireBackend::connect(PipeWireConfig {
                bus: h.bus.clone(),
                method_timeout: Duration::from_secs(10),
            })
            .map_err(|e| e.to_string())?;
            let hub = CallHub::new();
            hub.start_host(Box::new(LinuxHfp::new(Session::new(Box::new(backend)))));
            Ok(hub)
        },
    )?;

    // 1. Decline from the hub.
    let n = h.number.clone();
    h.step("remote party rings", |h| {
        h.phone.ring(&n).map_err(modem_err)
    })?;
    let reject: ActionTarget = h.step_noted(
        "hub offers Answer and Decline over Bluetooth for the ringing call",
        |h| {
            let hub = hub.clone();
            let (answer, reject) = h.wait("the hub to show the ringing call", |_| {
                Ok(hub
                    .view()
                    .ringing()
                    .and_then(|r| Some((r.answer.clone()?, r.reject.clone()?))))
            })?;
            if answer.path != ControlPath::Hfp || reject.path != ControlPath::Hfp {
                return Err("controls are not on the Bluetooth path".to_string());
            }
            Ok((
                reject,
                Some("caller shown without exposing the number".to_string()),
            ))
        },
    )?;
    let before = h.commands_sent();
    h.step("hub Decline reaches the modem", |h| {
        hub.act(&reject, CallAction::Reject)
            .map_err(|e| format!("Decline refused: {}", e.message))?;
        h.wait_modem_resending(
            "modem Idle",
            &|_| hub.act(&reject, CallAction::Reject).map_err(|e| e.message),
            |w| Ok(matches!(w.modem()?, ModemState::Idle)),
        )
        .map(|_| ())
    })?;
    h.step("hub view has no calls after the decline", |h| {
        h.wait("an empty hub view", |_| {
            Ok(hub.view().rows.is_empty().then_some(()))
        })
    })?;

    // 2. A stale target does nothing.
    h.step(
        "a decision on the ended call is refused and sends nothing",
        |h| match hub.act(&reject, CallAction::Reject) {
            Err(_) if matches!(h.phone.state().map_err(modem_err)?, ModemState::Idle) => Ok(()),
            Err(_) => Err("the modem left Idle".to_string()),
            Ok(()) => Err("a stale target was accepted".to_string()),
        },
    )?;

    // 3. Answer, check the route, hang up.
    h.step("remote party rings again", |h| {
        let n = h.number.clone();
        h.phone.ring(&n).map_err(modem_err)
    })?;
    let answer = h.step("hub offers Answer again", |h| {
        h.wait("the hub to show the second ringing call", |_| {
            Ok(hub.view().ringing().and_then(|r| r.answer.clone()))
        })
    })?;
    h.step_noted("hub Answer makes the modem Active", |h| {
        hub.act(&answer, CallAction::Answer)
            .map_err(|e| format!("Answer refused: {}", e.message))?;
        let resent = h.wait_modem_resending(
            "modem Active",
            &|_| hub.act(&answer, CallAction::Answer).map_err(|e| e.message),
            |w| Ok(matches!(w.modem()?, ModemState::Active { .. })),
        )?;
        Ok((
            (),
            (resent > 0).then(|| format!("Answer re-sent {resent} time(s)")),
        ))
    })?;
    let hangup = h.step_noted(
        "hub shows a live call with Hang up and the audio route",
        |h| {
            h.wait("a live call with its audio on this computer", |_| {
                let v = hub.view();
                Ok(match (v.live(), v.audio.route) {
                    (Some(r), AudioRoute::Computer { .. }) => {
                        r.hangup.clone().map(|t| (t, v.mute.is_some()))
                    }
                    _ => None,
                })
            })
            .map(|(t, mute)| {
                (
                    t,
                    Some(format!(
                        "mute offered: {mute} (Linux has no microphone mute API)"
                    )),
                )
            })
        },
    )?;
    h.step("hub Hang up ends the call", |h| {
        hub.act(&hangup, CallAction::Hangup)
            .map_err(|e| format!("Hang up refused: {}", e.message))?;
        h.modem_is("modem Idle", |m| matches!(m, ModemState::Idle))?;
        h.wait("an empty hub view", |_| {
            Ok(hub.view().rows.is_empty().then_some(()))
        })
    })?;
    h.step(
        "exactly the hub's decisions were sent (the harness sent none)",
        |h| {
            if h.commands_sent() == before {
                Ok(())
            } else {
                Err("the harness session sent commands".to_string())
            }
        },
    )
}
