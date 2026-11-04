// SPDX-License-Identifier: GPL-3.0-or-later
//! The call-audio controller: puts a live call's voice on the headphones and microphone the user
//! picked, keeps it there, and undoes everything when the call ends.
//!
//! It runs on the Bluetooth call thread and is fed one [`PhoneInput`] per phone per poll. It
//! acts on the audio server itself and returns the telephony requests (the phone's `RejectSCO`
//! flag, `Activate`) for the caller to send, so it can be tested without D-Bus.
//!
//! Policy (see `docs/reports/call-audio-pc-headset.md`):
//!
//! * **Answered or dialled here** ([`Want::Computer`]): accept the phone's voice link
//!   (`RejectSCO=false`), make sure the phone's card is in its `audio-gateway` profile, ask the
//!   phone for the voice link (`Activate`) if it has not opened one shortly after the call went
//!   live, and move the two voice streams to the picked devices.
//! * **Answered on the phone** ([`Want::Auto`]): do not take the audio. While the call rings and
//!   for a few seconds after it is answered, the phone's voice link is refused (`RejectSCO=true`),
//!   which is what keeps Android from moving the audio to the "headset" (this computer). After
//!   that the flag is restored, so picking "Bluetooth" in the phone's call screen still works; if
//!   the phone then sends the audio here it is routed like any other call.
//! * **"Use the phone"** ([`Want::Phone`]): refuse the voice link and switch the phone's card off
//!   for the rest of the call, which closes the link; the phone takes the audio back.
//! * Within 3 s of the call being live here, the route is checked (voice link open, streams
//!   linked, microphone not muted) and every problem becomes a line with a fix; digital silence
//!   is reported from the second level check (8 s) on, because the first seconds of a call are
//!   often truly silent. Nothing fails silently.
//! * Every change that outlives a stream (a card profile, a microphone mute, `RejectSCO`) is
//!   written to the restore file *before* it is made and undone when the call ends, at shutdown,
//!   or at the next start after a crash.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::audio::model::{BtAddress, ProfileKind, ServerSnapshot};
use crate::audio::prefs::{self, CardRestore, Choice, MuteRestore, Paths, RejectRestore, Restore};
use crate::audio::route::{self, FlowProblem, PhoneVoice, Resolved, Role};
use crate::audio::server::{AudioServer, PeakTarget, ServerError};
use crate::model::TransportState;

/// Where the user wants this call's audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Want {
    /// Nobody asked: the call was answered or placed on the phone. Do not take the audio.
    Auto,
    /// Answered/dialled here or "use computer audio": bring it here.
    Computer,
    /// "Use the phone": send it back.
    Phone,
}

/// One phone, as the call layer sees it in this poll.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhoneInput {
    pub address: BtAddress,
    /// A call is ringing (incoming or waiting).
    pub ringing: bool,
    /// A call is live (active, held, dialing or alerting).
    pub live: bool,
    pub transport: Option<TransportState>,
    /// The phone transport's `RejectSCO` flag, if the service exposes it.
    pub reject_sco: Option<bool>,
}

/// Telephony requests for the caller to send through the call backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    SetRejectSco { address: BtAddress, reject: bool },
    Activate { address: BtAddress },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "place", rename_all = "snake_case")]
pub enum Place {
    /// The voice is on the phone (no voice link to this computer).
    Phone,
    /// Asked for, not here yet.
    Connecting,
    /// Playing on `output`, recording from `input`.
    Computer {
        output: String,
        input: String,
        codec: Option<String>,
    },
}

