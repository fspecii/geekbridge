// SPDX-License-Identifier: GPL-3.0-or-later
//! Answering, declining and ending phone calls from this computer.
//!
//! The hub merges every place that can see or control a call into the one view the apps show
//! ([`bridge_core::calls_view`]):
//!
//! * **Host Bluetooth calling** ([`HostCalls`]): this computer as the HFP hands-free unit. On Linux
//!   that is `bridge-calls` over PipeWire's telephony service. Windows and macOS adapters plug in
//!   through the same trait. This is the path that carries call audio.
//! * **The GeekBridge link** (`geekbridge.calls.*`, [`bridge_core::calls_link`]): the paired phone
//!   reports ringing/idle and executes answer/decline/end through Android's `TelecomManager`.
//!   Signalling only; audio stays on the phone unless the HFP link above carries it.
//!
//! Nothing here is offered to the user unless it passed the same pre-flight that execution uses, and
//! every control request is checked against the *current* view first, so a stale click cannot hit a
//! different call. Link packets are honoured only for devices the user enabled ("Telephony" in the
//! device permissions). Phone numbers and names live in memory only and are never logged.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use bridge_core::calls::Support;
use bridge_core::calls::{CallCommand, CallState, Sensitive};
use bridge_core::calls_link::{self as link, LinkAction, LinkCallReport};
use bridge_core::calls_view::{self as view, ActionTarget, CallsView, ControlPath, SourceSnapshot};
use bridge_core::ipc::{CallAction, CallsStatus, ErrorBody, ErrorCode};
use tokio::sync::mpsc;

const LINK_PREFIX: &str = "link:";
/// Requests to a phone that stay unanswered this long are forgotten (the phone has no reply path
/// if it was killed in between).
const PENDING_TTL: Duration = Duration::from_secs(20);
const MAX_PENDING: usize = 4;
const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const HOST_POLL: Duration = Duration::from_millis(250);

fn err(code: ErrorCode, message: &str) -> ErrorBody {
    ErrorBody {
        code,
        message: message.to_owned(),
    }
}

// ------------------------------------------------------------------ host backends

/// A computer-side backend that talks Bluetooth hands-free to phones. Runs on its own thread.
pub trait HostCalls: Send + 'static {
    /// Short name for diagnostics ("Bluetooth hands-free (Linux)").
    fn label(&self) -> String;
    /// Wait up to `wait` for changes, then return the current snapshot of every phone.
    fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String>;
    /// Execute a command on the phone behind `source`. `Ok` means accepted for dispatch.
    fn execute(&mut self, source: &str, command: &CallCommand) -> Result<(), String>;
}

struct HostCommand {
    source: String,
    command: CallCommand,
    reply: std_mpsc::SyncSender<Result<(), String>>,
}

#[derive(Default)]
struct HostShared {
    snapshots: Vec<SourceSnapshot>,
    label: String,
    problem: Option<String>,
}

struct HostHandle {
    shared: Arc<Mutex<HostShared>>,
    commands: std_mpsc::Sender<HostCommand>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for HostHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------ notifications

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingChoice {
    Answer,
    Decline,
}

/// What the desktop shows when a call rings while no Calls window is open.
pub trait CallNotifier: Send + Sync {
    /// `can_answer`/`can_decline` say which buttons to offer. `reply` may be dropped unused.
    fn ringing(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    );
}

/// Best-effort freedesktop notification through `notify-send` with Answer/Decline actions
/// (libnotify >= 0.8). Without the tool or action support nothing is shown; the Calls window and
/// the phone itself still work.
pub struct NotifySendCalls;
impl NotifySendCalls {
    pub fn args(caller: &str, can_answer: bool, can_decline: bool) -> Vec<String> {
        let clean = crate::notify_text::plain(caller, 80);
        let mut args = vec![
            "--app-name=GeekBridge".to_owned(),
            "--urgency=critical".to_owned(),
            "--category=call.incoming".to_owned(),
            "--expire-time=30000".to_owned(),
        ];
        if can_answer || can_decline {
            args.push("--wait".into());
        }
        if can_answer {
            args.push("--action=answer=Answer".into());
        }
        if can_decline {
            args.push("--action=decline=Decline".into());
        }
        if can_answer || can_decline {
            // A click on the body (mako draws no buttons) opens the Calls window; it never answers.
            args.push("--action=default=Open".into());
        }
        // `--`: a caller-ID name that starts with a dash is text, not an option.
        args.push("--".into());
        args.push("Incoming call".into());
        args.push(clean);
        args
    }
}
impl CallNotifier for NotifySendCalls {
    fn ringing(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    ) {
        let args = Self::args(caller, can_answer, can_decline);
        std::thread::spawn(move || {
            let out = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            if let Ok(out) = out {
                match String::from_utf8_lossy(&out.stdout).trim() {
                    "answer" if can_answer => reply(RingChoice::Answer),
                    "decline" if can_decline => reply(RingChoice::Decline),
                    "default" => crate::desktop_notify::open_app("geekbridge-calls"),
                    _ => {}
                }
            }
        });
    }
}

// ------------------------------------------------------------------ the hub

/// A request for the phone, queued for the link task that owns the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub request_id: String,
    pub action: LinkAction,
    /// The number of a `dial` request.
    pub number: Option<Sensitive>,
}

struct LinkPhone {
    /// Which `link_up` created this entry; see [`CallHub::link_down_generation`].
    generation: u64,
    name: String,
    enabled: bool,
    report: Option<LinkCallReport>,
    state: CallState,
    tx: mpsc::Sender<LinkRequest>,
    pending: Vec<(String, LinkAction, Instant)>,
}

