// SPDX-License-Identifier: GPL-3.0-or-later
//! The whole incoming/outgoing call chain on one machine, deterministic, no Bluetooth, no display:
//!
//! * a **stand-in phone** that is a real `bridge_kde` endpoint (TLS over loopback) and behaves like
//!   the Android app + its Rust core: three call-state sources (`default`, `sub2/slot1`, `broadcast`)
//!   merged (ringing > off-hook > idle) and reported only on change, the last report greeted to a
//!   new connection and resent on a `report` ask, and `answer` / `reject` / `hangup` / `dial`
//!   carried out like `TelecomManager` (a request for a call that is not there is `no_such_call`);
//! * the **real service** (`Daemon`, control API, `CallHub`) with
//! * a **fake Bluetooth hands-free host** that mirrors the same phone (optionally a moment late, as
//!   real HFP indicators are) and records the audio routing the real host would do (answer or dial
//!   here: the voice comes here; call over: restored);
//! * a **fake card launcher** counting `geekbridge-calls --overlay` starts and kills, and a
//!   **recording banner notifier** whose Answer/Decline buttons the test can press late.
//!
//! Each test drives one scripted call timeline and checks what a user would see: cards started and
//! stopped, banners, rows in the call list, requests the phone got, audio routes, notices.

mod support;

use bridge_core::calls::{
    AudioCodec, AudioLinkState, CallCommand, CallDirection, CallEvent, CallId, CallParty,
    CallPhase, CallState, HfpEvidence, HfpHost, LinkState,
};
use bridge_core::calls::{CallCapabilities, Sensitive};
use bridge_core::calls_link::{self as link, LinkAction, LinkCallReport, LinkFailure, PhoneState};
use bridge_core::calls_view::{ControlPath, SourceSnapshot};
use bridge_core::ipc::{CallAction, CallsStatus, Feature, Request, Response};
use bridge_kde as kde;
use bridge_service::call_control::{
    CallNotifier, HostCalls, RingChoice, STALE_CALL_NOTICE, Timings,
};
use bridge_service::call_overlay::{Cannot, Card, Launcher};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::{Phone, Rig, pair_phone, reconnect};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

const SOURCES: [&str; 3] = ["default", "sub2/slot1", "broadcast"];

fn timings() -> Timings {
    Timings {
        tick: Duration::from_millis(50),
        ring_quiet: Duration::from_millis(1500),
        live_quiet: Duration::from_millis(3000),
        probe_grace: Duration::from_millis(1000),
        probes: 2,
        flap_memory: Duration::from_secs(20),
        overlay_linger: Duration::from_millis(800),
    }
}

// ---------------------------------------------------------------- the card launcher

#[derive(Default)]
struct Cards {
    launches: AtomicUsize,
    kills: AtomicUsize,
    fail: AtomicBool,
    /// One flag per launched card: running while true.
    alive: Mutex<Vec<Arc<AtomicBool>>>,
}
struct FakeCard {
    alive: Arc<AtomicBool>,
    cards: Arc<Cards>,
}
impl Card for FakeCard {
    fn running(&mut self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    fn kill(&mut self) {
        if self.alive.swap(false, Ordering::SeqCst) {
            self.cards.kills.fetch_add(1, Ordering::SeqCst);
        }
    }
}
struct FakeLauncher(Arc<Cards>);
impl Launcher for FakeLauncher {
    fn launch(&self) -> Result<Box<dyn Card>, Cannot> {
        self.0.launches.fetch_add(1, Ordering::SeqCst);
        if self.0.fail.load(Ordering::SeqCst) {
            return Err(Cannot::NoDisplay);
        }
        let alive = Arc::new(AtomicBool::new(true));
        self.0.alive.lock().unwrap().push(alive.clone());
        Ok(Box::new(FakeCard {
            alive,
            cards: self.0.clone(),
        }))
    }
}
impl Cards {
    fn launches(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
    }
    fn kills(&self) -> usize {
        self.kills.load(Ordering::SeqCst)
    }
    fn running(&self) -> usize {
        self.alive
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.load(Ordering::SeqCst))
            .count()
    }
    /// The card closes itself (what the real one does once the ring is over).
    fn close_all(&self) {
        for a in self.alive.lock().unwrap().iter() {
            a.store(false, Ordering::SeqCst);
        }
    }
}

// ---------------------------------------------------------------- the banner