/// What the call audio of one phone is doing, for the status lines and the UI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioStatus {
    pub want: Want,
    pub place: Place,
    /// `Some` once the call's microphone is known.
    pub muted: Option<bool>,
    /// One line each, with a fix.
    pub problems: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Ask the phone for the voice link this long after the call went live without one.
    pub activate_after: Duration,
    pub activate_retry: Duration,
    pub max_activations: u8,
    /// Judge the route this long after the call is live here.
    pub check_after: Duration,
    /// Second, last level measurement.
    pub recheck_after: Duration,
    /// How long the voice link stays refused after a call answered on the phone goes live.
    pub keep_on_phone_for: Duration,
    pub peak_window: Duration,
    pub reconnect_every: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            activate_after: Duration::from_millis(1500),
            activate_retry: Duration::from_secs(3),
            max_activations: 2,
            check_after: Duration::from_secs(3),
            recheck_after: Duration::from_secs(8),
            keep_on_phone_for: Duration::from_secs(5),
            peak_window: Duration::from_millis(600),
            reconnect_every: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
struct Engagement {
    want: Want,
    /// When `want` last changed (or the call was first seen).
    want_since: Instant,
    live_since: Option<Instant>,
    activations: u8,
    last_activation: Option<Instant>,
    /// Output and input actually in use.
    routed: Option<(String, String)>,
    codec: Option<String>,
    notes: Vec<FlowProblem>,
    problems: Vec<FlowProblem>,
    checks: u8,
    rx_heard: bool,
    mic_heard: bool,
    user_mute: Option<bool>,
    muted: Option<bool>,
    /// `RejectSCO` before GeekBridge first changed it during this call.
    reject_prev: Option<bool>,
    choice: Choice,
    /// Cards and microphones this call changed (undone at its end, even after the route
    /// moved or the phone left).
    touched_cards: Vec<String>,
    touched_sources: Vec<String>,
}

impl Engagement {
    fn new(want: Want, now: Instant, choice: Choice) -> Self {
        Self {
            want,
            want_since: now,
            live_since: None,
            activations: 0,
            last_activation: None,
            routed: None,
            codec: None,
            notes: Vec::new(),
            problems: Vec::new(),
            checks: 0,
            rx_heard: false,
            mic_heard: false,
            user_mute: None,
            muted: None,
            reject_prev: None,
            choice,
            touched_cards: Vec::new(),
            touched_sources: Vec::new(),
        }
    }
}

type Connector<S> = Box<dyn FnMut() -> Result<S, ServerError> + Send>;
type Clock = Box<dyn Fn() -> Instant + Send>;

/// One controller per call backend.
pub struct CallAudio<S: AudioServer> {
    connect: Connector<S>,
    server: Option<S>,
    last_connect: Option<Instant>,
    server_problem: Option<String>,
    paths: Paths,
    options: Options,
    clock: Clock,
    calls: BTreeMap<BtAddress, Engagement>,
    /// Intent given before the call was seen (e.g. dialled here).
    pending: BTreeMap<BtAddress, (Want, Instant)>,
    restore: Restore,
    recovered: bool,
}

