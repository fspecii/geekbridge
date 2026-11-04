// SPDX-License-Identifier: GPL-3.0-or-later
//! The one-button "Set up call audio": a state machine that drives the service's pieces in order and
//! moves on by itself.
//!
//! Adapter present, Bluetooth on, visible to the phone for two minutes, the phone's pairing prompt and
//! the code compared on both screens, pair + trust + hands-free connect (the service does these in
//! `CallAudioPair { Start }`), the user's configuration fix when the sound server needs it, choose the
//! headphones and microphone, a test tone, done.
//!
//! This module has no I/O and no clock of its own: the caller passes the time in milliseconds and what
//! the service said ([`Observation`]); it answers with what to send next ([`Act`]) and keeps the stage
//! the window shows. [`Backend`] is the seam to the service. The real one is a few requests in the
//! window crate; [`FakeBackend`] is a scripted service for tests and screenshots.
//!
//! Integration points with the call-audio backend (`bridge_calls::setup`, `hear`): the IPC requests
//! `HearCalls` and `CallAudioPair { Start | Confirm | Cancel | FixConfig | Forget }`, the step ids of
//! `HearCalls.steps` (`paired`, `connected`, `hfp`, `wireplumber`, `powered`) and the phases of
//! `HearCalls.setup.phase` (`idle`, `working`, `confirm`, `done`, `failed`). If the service renames
//! one of them, change it in [`ids`] (the only place they are spelled); the fake speaks the same names.
use crate::{
    call_audio::{Setup, TestPhase},
    ipc::{CallAudioSetupAction, HearCalls, HearCallsStep},
};
use std::collections::VecDeque;

/// Names the service uses; the only place this module spells them.
pub mod ids {
    pub const PHASE_IDLE: &str = "idle";
    pub const PHASE_WORKING: &str = "working";
    pub const PHASE_CONFIRM: &str = "confirm";
    pub const PHASE_DONE: &str = "done";
    pub const PHASE_FAILED: &str = "failed";
    pub const STEP_POWERED: &str = "powered";
    pub const STEP_PAIRED: &str = "paired";
    pub const STEP_CONNECTED: &str = "connected";
    pub const STEP_HFP: &str = "hfp";
    pub const STEP_WIREPLUMBER: &str = "wireplumber";
    pub const STATE_OK: &str = "ok";
    pub const STATE_FAIL: &str = "fail";
}

/// How long to wait for the service to show it started before calling it a failure.
const START_GRACE_MS: u64 = 8_000;
/// After the pairing says "done", how long the hands-free link may take to show as connected.
const LINK_GRACE_MS: u64 = 20_000;
/// How long after pressing "Test" a result is accepted without having seen the test run.
const TEST_GRACE_MS: u64 = 12_000;

/// The seven things the person sees, in order. `Stage::position` gives the step number.
pub const STEP_COUNT: usize = 7;

/// Why the flow stopped. The window turns each into one sentence and one fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The service reported a problem; its own words (shown as detail, never parsed for the fix).
    Service(String),
    /// The service did not start the setup.
    NoAnswer,
    /// Paired but the hands-free link never came up.
    HandsFree,
    /// The test tone could not be played.
    NoTone,
    /// The microphone could not be opened.
    NoMicrophone,
    /// The microphone heard nothing.
    Silent,
    /// The test never ran.
    TestStuck,
    /// No headphones or speakers found to choose from.
    NoDevices,
    /// The service cannot do this (old service, other system).
    Unsupported(String),
}

/// Which one-line fix goes with a failure; the window maps these to its catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixHint {
    StartService,
    Unblock,
    PhoneBluetooth,
    PhoneCalls,
    TryAgain,
    Devices,
    Microphone,
    Update,
}