type Reply = Box<dyn FnOnce(RingChoice) + Send>;
#[derive(Default)]
struct Banners {
    shown: Mutex<Vec<(String, bool, bool)>>,
    /// The "Decline with message" texts each banner offered (empty = no such buttons).
    texts: Mutex<Vec<Vec<String>>>,
    replies: Mutex<Vec<Option<Reply>>>,
}
struct FakeNotifier(Arc<Banners>);
impl CallNotifier for FakeNotifier {
    fn ringing(&self, caller: &str, can_answer: bool, can_decline: bool, reply: Reply) {
        self.ringing_with_replies(caller, can_answer, can_decline, &[], reply);
    }
    fn ringing_with_replies(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        texts: &[String],
        reply: Reply,
    ) {
        self.0.texts.lock().unwrap().push(texts.to_vec());
        self.0
            .shown
            .lock()
            .unwrap()
            .push((caller.to_owned(), can_answer, can_decline));
        self.0.replies.lock().unwrap().push(Some(reply));
    }
}
impl Banners {
    fn count(&self) -> usize {
        self.shown.lock().unwrap().len()
    }
    /// Press a button on banner `i` (0 = first). Blocking work, like the real notify-send thread.
    async fn press(&self, i: usize, choice: RingChoice) {
        let reply = self.replies.lock().unwrap()[i]
            .take()
            .expect("pressed twice");
        tokio::task::spawn_blocking(move || reply(choice))
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------- the phone

/// The phone as the Bluetooth side and the test touch it: the state lives in the model (like the
/// phone's own telephony), the packets it causes go out only while the link is up (like the core,
/// which sends to connected computers only and greets the next one with the latest state).
#[derive(Clone)]
struct PhoneHandle {
    model: Arc<Mutex<Model>>,
    tx: mpsc::UnboundedSender<Vec<kde::Packet>>,
    connected: Arc<AtomicBool>,
}
impl PhoneHandle {
    fn apply(&self, f: impl FnOnce(&mut Model) -> Vec<kde::Packet>) {
        let out = f(&mut self.model.lock().unwrap());
        if !out.is_empty() && self.connected.load(Ordering::SeqCst) {
            let _ = self.tx.send(out);
        }
    }
}

struct Model {
    sources: BTreeMap<&'static str, PhoneState>,
    /// `None` until the first report (the app's merger starts empty, so its first idle is sent).
    merged: Option<PhoneState>,
    /// The core's last report (greeted to new connections, resent on `report`).
    last: Option<Map<String, Value>>,
    name: Option<&'static str>,
    /// The caller's number in the ring report (the phone shares it when READ_CALL_LOG is granted).
    number: Option<&'static str>,
    /// Texts the phone was asked to send: (address, body).
    texts: Vec<(String, String)>,
    can_dial: bool,
    /// The phone says it can play keypad tones (an older app does not).
    can_tones: bool,
    /// Keypad tones the stand-in played into the live call, in order.
    tones: Vec<char>,
    /// Sends nothing at all (a frozen app on a link that still looks up).
    silent: bool,
    /// How long the phone takes to act on a request (TelecomManager is not instant).
    slow: Duration,
    requests: Vec<LinkAction>,
    states_sent: Vec<PhoneState>,
    hfp: Option<Arc<Mutex<HfpWorld>>>,
}

impl Model {
    fn report(&self, state: PhoneState) -> Map<String, Value> {
        LinkCallReport {
            state,
            number: (state != PhoneState::Idle)
                .then_some(())
                .and(self.number.map(Sensitive::new)),
            name: (state != PhoneState::Idle)
                .then_some(())
                .and(self.name.map(Sensitive::new)),
            can_answer: true,
            can_end: true,
            can_dial: self.can_dial,
            can_tones: self.can_tones,
        }
        .body()
    }

    fn merge(&self) -> PhoneState {
        let v: Vec<_> = self.sources.values().copied().collect();
        if v.contains(&PhoneState::Ringing) {
            PhoneState::Ringing
        } else if v.contains(&PhoneState::OffHook) {
            PhoneState::OffHook
        } else {
            PhoneState::Idle
        }
    }

    fn set_source(&mut self, source: &'static str, state: PhoneState) -> Vec<kde::Packet> {
        self.sources.insert(source, state);
        let m = self.merge();
        if Some(m) == self.merged {
            return Vec::new();
        }
        self.merged = Some(m);
        let body = self.report(m);
        self.last = Some(body.clone());
        self.states_sent.push(m);
        if let Some(h) = &self.hfp {
            h.lock().unwrap().phone_changed(m);
        }
        vec![kde::Packet::new(link::STATE, body)]
    }

    fn all(&mut self, state: PhoneState) -> Vec<kde::Packet> {
        SOURCES
            .iter()
            .flat_map(|s| self.set_source(s, state))
            .collect()
    }

    /// What `TelecomManager` would do; `Err` = nothing to act on.
    fn act(&mut self, action: LinkAction) -> Result<Vec<kde::Packet>, LinkFailure> {
        match (action, self.merged.unwrap_or(PhoneState::Idle)) {
            (LinkAction::Answer, PhoneState::Ringing) => Ok(self.all(PhoneState::OffHook)),
            (LinkAction::Reject, PhoneState::Ringing) => Ok(self.all(PhoneState::Idle)),
            (LinkAction::Hangup, PhoneState::OffHook) => Ok(self.all(PhoneState::Idle)),
            (LinkAction::Dial, PhoneState::Idle) => Ok(self.all(PhoneState::OffHook)),
            _ => Err(LinkFailure::NoSuchCall),
        }
    }

    fn request(&mut self, body: &Map<String, Value>) -> Vec<kde::Packet> {
        let (id, action) = link::parse_request(body).unwrap();
        self.requests.push(action);
        if action == LinkAction::Report {
            return self
                .last
                .clone()
                .map(|b| kde::Packet::new(link::STATE, b))
                .into_iter()
                .collect();
        }
        let (mut out, result) = match if action == LinkAction::Tone {
            // The core already refused anything but one of 0-9 * # (parse_request above).
            let tone = link::parse_tone(body).unwrap();
            match self.merged {
                Some(PhoneState::OffHook) if self.can_tones => {
                    self.tones.push(tone);
                    Ok(Vec::new())
                }
                _ => Err(LinkFailure::NoSuchCall),
            }
        } else {
            self.act(action)
        } {
            Ok(states) => (states, Ok(())),
            Err(f) => (Vec::new(), Err(f)),
        };
        // Android: the result goes out after TelecomManager returned; the state change follows
        // from the listener. Here the result comes first, then the states.
        out.insert(
            0,
            kde::Packet::new(link::RESULT, link::result_body(&id, result)),
        );
        out
    }
}

struct SimPhone {
    phone: Arc<Phone>,
    model: Arc<Mutex<Model>>,
    handle: PhoneHandle,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<kde::Packet>>>>,
    stop: Option<oneshot::Sender<()>>,
}

fn calls_phone(name: &str, takes_texts: bool) -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        name,
        kde::DeviceType::Phone,
        ["kdeconnect.ping", link::REQUEST]
            .into_iter()
            .chain(takes_texts.then_some(bridge_kde::sms::REQUEST))
            .map(String::from)
            .collect(),
        vec![
            "kdeconnect.ping".into(),
            link::STATE.into(),
            link::RESULT.into(),
        ],
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

async fn send_all(link: &mut kde::TrustedLink, model: &Mutex<Model>, packets: Vec<kde::Packet>) {
    if model.lock().unwrap().silent {
        return;
    }
    for p in packets {
        if link.send(&p).await.is_err() {
            return;
        }
    }
}

async fn run_phone(
    mut link: kde::TrustedLink,
    tx: mpsc::UnboundedSender<Vec<kde::Packet>>,
    model: Arc<Mutex<Model>>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<Vec<kde::Packet>>>>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut rx = rx.lock().await;
    // The core greets a new connection with the last report.
    let greet = model.lock().unwrap().last.clone();
    if let Some(b) = greet {
        send_all(&mut link, &model, vec![kde::Packet::new(link::STATE, b)]).await;
    }
    loop {
        tokio::select! {
            _ = &mut stop => return,
            ev = link.recv() => match ev {
                Ok(kde::LinkEvent::Packet(p)) if p.kind == link::REQUEST => {
                    let slow = model.lock().unwrap().slow;
                    sleep(slow).await;
                    let out = model.lock().unwrap().request(&p.body);
                    // One queue for everything the phone sends, as in the core (`Ctl`): a
                    // reply never overtakes a state change queued before it.
                    let _ = tx.send(out);
                }
                Ok(kde::LinkEvent::Packet(p)) if p.kind == bridge_kde::sms::REQUEST => {
                    let address = p.body["addresses"][0]["address"].as_str().unwrap_or("");
                    let body = p.body["messageBody"].as_str().unwrap_or("");
                    model
                        .lock()
                        .unwrap()
                        .texts
                        .push((address.to_owned(), body.to_owned()));
                }
                Ok(_) => {}
                Err(_) => return,
            },
            Some(out) = rx.recv() => send_all(&mut link, &model, out).await,
        }
    }
}

impl SimPhone {
    /// Pair `name` with the service, switch Telephony on for it and start its loop.
    /// `phone_takes_texts`: the phone's Messages switch is on (it accepts text
    /// send requests); `allowed_here`: Messages is allowed for it on the computer.
    async fn pair_with_texts(
        rig: &Rig,
        name: &str,
        phone_takes_texts: bool,
        allowed_here: bool,
    ) -> Self {
        let phone = calls_phone(name, phone_takes_texts);
        let link = pair_phone(rig, &phone).await;
        rig.ok(Request::SetConsent {
            device_id: phone.id(),
            feature: Feature::Telephony,
            enabled: true,
        })
        .await;
        if allowed_here {
            rig.ok(Request::SetConsent {
                device_id: phone.id(),
                feature: Feature::Sms,
                enabled: true,
            })
            .await;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let model = Arc::new(Mutex::new(Model {
            sources: BTreeMap::new(),
            merged: None,
            last: None,
            name: Some("Ada"),
            number: None,
            texts: Vec::new(),
            can_dial: false,
            can_tones: true,
            tones: Vec::new(),
            silent: false,
            slow: Duration::ZERO,
            requests: Vec::new(),
            states_sent: Vec::new(),
            hfp: None,
        }));
        let mut p = Self {
            phone,
            model: model.clone(),
            handle: PhoneHandle {
                model,
                tx,
                connected: Arc::new(AtomicBool::new(false)),
            },
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            stop: None,
        };
        p.start(link);
        // The app reports its initial idle once the listener is registered.
        for s in SOURCES {
            p.source(s, PhoneState::Idle);
        }
        let tag = format!("({name}): ready");
        until(rig, "the phone's first report", move |c| {
            c.sources.iter().any(|l| l.contains(&tag))
        })
        .await;
        p
    }

    fn start(&mut self, link: kde::TrustedLink) {
        let (stop, stopped) = oneshot::channel();
        self.stop = Some(stop);
        self.handle.connected.store(true, Ordering::SeqCst);
        tokio::spawn(run_phone(
            link,
            self.handle.tx.clone(),
            self.model.clone(),
            self.rx.clone(),
            stopped,
        ));
    }

    fn source(&self, s: &'static str, st: PhoneState) {
        self.handle.apply(|m| m.set_source(s, st));
    }

    /// The user acts on the handset; the state reaches the PC later, with [`Self::deliver`].
    fn by_hand(&self, a: LinkAction) -> Vec<kde::Packet> {
        self.model.lock().unwrap().act(a).unwrap()
    }
    fn deliver(&self, out: Vec<kde::Packet>) {
        self.handle.apply(|_| out);
    }

    /// Every source reports `st`, in the order a dual-SIM phone delivers them.
    fn ring_all(&self, st: PhoneState) {
        for s in SOURCES {
            self.source(s, st);
        }
    }

    /// The link drops (Wi-Fi gone, app killed): the connection closes.
    async fn drop_link(&mut self) {
        self.handle.connected.store(false, Ordering::SeqCst);
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        // The loop releases the queue when it ends; what was still queued dies with the
        // connection, as in the core.
        let mut rx = self.rx.lock().await;
        while rx.try_recv().is_ok() {}
    }

    async fn reconnect(&mut self, rig: &Rig) {
        let link = reconnect(rig, &self.phone).await;
        self.start(link);
    }

    fn texts(&self) -> Vec<(String, String)> {
        self.model.lock().unwrap().texts.clone()
    }
    fn requests(&self) -> Vec<LinkAction> {
        self.model.lock().unwrap().requests.clone()
    }
    fn controls(&self) -> Vec<LinkAction> {
        self.requests()
            .into_iter()
            .filter(|a| *a != LinkAction::Report)
            .collect()
    }
    fn states_sent(&self) -> Vec<PhoneState> {
        self.model.lock().unwrap().states_sent.clone()
    }
}

// ---------------------------------------------------------------- Bluetooth hands-free

struct HfpWorld {
    up: bool,
    label: String,
    /// What the phone's HFP indicators say, applied `lag` after the phone changed.
    seen: PhoneState,
    queued: Vec<(Instant, PhoneState)>,
    lag: Duration,
    routed: bool,
    /// Commands the hub sent the host.
    commands: Vec<&'static str>,
    /// Keypad tones the host was asked to send.
    tones: Vec<String>,
    /// Audio route changes the real host would make: "computer", "phone", "restore".
    routes: Vec<&'static str>,
    phone: Option<PhoneHandle>,
}

impl HfpWorld {
    fn phone_changed(&mut self, st: PhoneState) {
        self.queued.push((Instant::now() + self.lag, st));
    }
    fn settle(&mut self) {
        let now = Instant::now();
        while let Some(&(at, st)) = self.queued.first() {
            if at > now {
                break;
            }
            self.queued.remove(0);
            if st == PhoneState::Idle && self.routed {
                self.routed = false;
                self.routes.push("restore");
            }
            self.seen = st;
        }
    }
    fn snapshot(&self) -> SourceSnapshot {
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::LinuxPipeWire,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                caller_id_notification: true,
                host_call_control_api: true,
                host_audio_control_api: true,
                audio_codec: self.routed.then_some(AudioCodec::Msbc),
                ..Default::default()
            },
        );
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        let phase = match self.seen {
            PhoneState::Idle => None,
            PhoneState::Ringing => Some(CallPhase::Incoming),
            PhoneState::OffHook => Some(CallPhase::Active),
        };
        if let Some(phase) = phase {
            state.apply(CallEvent::CallUpdated {
                id: CallId::new("/org/bluez/hci0/dev_X/call1"),
                direction: CallDirection::Unknown,
                phase,
                party: CallParty::default(),
            });
        }
        state.apply(CallEvent::Audio {
            state: if self.routed {
                AudioLinkState::Connected {
                    codec: AudioCodec::Msbc,
                }
            } else {
                AudioLinkState::Disconnected
            },
        });
        SourceSnapshot {
            key: "hfp:/org/bluez/hci0/dev_X".into(),
            label: self.label.clone(),
            caps,
            state,
        }
    }
}

struct FakeHfp(Arc<Mutex<HfpWorld>>);
impl HostCalls for FakeHfp {
    fn label(&self) -> String {
        "Bluetooth hands-free (test)".into()
    }
    fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
        std::thread::sleep(wait.min(Duration::from_millis(20)));
        let mut w = self.0.lock().unwrap();
        w.settle();
        Ok(if w.up { vec![w.snapshot()] } else { vec![] })
    }
    fn execute(&mut self, _source: &str, command: &CallCommand) -> Result<(), String> {
        let mut guard = self.0.lock().unwrap();
        let w = &mut *guard;
        let (name, act) = match command {
            CallCommand::Answer { .. } => ("answer", Some(LinkAction::Answer)),
            CallCommand::Reject { .. } => ("reject", Some(LinkAction::Reject)),
            CallCommand::Hangup { .. } => ("hangup", Some(LinkAction::Hangup)),
            CallCommand::Dial { .. } => ("dial", Some(LinkAction::Dial)),
            CallCommand::ConnectAudio => ("connect_audio", None),
            CallCommand::DisconnectAudio => ("disconnect_audio", None),
            CallCommand::SendTones { tones } => {
                w.tones.push(tones.expose().to_owned());
                ("tones", None)
            }
            _ => ("other", None),
        };
        w.commands.push(name);
        // LinuxHfp::execute: answered or dialled here (or "use computer") brings the voice here.
        match command {
            CallCommand::Answer { .. } | CallCommand::Dial { .. } | CallCommand::ConnectAudio => {
                w.routed = true;
                w.routes.push("computer");
            }
            CallCommand::DisconnectAudio => {
                w.routed = false;
                w.routes.push("phone");
            }
            _ => {}
        }
        let phone = w.phone.clone();
        drop(guard);
        // The phone carries it out and reports the change (outside the host's lock: the phone
        // also tells this host about its state).
        if let (Some(a), Some(p)) = (act, phone) {
            p.apply(|m| m.act(a).unwrap_or_default());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- the rig

struct World {
    rig: Rig,
    cards: Arc<Cards>,
    banners: Arc<Banners>,
    hfp: Option<Arc<Mutex<HfpWorld>>>,
}

/// `hfp`: `Some((label, lag))` adds a hands-free host that mirrors the first phone.
async fn world(hfp: Option<(&str, Duration)>) -> World {
    let cards = Arc::new(Cards::default());
    let banners = Arc::new(Banners::default());
    let hfp = hfp.map(|(label, lag)| {
        Arc::new(Mutex::new(HfpWorld {
            up: true,
            label: label.to_owned(),
            seen: PhoneState::Idle,
            queued: Vec::new(),
            lag,
            routed: false,
            commands: Vec::new(),
            tones: Vec::new(),
            routes: Vec::new(),
            phone: None,
        }))
    });
    let (c, b, h) = (cards.clone(), banners.clone(), hfp.clone());
    let rig = Rig::start_with(move |o| {
        o.call_timings = timings();
        o.call_overlay = Some(Arc::new(FakeLauncher(c)));
        o.call_notifier = Some(Arc::new(FakeNotifier(b)));
        if let Some(h) = h {
            o.call_host = Mutex::new(Some(Box::new(FakeHfp(h))));
        }
    })
    .await;
    World {
        rig,
        cards,
        banners,
        hfp,
    }
}

impl World {
    async fn phone(&self, name: &str) -> SimPhone {
        self.phone_with_texts(name, false, false).await
    }
    async fn phone_with_texts(&self, name: &str, takes: bool, allowed: bool) -> SimPhone {
        let p = SimPhone::pair_with_texts(&self.rig, name, takes, allowed).await;
        if let Some(h) = &self.hfp
            && h.lock().unwrap().phone.is_none()
        {
            h.lock().unwrap().phone = Some(p.handle.clone());
            p.model.lock().unwrap().hfp = Some(h.clone());
        }
        p
    }
    fn hfp(&self) -> std::sync::MutexGuard<'_, HfpWorld> {
        self.hfp.as_ref().unwrap().lock().unwrap()
    }
    async fn calls(&self) -> CallsStatus {
        calls(&self.rig).await
    }
    async fn act(&self, target: bridge_core::calls_view::ActionTarget, action: CallAction) {
        self.rig.ok(Request::CallAction { target, action }).await;
    }
}

async fn calls(rig: &Rig) -> CallsStatus {
    match rig.call(Request::Calls).await.unwrap() {
        Response::Calls(c) => *c,
        other => panic!("expected calls, got {other:?}"),
    }
}

async fn until(rig: &Rig, what: &str, cond: impl Fn(&CallsStatus) -> bool) -> CallsStatus {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let c = calls(rig).await;
        if cond(&c) {
            return c;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}: {c:?}");
        sleep(Duration::from_millis(20)).await;
    }
}

async fn eventually(what: &str, cond: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        sleep(Duration::from_millis(20)).await;
    }
}