impl<S: AudioServer> CallAudio<S> {
    pub fn new(
        connect: impl FnMut() -> Result<S, ServerError> + Send + 'static,
        state_dir: &Path,
    ) -> Self {
        Self {
            connect: Box::new(connect),
            server: None,
            last_connect: None,
            server_problem: None,
            paths: Paths::in_dir(state_dir),
            options: Options::default(),
            clock: Box::new(Instant::now),
            calls: BTreeMap::new(),
            pending: BTreeMap::new(),
            restore: Restore::default(),
            recovered: false,
        }
    }

    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// Replace the clock (deterministic tests).
    pub fn with_clock(mut self, clock: impl Fn() -> Instant + Send + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    fn server(&mut self) -> Option<&mut S> {
        if self.server.is_none() {
            let now = self.now();
            if self
                .last_connect
                .is_some_and(|t| now.duration_since(t) < self.options.reconnect_every)
            {
                return None;
            }
            self.last_connect = Some(now);
            match (self.connect)() {
                Ok(s) => {
                    self.server = Some(s);
                    self.server_problem = None;
                }
                Err(e) => {
                    self.server_problem = Some(format!(
                        "The sound server cannot be reached ({e}); the call audio cannot be routed. Check: systemctl --user status pipewire pipewire-pulse wireplumber"
                    ));
                    return None;
                }
            }
        }
        self.server.as_mut()
    }

    fn drop_server_on(&mut self, e: &ServerError) {
        if e.is_connection_loss() {
            self.server = None;
        }
    }

    fn snapshot(&mut self) -> Option<ServerSnapshot> {
        let r = self.server()?.snapshot();
        match r {
            Ok(s) => Some(s),
            Err(e) => {
                self.server_problem = Some(format!("Reading the sound server failed: {e}"));
                self.drop_server_on(&e);
                None
            }
        }
    }

    fn persist(&mut self) {
        let _ = prefs::save_restore(&self.paths.restore, &self.restore);
    }

    // ------------------------------------------------------------------ intents

    /// The user answered, dialled or asked for the audio here (or asked for it back on the
    /// phone). Returns telephony requests to send *before* the answer/dial goes out, so the
    /// phone's voice link is accepted.
    pub fn want(&mut self, phone: BtAddress, want: Want, reject_sco: Option<bool>) -> Vec<Request> {
        let now = self.now();
        let mut out = Vec::new();
        match self.calls.get_mut(&phone) {
            Some(e) => {
                if e.want != want {
                    e.want = want;
                    e.want_since = now;
                    e.checks = 0;
                    e.problems.clear();
                    e.activations = 0;
                    e.last_activation = None;
                }
            }
            None => {
                self.pending.insert(phone, (want, now));
            }
        }
        let desired = match want {
            Want::Computer => Some(false),
            Want::Phone => Some(true),
            Want::Auto => None,
        };
        if let (Some(d), Some(cur)) = (desired, reject_sco)
            && d != cur
        {
            self.note_reject_change(phone, cur);
            out.push(Request::SetRejectSco {
                address: phone,
                reject: d,
            });
        }
        out
    }

    fn note_reject_change(&mut self, phone: BtAddress, current: bool) {
        let addr = phone.expose();
        if !self.restore.reject.iter().any(|r| r.address == addr) {
            self.restore.reject.push(RejectRestore {
                address: addr,
                previous: current,
            });
            self.persist();
        }
        if let Some(e) = self.calls.get_mut(&phone) {
            e.reject_prev.get_or_insert(current);
        }
    }

    /// Mute or unmute the microphone the call uses. Errors are user-facing lines.
    pub fn set_mute(&mut self, phone: BtAddress, mute: bool) -> Result<(), String> {
        let Some(input) = self
            .calls
            .get(&phone)
            .and_then(|e| e.routed.as_ref().map(|r| r.1.clone()))
        else {
            return Err(
                "the call audio is not on this computer, so there is no microphone to mute".into(),
            );
        };
        if let Some(e) = self.calls.get_mut(&phone) {
            e.user_mute = Some(mute);
        }
        let snap = self
            .snapshot()
            .ok_or("the sound server cannot be reached")?;
        self.apply_mute(phone, &snap, &input, mute)
    }

    fn apply_mute(
        &mut self,
        phone: BtAddress,
        snap: &ServerSnapshot,
        source: &str,
        mute: bool,
    ) -> Result<(), String> {
        let Some(current) = snap
            .sources
            .iter()
            .find(|s| s.name == source)
            .map(|s| s.mute)
        else {
            return Err(format!("the microphone {source} is gone"));
        };
        if current != mute {
            match self.restore.mutes.iter_mut().find(|m| m.source == source) {
                Some(m) => m.set_to = mute,
                None => self.restore.mutes.push(MuteRestore {
                    source: source.to_owned(),
                    was_muted: current,
                    set_to: mute,
                }),
            }
            self.persist();
            let r = match self.server() {
                Some(s) => s.set_source_mute(source, mute),
                None => return Err("the sound server cannot be reached".into()),
            };
            if let Err(e) = r {
                self.drop_server_on(&e);
                return Err(format!("could not change the microphone mute: {e}"));
            }
        }
        if let Some(e) = self.calls.get_mut(&phone) {
            if !e.touched_sources.iter().any(|s| s == source) {
                e.touched_sources.push(source.to_owned());
            }
            e.muted = Some(mute);
        }
        Ok(())
    }

    /// The user's pick changed: use it from the next poll on for live calls.
    pub fn reload_choice(&mut self) {
        let c = prefs::load_choice(&self.paths.choice);
        for e in self.calls.values_mut() {
            e.choice = c.clone();
        }
    }

    // ------------------------------------------------------------------ status

    pub fn status(&self, phone: BtAddress) -> Option<AudioStatus> {
        let e = self.calls.get(&phone)?;
        let place = match (&e.routed, e.want) {
            (Some((o, i)), w) if w != Want::Phone => Place::Computer {
                output: o.clone(),
                input: i.clone(),
                codec: e.codec.clone(),
            },
            (None, Want::Computer) => Place::Connecting,
            _ => Place::Phone,
        };
        let mut problems: Vec<String> = e
            .notes
            .iter()
            .chain(e.problems.iter())
            .map(FlowProblem::message)
            .collect();
        if let Some(p) = &self.server_problem {
            problems.push(p.clone());
        }
        Some(AudioStatus {
            want: e.want,
            place,
            muted: e.muted,
            problems,
        })
    }

    /// Lines for every phone with a call: where the audio is and what is wrong.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for addr in self.calls.keys() {
            let Some(s) = self.status(*addr) else {
                continue;
            };
            out.push(match &s.place {
                Place::Phone => "Call audio: on the phone.".to_owned(),
                Place::Connecting => "Call audio: asking the phone to send it here...".to_owned(),
                Place::Computer {
                    output,
                    input,
                    codec,
                } => format!(
                    "Call audio: on this computer (speaker: {output}; microphone: {input}{}{}).",
                    codec
                        .as_deref()
                        .map(|c| format!("; codec {}", codec_label(c)))
                        .unwrap_or_default(),
                    match s.muted {
                        Some(true) => "; microphone muted",
                        _ => "",
                    }
                ),
            });
            out.extend(s.problems);
        }
        out
    }

    // ------------------------------------------------------------------ poll

    /// Advance every phone's call audio by one poll. Returns telephony requests to send.
    pub fn tick(&mut self, phones: &[PhoneInput]) -> Vec<Request> {
        let mut out = Vec::new();
        if !self.recovered {
            self.recover(phones, &mut out);
        }
        let now = self.now();
        self.pending
            .retain(|_, (_, at)| now.duration_since(*at) < Duration::from_secs(60));

        let ended: Vec<BtAddress> = self
            .calls
            .keys()
            .filter(|a| {
                !phones
                    .iter()
                    .any(|p| p.address == **a && (p.ringing || p.live))
            })
            .copied()
            .collect();
        for addr in ended {
            let input = phones.iter().find(|p| p.address == addr);
            self.release(addr, input, &mut out);
        }

        let busy: Vec<&PhoneInput> = phones.iter().filter(|p| p.ringing || p.live).collect();
        if busy.is_empty() {
            return out;
        }
        for p in &busy {
            if !self.calls.contains_key(&p.address) {
                let want = self
                    .pending
                    .remove(&p.address)
                    .map_or(Want::Auto, |(w, _)| w);
                let choice = prefs::load_choice(&self.paths.choice);
                self.calls
                    .insert(p.address, Engagement::new(want, now, choice));
            }
        }
        let Some(snap) = self.snapshot() else {
            return out;
        };
        for p in busy {
            self.step(p, &snap, &mut out);
        }
        out
    }

    fn step(&mut self, p: &PhoneInput, snap: &ServerSnapshot, out: &mut Vec<Request>) {
        let now = self.now();
        let opts = self.options;
        let addr = p.address;
        let voice = route::phone_voice(snap, addr);
        let (want, live_since, want_since) = {
            let e = self.calls.get_mut(&addr).expect("engaged");
            if p.live && e.live_since.is_none() {
                e.live_since = Some(now);
            }
            if !p.live {
                e.live_since = None;
            }
            (e.want, e.live_since, e.want_since)
        };

        // ---- the phone's RejectSCO flag
        if let Some(cur) = p.reject_sco {
            let desired = match want {
                Want::Computer => Some(false),
                Want::Phone => Some(true),
                Want::Auto => {
                    let holding = !p.live
                        || live_since
                            .is_some_and(|t| now.duration_since(t) < opts.keep_on_phone_for);
                    if holding {
                        Some(true)
                    } else {
                        // Hand the flag back so a later pick of Bluetooth on the phone works.
                        self.calls[&addr].reject_prev
                    }
                }
            };
            if let Some(d) = desired
                && d != cur
            {
                self.note_reject_change(addr, cur);
                out.push(Request::SetRejectSco {
                    address: addr,
                    reject: d,
                });
            }
        }

        if !snap.identity.is_pipewire() {
            let e = self.calls.get_mut(&addr).expect("engaged");
            e.problems = vec![FlowProblem::NotPipeWire];
            return;
        }

        // ---- the phone's card profile
        if let Some(card) = &voice.card {
            let kind = card.active_kind();
            let target = match want {
                Want::Computer if kind != Some(ProfileKind::PhoneGateway) => card
                    .gateway_profile()
                    .filter(|g| g.available)
                    .map(|g| g.name.clone()),
                Want::Phone if voice.streams_present() => Some("off".to_owned()),
                _ => None,
            };
            if let Some(target) = target {
                let previous = card.active_profile.clone().unwrap_or_else(|| "off".into());
                match self.restore.cards.iter_mut().find(|c| c.card == card.name) {
                    Some(c) => c.set_to = target.clone(),
                    None => self.restore.cards.push(CardRestore {
                        card: card.name.clone(),
                        previous,
                        set_to: target.clone(),
                    }),
                }
                self.persist();
                if let Some(e) = self.calls.get_mut(&addr)
                    && !e.touched_cards.contains(&card.name)
                {
                    e.touched_cards.push(card.name.clone());
                }
                let index = card.index;
                if let Some(s) = self.server()
                    && let Err(e) = s.set_card_profile(index, &target)
                {
                    self.drop_server_on(&e);
                    self.server_problem = Some(format!(
                        "Could not switch the phone's Bluetooth audio profile to {target}: {e}"
                    ));
                }
            }
        }

        // ---- ask the phone for the voice link
        if want == Want::Computer && p.live && !voice.streams_present() {
            let link_up = matches!(
                p.transport,
                Some(TransportState::Active | TransportState::Pending)
            );
            let since = live_since.map_or(want_since, |l| l.max(want_since));
            let e = self.calls.get_mut(&addr).expect("engaged");
            if !link_up
                && now.duration_since(since) >= opts.activate_after
                && e.activations < opts.max_activations
                && e.last_activation
                    .is_none_or(|t| now.duration_since(t) >= opts.activate_retry)
            {
                e.activations += 1;
                e.last_activation = Some(now);
                out.push(Request::Activate { address: addr });
            }
        }

        // ---- route the voice streams to the picked devices
        if want != Want::Phone && voice.streams_present() {
            self.route(addr, snap, &voice);
        } else if let Some(e) = self.calls.get_mut(&addr) {
            e.routed = None;
            e.notes.clear();
        }

        // ---- keep the user's mute on the microphone in use
        let mute_job = self.calls.get(&addr).and_then(|e| {
            let input = e.routed.as_ref()?.1.clone();
            Some((input, e.user_mute))
        });
        if let Some((input, user_mute)) = mute_job {
            let current = snap
                .sources
                .iter()
                .find(|s| s.name == input)
                .map(|s| s.mute);
            if let Some(m) = user_mute {
                if current != Some(m) {
                    let _ = self.apply_mute(addr, snap, &input, m);
                }
            } else if let Some(e) = self.calls.get_mut(&addr) {
                e.muted = current;
            }
        }

        // ---- judge the route
        self.check(p, snap, &voice);
    }

    fn route(&mut self, addr: BtAddress, snap: &ServerSnapshot, voice: &PhoneVoice) {
        let choice = self.calls[&addr].choice.clone();
        let out = route::resolve(snap, Role::Output, choice.output.as_deref());
        let inp = route::resolve(snap, Role::Input, choice.input.as_deref());
        let mut notes = Vec::new();
        for (role, r) in [(Role::Output, &out), (Role::Input, &inp)] {
            match r {
                Some(Resolved::Fallback { wanted, .. }) => notes.push(FlowProblem::PickedMissing {
                    role,
                    wanted: wanted.clone(),
                }),
                None => notes.push(match role {
                    Role::Output => FlowProblem::NoOutput,
                    Role::Input => FlowProblem::NoInput,
                }),
                _ => {}
            }
        }
        let sink_name = |idx: Option<u32>| {
            idx.and_then(|i| snap.sinks.iter().find(|s| s.index == i))
                .map(|s| s.name.clone())
        };
        let source_name = |idx: Option<u32>| {
            idx.and_then(|i| snap.sources.iter().find(|s| s.index == i))
                .map(|s| s.name.clone())
        };
        let mut failure = None;
        if let (Some(rx), Some(target)) = (&voice.rx, &out)
            && sink_name(rx.device).as_deref() != Some(target.name())
        {
            let (index, name) = (rx.index, target.name().to_owned());
            if let Some(s) = self.server()
                && let Err(e) = s.move_sink_input(index, &name)
            {
                failure = Some(format!("Could not move the caller's voice to {name}: {e}"));
                self.drop_server_on(&e);
            }
        }
        if let (Some(tx), Some(target)) = (&voice.tx, &inp)
            && source_name(tx.device).as_deref() != Some(target.name())
        {
            let (index, name) = (tx.index, target.name().to_owned());
            if let Some(s) = self.server()
                && let Err(e) = s.move_source_output(index, &name)
            {
                failure = Some(format!(
                    "Could not connect the microphone {name} to the call: {e}"
                ));
                self.drop_server_on(&e);
            }
        }
        if failure.is_some() {
            self.server_problem = failure;
        }
        let e = self.calls.get_mut(&addr).expect("engaged");
        e.notes = notes;
        e.codec = voice.codec().map(str::to_owned);
        e.routed = match (&out, &inp) {
            (Some(o), Some(i)) => Some((o.name().to_owned(), i.name().to_owned())),
            (Some(o), None) => Some((o.name().to_owned(), String::new())),
            (None, Some(i)) => Some((String::new(), i.name().to_owned())),
            (None, None) => None,
        };
    }

    fn check(&mut self, p: &PhoneInput, snap: &ServerSnapshot, voice: &PhoneVoice) {
        let now = self.now();
        let opts = self.options;
        let addr = p.address;
        let (want, start, checks, rx_heard, mic_heard, routed, user_mute) = {
            let e = &self.calls[&addr];
            let start = e.live_since.map(|l| l.max(e.want_since));
            (
                e.want,
                start,
                e.checks,
                e.rx_heard,
                e.mic_heard,
                e.routed.clone(),
                e.user_mute,
            )
        };
        let Some(start) = start else {
            return;
        };
        let here = want == Want::Computer || (want == Want::Auto && voice.streams_present());
        if !here || now.duration_since(start) < opts.check_after {
            return;
        }
        let mut problems = Vec::new();
        if !voice.streams_present() {
            if want == Want::Computer {
                problems.push(FlowProblem::NoVoiceLink);
            }
        } else {
            if voice.rx.as_ref().is_some_and(|s| s.device.is_none()) {
                problems.push(FlowProblem::RxUnlinked);
            }
            if voice.tx.as_ref().is_some_and(|s| s.device.is_none()) {
                problems.push(FlowProblem::TxUnlinked);
            }
            if let Some((_, input)) = &routed {
                let src = snap.sources.iter().find(|s| &s.name == input);
                if src.is_some_and(|s| s.mute) && user_mute != Some(true) {
                    problems.push(FlowProblem::MicMuted {
                        device: route::describe(snap, Role::Input, input),
                    });
                }
            }
        }
        // Level checks: at most twice per call, only until sound has been seen once. The first
        // one can only clear: a few seconds into a call true digital silence is normal (dialling
        // before the ringback, a far end that has not spoken, a source just resumed from
        // suspend), so silence is reported only when the later check finds it too.
        let may_report_silence = checks >= 1;
        let due = match checks {
            0 => true,
            1 => now.duration_since(start) >= opts.recheck_after,
            _ => false,
        };
        let mut heard = (rx_heard, mic_heard);
        if due && voice.streams_present() {
            if !rx_heard
                && let Some(rx) = &voice.rx
                && let Some(sink) = rx
                    .device
                    .and_then(|i| snap.sinks.iter().find(|s| s.index == i))
            {
                let t = PeakTarget::SinkInput {
                    index: rx.index,
                    sink: sink.name.clone(),
                };
                match self.measure(&t) {
                    Some(v) if v > 0.0 => heard.0 = true,
                    Some(_) if may_report_silence => problems.push(FlowProblem::RxSilent),
                    _ => {}
                }
            }
            if !mic_heard
                && user_mute != Some(true)
                && let Some((_, input)) = &routed
                && !input.is_empty()
                && !snap.sources.iter().any(|s| &s.name == input && s.mute)
            {
                match self.measure(&PeakTarget::Source(input.clone())) {
                    Some(v) if v > 0.0 => heard.1 = true,
                    Some(_) if may_report_silence => problems.push(FlowProblem::MicSilent {
                        device: route::describe(snap, Role::Input, input),
                    }),
                    _ => {}
                }
            }
        } else {
            // Between level checks, keep the last level findings.
            let e = &self.calls[&addr];
            problems.extend(
                e.problems
                    .iter()
                    .filter(|p| matches!(p, FlowProblem::RxSilent | FlowProblem::MicSilent { .. }))
                    .cloned(),
            );
        }
        let e = self.calls.get_mut(&addr).expect("engaged");
        if due {
            e.checks += 1;
        }
        e.rx_heard = heard.0;
        e.mic_heard = heard.1;
        if heard.0 {
            problems.retain(|p| *p != FlowProblem::RxSilent);
        }
        if heard.1 {
            problems.retain(|p| !matches!(p, FlowProblem::MicSilent { .. }));
        }
        e.problems = problems;
    }

    fn measure(&mut self, t: &PeakTarget) -> Option<f32> {
        let window = self.options.peak_window;
        let s = self.server()?;
        match s.peak(t, window) {
            Ok(v) => v,
            Err(e) => {
                self.drop_server_on(&e);
                None
            }
        }
    }

    // ------------------------------------------------------------------ undo

    fn release(&mut self, addr: BtAddress, present: Option<&PhoneInput>, out: &mut Vec<Request>) {
        let Some(e) = self.calls.remove(&addr) else {
            return;
        };
        // Phone flag: back to what it was, if the phone is still here.
        let key = addr.expose();
        if let Some(pos) = self.restore.reject.iter().position(|r| r.address == key) {
            let r = self.restore.reject.remove(pos);
            if let Some(p) = present
                && p.reject_sco.is_some_and(|cur| cur != r.previous)
            {
                out.push(Request::SetRejectSco {
                    address: addr,
                    reject: r.previous,
                });
            }
        }
        if !e.touched_cards.is_empty() || !e.touched_sources.is_empty() {
            match self.snapshot() {
                Some(snap) => self.undo(
                    &snap,
                    Undo::Only {
                        cards: &e.touched_cards,
                        sources: &e.touched_sources,
                    },
                ),
                // The sound server is away: the entries stay in the restore file and are
                // replayed at the next start.
                None => self.recovered = false,
            }
        }
        self.persist();
    }

    /// Undo recorded card and mute changes.
    fn undo(&mut self, snap: &ServerSnapshot, which: Undo<'_>) {
        let (cards_only, sources_only, all): (&[String], &[String], bool) = match which {
            Undo::All => (&[], &[], true),
            Undo::Only { cards, sources } => (cards, sources, false),
        };
        let cards: Vec<CardRestore> = self
            .restore
            .cards
            .iter()
            .filter(|c| all || cards_only.contains(&c.card))
            .cloned()
            .collect();
        for c in cards {
            let now = snap.cards.iter().find(|x| x.name == c.card);
            let done = match now {
                // The card is gone (phone disconnected): nothing left to undo.
                None => true,
                Some(x) if x.active_profile.as_deref() == Some(c.set_to.as_str()) => {
                    let index = x.index;
                    match self.server() {
                        Some(s) => match s.set_card_profile(index, &c.previous) {
                            Ok(()) => true,
                            Err(e) => {
                                self.drop_server_on(&e);
                                false
                            }
                        },
                        None => false,
                    }
                }
                // Someone else changed it since: leave it.
                Some(_) => true,
            };
            if done {
                self.restore.cards.retain(|x| x.card != c.card);
            }
        }
        let mutes: Vec<MuteRestore> = self
            .restore
            .mutes
            .iter()
            .filter(|m| all || sources_only.contains(&m.source))
            .cloned()
            .collect();
        for m in mutes {
            let now = snap.sources.iter().find(|x| x.name == m.source);
            let done = match now {
                None => true,
                Some(x) if x.mute == m.set_to && m.set_to != m.was_muted => match self.server() {
                    Some(s) => match s.set_source_mute(&m.source, m.was_muted) {
                        Ok(()) => true,
                        Err(e) => {
                            self.drop_server_on(&e);
                            false
                        }
                    },
                    None => false,
                },
                Some(_) => true,
            };
            if done {
                self.restore.mutes.retain(|x| x.source != m.source);
            }
        }
    }

    /// Replay the restore file left by a previous run.
    fn recover(&mut self, phones: &[PhoneInput], out: &mut Vec<Request>) {
        let file = prefs::load_restore(&self.paths.restore);
        if file.is_empty() {
            self.recovered = true;
            return;
        }
        // Never undo under a live call's feet; replay once calls are over.
        if !self.calls.is_empty() {
            return;
        }
        let Some(snap) = self.snapshot() else {
            return;
        };
        self.restore = file;
        self.undo(&snap, Undo::All);
        let reject = std::mem::take(&mut self.restore.reject);
        for r in reject {
            let Some(addr) = BtAddress::parse(&r.address) else {
                continue;
            };
            // A phone that is not connected gets the service default again on reconnect.
            if let Some(p) = phones.iter().find(|p| p.address == addr)
                && p.reject_sco.is_some_and(|cur| cur != r.previous)
            {
                out.push(Request::SetRejectSco {
                    address: addr,
                    reject: r.previous,
                });
            }
        }
        self.persist();
        self.recovered = self.restore.is_empty();
    }

    /// Undo everything now (service shutdown). Telephony flags are returned for the caller.
    pub fn shutdown(&mut self, phones: &[PhoneInput]) -> Vec<Request> {
        let mut out = Vec::new();
        let addrs: Vec<BtAddress> = self.calls.keys().copied().collect();
        for a in addrs {
            let p = phones.iter().find(|p| p.address == a);
            self.release(a, p, &mut out);
        }
        out
    }
}

#[derive(Clone, Copy)]
enum Undo<'a> {
    All,
    Only {
        cards: &'a [String],
        sources: &'a [String],
    },
}

fn codec_label(c: &str) -> String {
    match c {
        "cvsd" => "CVSD, narrowband 8 kHz".into(),
        "msbc" => "mSBC, wideband 16 kHz".into(),
        "lc3_swb" | "lc3-swb" => "LC3-SWB, 32 kHz".into(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