impl Failure {
    /// Classify the service's own words into a fix. Unknown text gets "try again".
    pub fn hint(&self) -> FixHint {
        match self {
            Failure::Service(m) => {
                let m = m.to_ascii_lowercase();
                if m.contains("cannot reach bluez") || m.contains("bluetooth.service") {
                    FixHint::StartService
                } else if m.contains("rfkill") || m.contains("could not turn bluetooth on") {
                    FixHint::Unblock
                } else if m.contains("calls") && m.contains("phone") || m.contains("hands-free") {
                    FixHint::PhoneCalls
                } else if m.contains("no phone")
                    || m.contains("not found")
                    || m.contains("timed out")
                    || m.contains("timeout")
                {
                    FixHint::PhoneBluetooth
                } else {
                    FixHint::TryAgain
                }
            }
            Failure::NoAnswer | Failure::TestStuck => FixHint::TryAgain,
            Failure::HandsFree => FixHint::PhoneCalls,
            Failure::NoTone | Failure::NoDevices => FixHint::Devices,
            Failure::NoMicrophone | Failure::Silent => FixHint::Microphone,
            Failure::Unsupported(_) => FixHint::Update,
        }
    }
}

/// Where the flow is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// Not started (or cancelled).
    #[default]
    Idle,
    /// Started, but this computer has no Bluetooth adapter: waits for one to be plugged in.
    NoAdapter,
    /// Switching Bluetooth on.
    PowerOn,
    /// Visible for two minutes; the phone must open Bluetooth settings and tap this computer.
    Visible,
    /// Compare this code with the phone's.
    Confirm {
        passkey: String,
        device: Option<String>,
    },
    /// Pairing, trusting and connecting the hands-free link; also the configuration fix.
    Connecting,
    /// Choose headphones and microphone; waits for the person.
    Devices,
    /// Tone and microphone test running.
    Testing,
    /// Everything works.
    Done,
    /// Stopped; `Flow::start` tries again.
    Failed(Failure),
}

impl Stage {
    /// 1-based step number for "Step n of 7"; `None` when idle, done or failed.
    pub fn position(&self) -> Option<usize> {
        Some(match self {
            Stage::NoAdapter => 1,
            Stage::PowerOn => 2,
            Stage::Visible => 3,
            Stage::Confirm { .. } => 4,
            Stage::Connecting => 5,
            Stage::Devices => 6,
            Stage::Testing => 7,
            Stage::Idle | Stage::Done | Stage::Failed(_) => return None,
        })
    }

    /// Something is in progress (the window keeps polling fast and shows Cancel).
    pub fn active(&self) -> bool {
        self.position().is_some()
    }
}

/// What the service said last. `hear` is `None` when the service is too old to know the request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    pub hear: Option<HearCalls>,
    pub setup: Option<Setup>,
}

impl Observation {
    fn step(&self, id: &str) -> Option<&HearCallsStep> {
        self.hear.as_ref()?.steps.iter().find(|s| s.id == id)
    }
    fn step_ok(&self, id: &str) -> bool {
        self.step(id).is_some_and(|s| s.state == ids::STATE_OK)
    }
    fn phase(&self) -> &str {
        self.hear
            .as_ref()
            .map_or(ids::PHASE_IDLE, |h| h.setup.phase.as_str())
    }
    pub fn no_bluetooth(&self) -> bool {
        self.hear.as_ref().is_some_and(|h| h.no_bluetooth)
    }
}

/// A request to the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Pair(CallAudioSetupAction),
    Choose {
        output: Option<String>,
        input: Option<String>,
    },
    Test,
}

/// The seam to the service: ask, and send one request (each returns the new view).
pub trait Backend: Send {
    fn observe(&mut self) -> Result<Observation, String>;
    fn act(&mut self, act: &Act) -> Result<Observation, String>;
}

/// The state machine. Drive it with [`Flow::start`], [`Flow::cancel`], [`Flow::confirm`],
/// [`Flow::use_devices`] and, on every new observation, [`Flow::advance`].
#[derive(Clone, Debug, Default)]
pub struct Flow {
    stage: Stage,
    /// Pressed Start; the pairing request is sent when an adapter exists.
    wanted: bool,
    started_at: Option<u64>,
    confirmed: bool,
    done_at: Option<u64>,
    fix_sent: bool,
    devices_ok: bool,
    test_sent_at: Option<u64>,
    test_seen_running: bool,
}