#[derive(Default)]
struct Inner {
    next_generation: u64,
    links: BTreeMap<String, LinkPhone>,
    notice: Option<String>,
    next_request: u64,
    announced: BTreeSet<String>,
    /// When a ringing call was first seen without a name or number (see [`UNNAMED_GRACE`]).
    unnamed_since: BTreeMap<String, Instant>,
}

/// The phone often reports "ringing" a moment before the caller's number follows. A notification
/// cannot be updated afterwards, so a call without a caller is held back this long before it is
/// announced as "Unknown caller" (a private number never gets one).
const UNNAMED_GRACE: Duration = Duration::from_millis(1500);

pub struct CallHub {
    inner: Mutex<Inner>,
    host: Mutex<Option<HostHandle>>,
    notifier: Mutex<Option<Arc<dyn CallNotifier>>>,
}

impl CallHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            host: Mutex::new(None),
            notifier: Mutex::new(None),
        })
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_notifier(&self, notifier: Arc<dyn CallNotifier>) {
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = Some(notifier);
    }

    /// Start the host Bluetooth backend on its own thread. Replaces a previous one.
    pub fn start_host(self: &Arc<Self>, mut host: Box<dyn HostCalls>) {
        let shared = Arc::new(Mutex::new(HostShared {
            label: host.label(),
            ..HostShared::default()
        }));
        let (commands, command_rx) = std_mpsc::channel::<HostCommand>();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = HostHandle {
            shared: shared.clone(),
            commands,
            stop: stop.clone(),
        };
        let hub: Weak<CallHub> = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("geekbridge-calls-host".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    while let Ok(cmd) = command_rx.try_recv() {
                        let result = host.execute(&cmd.source, &cmd.command);
                        let _ = cmd.reply.send(result);
                    }
                    let polled = host.poll(HOST_POLL);
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        match polled {
                            Ok(snaps) => {
                                g.snapshots = snaps;
                                g.problem = None;
                            }
                            Err(problem) => {
                                g.snapshots.clear();
                                g.problem = Some(problem);
                            }
                        }
                    }
                    match hub.upgrade() {
                        Some(hub) => hub.announce_new_calls(),
                        None => return,
                    }
                    if shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .problem
                        .is_some()
                    {
                        // A broken backend is retried slowly, not in a tight loop.
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            });
        if spawned.is_ok() {
            *self.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        }
    }

    // -------------------------------------------------------------- link side

    /// A trusted link to `device_id` is up. The returned receiver yields requests to send to the
    /// phone; dropping it (link gone) is how the hub learns the link is unusable.
    pub fn link_up(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        enabled: bool,
    ) -> mpsc::Receiver<LinkRequest> {
        self.link_up_generation(device_id, name, enabled).0
    }

    /// [`Self::link_up`], also returning the generation for [`Self::link_down_generation`].
    pub fn link_up_generation(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        enabled: bool,
    ) -> (mpsc::Receiver<LinkRequest>, u64) {
        let (tx, rx) = mpsc::channel(8);
        let mut inner = self.inner();
        inner.next_generation += 1;
        let generation = inner.next_generation;
        inner.links.insert(
            device_id.to_owned(),
            LinkPhone {
                generation,
                name: name.chars().filter(|c| !c.is_control()).take(80).collect(),
                enabled,
                report: None,
                state: CallState::default(),
                tx,
                pending: Vec::new(),
            },
        );
        (rx, generation)
    }

    pub fn link_down(self: &Arc<Self>, device_id: &str) {
        self.inner().links.remove(device_id);
        self.announce_new_calls();
    }

    /// Remove `device_id` only if it is still the link `generation` created. A session displaced by
    /// a reconnect ends after its successor registered and must not take the successor down.
    pub fn link_down_generation(self: &Arc<Self>, device_id: &str, generation: u64) {
        {
            let mut inner = self.inner();
            if inner
                .links
                .get(device_id)
                .is_some_and(|p| p.generation != generation)
            {
                return;
            }
            inner.links.remove(device_id);
        }
        self.announce_new_calls();
    }

    /// Per-device "Telephony" consent changed. Turning it off forgets what the phone reported.
    pub fn set_enabled(self: &Arc<Self>, device_id: &str, enabled: bool) {
        if let Some(phone) = self.inner().links.get_mut(device_id) {
            phone.enabled = enabled;
            if !enabled {
                phone.report = None;
                phone.state = CallState::default();
                phone.pending.clear();
            }
        }
    }

    /// A `geekbridge.calls.*` packet arrived. False if it was malformed or unexpected (the caller
    /// counts that as misbehaviour); packets from a device without consent are dropped quietly.
    pub fn on_packet(
        self: &Arc<Self>,
        device_id: &str,
        kind: &str,
        body: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        let ok = {
            let mut guard = self.inner();
            let Inner { links, notice, .. } = &mut *guard;
            let Some(phone) = links.get_mut(device_id) else {
                return true;
            };
            if !phone.enabled {
                return true;
            }
            match kind {
                link::STATE => match LinkCallReport::parse(body) {
                    Ok(report) => {
                        report.apply(&mut phone.state);
                        phone.report = Some(report);
                        if phone.state.calls.is_empty() {
                            // A fresh call starts without the previous call's refusal.
                            *notice = None;
                        }
                        true
                    }
                    Err(_) => false,
                },
                link::RESULT => match link::parse_result(body) {
                    Ok((id, outcome)) => {
                        let known = phone.pending.iter().any(|(p, _, _)| *p == id);
                        phone.pending.retain(|(p, _, _)| *p != id);
                        if let (true, Err(reason)) = (known, outcome) {
                            *notice = Some(reason.message().to_owned());
                        }
                        known
                    }
                    Err(_) => false,
                },
                _ => false,
            }
        };
        self.announce_new_calls();
        ok
    }

    // -------------------------------------------------------------- view

    fn sources(&self) -> (Vec<SourceSnapshot>, Vec<String>) {
        let mut snaps = Vec::new();
        let mut lines = Vec::new();
        {
            let inner = self.inner();
            for (id, p) in &inner.links {
                if !p.enabled {
                    lines.push(format!(
                        "Phone link ({}): allow Calls for this phone in its permissions to see and answer calls from here.",
                        p.name
                    ));
                } else if let Some(report) = &p.report {
                    snaps.push(SourceSnapshot {
                        key: format!("{LINK_PREFIX}{id}"),
                        label: p.name.clone(),
                        caps: report.capabilities(),
                        state: p.state.clone(),
                    });
                    if !report.can_answer && !report.can_dial {
                        lines.push(format!(
                            "Phone link ({}): calls are visible, but the phone has not allowed GeekBridge to answer or place them.",
                            p.name
                        ));
                    } else if !report.can_dial {
                        lines.push(format!(
                            "Phone link ({}): ready to answer; to dial from here, allow GeekBridge to make phone calls in the phone app's setup.",
                            p.name
                        ));
                    } else {
                        lines.push(format!("Phone link ({}): ready.", p.name));
                    }
                } else {
                    lines.push(format!(
                        "Phone link ({}): no call state received. On the phone, turn on \"Share call status with my computer\" in Setup > Answer calls (or update the phone app).",
                        p.name
                    ));
                }
            }
        }
        if let Some(host) = self.host.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let g = host.shared.lock().unwrap_or_else(|e| e.into_inner());
            match &g.problem {
                Some(problem) => lines.push(format!("{}: {problem}", g.label)),
                None if g.snapshots.is_empty() => {
                    lines.push(format!("{}: no phone connected for calls.", g.label))
                }
                None => lines.push(format!("{}: ready.", g.label)),
            }
            snaps.extend(g.snapshots.iter().cloned());
        } else {
            lines.push("Bluetooth hands-free calling is not available on this computer.".into());
        }
        // Why the voice cannot come here (no adapter / not paired / ready) and the next step.
        #[cfg(all(target_os = "linux", not(test)))]
        if !snaps.iter().any(|s| s.key.starts_with("hfp:")) {
            lines.push(bridge_calls::readiness::detect().line());
        }
        (snaps, lines)
    }

    /// Why the voice cannot come to this computer, when control works anyway. Only on Linux, only
    /// while no hands-free phone is registered (the same facts as the `sources` line).
    fn audio_note(snaps: &[SourceSnapshot]) -> Option<String> {
        let _ = snaps;
        #[cfg(all(target_os = "linux", not(test)))]
        if !snaps.iter().any(|s| s.key.starts_with("hfp:")) {
            let r = bridge_calls::readiness::detect();
            if r != bridge_calls::readiness::Readiness::Ready {
                return Some(r.line());
            }
        }
        None
    }

    pub fn view(&self) -> CallsView {
        view::build(&self.sources().0)
    }

    pub fn status(&self) -> CallsStatus {
        let (snaps, sources) = self.sources();
        CallsStatus {
            view: view::build(&snaps),
            notice: self.inner().notice.clone(),
            sources,
            audio_note: Self::audio_note(&snaps),
        }
    }

    // -------------------------------------------------------------- control

    /// Execute a user's decision. Blocking (host commands wait for the backend thread).
    pub fn act(
        self: &Arc<Self>,
        target: &ActionTarget,
        action: CallAction,
    ) -> Result<(), ErrorBody> {
        let view = self.view();
        let offered = view.rows.iter().any(|r| {
            let slot = match action {
                CallAction::Answer => &r.answer,
                CallAction::Reject => &r.reject,
                CallAction::Hangup => &r.hangup,
            };
            slot.as_ref() == Some(target)
        });
        if !offered {
            return Err(err(
                ErrorCode::NotFound,
                "that call changed; nothing was sent",
            ));
        }
        let call = target.call.clone();
        let command = match action {
            CallAction::Answer => CallCommand::Answer { call },
            CallAction::Reject => CallCommand::Reject { call },
            CallAction::Hangup => CallCommand::Hangup { call },
        };
        let result = match target.path {
            ControlPath::Link => {
                let wire = match action {
                    CallAction::Answer => LinkAction::Answer,
                    CallAction::Reject => LinkAction::Reject,
                    CallAction::Hangup => LinkAction::Hangup,
                };
                self.send_link(&target.source, wire, None)
            }
            ControlPath::Hfp => self.send_host(&target.source, &command),
        };
        let mut inner = self.inner();
        match &result {
            Ok(()) => inner.notice = None,
            Err(e) => inner.notice = Some(e.message.clone()),
        }
        result
    }

    pub fn mute(&self, source: &str, muted: bool) -> Result<(), ErrorBody> {
        let offered = self.view().mute.is_some_and(|m| m.source == source);
        if !offered {
            return Err(err(ErrorCode::Unsupported, "muting is not available now"));
        }
        self.send_host(source, &CallCommand::SetMicrophoneMute { muted })
    }

    pub fn connect_audio(&self, source: &str) -> Result<(), ErrorBody> {
        let offered = self.view().audio.connect.as_deref() == Some(source);
        if !offered {
            return Err(err(
                ErrorCode::Unsupported,
                "moving the audio to this computer is not available now",
            ));
        }
        self.send_host(source, &CallCommand::ConnectAudio)
    }

    /// Place a call. `source` must be the dial target the current view offers (which already means:
    /// consent for Telephony is on, the phone reported it may place calls and no call is going on),
    /// and `number` must be plain digits. Blocking.
    pub fn dial(self: &Arc<Self>, source: &str, number: &Sensitive) -> Result<(), ErrorBody> {
        if !link::valid_dial_number(number.expose()) {
            return Err(err(
                ErrorCode::BadRequest,
                "that is not a number this computer can dial (digits, optional leading +)",
            ));
        }
        let offered = self.view().dial.filter(|d| d.source == source);
        let Some(offered) = offered else {
            return Err(err(
                ErrorCode::NotFound,
                "dialing is not available right now; nothing was sent",
            ));
        };
        let result = match offered.path {
            ControlPath::Link => self.send_link(source, LinkAction::Dial, Some(number)),
            ControlPath::Hfp => self.send_host(
                source,
                &CallCommand::Dial {
                    number: number.clone(),
                },
            ),
        };
        let mut inner = self.inner();
        match &result {
            Ok(()) => inner.notice = None,
            Err(e) => inner.notice = Some(e.message.clone()),
        }
        result
    }

    fn send_link(
        &self,
        source: &str,
        wire: LinkAction,
        dial: Option<&Sensitive>,
    ) -> Result<(), ErrorBody> {
        let device_id = source
            .strip_prefix(LINK_PREFIX)
            .ok_or_else(|| err(ErrorCode::BadRequest, "unknown call source"))?;
        let mut inner = self.inner();
        inner.next_request += 1;
        let request_id = format!("c{}", inner.next_request);
        let phone = inner
            .links
            .get_mut(device_id)
            .ok_or_else(|| err(ErrorCode::NotConnected, "the phone is not connected"))?;
        phone
            .pending
            .retain(|(_, _, at)| at.elapsed() < PENDING_TTL);
        if phone.pending.len() >= MAX_PENDING {
            return Err(err(ErrorCode::Busy, "the phone has not answered yet"));
        }
        phone
            .tx
            .try_send(LinkRequest {
                request_id: request_id.clone(),
                action: wire,
                number: dial.cloned(),
            })
            .map_err(|_| err(ErrorCode::NotConnected, "the phone link is busy or closed"))?;
        phone.pending.push((request_id, wire, Instant::now()));
        Ok(())
    }

    fn send_host(&self, source: &str, command: &CallCommand) -> Result<(), ErrorBody> {
        let (reply, wait) = std_mpsc::sync_channel(1);
        {
            let host = self.host.lock().unwrap_or_else(|e| e.into_inner());
            let host = host
                .as_ref()
                .ok_or_else(|| err(ErrorCode::Unsupported, "Bluetooth calling is not available"))?;
            host.commands
                .send(HostCommand {
                    source: source.to_owned(),
                    command: command.clone(),
                    reply,
                })
                .map_err(|_| err(ErrorCode::Failed, "Bluetooth calling stopped"))?;
        }
        match wait.recv_timeout(HOST_COMMAND_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(why)) => Err(err(ErrorCode::Failed, &why)),
            Err(_) => Err(err(
                ErrorCode::Failed,
                "Bluetooth calling did not answer in time; check the phone before retrying",
            )),
        }
    }

    // -------------------------------------------------------------- ringing

    /// Show a desktop notification once per newly ringing call and forget calls that are gone.
    fn announce_new_calls(self: &Arc<Self>) {
        let view = self.view();
        let fresh: Vec<_> = {
            let mut inner = self.inner();
            let ringing: BTreeSet<String> = view
                .rows
                .iter()
                .filter(|r| r.ringing())
                .map(|r| r.key.clone())
                .collect();
            let before = inner.announced.len();
            inner.announced.retain(|k| ringing.contains(k));
            if inner.announced.len() != before {
                crate::tray::poke(); // a call stopped ringing
            }
            inner.unnamed_since.retain(|k, _| ringing.contains(k));
            let mut recheck = false;
            let mut fresh = Vec::new();
            for r in view.rows.iter().filter(|r| r.ringing()) {
                if inner.announced.contains(&r.key) {
                    continue;
                }
                if r.name.is_none() && r.number.is_none() {
                    let since = *inner
                        .unnamed_since
                        .entry(r.key.clone())
                        .or_insert_with(Instant::now);
                    if since.elapsed() < UNNAMED_GRACE {
                        recheck = true;
                        continue;
                    }
                }
                inner.announced.insert(r.key.clone());
                crate::tray::poke(); // a call started ringing
                fresh.push(r.clone());
            }
            if recheck {
                let hub = self.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(100));
                    hub.announce_new_calls();
                });
            }
            fresh
        };
        let Some(notifier) = self
            .notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        for row in fresh {
            let hub = self.clone();
            let answer = row.answer.clone();
            let reject = row.reject.clone();
            let (can_answer, can_decline) = (answer.is_some(), reject.is_some());
            notifier.ringing(
                &row.caller_label(),
                can_answer,
                can_decline,
                Box::new(move |choice| {
                    let _ = match (choice, answer, reject) {
                        (RingChoice::Answer, Some(t), _) => hub.act(&t, CallAction::Answer),
                        (RingChoice::Decline, _, Some(t)) => hub.act(&t, CallAction::Reject),
                        _ => Ok(()),
                    };
                }),
            );
        }
    }
}