/// Watch `cond` for `d`: it must hold the whole time.
async fn holds(what: &str, d: Duration, cond: impl Fn() -> bool) {
    let end = Instant::now() + d;
    while Instant::now() < end {
        assert!(cond(), "{what}");
        sleep(Duration::from_millis(20)).await;
    }
}

fn ringing(c: &CallsStatus) -> bool {
    c.view.ringing().is_some()
}
fn live(c: &CallsStatus) -> bool {
    c.view.live().is_some() && c.view.ringing().is_none()
}
fn idle(c: &CallsStatus) -> bool {
    c.view.rows.is_empty()
}

// ---------------------------------------------------------------- timelines

#[tokio::test]
async fn incoming_from_three_sources_is_one_call_one_banner_one_card_and_answered_on_the_pc() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;

    // The dual-SIM phone: the default callback, the SIM's callback and the broadcast all ring.
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert_eq!(c.view.rows.len(), 1, "one call, not three: {c:?}");
    eventually("the banner", || w.banners.count() == 1).await;
    eventually("the card", || w.cards.launches() == 1).await;
    // Sources re-reporting RINGING, and the hub's own "still ringing?" asks, change nothing.
    phone.source("broadcast", PhoneState::Ringing);
    phone.source("default", PhoneState::Ringing);
    holds(
        "no second banner or card",
        Duration::from_millis(2500),
        || w.banners.count() == 1 && w.cards.launches() == 1,
    )
    .await;
    assert_eq!(phone.states_sent(), [PhoneState::Idle, PhoneState::Ringing]);
    assert!(
        phone.requests().contains(&LinkAction::Report),
        "a long ring is confirmed with the phone"
    );
    let c = until(&w.rig, "still ringing", ringing).await;

    // Answer on the card, double-clicked while the phone is still busy: the phone is asked once.
    phone.model.lock().unwrap().slow = Duration::from_millis(400);
    let row = c.view.ringing().unwrap().clone();
    let target = row.answer.clone().unwrap();
    assert_eq!(target.path, ControlPath::Link);
    w.act(target.clone(), CallAction::Answer).await;
    let _ = w
        .rig
        .call(Request::CallAction {
            target,
            action: CallAction::Answer,
        })
        .await;
    let c = until(&w.rig, "the live call", live).await;
    assert_eq!(phone.controls(), [LinkAction::Answer]);
    assert_eq!(c.notice, None, "the second click is not an error: {c:?}");
    assert_eq!(
        c.view.audio.route,
        bridge_core::calls_view::AudioRoute::Phone
    );

    // The card stays up through the call (it is the in-call card now), well past the linger.
    holds(
        "the card stays during the call",
        Duration::from_millis(1500),
        || w.cards.running() == 1,
    )
    .await;
    assert_eq!(w.cards.kills(), 0);

    // Hang up from the PC.
    let hang = c.view.live().unwrap().hangup.clone().unwrap();
    w.act(hang, CallAction::Hangup).await;
    let c = until(&w.rig, "idle", idle).await;
    assert_eq!(phone.controls(), [LinkAction::Answer, LinkAction::Hangup]);
    assert_eq!(c.notice, None);
    // The fake card never closes itself: the supervisor stops it a linger after the call ended.
    eventually("the card stopped", || w.cards.running() == 0).await;
    assert_eq!(
        (w.banners.count(), w.cards.launches(), w.cards.kills()),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn bluetooth_seeing_the_ring_after_the_link_is_still_one_banner_and_answer_routes_audio_here()
{
    // HFP indicators arrive 300 ms after the link's report: the row's key changes under the card.
    let w = world(Some(("Bluetooth phone", Duration::from_millis(300)))).await;
    let phone = w.phone("Fold").await;

    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    let c = until(&w.rig, "Bluetooth joined the row", |c| {
        c.view
            .ringing()
            .is_some_and(|r| r.parts.len() == 2 && r.key.starts_with("hfp:"))
    })
    .await;
    assert_eq!(c.view.rows.len(), 1, "{c:?}");
    holds("one banner, one card", Duration::from_millis(500), || {
        w.banners.count() == 1 && w.cards.launches() == 1
    })
    .await;
    let row = c.view.ringing().unwrap();
    assert!(row.parts.iter().any(|p| p.starts_with("link:")));

    // Bluetooth answers (it brings the voice): the host routes, the link sends nothing.
    let target = row.answer.clone().unwrap();
    assert_eq!(target.path, ControlPath::Hfp);
    w.act(target, CallAction::Answer).await;
    let c = until(&w.rig, "live with the audio here", |c| {
        live(c)
            && matches!(
                c.view.audio.route,
                bridge_core::calls_view::AudioRoute::Computer { .. }
            )
    })
    .await;
    assert_eq!(c.view.rows.len(), 1, "link and Bluetooth merged: {c:?}");
    assert_eq!(w.hfp().commands, ["answer"]);
    assert_eq!(w.hfp().routes, ["computer"]);
    assert!(phone.controls().is_empty(), "{:?}", phone.controls());

    // Hang up from the PC through Bluetooth: the audio is restored.
    let hang = c.view.live().unwrap().hangup.clone().unwrap();
    assert_eq!(hang.path, ControlPath::Hfp);
    w.act(hang, CallAction::Hangup).await;
    until(&w.rig, "idle", idle).await;
    eventually("restored", || w.hfp().routes == ["computer", "restore"]).await;
    assert_eq!((w.banners.count(), w.cards.launches()), (1, 1));
}

#[tokio::test]
async fn answered_on_the_phone_keeps_the_audio_on_the_phone() {
    let w = world(Some(("Bluetooth phone", Duration::from_millis(100)))).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    // The user picks up on the handset; the sources follow one by one.
    phone.ring_all(PhoneState::OffHook);
    let c = until(&w.rig, "live", |c| {
        live(c) && c.view.rows[0].parts.len() == 2
    })
    .await;
    assert_eq!(
        c.view.audio.route,
        bridge_core::calls_view::AudioRoute::Phone
    );
    assert!(w.hfp().commands.is_empty() && w.hfp().routes.is_empty());
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
    sleep(Duration::from_millis(300)).await;
    assert!(w.hfp().routes.is_empty(), "nothing to restore");
    assert!(phone.controls().is_empty());
}

#[tokio::test]
async fn a_missed_call_closes_everything_and_leaves_no_card_process() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("the card", || w.cards.running() == 1).await;
    phone.ring_all(PhoneState::Idle);
    let c = until(&w.rig, "idle", idle).await;
    assert_eq!(c.notice, None);
    // The fake card never closes itself (a hung window): the supervisor stops it.
    eventually("the card stopped", || w.cards.running() == 0).await;
    assert_eq!((w.cards.launches(), w.cards.kills()), (1, 1));
}