impl Flow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stage(&self) -> &Stage {
        &self.stage
    }

    /// The primary button. Starts over from any stage that is not running.
    pub fn start(&mut self, obs: &Observation, now_ms: u64) -> Vec<Act> {
        if self.stage.active() {
            return Vec::new();
        }
        *self = Self {
            wanted: true,
            ..Self::default()
        };
        let Some(h) = &obs.hear else {
            self.stage = Stage::Failed(Failure::Unsupported(String::new()));
            self.wanted = false;
            return Vec::new();
        };
        if h.no_bluetooth {
            self.stage = Stage::NoAdapter;
            return Vec::new();
        }
        self.send_start(now_ms)
    }

    fn send_start(&mut self, now_ms: u64) -> Vec<Act> {
        self.wanted = false;
        self.started_at = Some(now_ms);
        self.stage = Stage::PowerOn;
        vec![Act::Pair(CallAudioSetupAction::Start { phone: None })]
    }

    /// Stop. Whatever the service was doing is cancelled; nothing already paired is removed.
    pub fn cancel(&mut self) -> Vec<Act> {
        let was = self.stage.active();
        *self = Self::default();
        if was {
            vec![Act::Pair(CallAudioSetupAction::Cancel)]
        } else {
            Vec::new()
        }
    }

    /// The person compared the code.
    pub fn confirm(&mut self, accept: bool) -> Vec<Act> {
        if !matches!(self.stage, Stage::Confirm { .. }) {
            return Vec::new();
        }
        if accept {
            self.confirmed = true;
            self.stage = Stage::Connecting;
        } else {
            *self = Self::default();
            self.stage = Stage::Failed(Failure::Service("pairing was refused here".into()));
        }
        vec![Act::Pair(CallAudioSetupAction::Confirm { accept })]
    }

    /// "Use these devices": go on to the test.
    pub fn use_devices(&mut self) {
        if self.stage == Stage::Devices {
            self.devices_ok = true;
        }
    }

    /// Run the test again from the done or failed screen (headphones already chosen).
    pub fn retest(&mut self) {
        if matches!(
            self.stage,
            Stage::Done | Stage::Failed(Failure::Silent | Failure::NoTone | Failure::NoMicrophone)
        ) {
            self.stage = Stage::Devices;
            self.devices_ok = true;
            self.test_sent_at = None;
            self.test_seen_running = false;
        }
    }

    fn fail(&mut self, f: Failure) {
        self.stage = Stage::Failed(f);
        self.wanted = false;
    }

    /// Fold in what the service says now; returns what to send. Safe to call on every poll.
    pub fn advance(&mut self, obs: &Observation, now_ms: u64) -> Vec<Act> {
        let mut acts = Vec::new();
        if matches!(self.stage, Stage::Idle | Stage::Done | Stage::Failed(_)) {
            return acts;
        }
        let Some(h) = &obs.hear else {
            self.fail(Failure::Unsupported(String::new()));
            return acts;
        };
        // Pulling the adapter out stops the flow; plugging one in starts a pressed button.
        if h.no_bluetooth {
            if self.stage != Stage::NoAdapter {
                let was_active = self.stage.active();
                self.wanted = true;
                self.stage = Stage::NoAdapter;
                if was_active {
                    acts.push(Act::Pair(CallAudioSetupAction::Cancel));
                }
            }
            return acts;
        }
        if self.stage == Stage::NoAdapter {
            return if self.wanted {
                self.send_start(now_ms)
            } else {
                self.stage = Stage::Idle;
                acts
            };
        }
        match self.stage {
            Stage::PowerOn | Stage::Visible | Stage::Confirm { .. } | Stage::Connecting => {
                self.pairing(obs, now_ms, &mut acts)
            }
            Stage::Devices => {
                if self.devices_ok {
                    if obs.setup.as_ref().is_some_and(|s| s.outputs.is_empty()) {
                        self.devices_ok = false;
                        self.fail(Failure::NoDevices);
                        return acts;
                    }
                    self.stage = Stage::Testing;
                    self.test_sent_at = Some(now_ms);
                    self.test_seen_running = false;
                    acts.push(Act::Test);
                }
            }
            Stage::Testing => self.testing(obs, now_ms),
            _ => {}
        }
        acts
    }

    fn pairing(&mut self, obs: &Observation, now_ms: u64, acts: &mut Vec<Act>) {
        let h = obs.hear.as_ref().expect("checked by the caller");
        match obs.phase() {
            ids::PHASE_CONFIRM => {
                self.stage = Stage::Confirm {
                    passkey: h.setup.passkey.clone().unwrap_or_default(),
                    device: h.setup.device.clone(),
                };
            }
            ids::PHASE_FAILED => {
                self.fail(Failure::Service(h.setup.message.clone()));
            }
            ids::PHASE_WORKING => {
                self.stage = if self.confirmed {
                    Stage::Connecting
                } else if obs.step(ids::STEP_POWERED).is_some() && !obs.step_ok(ids::STEP_POWERED) {
                    Stage::PowerOn
                } else {
                    Stage::Visible
                };
            }
            ids::PHASE_DONE => {
                let since = *self.done_at.get_or_insert(now_ms);
                self.stage = Stage::Connecting;
                let wp_broken = obs
                    .step(ids::STEP_WIREPLUMBER)
                    .is_some_and(|s| s.state == ids::STATE_FAIL);
                if wp_broken && !self.fix_sent {
                    self.fix_sent = true;
                    acts.push(Act::Pair(CallAudioSetupAction::FixConfig));
                }
                let linked = obs.step_ok(ids::STEP_PAIRED)
                    && obs.step_ok(ids::STEP_CONNECTED)
                    && obs.step_ok(ids::STEP_HFP);
                if linked {
                    self.stage = Stage::Devices;
                } else if now_ms.saturating_sub(since) > LINK_GRACE_MS {
                    self.fail(Failure::HandsFree);
                }
            }
            _ => {
                // idle: our Start has not shown yet.
                let since = self.started_at.unwrap_or(now_ms);
                if now_ms.saturating_sub(since) > START_GRACE_MS {
                    self.fail(Failure::NoAnswer);
                }
            }
        }
    }

    fn testing(&mut self, obs: &Observation, now_ms: u64) {
        let Some(s) = &obs.setup else { return };
        let sent = self.test_sent_at.unwrap_or(now_ms);
        match s.test.phase {
            TestPhase::Tone | TestPhase::Listening => self.test_seen_running = true,
            TestPhase::Done if self.test_seen_running => {
                if s.test.tone_played == Some(false) {
                    self.fail(Failure::NoTone);
                } else if s.test.mic_opened == Some(false) {
                    self.fail(Failure::NoMicrophone);
                } else if s.test.heard() {
                    self.stage = Stage::Done;
                } else {
                    self.fail(Failure::Silent);
                }
            }
            _ => {
                if now_ms.saturating_sub(sent) > TEST_GRACE_MS {
                    self.fail(Failure::TestStuck);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// A scripted service

/// A service that plays a script: for tests and screenshots, never talks to anything. Each request
/// moves a small simulation of the real service's pairing along; `observe` advances it one tick.
#[derive(Debug)]
pub struct FakeBackend {
    pub hear: HearCalls,
    pub setup: Setup,
    /// What the service would do next on each observe while a run is going.
    script: VecDeque<Beat>,
    pub sent: Vec<Act>,
    /// Fail every request (a service that went away).
    pub down: bool,
}

/// One tick of the scripted pairing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Beat {
    Working,
    Confirm(String),
    /// Pairing finished; the hands-free link shows as connected.
    Done,
    /// Pairing finished but the link never connects.
    DoneUnlinked,
    Fail(String),
    /// Test result: heard / silent.
    Heard(bool),
}

fn step(id: &str, ok: bool) -> HearCallsStep {
    HearCallsStep {
        id: id.into(),
        title: id.into(),
        state: if ok { ids::STATE_OK } else { ids::STATE_FAIL }.into(),
        detail: String::new(),
        fix: None,
    }
}

impl FakeBackend {
    /// A computer with an adapter that is switched off and no phone set up.
    pub fn new(adapter: bool, setup: Setup, script: Vec<Beat>) -> Self {
        let hear = HearCalls {
            no_bluetooth: !adapter,
            steps: vec![
                step(ids::STEP_POWERED, false),
                step(ids::STEP_PAIRED, false),
                step(ids::STEP_CONNECTED, false),
                step(ids::STEP_HFP, false),
            ],
            setup: crate::ipc::CallAudioSetupState {
                phase: ids::PHASE_IDLE.into(),
                ..Default::default()
            },
            ..Default::default()
        };
        Self {
            hear,
            setup,
            script: script.into(),
            sent: Vec::new(),
            down: false,
        }
    }

    pub fn plug_in_adapter(&mut self) {
        self.hear.no_bluetooth = false;
    }

    fn set_step(&mut self, id: &str, ok: bool) {
        if let Some(s) = self.hear.steps.iter_mut().find(|s| s.id == id) {
            s.state = if ok { ids::STATE_OK } else { ids::STATE_FAIL }.into();
        }
    }

    fn view(&self) -> Observation {
        Observation {
            hear: Some(self.hear.clone()),
            setup: Some(self.setup.clone()),
        }
    }

    fn tick(&mut self) {
        let running = matches!(
            self.hear.setup.phase.as_str(),
            ids::PHASE_WORKING | ids::PHASE_CONFIRM
        );
        let testing = matches!(
            self.setup.test.phase,
            TestPhase::Tone | TestPhase::Listening
        );
        if !running && !testing {
            return;
        }
        if self.setup.test.phase == TestPhase::Tone {
            self.setup.test.phase = TestPhase::Listening;
            return;
        }
        // The confirm beat waits for the answer.
        if self.hear.setup.phase == ids::PHASE_CONFIRM {
            return;
        }
        let Some(beat) = self.script.pop_front() else {
            return;
        };
        match beat {
            Beat::Working => {
                self.hear.setup.phase = ids::PHASE_WORKING.into();
                self.set_step(ids::STEP_POWERED, true);
            }
            Beat::Confirm(code) => {
                self.hear.setup.phase = ids::PHASE_CONFIRM.into();
                self.hear.setup.passkey = Some(code);
                self.hear.setup.device = Some("Phone".into());
            }
            Beat::Done | Beat::DoneUnlinked => {
                self.hear.setup.phase = ids::PHASE_DONE.into();
                self.hear.setup.passkey = None;
                let linked = beat == Beat::Done;
                self.set_step(ids::STEP_PAIRED, true);
                self.set_step(ids::STEP_CONNECTED, linked);
                self.set_step(ids::STEP_HFP, linked);
            }
            Beat::Fail(m) => {
                self.hear.setup.phase = ids::PHASE_FAILED.into();
                self.hear.setup.message = m;
            }
            Beat::Heard(heard) => {
                self.setup.test.phase = TestPhase::Done;
                self.setup.test.levels = if heard { vec![2, 40, 33] } else { vec![0, 0] };
            }
        }
    }
}

impl Backend for FakeBackend {
    fn observe(&mut self) -> Result<Observation, String> {
        if self.down {
            return Err("service down".into());
        }
        self.tick();
        Ok(self.view())
    }

    fn act(&mut self, act: &Act) -> Result<Observation, String> {
        if self.down {
            return Err("service down".into());
        }
        self.sent.push(act.clone());
        match act {
            Act::Pair(CallAudioSetupAction::Start { .. }) => {
                self.hear.setup.phase = ids::PHASE_WORKING.into();
                self.hear.setup.message.clear();
            }
            Act::Pair(CallAudioSetupAction::Confirm { accept }) => {
                self.hear.setup.phase = if *accept {
                    ids::PHASE_WORKING
                } else {
                    ids::PHASE_FAILED
                }
                .into();
                self.hear.setup.passkey = None;
            }
            Act::Pair(CallAudioSetupAction::Cancel) => {
                self.hear.setup.phase = ids::PHASE_IDLE.into();
                self.script.clear();
            }
            Act::Pair(CallAudioSetupAction::FixConfig) => {
                self.set_step(ids::STEP_WIREPLUMBER, true);
            }
            Act::Pair(CallAudioSetupAction::Forget) => {
                self.hear.setup = Default::default();
                self.hear.setup.phase = ids::PHASE_IDLE.into();
                for id in [ids::STEP_PAIRED, ids::STEP_CONNECTED, ids::STEP_HFP] {
                    self.set_step(id, false);
                }
            }
            Act::Choose { output, input } => {
                self.setup.chosen_output = output.clone();
                self.setup.chosen_input = input.clone();
            }
            Act::Test => {
                self.setup.test.phase = TestPhase::Tone;
                self.setup.test.levels.clear();
                self.setup.test.tone_played = Some(true);
                self.setup.test.mic_opened = Some(true);
            }
        }
        Ok(self.view())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call_audio::{Device, LinuxFacts, Rfkill, Selection, build_linux};

    fn setup() -> Setup {
        let dev = |id: &str| Device {
            id: id.into(),
            label: id.into(),
            bluetooth: false,
            default: true,
        };
        build_linux(&LinuxFacts {
            adapters: 1,
            rfkill: Some(Rfkill::default()),
            powered: Some(true),
            paired_phones: Some(vec![]),
            connected_phones: 0,
            registered_phones: 0,
            outputs: vec![dev("out")],
            inputs: vec![dev("in")],
            selection: Selection::default(),
            audio_server_ok: true,
        })
    }

    /// Run the flow against a backend: each loop is one poll (500 ms).
    struct Rig {
        flow: Flow,
        be: FakeBackend,
        now: u64,
        seen: Vec<Stage>,
    }

    impl Rig {
        fn new(adapter: bool, script: Vec<Beat>) -> Self {
            Self {
                flow: Flow::new(),
                be: FakeBackend::new(adapter, setup(), script),
                now: 0,
                seen: Vec::new(),
            }
        }
        fn send(&mut self, acts: Vec<Act>) {
            for a in acts {
                let _ = self.be.act(&a);
            }
        }
        fn press(&mut self) {
            let obs = self.be.observe().unwrap();
            let a = self.flow.start(&obs, self.now);
            self.send(a);
            self.note();
        }
        fn note(&mut self) {
            if self.seen.last() != Some(self.flow.stage()) {
                self.seen.push(self.flow.stage().clone());
            }
        }
        fn poll(&mut self, n: usize) {
            for _ in 0..n {
                self.now += 500;
                let obs = self.be.observe().unwrap();
                let a = self.flow.advance(&obs, self.now);
                self.send(a);
                self.note();
            }
        }
        fn until(&mut self, f: impl Fn(&Stage) -> bool, max: usize) {
            for _ in 0..max {
                if f(self.flow.stage()) {
                    return;
                }
                self.poll(1);
            }
            panic!("stuck at {:?}; seen {:?}", self.flow.stage(), self.seen);
        }
    }

    fn happy() -> Vec<Beat> {
        vec![
            Beat::Working,
            Beat::Confirm("123456".into()),
            Beat::Working,
            Beat::Done,
            Beat::Heard(true),
        ]
    }

    #[test]
    fn the_whole_path_advances_by_itself_with_one_press_and_one_confirmation() {
        let mut r = Rig::new(true, happy());
        r.press();
        assert_eq!(
            r.be.sent,
            [Act::Pair(CallAudioSetupAction::Start { phone: None })]
        );
        r.until(|s| matches!(s, Stage::Confirm { .. }), 10);
        assert_eq!(
            r.flow.stage(),
            &Stage::Confirm {
                passkey: "123456".into(),
                device: Some("Phone".into())
            }
        );
        let a = r.flow.confirm(true);
        r.send(a);
        r.until(|s| *s == Stage::Devices, 10);
        // It waits for the person to take the devices.
        r.poll(5);
        assert_eq!(r.flow.stage(), &Stage::Devices);
        r.flow.use_devices();
        r.until(|s| *s == Stage::Done, 20);
        assert!(r.be.sent.contains(&Act::Test));
        // Order of what was shown.
        let order: Vec<usize> = r.seen.iter().filter_map(|s| s.position()).collect();
        assert!(order.windows(2).all(|w| w[0] <= w[1]), "{order:?}");
        assert_eq!(*order.first().unwrap(), 2);
        assert_eq!(*order.last().unwrap(), 7);
    }

    #[test]
    fn no_adapter_waits_and_starts_when_one_is_plugged_in() {
        let mut r = Rig::new(false, happy());
        r.press();
        assert_eq!(r.flow.stage(), &Stage::NoAdapter);
        assert!(r.be.sent.is_empty());
        r.poll(4);
        assert_eq!(r.flow.stage(), &Stage::NoAdapter);
        r.be.plug_in_adapter();
        r.poll(1);
        assert_eq!(r.flow.stage(), &Stage::PowerOn);
        assert_eq!(r.be.sent.len(), 1);
    }

    #[test]
    fn unplugging_mid_run_cancels_and_waits_again() {
        let mut r = Rig::new(true, happy());
        r.press();
        r.poll(1);
        r.be.hear.no_bluetooth = true;
        r.poll(1);
        assert_eq!(r.flow.stage(), &Stage::NoAdapter);
        assert_eq!(
            r.be.sent.last(),
            Some(&Act::Pair(CallAudioSetupAction::Cancel))
        );
    }

    #[test]
    fn cancel_returns_to_idle_and_tells_the_service() {
        let mut r = Rig::new(true, happy());
        r.press();
        r.poll(2);
        let a = r.flow.cancel();
        assert_eq!(a, [Act::Pair(CallAudioSetupAction::Cancel)]);
        assert_eq!(r.flow.stage(), &Stage::Idle);
        assert!(
            r.flow.cancel().is_empty(),
            "nothing running, nothing to cancel"
        );
    }

    #[test]
    fn refusing_the_code_fails_with_a_try_again_fix() {
        let mut r = Rig::new(true, happy());
        r.press();
        r.until(|s| matches!(s, Stage::Confirm { .. }), 10);
        let a = r.flow.confirm(false);
        r.send(a);
        assert!(matches!(r.flow.stage(), Stage::Failed(Failure::Service(_))));
        // Press again: starts over.
        r.be.script = happy().into();
        r.press();
        assert_eq!(r.flow.stage(), &Stage::PowerOn);
    }

    #[test]
    fn a_service_failure_shows_its_words_and_a_fix_by_kind() {
        let mut r = Rig::new(
            true,
            vec![
                Beat::Working,
                Beat::Fail("cannot reach BlueZ (is bluetooth.service running?)".into()),
            ],
        );
        r.press();
        r.until(|s| matches!(s, Stage::Failed(_)), 10);
        let Stage::Failed(f) = r.flow.stage().clone() else {
            unreachable!()
        };
        assert_eq!(f.hint(), FixHint::StartService);
        for (m, h) in [
            (
                "could not turn Bluetooth on (x); try: rfkill unblock bluetooth",
                FixHint::Unblock,
            ),
            ("no phone found within 150 s", FixHint::PhoneBluetooth),
            (
                "the phone did not accept the calls (hands-free) connection",
                FixHint::PhoneCalls,
            ),
            ("something odd", FixHint::TryAgain),
        ] {
            assert_eq!(Failure::Service(m.into()).hint(), h, "{m}");
        }
    }

    #[test]
    fn a_hands_free_link_that_never_comes_up_fails_after_a_grace_period() {
        let mut r = Rig::new(true, vec![Beat::Working, Beat::DoneUnlinked]);
        r.press();
        r.until(|s| matches!(s, Stage::Failed(_)), 100);
        assert_eq!(r.flow.stage(), &Stage::Failed(Failure::HandsFree));
        assert_eq!(Failure::HandsFree.hint(), FixHint::PhoneCalls);
    }

    #[test]
    fn a_service_that_never_starts_is_reported() {
        let mut r = Rig::new(true, vec![]);
        r.press();
        r.be.hear.setup.phase = ids::PHASE_IDLE.into(); // the start was lost
        r.until(|s| matches!(s, Stage::Failed(_)), 40);
        assert_eq!(r.flow.stage(), &Stage::Failed(Failure::NoAnswer));
    }

    #[test]
    fn the_configuration_fix_is_sent_once_and_without_asking_again() {
        let mut r = Rig::new(true, happy());
        r.be.hear.steps.push(step(ids::STEP_WIREPLUMBER, false));
        r.press();
        r.until(|s| matches!(s, Stage::Confirm { .. }), 10);
        let a = r.flow.confirm(true);
        r.send(a);
        r.until(|s| *s == Stage::Devices, 10);
        let fixes =
            r.be.sent
                .iter()
                .filter(|a| **a == Act::Pair(CallAudioSetupAction::FixConfig))
                .count();
        assert_eq!(fixes, 1);
    }

    #[test]
    fn a_silent_microphone_fails_the_test_and_retest_runs_it_again() {
        let mut beats = happy();
        *beats.last_mut().unwrap() = Beat::Heard(false);
        beats.push(Beat::Heard(true));
        let mut r = Rig::new(true, beats);
        r.press();
        r.until(|s| matches!(s, Stage::Confirm { .. }), 10);
        let a = r.flow.confirm(true);
        r.send(a);
        r.until(|s| *s == Stage::Devices, 10);
        r.flow.use_devices();
        r.until(|s| matches!(s, Stage::Failed(_)), 20);
        assert_eq!(r.flow.stage(), &Stage::Failed(Failure::Silent));
        assert_eq!(Failure::Silent.hint(), FixHint::Microphone);
        r.be.setup.test.phase = TestPhase::Idle;
        r.flow.retest();
        r.until(|s| *s == Stage::Done, 20);
        assert_eq!(r.be.sent.iter().filter(|a| **a == Act::Test).count(), 2);
    }

    #[test]
    fn a_stale_test_result_is_not_taken_for_the_new_test() {
        let mut r = Rig::new(true, happy());
        r.be.setup.test.phase = TestPhase::Done;
        r.be.setup.test.levels = vec![50, 60];
        r.press();
        r.until(|s| matches!(s, Stage::Confirm { .. }), 10);
        let a = r.flow.confirm(true);
        r.send(a);
        r.until(|s| *s == Stage::Devices, 10);
        r.flow.use_devices();
        r.poll(1);
        // The old Done is on the screen, the new run has not shown yet.
        assert_ne!(r.flow.stage(), &Stage::Done);
    }

    #[test]
    fn an_old_service_without_the_checklist_request_is_called_unsupported() {
        let mut f = Flow::new();
        let obs = Observation {
            hear: None,
            setup: Some(setup()),
        };
        assert!(f.start(&obs, 0).is_empty());
        assert!(matches!(f.stage(), Stage::Failed(Failure::Unsupported(_))));
        assert_eq!(Failure::Unsupported(String::new()).hint(), FixHint::Update);
    }

    #[test]
    fn confirm_and_devices_are_ignored_out_of_turn() {
        let mut f = Flow::new();
        assert!(f.confirm(true).is_empty());
        f.use_devices();
        f.retest();
        assert_eq!(f.stage(), &Stage::Idle);
        assert_eq!(f.stage().position(), None);
        assert!(!f.stage().active());
    }

    #[test]
    fn steps_are_numbered_one_to_seven() {
        let all = [
            Stage::NoAdapter,
            Stage::PowerOn,
            Stage::Visible,
            Stage::Confirm {
                passkey: String::new(),
                device: None,
            },
            Stage::Connecting,
            Stage::Devices,
            Stage::Testing,
        ];
        let n: Vec<usize> = all.iter().map(|s| s.position().unwrap()).collect();
        assert_eq!(n, (1..=STEP_COUNT).collect::<Vec<_>>());
    }

    #[test]
    fn real_vocabulary_is_known() {
        // The names the call-audio service uses (docs/reports/call-audio-pc-headset.md).
        let h = HearCalls::default();
        assert!(h.steps.is_empty());
        for p in [
            ids::PHASE_IDLE,
            ids::PHASE_WORKING,
            ids::PHASE_CONFIRM,
            ids::PHASE_DONE,
            ids::PHASE_FAILED,
        ] {
            assert!(!p.is_empty());
        }
    }

    #[test]
    fn a_dead_service_is_an_error_not_a_state() {
        let mut be = FakeBackend::new(true, setup(), vec![]);
        be.down = true;
        assert!(be.observe().is_err());
        assert!(be.act(&Act::Test).is_err());
    }
}