// ------------------------------------------------------------------ Linux host

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use bridge_calls::{DeviceRef, Session};
    use bridge_core::calls::{CallFeature, UnsupportedReason};

    /// This computer as the Bluetooth hands-free unit, through `bridge-calls` (PipeWire telephony).
    pub struct LinuxHfp {
        session: Session,
        devices: BTreeMap<String, DeviceRef>,
    }
    impl LinuxHfp {
        pub fn new(session: Session) -> Self {
            Self {
                session,
                devices: BTreeMap::new(),
            }
        }
    }
    impl HostCalls for LinuxHfp {
        fn label(&self) -> String {
            "Bluetooth hands-free (Linux)".into()
        }
        fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
            self.session.pump(wait).map_err(|e| e.to_string())?;
            self.devices.clear();
            let mut out = Vec::new();
            for (device, state, caps) in self.session.phones() {
                let key = format!("hfp:{}", device.path);
                let mut caps = caps.clone();
                // The telephony API has no microphone mute; mute is a host audio-graph matter.
                caps.set(
                    CallFeature::MuteMicrophone,
                    Support::Unsupported {
                        reason: UnsupportedReason::NoPlatformApi,
                    },
                );
                self.devices.insert(key.clone(), device.clone());
                out.push(SourceSnapshot {
                    key,
                    label: "Bluetooth phone".into(),
                    caps,
                    state: state.clone(),
                });
            }
            Ok(out)
        }
        fn execute(&mut self, source: &str, command: &CallCommand) -> Result<(), String> {
            let device = self
                .devices
                .get(source)
                .ok_or_else(|| "that phone is no longer connected over Bluetooth".to_owned())?;
            match command {
                CallCommand::Reject { call } => self.session.reject(device, call),
                other => self.session.execute(device, other),
            }
            .map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::calls::{
        AudioCodec, AudioLinkState, CallCapabilities, CallDirection, CallEvent, CallId, CallParty,
        CallPhase, HfpEvidence, HfpHost, LinkState, Sensitive,
    };
    use bridge_core::calls_link::LinkFailure;
    use serde_json::{Map, Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn body(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_displaced_session_does_not_remove_its_successor() {
        let hub = CallHub::new();
        let (_old_rx, old) = hub.link_up_generation("dev", "Pixel", true);
        let (_new_rx, new) = hub.link_up_generation("dev", "Pixel", true);
        assert_ne!(old, new);
        hub.link_down_generation("dev", old);
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true}))
        ));
        assert!(hub.view().ringing().is_some(), "successor must survive");
        hub.link_down_generation("dev", new);
        assert!(hub.view().ringing().is_none());
    }

    #[test]
    fn link_ring_answer_and_end_flow() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        assert!(hub.view().rows.is_empty());
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+155","name":"Ada","canAnswer":true,"canEnd":true}))
        ));
        let v = hub.view();
        let row = v.ringing().unwrap();
        assert_eq!(row.caller_label(), "Ada");
        let target = row.answer.clone().unwrap();
        assert_eq!(target.path, ControlPath::Link);

        hub.act(&target, CallAction::Answer).unwrap();
        let req = rx.try_recv().unwrap();
        assert_eq!(req.action, LinkAction::Answer);
        // The phone confirms, then reports the call as active.
        assert!(hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&req.request_id, Ok(()))
        ));
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"offhook","canAnswer":true,"canEnd":true})),
        );
        let v = hub.view();
        assert!(v.ringing().is_none());
        let live = v.live().unwrap();
        assert!(live.hangup.is_some());
        assert!(
            v.audio
                .hint
                .as_deref()
                .unwrap()
                .contains("Answered on the phone")
        );

        let hang = live.hangup.clone().unwrap();
        hub.act(&hang, CallAction::Hangup).unwrap();
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Hangup);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().rows.is_empty());
    }

    #[test]
    fn stale_or_forged_targets_send_nothing() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        let target = hub.view().ringing().unwrap().answer.clone().unwrap();
        // Wrong action for the slot.
        assert!(hub.act(&target, CallAction::Hangup).is_err());
        // The call ended in between.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.act(&target, CallAction::Answer).is_err());
        let mut forged = target.clone();
        forged.source = "link:other".into();
        assert!(hub.act(&forged, CallAction::Answer).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn nothing_is_offered_without_consent_or_phone_permission() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", false);
        // Without consent the phone's report is ignored entirely.
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true}))
        ));
        assert!(hub.view().rows.is_empty());
        let st = hub.status();
        assert!(st.sources.iter().any(|l| l.contains("allow Calls")));

        hub.set_enabled("dev", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":false,"canEnd":false})),
        );
        let v = hub.view();
        let row = v.ringing().unwrap();
        assert!(row.answer.is_none() && row.reject.is_none());
        assert!(row.missing_control.is_some());
        assert!(rx.try_recv().is_err());

        hub.set_enabled("dev", false);
        assert!(hub.view().rows.is_empty());
    }

    #[test]
    fn refusal_from_the_phone_becomes_a_notice() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        let t = hub.view().ringing().unwrap().answer.clone().unwrap();
        hub.act(&t, CallAction::Answer).unwrap();
        let id = rx.try_recv().unwrap().request_id;
        hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&id, Err(LinkFailure::PermissionMissing)),
        );
        assert!(hub.status().notice.unwrap().contains("not allowed"));
        // An unsolicited result is misbehaviour, a malformed state too.
        assert!(!hub.on_packet("dev", link::RESULT, &link::result_body("zz", Ok(()))));
        assert!(!hub.on_packet("dev", link::STATE, &body(json!({"state":"bogus"}))));
    }

    #[test]
    fn link_down_ends_the_call_view() {
        let hub = CallHub::new();
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().ringing().is_some());
        hub.link_down("dev");
        assert!(hub.view().rows.is_empty());
    }

    // ---- a scripted host backend

    struct FakeHost {
        state: Arc<Mutex<SourceSnapshot>>,
        log: Arc<Mutex<Vec<CallCommand>>>,
    }
    impl HostCalls for FakeHost {
        fn label(&self) -> String {
            "Fake Bluetooth".into()
        }
        fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
            std::thread::sleep(wait.min(Duration::from_millis(10)));
            Ok(vec![self.state.lock().unwrap().clone()])
        }
        fn execute(&mut self, _source: &str, command: &CallCommand) -> Result<(), String> {
            self.log.lock().unwrap().push(command.clone());
            Ok(())
        }
    }
    fn host_snapshot(phase: CallPhase, audio: bool) -> SourceSnapshot {
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::LinuxPipeWire,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                host_call_control_api: true,
                host_audio_control_api: true,
                ..Default::default()
            },
        );
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        state.apply(CallEvent::CallUpdated {
            id: CallId::new("c1"),
            direction: CallDirection::Incoming,
            phase,
            party: CallParty {
                number: Some(Sensitive::new("+1555")),
                name: None,
            },
        });
        if audio {
            state.apply(CallEvent::Audio {
                state: AudioLinkState::Connected {
                    codec: AudioCodec::Msbc,
                },
            });
        }
        SourceSnapshot {
            key: "hfp:ag1".into(),
            label: "Bluetooth phone".into(),
            caps,
            state,
        }
    }

    fn wait_for(hub: &CallHub, f: impl Fn(&CallsView) -> bool) -> CallsView {
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let v = hub.view();
            if f(&v) {
                return v;
            }
            assert!(Instant::now() < end, "view never became ready: {v:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn host_backend_answers_rejects_and_notifies_once() {
        struct Count(Arc<AtomicUsize>);
        impl CallNotifier for Count {
            fn ringing(&self, _: &str, a: bool, d: bool, _: Box<dyn FnOnce(RingChoice) + Send>) {
                assert!(a && d);
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let rings = Arc::new(AtomicUsize::new(0));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Count(rings.clone())));
        let state = Arc::new(Mutex::new(host_snapshot(CallPhase::Incoming, false)));
        let log = Arc::new(Mutex::new(Vec::new()));
        hub.start_host(Box::new(FakeHost {
            state: state.clone(),
            log: log.clone(),
        }));
        let v = wait_for(&hub, |v| v.ringing().is_some());
        let row = v.ringing().unwrap();
        assert_eq!(row.answer.as_ref().unwrap().path, ControlPath::Hfp);

        hub.act(&row.reject.clone().unwrap(), CallAction::Reject)
            .unwrap();
        hub.act(&row.answer.clone().unwrap(), CallAction::Answer)
            .unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                CallCommand::Reject {
                    call: CallId::new("c1")
                },
                CallCommand::Answer {
                    call: CallId::new("c1")
                }
            ]
        );
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(rings.load(Ordering::SeqCst), 1, "one notification per call");

        // The call goes live with audio on the computer: mute and hang up are offered.
        *state.lock().unwrap() = host_snapshot(CallPhase::Active, true);
        let v = wait_for(&hub, |v| v.live().is_some());
        hub.act(
            &v.live().unwrap().hangup.clone().unwrap(),
            CallAction::Hangup,
        )
        .unwrap();
        assert_eq!(
            log.lock().unwrap().last(),
            Some(&CallCommand::Hangup {
                call: CallId::new("c1")
            })
        );
    }

    #[test]
    fn a_caller_label_cannot_become_an_option_or_markup() {
        // A caller-ID name is chosen by the caller (S2-3 in docs/reports/security-review-2.md).
        let a = NotifySendCalls::args("--action=answer=Ignore me", true, true);
        let dashdash = a.iter().position(|s| s == "--").expect("positional marker");
        assert!(a[dashdash + 1..].iter().any(|s| s == "Incoming call"));
        assert!(a[..dashdash].iter().all(|s| !s.contains("Ignore me")));
        let b = NotifySendCalls::args("<a href=\"http://evil\">Bank</a>", false, false);
        assert!(!b.last().unwrap().contains(['<', '>']));
    }

    #[test]
    fn notify_send_arguments_follow_the_available_controls() {
        let a = NotifySendCalls::args("Ada\nLovelace", true, true);
        assert!(a.contains(&"--action=answer=Answer".to_owned()));
        assert!(a.contains(&"--action=decline=Decline".to_owned()));
        assert_eq!(a.last().unwrap(), "AdaLovelace");
        let b = NotifySendCalls::args("x", false, false);
        assert!(!b.iter().any(|s| s.starts_with("--action")) && !b.contains(&"--wait".to_owned()));
    }

    // ---- test-gaps-4: dialing from this computer, the unnamed-caller grace and the pending-request cap

    /// A consenting phone that is idle and says it may place calls.
    fn dialer(hub: &Arc<CallHub>, id: &str) -> mpsc::Receiver<LinkRequest> {
        let rx = hub.link_up(id, "Pixel", true);
        assert!(hub.on_packet(
            id,
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true,"canDial":true}))
        ));
        rx
    }

    fn number(s: &str) -> Sensitive {
        Sensitive::new(s)
    }

    #[test]
    fn dial_over_the_link_sends_exactly_the_number_once_and_a_refusal_becomes_a_notice() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let dial = hub.view().dial.expect("dial offered");
        assert_eq!(
            (dial.source.as_str(), dial.path),
            ("link:dev", ControlPath::Link)
        );
        assert_eq!(dial.phone, "Pixel");
        hub.dial(&dial.source, &number("+15550142")).unwrap();
        let req = rx.try_recv().unwrap();
        assert_eq!(req.action, LinkAction::Dial);
        assert_eq!(req.number.as_ref().unwrap().expose(), "+15550142");
        assert!(rx.try_recv().is_err(), "one request per dial");
        assert!(hub.status().notice.is_none());
        // The phone refuses (CALL_PHONE revoked meanwhile): the person sees why, the number is not logged.
        assert!(hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&req.request_id, Err(LinkFailure::PermissionMissing))
        ));
        let notice = hub.status().notice.unwrap();
        assert!(notice.contains("not allowed") && !notice.contains("5550142"));
        // A confirmed dial clears an older notice when the next one succeeds.
        hub.dial(&dial.source, &number("112")).unwrap();
        assert!(hub.status().notice.is_none());
        assert_eq!(rx.try_recv().unwrap().number.unwrap().expose(), "112");
    }

    #[test]
    fn dial_rejects_anything_that_is_not_a_plain_number_before_looking_at_the_source() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let too_long = "1".repeat(link::MAX_DIAL_DIGITS + 1);
        for bad in [
            "",
            "+",
            "12 34",
            "*21#",
            "#31#",
            "1,2",
            "12;3",
            "++1",
            "1+2",
            "abc",
            "\u{0661}\u{0662}",
            "12\n",
            &too_long,
        ] {
            let e = hub.dial("link:dev", &number(bad)).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest, "{bad:?}");
            // Same answer for a source that does not exist: the number is judged first.
            assert_eq!(
                hub.dial("nope", &number(bad)).unwrap_err().code,
                ErrorCode::BadRequest
            );
        }
        assert!(rx.try_recv().is_err(), "nothing was sent for any of them");
        assert!(
            hub.status().notice.is_none(),
            "a malformed number is not a phone refusal"
        );
        // The longest allowed number goes through.
        hub.dial(
            "link:dev",
            &number(&format!("+{}", "9".repeat(link::MAX_DIAL_DIGITS))),
        )
        .unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dial_is_refused_unless_the_current_view_offers_that_exact_source() {
        let hub = CallHub::new();
        // No phone at all.
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        // A phone without the permission to place calls.
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        // Allowed on the phone, but the computer withdrew Telephony consent: also nothing.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        assert!(hub.view().dial.is_some());
        hub.set_enabled("dev", false);
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.set_enabled("dev", true);
        // consent back on, but the phone must report again before anything is offered
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        // A forged or foreign source never dials, and a hands-free source is not the link.
        for source in ["link:other", "link:", "hfp:dev", "dev", "", "LINK:dev"] {
            assert_eq!(
                hub.dial(source, &number("112")).unwrap_err().code,
                ErrorCode::NotFound,
                "{source}"
            );
        }
        assert!(rx.try_recv().is_err());
        hub.dial("link:dev", &number("112")).unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dial_is_not_offered_during_a_call_and_returns_when_it_ends() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+155","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"offhook","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        assert!(
            hub.dial("link:dev", &number("112")).is_err(),
            "a live call blocks a second one"
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        hub.dial("link:dev", &number("112")).unwrap();
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Dial);
    }

    #[test]
    fn dial_with_two_phones_goes_to_the_offered_one_only() {
        let hub = CallHub::new();
        let mut a = dialer(&hub, "a");
        let mut b = dialer(&hub, "b");
        let offered = hub.view().dial.unwrap().source;
        hub.dial(&offered, &number("112")).unwrap();
        let (got_a, got_b) = (a.try_recv().is_ok(), b.try_recv().is_ok());
        assert!(got_a ^ got_b, "exactly one phone dials, not both");
        // The other phone's source is not what the view offers, so it is refused.
        let other = if offered == "link:a" {
            "link:b"
        } else {
            "link:a"
        };
        assert_eq!(
            hub.dial(other, &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_phone_that_stops_answering_cannot_be_flooded_with_dials() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let mut ids = Vec::new();
        for _ in 0..MAX_PENDING {
            hub.dial("link:dev", &number("112")).unwrap();
            ids.push(rx.try_recv().unwrap().request_id);
        }
        let e = hub.dial("link:dev", &number("112")).unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        assert!(rx.try_recv().is_err(), "the refused dial was not queued");
        assert_eq!(
            hub.status().notice.as_deref(),
            Some("the phone has not answered yet")
        );
        // Request ids are unique, so an answer can only match its own request.
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        // One answer frees one slot, and an answer is accepted once only.
        assert!(hub.on_packet("dev", link::RESULT, &link::result_body(&ids[0], Ok(()))));
        assert!(!hub.on_packet("dev", link::RESULT, &link::result_body(&ids[0], Ok(()))));
        hub.dial("link:dev", &number("112")).unwrap();
        assert!(hub.status().notice.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::Busy
        );
        // Turning Telephony off forgets what was pending, so a re-enabled phone starts clean.
        hub.set_enabled("dev", false);
        hub.set_enabled("dev", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        hub.dial("link:dev", &number("112")).unwrap();
    }

    #[test]
    fn dial_on_a_dead_link_reports_not_connected_and_queues_nothing() {
        let hub = CallHub::new();
        let rx = dialer(&hub, "dev");
        drop(rx); // the link task is gone
        let e = hub.dial("link:dev", &number("112")).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotConnected);
        assert!(hub.status().notice.is_some());
        // The failed send must not use up one of the pending slots.
        for _ in 0..MAX_PENDING + 2 {
            assert_eq!(
                hub.dial("link:dev", &number("112")).unwrap_err().code,
                ErrorCode::NotConnected
            );
        }
    }

    #[test]
    fn a_full_link_queue_is_reported_instead_of_blocking() {
        let hub = CallHub::new();
        let _rx = dialer(&hub, "dev"); // never drained: capacity 8, pending cap 4
        // Answer each one so the pending cap is never what stops us; the channel is.
        let mut refused = None;
        for _ in 0..20 {
            match hub.dial("link:dev", &number("112")) {
                Ok(()) => {
                    let id = format!("c{}", hub.inner().next_request);
                    hub.on_packet("dev", link::RESULT, &link::result_body(&id, Ok(())));
                }
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        let e = refused.expect("the bounded queue pushed back");
        assert_eq!(e.code, ErrorCode::NotConnected);
        assert!(e.message.contains("busy or closed"));
    }

    #[test]
    fn dial_through_hands_free_runs_on_the_host_and_a_host_failure_is_shown() {
        use bridge_core::calls::{CallFeature, LinkState, Support};
        struct Flaky {
            snap: SourceSnapshot,
            log: Arc<Mutex<Vec<CallCommand>>>,
            fail: Arc<Mutex<Option<String>>>,
        }
        impl HostCalls for Flaky {
            fn label(&self) -> String {
                "Flaky".into()
            }
            fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
                std::thread::sleep(wait.min(Duration::from_millis(10)));
                Ok(vec![self.snap.clone()])
            }
            fn execute(&mut self, _s: &str, c: &CallCommand) -> Result<(), String> {
                self.log.lock().unwrap().push(c.clone());
                match self.fail.lock().unwrap().clone() {
                    Some(why) => Err(why),
                    None => Ok(()),
                }
            }
        }
        let mut snap = host_snapshot(CallPhase::Active, false);
        snap.state = CallState::default();
        snap.state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        snap.caps.set(CallFeature::Dial, Support::Declared);
        let (log, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(None)));
        let hub = CallHub::new();
        hub.start_host(Box::new(Flaky {
            snap,
            log: log.clone(),
            fail: fail.clone(),
        }));
        let v = wait_for(&hub, |v| v.dial.is_some());
        let dial = v.dial.unwrap();
        assert_eq!(
            (dial.source.as_str(), dial.path),
            ("hfp:ag1", ControlPath::Hfp)
        );
        hub.dial(&dial.source, &number("+15550142")).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![CallCommand::Dial {
                number: number("+15550142")
            }]
        );
        // The stack refuses: the failure reaches the person and nothing else was attempted.
        *fail.lock().unwrap() = Some("adapter busy".into());
        let e = hub.dial(&dial.source, &number("112")).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Failed, "adapter busy")
        );
        assert_eq!(hub.status().notice.as_deref(), Some("adapter busy"));
        // An invalid number never reaches the host.
        let before = log.lock().unwrap().len();
        assert!(hub.dial(&dial.source, &number("*21#")).is_err());
        assert_eq!(log.lock().unwrap().len(), before);
    }

    #[test]
    fn host_commands_without_a_host_fail_plainly() {
        let hub = CallHub::new();
        let e = hub
            .send_host("hfp:x", &CallCommand::ConnectAudio)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert_eq!(
            hub.mute("hfp:x", true).unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert_eq!(
            hub.connect_audio("hfp:x").unwrap_err().code,
            ErrorCode::Unsupported
        );
        // A source that is not a link source cannot be sent a link request.
        assert_eq!(
            hub.send_link("hfp:x", LinkAction::Answer, None)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            hub.send_link("link:gone", LinkAction::Answer, None)
                .unwrap_err()
                .code,
            ErrorCode::NotConnected
        );
    }

    struct Rings(Arc<Mutex<Vec<String>>>);
    impl CallNotifier for Rings {
        fn ringing(&self, caller: &str, _: bool, _: bool, _: Box<dyn FnOnce(RingChoice) + Send>) {
            self.0.lock().unwrap().push(caller.to_owned());
        }
    }

    #[test]
    fn a_ring_without_a_caller_waits_for_the_number_and_is_announced_once() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        // "ringing" arrives first, the number follows within the grace period.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        assert!(
            rings.lock().unwrap().is_empty(),
            "held back while the caller is unknown"
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(*rings.lock().unwrap(), vec!["+15550142".to_owned()]);
        // The delayed re-check that was queued finds it already announced and says nothing more.
        std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(400));
        assert_eq!(rings.lock().unwrap().len(), 1);
        // Repeated identical reports do not repeat the banner.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(rings.lock().unwrap().len(), 1);
        // The call ends; a later call from the same number rings (and is announced) again.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(rings.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_private_number_is_announced_as_unknown_after_the_grace_period_not_before() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        std::thread::sleep(UNNAMED_GRACE / 2);
        assert!(rings.lock().unwrap().is_empty());
        let end = Instant::now() + Duration::from_secs(5);
        while rings.lock().unwrap().is_empty() {
            assert!(Instant::now() < end, "never announced");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*rings.lock().unwrap(), vec!["Unknown caller".to_owned()]);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(rings.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_call_that_stops_ringing_during_the_grace_period_is_never_announced() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(400));
        assert!(
            rings.lock().unwrap().is_empty(),
            "a missed call is not announced after it ended"
        );
    }

    #[test]
    fn the_banner_buttons_follow_the_controls_and_press_through_the_hub() {
        type Reply = Box<dyn FnOnce(RingChoice) + Send>;
        struct Press(Mutex<Option<Reply>>);
        impl CallNotifier for Press {
            fn ringing(
                &self,
                _: &str,
                a: bool,
                d: bool,
                reply: Box<dyn FnOnce(RingChoice) + Send>,
            ) {
                assert!(a && d);
                *self.0.lock().unwrap() = Some(reply);
            }
        }
        let p = Arc::new(Press(Mutex::new(None)));
        let hub = CallHub::new();
        hub.set_notifier(p.clone());
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","name":"Ada","canAnswer":true,"canEnd":true})),
        );
        let reply = p.0.lock().unwrap().take().expect("banner shown");
        reply(RingChoice::Decline);
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Reject);
        // A late press on a stale banner after the call ended sends nothing.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","name":"Bob","canAnswer":true,"canEnd":true})),
        );
        let fresh = p.0.lock().unwrap().take().unwrap();
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        fresh(RingChoice::Answer);
        assert!(rx.try_recv().is_err());
    }
}