#[tokio::test]
async fn declined_from_the_pc_then_the_caller_rings_again_and_the_old_banner_cannot_answer_it() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    eventually("banner 1", || w.banners.count() == 1).await;
    w.act(
        c.view.ringing().unwrap().reject.clone().unwrap(),
        CallAction::Reject,
    )
    .await;
    until(&w.rig, "declined", idle).await;
    assert_eq!(phone.controls(), [LinkAction::Reject]);
    // The card was closing (its "Call declined" line) when the caller rang again.
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing again", ringing).await;
    eventually("banner 2", || w.banners.count() == 2).await;
    w.cards.close_all();
    eventually("a card for the second ring", || w.cards.launches() == 2).await;

    // The first banner (it waits 30 s) is pressed now: it belongs to the declined ring.
    w.banners.press(0, RingChoice::Answer).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(phone.controls(), [LinkAction::Reject], "nothing sent");
    assert!(ringing(&w.calls().await));
    // The second banner answers.
    w.banners.press(1, RingChoice::Answer).await;
    until(&w.rig, "answered", live).await;
    assert_eq!(phone.controls(), [LinkAction::Reject, LinkAction::Answer]);
}

#[tokio::test]
async fn outgoing_dial_from_the_pc_shows_no_ring_and_no_card() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.model.lock().unwrap().can_dial = true;
    // The permission change reaches the PC with the next report.
    phone.source("default", PhoneState::OffHook);
    phone.source("default", PhoneState::Idle);
    let c = until(&w.rig, "dial offered", |c| c.view.dial.is_some()).await;
    let source = c.view.dial.unwrap().source;
    w.rig
        .ok(Request::CallDial {
            source,
            number: Sensitive::new("+15550100"),
        })
        .await;
    let c = until(&w.rig, "the outgoing call", live).await;
    assert_eq!(phone.controls(), [LinkAction::Dial]);
    assert!(c.view.live().unwrap().hangup.is_some());
    holds("no banner, no card", Duration::from_millis(500), || {
        w.banners.count() == 0 && w.cards.launches() == 0
    })
    .await;
    // OFFHOOK without RINGING also comes from a call dialled on the handset.
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
}

#[tokio::test]
async fn a_link_drop_mid_call_leaves_no_ghost_and_the_reconnect_restores_the_call() {
    let w = world(None).await;
    let mut phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    until(&w.rig, "live", live).await;

    phone.drop_link().await;
    until(&w.rig, "no ghost call after the link dropped", idle).await;
    phone.reconnect(&w.rig).await;
    // The phone greets the new connection with its current state.
    let c = until(&w.rig, "the call is back", live).await;
    assert_eq!(c.view.rows.len(), 1);
    // It ends now, and the PC follows.
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;

    // The call ends while the link is down: nothing comes back after the reconnect.
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    phone.ring_all(PhoneState::OffHook);
    until(&w.rig, "live", live).await;
    phone.drop_link().await;
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "gone", idle).await;
    phone.reconnect(&w.rig).await;
    sleep(Duration::from_millis(500)).await;
    assert!(idle(&w.calls().await), "no ghost after the reconnect");
}

#[tokio::test]
async fn a_link_flap_during_a_ring_is_the_same_ring() {
    let w = world(None).await;
    let mut phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("banner and card", || {
        w.banners.count() == 1 && w.cards.launches() == 1
    })
    .await;
    phone.drop_link().await;
    until(&w.rig, "link gone", idle).await;
    phone.reconnect(&w.rig).await;
    until(&w.rig, "ringing after the reconnect", ringing).await;
    holds(
        "no second banner or card for the same ring",
        Duration::from_millis(500),
        || w.banners.count() == 1 && w.cards.launches() == 1,
    )
    .await;
    // The card had closed itself in the gap: the ring gets it back, once.
    w.cards.close_all();
    phone.drop_link().await;
    until(&w.rig, "link gone", idle).await;
    phone.reconnect(&w.rig).await;
    until(&w.rig, "ringing", ringing).await;
    eventually("the card is back", || w.cards.launches() == 2).await;
    assert_eq!(w.banners.count(), 1);
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
}

#[tokio::test]
async fn a_lost_idle_is_cleared_by_the_watchdog_and_the_card_goes() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("the card", || w.cards.running() == 1).await;
    // The phone app freezes: no IDLE, no answer to "report", but the connection stays up.
    phone.model.lock().unwrap().silent = true;
    phone.ring_all(PhoneState::Idle);
    let c = until(&w.rig, "the ghost ring cleared", idle).await;
    assert_eq!(c.notice.as_deref(), Some(STALE_CALL_NOTICE));
    eventually("the card stopped", || w.cards.running() == 0).await;
    let asks = phone
        .requests()
        .iter()
        .filter(|a| **a == LinkAction::Report)
        .count();
    assert!(asks >= 2, "asked before giving up: {asks}");
    // The phone recovers while the caller still rings: the same ring (its banner was shown),
    // and the phone talking again clears the notice.
    phone.model.lock().unwrap().silent = false;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert_eq!(c.notice, None);
    assert_eq!(w.banners.count(), 1);
    // That ring ends; the next one is a new call with its own banner.
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("banner 2", || w.banners.count() == 2).await;
}

#[tokio::test]
async fn answering_from_the_pc_as_the_phone_picks_up_by_hand_is_not_an_error() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    let target = c.view.ringing().unwrap().answer.clone().unwrap();
    // The user answered on the handset; the PC's click was already on its way, and the phone's
    // "no such call" comes back before its off-hook report.
    let picked_up = phone.by_hand(LinkAction::Answer);
    w.act(target, CallAction::Answer).await;
    eventually("the phone got the click", || {
        phone.controls() == [LinkAction::Answer]
    })
    .await;
    sleep(Duration::from_millis(200)).await;
    phone.deliver(picked_up);
    let c = until(&w.rig, "live", live).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(phone.controls(), [LinkAction::Answer]);
    assert_eq!(w.calls().await.notice, None, "{c:?}");

    // Decline racing the caller hanging up.
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    let reject = c.view.ringing().unwrap().reject.clone().unwrap();
    let hung_up = phone.by_hand(LinkAction::Reject);
    w.act(reject, CallAction::Reject).await;
    eventually("the phone got the click", || phone.controls().len() == 2).await;
    sleep(Duration::from_millis(200)).await;
    phone.deliver(hung_up);
    until(&w.rig, "idle", idle).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(w.calls().await.notice, None);
}

#[tokio::test]
async fn a_second_phone_rings_while_the_first_is_in_a_call() {
    // Bluetooth is set up for phone A (named after it); phone B has the link only.
    let w = world(Some(("Phone A", Duration::from_millis(50)))).await;
    let a = w.phone("Phone A").await;
    let b = w.phone("Phone B").await;
    a.ring_all(PhoneState::Ringing);
    b.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "both ringing, A merged", |c| {
        c.view.rows.len() == 2 && c.view.rows.iter().any(|r| r.parts.len() == 2)
    })
    .await;
    let b_row = c
        .view
        .rows
        .iter()
        .find(|r| r.parts.len() == 1)
        .unwrap()
        .clone();
    assert_eq!(b_row.phone, "Phone B");
    assert_eq!(b_row.answer.as_ref().unwrap().path, ControlPath::Link);
    eventually("two banners", || w.banners.count() == 2).await;
    assert_eq!(w.cards.launches(), 1, "one card follows both rings");

    // A answers on the handset; B keeps ringing; answering B goes to B only.
    a.ring_all(PhoneState::OffHook);
    let c = until(&w.rig, "A live, B ringing", |c| {
        c.view.live().is_some() && c.view.ringing().is_some_and(|r| r.phone == "Phone B")
    })
    .await;
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    until(&w.rig, "both live", |c| {
        c.view.rows.len() == 2 && c.view.ringing().is_none()
    })
    .await;
    assert_eq!(b.controls(), [LinkAction::Answer]);
    assert!(a.controls().is_empty());
    assert!(w.hfp().commands.is_empty());
}

#[tokio::test]
async fn a_ring_on_phone_b_never_joins_phone_a_s_bluetooth_ring() {
    // A's ring reaches this computer over Bluetooth only (its link stays quiet); B rings over the
    // link at the same time. Before, B's ring joined A's Bluetooth row and Answer went to A.
    let w = world(Some(("Phone A", Duration::ZERO))).await;
    let a = w.phone("Phone A").await;
    let b = w.phone("Phone B").await;
    a.model.lock().unwrap().silent = true;
    a.ring_all(PhoneState::Ringing);
    until(&w.rig, "A ringing over Bluetooth", ringing).await;
    b.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "two rows", |c| c.view.rows.len() == 2).await;
    let b_row = c
        .view
        .rows
        .iter()
        .find(|r| r.parts.iter().any(|p| p.starts_with("link:")))
        .unwrap();
    assert_eq!(b_row.parts.len(), 1, "{c:?}");
    assert_eq!(b_row.answer.as_ref().unwrap().path, ControlPath::Link);
    w.act(b_row.answer.clone().unwrap(), CallAction::Answer)
        .await;
    eventually("B answered", || b.controls() == [LinkAction::Answer]).await;
    assert!(w.hfp().commands.is_empty(), "nothing went to phone A");
}

#[tokio::test]
async fn bluetooth_connecting_after_the_call_started_merges_without_ringing_or_routing() {
    let w = world(Some(("Bluetooth phone", Duration::ZERO))).await;
    w.hfp().up = false;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    phone.ring_all(PhoneState::OffHook);
    until(&w.rig, "live", live).await;
    w.hfp().up = true;
    let c = until(&w.rig, "merged", |c| {
        live(c) && c.view.rows.len() == 1 && c.view.rows[0].parts.len() == 2
    })
    .await;
    assert_eq!(
        c.view.audio.route,
        bridge_core::calls_view::AudioRoute::Phone
    );
    assert_eq!(w.banners.count(), 1);
    assert!(w.hfp().routes.is_empty());
}

#[tokio::test]
async fn without_telephony_consent_nothing_rings_here() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    w.rig
        .ok(Request::SetConsent {
            device_id: phone.phone.id(),
            feature: Feature::Telephony,
            enabled: false,
        })
        .await;
    phone.ring_all(PhoneState::Ringing);
    holds(
        "no ring, banner or card",
        Duration::from_millis(600),
        || w.banners.count() == 0 && w.cards.launches() == 0,
    )
    .await;
    assert!(idle(&w.calls().await));
}

#[tokio::test]
async fn a_card_that_cannot_start_leaves_the_banner_and_is_not_retried_in_a_loop() {
    let w = world(None).await;
    w.cards.fail.store(true, Ordering::SeqCst);
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("the banner", || w.banners.count() == 1).await;
    holds("one launch attempt", Duration::from_millis(600), || {
        w.cards.launches() == 1
    })
    .await;
}

// ---------------------------------------------------------------- keypad tones (DTMF)

fn tone(source: &str, t: &str) -> Request {
    Request::CallTone {
        source: source.to_owned(),
        tone: Sensitive::new(t),
    }
}

/// A call that is answered and live on the phone.
async fn live_call(w: &World, phone: &SimPhone) -> CallsStatus {
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "the ring", ringing).await;
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    until(&w.rig, "the live call", live).await
}

fn trace_lines() -> Vec<String> {
    bridge_service::call_trace::snapshot()
        .into_iter()
        .filter(|e| e.detail.contains("keypad tones"))
        .map(|e| e.detail)
        .collect()
}

#[tokio::test]
async fn keypad_tones_reach_the_phone_over_the_link_in_order_and_only_while_the_call_is_live() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    // Ringing: no keypad yet.
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "the ring", ringing).await;
    assert!(c.view.tones.is_none());
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    let c = until(&w.rig, "the live call", live).await;
    let t = c.view.tones.clone().expect("a keypad for the live call");
    assert_eq!(t.path, ControlPath::Link);
    for d in "1*0#9".chars() {
        w.rig.ok(tone(&t.source, &d.to_string())).await;
    }
    eventually("the phone played them", || {
        phone.model.lock().unwrap().tones.len() == 5
    })
    .await;
    assert_eq!(phone.model.lock().unwrap().tones, ['1', '*', '0', '#', '9']);
    // The trace has the count and never a tone.
    let lines = trace_lines();
    assert!(lines.iter().any(|l| l.contains("sent=5")), "{lines:?}");
    // The call ends: the keypad goes with it, and a late tap reaches nobody.
    phone.ring_all(PhoneState::Idle);
    let c = until(&w.rig, "idle", idle).await;
    assert!(c.view.tones.is_none());
    let late = w.rig.call(tone(&t.source, "5")).await;
    assert!(late.is_err());
    sleep(Duration::from_millis(100)).await;
    assert_eq!(phone.model.lock().unwrap().tones.len(), 5);
}

#[tokio::test]
async fn an_older_phone_without_the_capability_gets_no_keypad_and_no_tones() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    phone.model.lock().unwrap().can_tones = false;
    phone.source("default", PhoneState::Ringing);
    let c = until(&w.rig, "the ring", ringing).await;
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    let c = until(&w.rig, "the live call", live).await;
    assert!(c.view.tones.is_none());
    let source = c.view.live().unwrap().hangup.clone().unwrap().source;
    assert!(w.rig.call(tone(&source, "1")).await.is_err());
    sleep(Duration::from_millis(100)).await;
    assert!(phone.model.lock().unwrap().tones.is_empty());
}

#[tokio::test]
async fn hostile_tone_requests_are_refused_before_anything_leaves_the_computer() {
    let w = world(None).await;
    let phone = w.phone("Fold").await;
    // No call at all: not offered, nothing sent, whatever the source says.
    for src in ["link:nobody", "", "hfp:x"] {
        assert!(w.rig.call(tone(src, "1")).await.is_err(), "{src:?}");
    }
    let c = live_call(&w, &phone).await;
    let source = c.view.tones.clone().unwrap().source;
    // Digits outside the keypad, several at once, empty, control characters, look-alike digits.
    for bad in [
        "", "12", "A", "D", "a", " ", "+", "\n", "\0", "1\n", "\u{0661}", "\u{ff11}", "*#", "11111",
    ] {
        assert!(w.rig.call(tone(&source, bad)).await.is_err(), "{bad:?}");
    }
    // A source the view does not offer.
    assert!(w.rig.call(tone("link:someone-else", "1")).await.is_err());
    sleep(Duration::from_millis(150)).await;
    assert!(
        phone.model.lock().unwrap().tones.is_empty(),
        "nothing reached the phone"
    );
    // A flood of valid taps is cut off by the service itself.
    let mut accepted = 0;
    let mut refused = 0;
    for _ in 0..(bridge_service::call_control::MAX_TONES * 3) {
        match w.rig.call(tone(&source, "5")).await {
            Ok(_) => accepted += 1,
            Err(_) => refused += 1,
        }
    }
    assert!(refused > 0, "no limit applied");
    assert!(
        accepted <= bridge_service::call_control::MAX_TONES + 2,
        "{accepted}"
    );
    sleep(Duration::from_millis(200)).await;
    assert!(phone.model.lock().unwrap().tones.len() <= bridge_service::call_control::MAX_TONES + 2);
}

#[tokio::test]
async fn keypad_tones_use_bluetooth_when_it_is_there_and_the_call_is_live() {
    let w = world(Some(("Fold", Duration::ZERO))).await;
    let phone = w.phone("Fold").await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "the ring", ringing).await;
    w.act(
        c.view.ringing().unwrap().answer.clone().unwrap(),
        CallAction::Answer,
    )
    .await;
    // The live call shows first over the link; the keypad moves to Bluetooth when the HFP side has
    // reported the call (slow runners see both orders), so wait for that, not for the first live view.
    let c = until(&w.rig, "the keypad over Bluetooth", |c| {
        live(c)
            && c.view
                .tones
                .as_ref()
                .is_some_and(|t| t.path == ControlPath::Hfp)
    })
    .await;
    let t = c.view.tones.expect("keypad");
    w.rig.ok(tone(&t.source, "7")).await;
    w.rig.ok(tone(&t.source, "#")).await;
    assert_eq!(w.hfp().tones, ["7", "#"]);
    assert!(
        phone.model.lock().unwrap().tones.is_empty(),
        "the link was not used"
    );
}

// ---------------------------------------------------------------- decline with a message

/// A phone whose Messages switch is on, allowed here, ringing with a plain number.
async fn texting_phone(w: &World) -> SimPhone {
    let phone = w.phone_with_texts("Fold", true, true).await;
    phone.model.lock().unwrap().number = Some("+1 (555) 010-0100");
    phone
}

#[tokio::test]
async fn decline_with_message_declines_once_and_sends_one_text_to_the_caller() {
    let w = world(None).await;
    let phone = texting_phone(&w).await;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert!(c.replies.offered, "{c:?}");
    assert_eq!(c.replies.texts, bridge_core::call_replies::defaults());
    // The banner offered the same three texts.
    eventually("the banner", || w.banners.count() == 1).await;
    assert_eq!(
        w.banners.texts.lock().unwrap()[0],
        bridge_core::call_replies::defaults()
    );
    let target = c.view.ringing().unwrap().reject.clone().unwrap();
    let ask = Request::CallDeclineReply { target, reply: 1 };
    // Double click: one decline, one text.
    w.rig.ok(ask.clone()).await;
    let _ = w.rig.call(ask).await;
    until(&w.rig, "declined", idle).await;
    eventually("the text", || !phone.texts().is_empty()).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!(phone.controls(), [LinkAction::Reject]);
    assert_eq!(
        phone.texts(),
        [("+15550100100".to_owned(), "I'm in a meeting".to_owned())]
    );
    // The call is listed as declined here, not missed, and no missed-call notice was raised.
    let c = calls(&w.rig).await;
    assert_eq!(c.notice, None, "{c:?}");
    assert!(!c.replies.offered);
}

#[tokio::test]
async fn decline_with_message_needs_messages_allowed_on_both_sides_and_a_number() {
    let w = world(None).await;
    // Allowed here, but the phone's Messages switch is off (it does not accept texts to send).
    let phone_off = w.phone_with_texts("Off", false, true).await;
    phone_off.model.lock().unwrap().number = Some("+15550100");
    phone_off.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert!(!c.replies.offered, "{c:?}");
    let target = c.view.ringing().unwrap().reject.clone().unwrap();
    assert_eq!(
        w.rig
            .refused(Request::CallDeclineReply {
                target: target.clone(),
                reply: 0
            })
            .await,
        bridge_core::ipc::ErrorCode::Unsupported
    );
    // Refused before anything happened: the call still rings, nothing went to the phone.
    sleep(Duration::from_millis(200)).await;
    assert_eq!(phone_off.controls(), []);
    assert!(ringing(&w.calls().await));
    assert!(w.banners.texts.lock().unwrap()[0].is_empty());
    phone_off.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
}

#[tokio::test]
async fn decline_with_message_is_hidden_when_this_computer_does_not_allow_messages_or_no_number() {
    let w = world(None).await;
    let phone = w.phone_with_texts("Quiet", true, false).await;
    phone.model.lock().unwrap().number = Some("+15550100");
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert!(!c.replies.offered, "no Messages consent here: {c:?}");
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
    // Now allowed, but the caller is hidden: nothing to text.
    w.rig
        .ok(Request::SetConsent {
            device_id: phone.phone.id(),
            feature: Feature::Sms,
            enabled: true,
        })
        .await;
    phone.model.lock().unwrap().number = None;
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert!(!c.replies.offered, "no number: {c:?}");
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "idle", idle).await;
    // A number that is not plain digits is never texted.
    phone.model.lock().unwrap().number = Some("*123#");
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert!(!c.replies.offered, "{c:?}");
}

#[tokio::test]
async fn the_banner_declines_with_a_text_and_an_old_banner_cannot_text_a_later_ring() {
    let w = world(None).await;
    let phone = texting_phone(&w).await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("banner 1", || w.banners.count() == 1).await;
    phone.ring_all(PhoneState::Idle);
    until(&w.rig, "the caller gave up", idle).await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing again", ringing).await;
    eventually("banner 2", || w.banners.count() == 2).await;
    // The first banner belongs to the ring that ended: nothing is declined or sent.
    w.banners.press(0, RingChoice::DeclineWith(0)).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!((phone.controls(), phone.texts()), (vec![], vec![]));
    // The second one declines and texts the third reply.
    w.banners.press(1, RingChoice::DeclineWith(2)).await;
    until(&w.rig, "declined", idle).await;
    eventually("the text", || !phone.texts().is_empty()).await;
    assert_eq!(phone.controls(), [LinkAction::Reject]);
    assert_eq!(phone.texts()[0].1, "Call me in a few minutes");
}

#[tokio::test]
async fn a_banner_button_never_sends_other_words_than_it_was_labelled_with() {
    let w = world(None).await;
    let phone = texting_phone(&w).await;
    phone.ring_all(PhoneState::Ringing);
    until(&w.rig, "ringing", ringing).await;
    eventually("the banner", || w.banners.count() == 1).await;
    // The list is edited while the banner is up: its second button said "I'm in a meeting".
    w.rig
        .ok(Request::CallRepliesSet {
            texts: vec!["One".into(), "Something else entirely".into()],
        })
        .await;
    w.banners.press(0, RingChoice::DeclineWith(1)).await;
    sleep(Duration::from_millis(300)).await;
    assert_eq!((phone.controls(), phone.texts()), (vec![], vec![]));
    assert!(ringing(&w.calls().await), "the call still rings");
}

#[tokio::test]
async fn saved_replies_replace_the_defaults_and_a_bad_list_is_refused() {
    let w = world(None).await;
    let phone = texting_phone(&w).await;
    w.rig
        .ok(Request::CallRepliesSet {
            texts: vec!["  Driving, text you later ".into(), String::new()],
        })
        .await;
    assert_eq!(
        w.rig
            .refused(Request::CallRepliesSet {
                texts: vec!["two\nlines".into()]
            })
            .await,
        bridge_core::ipc::ErrorCode::BadRequest
    );
    phone.ring_all(PhoneState::Ringing);
    let c = until(&w.rig, "ringing", ringing).await;
    assert_eq!(c.replies.texts, ["Driving, text you later"]);
    let target = c.view.ringing().unwrap().reject.clone().unwrap();
    // Only one reply exists now.
    assert_eq!(
        w.rig
            .refused(Request::CallDeclineReply {
                target: target.clone(),
                reply: 1
            })
            .await,
        bridge_core::ipc::ErrorCode::BadRequest
    );
    w.rig
        .ok(Request::CallDeclineReply { target, reply: 0 })
        .await;
    eventually("the text", || !phone.texts().is_empty()).await;
    assert_eq!(phone.texts()[0].1, "Driving, text you later");
    // An empty list restores the defaults.
    w.rig.ok(Request::CallRepliesSet { texts: vec![] }).await;
    assert_eq!(
        calls(&w.rig).await.replies.texts,
        bridge_core::call_replies::defaults()
    );
}
