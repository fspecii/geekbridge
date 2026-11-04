// SPDX-License-Identifier: GPL-3.0-or-later
//! "Call audio on this computer": the guided checklist in Settings and in the Calls window.
//!
//! A worker thread asks the service for the checklist (`Request::CallAudioSetup`) while the panel is
//! on screen and runs the user's choices; the window only draws the latest answer. Every step says
//! its state in words (never by colour alone), a failing step shows a one-line fix and, when there is
//! one, a button that copies the command. Nothing here runs a command: the commands are for the
//! clipboard. The test button asks the service to play a tone and listen to the microphone for five
//! seconds; no call is involved.
use crate::{
    a11y,
    connection::Config,
    i18n::{tr, tr_args},
    nothing::{S1, S2, S3},
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    call_audio::{
        self, Device, FixKind, Setup, Step, StepId, StepState, TestPhase, TestProgress, Verdict,
    },
    call_audio_flow::{Act, Backend, Failure, FixHint, Flow, Observation, STEP_COUNT, Stage},
    ipc::{CallAudioSetupAction, HearCalls, Request, Response},
    report,
};
use eframe::egui::{self, Color32, RichText, Sense, vec2};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Calm refresh while nothing is happening (a USB adapter can appear at any moment).
const POLL: Duration = Duration::from_secs(3);
/// Faster while the test runs, for a smooth meter.
const POLL_TEST: Duration = Duration::from_millis(150);
/// While the one-button setup runs, so each step follows the last within a second.
const POLL_FLOW: Duration = Duration::from_millis(800);
/// The worker idles when the panel has not been drawn for this long.
const IDLE_AFTER: Duration = Duration::from_secs(6);

pub fn step_title(id: StepId) -> &'static str {
    match id {
        StepId::Adapter => tr("callaudio.step.adapter"),
        StepId::Unblocked => tr("callaudio.step.unblocked"),
        StepId::Powered => tr("callaudio.step.powered"),
        StepId::Paired => tr("callaudio.step.paired"),
        StepId::Hfp => tr("callaudio.step.hfp"),
        StepId::Headset => tr("callaudio.step.headset"),
    }
}

pub fn state_text(s: StepState) -> &'static str {
    match s {
        StepState::Ok => tr("callaudio.state.ok"),
        StepState::Todo => tr("callaudio.state.todo"),
        StepState::Waiting => tr("callaudio.state.waiting"),
        StepState::Unknown => tr("callaudio.state.unknown"),
    }
}

pub fn fix_text(k: FixKind) -> &'static str {
    match k {
        FixKind::NoAdapter => tr("callaudio.fix.no_adapter"),
        FixKind::HardBlocked => tr("callaudio.fix.hard_blocked"),
        FixKind::Unblock => tr("callaudio.fix.unblock"),
        FixKind::StartBluetooth => tr("callaudio.fix.start_bluetooth"),
        FixKind::PowerOn => tr("callaudio.fix.power_on"),
        FixKind::Pair => tr("callaudio.fix.pair"),
        FixKind::Connect => tr("callaudio.fix.connect"),
        FixKind::ChooseDevices => tr("callaudio.fix.choose_devices"),
        FixKind::WindowsAdapter => tr("callaudio.fix.windows_adapter"),
        FixKind::WindowsPair => tr("callaudio.fix.windows_pair"),
        FixKind::WindowsPhoneCalls => tr("callaudio.fix.windows_phone_calls"),
        FixKind::WindowsSound => tr("callaudio.fix.windows_sound"),
    }
}

pub fn verdict_text(v: Verdict) -> &'static str {
    match v {
        Verdict::Ready => tr("callaudio.verdict.ready"),
        Verdict::Todo => tr("callaudio.verdict.todo"),
        Verdict::NoBluetooth => tr("callaudio.verdict.no_bluetooth"),
        Verdict::MacFallback => tr("callaudio.verdict.mac"),
        Verdict::Unsupported => tr("callaudio.verdict.unsupported"),
    }
}

/// What the test is saying right now, in words; `None` before it has run.
pub fn test_text(t: &TestProgress) -> Option<&'static str> {
    match t.phase {
        TestPhase::Idle => None,
        TestPhase::Tone => Some(tr("callaudio.test.tone")),
        TestPhase::Listening => Some(tr("callaudio.test.listen")),
        TestPhase::Done => Some(if t.tone_played == Some(false) {
            tr("callaudio.test.tone_failed")
        } else if t.mic_opened == Some(false) {
            tr("callaudio.test.mic_failed")
        } else if t.heard() {
            tr("callaudio.test.heard")
        } else {
            tr("callaudio.test.silent")
        }),
    }
}

pub fn device_label(d: &Device) -> String {
    let base = if d.bluetooth {
        tr_args("callaudio.device.bluetooth", &[("label", &d.label)])
    } else {
        d.label.clone()
    };
    if d.default {
        tr_args("callaudio.device.default", &[("label", &base)])
    } else {
        base
    }
}

/// The guide, on the project site. Opened only on a click.
pub fn guide_url() -> String {
    format!(
        "https://github.com/{}/blob/main/{}",
        report::repo_from_env(),
        call_audio::DOC
    )
}

pub fn failure_title(f: &Failure) -> &'static str {
    match f {
        Failure::Service(_) => tr("callaudio.flow.fail.service"),
        Failure::NoAnswer => tr("callaudio.flow.fail.no_answer"),
        Failure::HandsFree => tr("callaudio.flow.fail.hands_free"),
        Failure::NoTone => tr("callaudio.flow.fail.no_tone"),
        Failure::NoMicrophone => tr("callaudio.flow.fail.no_mic"),
        Failure::Silent => tr("callaudio.flow.fail.silent"),
        Failure::TestStuck => tr("callaudio.flow.fail.stuck"),
        Failure::NoDevices => tr("callaudio.flow.fail.no_devices"),
        Failure::Unsupported(_) => tr("callaudio.flow.fail.unsupported"),
    }
}

pub fn hint_text(h: FixHint) -> &'static str {
    match h {
        FixHint::StartService => tr("callaudio.flow.hint.start_service"),
        FixHint::Unblock => tr("callaudio.flow.hint.unblock"),
        FixHint::PhoneBluetooth => tr("callaudio.flow.hint.phone_bluetooth"),
        FixHint::PhoneCalls => tr("callaudio.flow.hint.phone_calls"),
        FixHint::TryAgain => tr("callaudio.flow.hint.try_again"),
        FixHint::Devices => tr("callaudio.flow.hint.devices"),
        FixHint::Microphone => tr("callaudio.flow.hint.microphone"),
        FixHint::Update => tr("callaudio.flow.hint.update"),
    }
}

/// What the one-button setup is doing, in one sentence (no step number).
pub fn stage_text(s: &Stage) -> &'static str {
    match s {
        Stage::Idle => tr("callaudio.flow.intro"),
        Stage::NoAdapter => tr("callaudio.flow.noadapter"),
        Stage::PowerOn => tr("callaudio.flow.power"),
        Stage::Visible => tr("callaudio.flow.visible"),
        Stage::Confirm { .. } => tr("callaudio.flow.confirm"),
        Stage::Connecting => tr("callaudio.flow.connecting"),
        Stage::Devices => tr("callaudio.flow.devices"),
        Stage::Testing => tr("callaudio.flow.testing"),
        Stage::Done => tr("callaudio.flow.done"),
        Stage::Failed(f) => failure_title(f),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub setup: Option<Setup>,
    /// The service's step-level view and the guided setup's state; `None` from a service that
    /// predates it (the one-button setup is then not offered).
    pub hear: Option<HearCalls>,
    /// Where the one-button setup is.
    pub stage: Stage,
    /// Why the service could not be asked, or refused the last choice; plain words.
    pub problem: Option<String>,
}

/// What the window asks the worker to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Act(Act),
    Flow(FlowEvent),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowEvent {
    Start,
    Cancel,
    Confirm(bool),
    UseDevices,
    Retest,
}

/// The real service: the checklist, the step-level view and one request at a time.
pub struct ServiceBackend {
    cfg: Config,
}

impl ServiceBackend {
    pub fn new() -> Self {
        Self {
            cfg: Config::from_environment(),
        }
    }
}

impl Default for ServiceBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for ServiceBackend {
    fn observe(&mut self) -> Result<Observation, String> {
        let setup = match self.cfg.request(&Request::CallAudioSetup)? {
            Response::CallAudioSetup(s) => *s,
            Response::Error(e) => return Err(e.message),
            _ => return Err(tr("feed.unexpected").into()),
        };
        // A service that predates the step-level view answers with an error: no one-button setup.
        let hear = match self.cfg.request(&Request::HearCalls) {
            Ok(Response::HearCalls(h)) => Some(*h),
            _ => None,
        };
        Ok(Observation {
            hear,
            setup: Some(setup),
        })
    }

    fn act(&mut self, act: &Act) -> Result<Observation, String> {
        let req = match act {
            Act::Pair(action) => Request::CallAudioPair {
                action: action.clone(),
            },
            Act::Choose { output, input } => Request::CallAudioChoose {
                output: output.clone(),
                input: input.clone(),
            },
            Act::Test => Request::CallAudioTest,
        };
        match self.cfg.request(&req)? {
            Response::Error(e) => Err(e.message),
            _ => self.observe(),
        }
    }
}

pub struct Feed {
    state: Arc<Mutex<Snapshot>>,
    commands: mpsc::Sender<Command>,
    seen: Arc<Mutex<Instant>>,
    stop: Arc<AtomicBool>,
}

impl Feed {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        Self::spawn_with(ServiceBackend::new(), wake)
    }

    pub fn spawn_with(
        mut backend: impl Backend + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(Snapshot::default()));
        let (commands, rx) = mpsc::channel::<Command>();
        let seen = Arc::new(Mutex::new(Instant::now()));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, saw, halt) = (state.clone(), seen.clone(), stop.clone());
        std::thread::Builder::new()
            .name("geekbridge-call-audio".into())
            .spawn(move || {
                let t0 = Instant::now();
                let now = || t0.elapsed().as_millis() as u64;
                let mut flow = Flow::new();
                let mut last = Observation::default();
                let mut problem: Option<String> = None;
                let publish = |last: &Observation, flow: &Flow, problem: Option<String>| {
                    let next = Snapshot {
                        setup: last.setup.clone(),
                        hear: last.hear.clone(),
                        stage: flow.stage().clone(),
                        problem,
                    };
                    let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                    if *g != next {
                        *g = next;
                        drop(g);
                        wake();
                    }
                };
                // Sends what the flow decided; a refusal is shown, the next poll shows the truth.
                let run = |backend: &mut dyn Backend,
                           acts: Vec<Act>,
                           last: &mut Observation,
                           problem: &mut Option<String>| {
                    for a in acts {
                        match backend.act(&a) {
                            Ok(o) => *last = o,
                            Err(why) => *problem = Some(why),
                        }
                    }
                };
                loop {
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                    let busy = flow.stage().active();
                    let idle = !busy
                        && saw.lock().unwrap_or_else(|e| e.into_inner()).elapsed() > IDLE_AFTER;
                    let mut wait = if busy { POLL_FLOW } else { POLL };
                    if !idle {
                        match backend.observe() {
                            Ok(o) => {
                                let acts = flow.advance(&o, now());
                                last = o;
                                run(&mut backend, acts, &mut last, &mut problem);
                                if let Some(s) = &last.setup
                                    && matches!(
                                        s.test.phase,
                                        TestPhase::Tone | TestPhase::Listening
                                    )
                                {
                                    wait = POLL_TEST;
                                }
                                publish(&last, &flow, problem.take());
                            }
                            Err(why) => {
                                last = Observation::default();
                                publish(&last, &flow, Some(why));
                            }
                        }
                    }
                    match rx.recv_timeout(if idle { IDLE_AFTER } else { wait }) {
                        Ok(c) => {
                            let acts = match c {
                                Command::Act(a) => vec![a],
                                Command::Flow(FlowEvent::Start) => flow.start(&last, now()),
                                Command::Flow(FlowEvent::Cancel) => flow.cancel(),
                                Command::Flow(FlowEvent::Confirm(yes)) => flow.confirm(yes),
                                Command::Flow(FlowEvent::UseDevices) => {
                                    flow.use_devices();
                                    Vec::new()
                                }
                                Command::Flow(FlowEvent::Retest) => {
                                    flow.retest();
                                    Vec::new()
                                }
                            };
                            run(&mut backend, acts, &mut last, &mut problem);
                            publish(&last, &flow, problem.take());
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn call audio thread");
        Self {
            state,
            commands,
            seen,
            stop,
        }
    }

    /// A feed that always shows the given view, for screenshots (`GEEKBRIDGE_CALLAUDIO_FIXTURE`).
    /// No worker runs.
    pub fn fixed(setup: Setup) -> Self {
        Self::frozen(Snapshot {
            setup: Some(setup),
            ..Snapshot::default()
        })
    }

    pub fn frozen(snapshot: Snapshot) -> Self {
        let (commands, _rx) = mpsc::channel();
        Self {
            state: Arc::new(Mutex::new(snapshot)),
            commands,
            seen: Arc::new(Mutex::new(Instant::now())),
            stop: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn send(&self, c: Command) {
        let _ = self.commands.send(c);
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Canned checklists for screenshots and tests.
pub fn fixture(name: &str) -> Option<Setup> {
    use call_audio::{LinuxFacts, Rfkill, Selection, build_linux};
    let dev = |id: &str, label: &str, default: bool| Device {
        id: id.into(),
        label: label.into(),
        bluetooth: false,
        default,
    };
    let base = LinuxFacts {
        adapters: 1,
        rfkill: Some(Rfkill::default()),
        powered: Some(true),
        paired_phones: Some(vec!["00:00:00:00:00:01".into()]),
        connected_phones: 0,
        registered_phones: 0,
        outputs: vec![
            dev("o1", "Headset Analog Stereo", true), // i18n-ok: screenshot fixture
            dev("o2", "Built-in Speakers", false),    // i18n-ok: screenshot fixture
        ],
        inputs: vec![dev("i1", "Headset Mono Microphone", true)], // i18n-ok: screenshot fixture
        selection: Selection::default(),
        audio_server_ok: true,
    };
    Some(match name {
        "no-bluetooth" => build_linux(&LinuxFacts {
            adapters: 0,
            rfkill: None,
            powered: None,
            paired_phones: None,
            ..base
        }),
        "connect" => build_linux(&base),
        "ready" => build_linux(&LinuxFacts {
            connected_phones: 1,
            ..base
        }),
        "testing" => {
            let mut s = build_linux(&LinuxFacts {
                connected_phones: 1,
                ..base
            });
            s.test = TestProgress {
                phase: TestPhase::Listening,
                levels: vec![2, 3, 9, 28, 35, 22, 41, 30, 12, 6, 4, 18, 33, 40, 25],
                tone_played: Some(true),
                mic_opened: Some(true),
            };
            s
        }
        "mac" => call_audio::mac_fallback(),
        "windows" => {
            let node = |name: &str, id: &str| call_audio::PnpDevice {
                status: "OK".into(),
                name: name.into(),
                instance_id: id.into(),
            };
            let bt = [
                node("Radio", "USB\\VID_8087&PID_0033\\1"), // i18n-ok: screenshot fixture
                node(
                    "Phone hands-free", // i18n-ok: screenshot fixture
                    "BTHENUM\\{0000111F-0000-1000-8000-00805F9B34FB}_VID&1\\2",
                ),
            ];
            call_audio::build_windows(Some(&bt), &[])
        }
        _ => return None,
    })
}

/// The step-level view a fixture pretends the service gave.
fn hear_fixture(no_bluetooth: bool, phone: Option<&str>, phase: &str) -> HearCalls {
    HearCalls {
        ready: phone.is_some(),
        no_bluetooth,
        setup: bridge_core::ipc::CallAudioSetupState {
            phase: phase.into(),
            phone: phone.map(str::to_owned),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Canned whole views for screenshots: the checklist fixtures plus the one-button setup's stages
/// (`flow-idle`, `flow-noadapter`, `flow-visible`, `flow-confirm`, `flow-devices`, `flow-testing`,
/// `flow-done`, `flow-failed`).
pub fn fixture_snapshot(name: &str) -> Option<Snapshot> {
    let snap = |setup: Setup, hear: HearCalls, stage: Stage| Snapshot {
        setup: Some(setup),
        hear: Some(hear),
        stage,
        problem: None,
    };
    let connect = || fixture("connect").expect("known");
    let ready = || fixture("ready").expect("known");
    Some(match name {
        "connect" => snap(connect(), hear_fixture(false, None, "idle"), Stage::Idle),
        "ready" | "testing" => snap(
            fixture(name)?,
            hear_fixture(false, Some("Galaxy Z Fold5"), "idle"), // i18n-ok: screenshot fixture
            Stage::Idle,
        ),
        "no-bluetooth" | "mac" | "windows" => snap(
            fixture(name)?,
            hear_fixture(name == "no-bluetooth", None, "idle"),
            Stage::Idle,
        ),
        "flow-idle" => snap(connect(), hear_fixture(false, None, "idle"), Stage::Idle),
        "flow-noadapter" => {
            let mut s = fixture("no-bluetooth")?;
            s.verdict = Verdict::Todo;
            snap(s, hear_fixture(true, None, "idle"), Stage::NoAdapter)
        }
        "flow-visible" => snap(
            connect(),
            hear_fixture(false, None, "working"),
            Stage::Visible,
        ),
        "flow-confirm" => snap(
            connect(),
            hear_fixture(false, None, "confirm"),
            Stage::Confirm {
                passkey: "482913".into(),
                device: Some("Galaxy Z Fold5".into()), // i18n-ok: screenshot fixture
            },
        ),
        "flow-connecting" => snap(
            connect(),
            hear_fixture(false, None, "working"),
            Stage::Connecting,
        ),
        "flow-devices" => snap(
            ready(),
            hear_fixture(false, Some("Galaxy Z Fold5"), "done"), // i18n-ok: screenshot fixture
            Stage::Devices,
        ),
        "flow-testing" => {
            let mut s = fixture("testing")?;
            s.test.levels.truncate(9);
            snap(
                s,
                hear_fixture(false, Some("Galaxy Z Fold5"), "done"), // i18n-ok: screenshot fixture
                Stage::Testing,
            )
        }
        "flow-done" => snap(
            ready(),
            hear_fixture(false, Some("Galaxy Z Fold5"), "done"), // i18n-ok: screenshot fixture
            Stage::Done,
        ),
        "flow-failed" => snap(
            connect(),
            hear_fixture(false, None, "failed"),
            Stage::Failed(Failure::HandsFree),
        ),
        _ => return None,
    })
}

pub struct CallAudioPanel {
    feed: Option<Feed>,
    /// The last time the checklist said "no adapter"; an adapter showing up afterwards is a hotplug.
    was_no_adapter: bool,
    adapter_found: bool,
    copied: Option<(StepId, Instant)>,
    opened_note: Option<String>,
    /// "Forget Bluetooth phone" was pressed once; the second press does it.
    confirm_forget: bool,
}

impl Default for CallAudioPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl CallAudioPanel {
    pub fn new() -> Self {
        Self {
            feed: None,
            was_no_adapter: false,
            adapter_found: false,
            copied: None,
            opened_note: None,
            confirm_forget: false,
        }
    }

    pub fn with_feed(feed: Feed) -> Self {
        Self {
            feed: Some(feed),
            ..Self::new()
        }
    }

    fn feed(&mut self, ctx: &egui::Context) -> &Feed {
        self.feed.get_or_insert_with(|| {
            match std::env::var("GEEKBRIDGE_CALLAUDIO_FIXTURE")
                .ok()
                .and_then(|n| fixture_snapshot(&n))
            {
                Some(s) => Feed::frozen(s),
                None => {
                    let wake = ctx.clone();
                    Feed::spawn(move || wake.request_repaint())
                }
            }
        })
    }

    /// The checklist verdict, starting the background check on first use. `None` until the service answered.
    /// Also notices an adapter appearing after it was missing (a USB dongle plugged in).
    pub fn current_verdict(&mut self, ctx: &egui::Context) -> Option<Verdict> {
        let v = self.feed(ctx).snapshot().setup.map(|s| s.verdict);
        match v {
            Some(Verdict::NoBluetooth) => self.was_no_adapter = true,
            Some(Verdict::MacFallback | Verdict::Unsupported) | None => {}
            Some(_) => {
                if self.was_no_adapter {
                    self.adapter_found = true;
                }
                self.was_no_adapter = false;
            }
        }
        v
    }

    /// Non-blocking warning when this computer has no Bluetooth adapter. Returns whether it was drawn.
    pub fn banner(&mut self, ui: &mut egui::Ui, pal: &Palette) -> bool {
        if self.current_verdict(&ui.ctx().clone()) != Some(Verdict::NoBluetooth) {
            return false;
        }
        let r = ui
            .horizontal_top(|ui| {
                theme::dot(ui, pal.warn, false);
                ui.add(egui::Label::new(theme::caption(pal, tr("callaudio.banner"))).wrap());
            })
            .response;
        a11y::describe(
            ui.ctx(),
            r.id,
            tr("callaudio.a11y.banner"),
            Some(tr("callaudio.banner")),
        );
        true
    }

    /// "Bluetooth adapter found" card after a hotplug. `true` when the user pressed its button.
    pub fn found_card(&mut self, ui: &mut egui::Ui, pal: &Palette) -> bool {
        self.current_verdict(&ui.ctx().clone());
        if !self.adapter_found {
            return false;
        }
        let mut go = false;
        ui.add(egui::Label::new(RichText::new(tr("callaudio.found")).color(pal.text)).wrap());
        ui.horizontal(|ui| {
            go = theme::pill_button(
                ui,
                pal,
                tr("callaudio.found.button"),
                ButtonKind::Primary,
                true,
            )
            .clicked();
            if theme::pill_button(
                ui,
                pal,
                tr("callaudio.found.dismiss"),
                ButtonKind::Plain,
                true,
            )
            .clicked()
            {
                self.adapter_found = false;
            }
        });
        if go {
            self.adapter_found = false;
        }
        go
    }

    /// Draws the whole checklist. `compact` drops the intro line (the Calls window is narrow).
    pub fn show(&mut self, ui: &mut egui::Ui, pal: &Palette, compact: bool) {
        let ctx = ui.ctx().clone();
        let snap = self.feed(&ctx).snapshot();
        if !compact {
            ui.add(egui::Label::new(theme::caption(pal, tr("callaudio.intro"))).wrap());
            ui.add_space(S2);
        }
        let Some(setup) = snap.setup.clone() else {
            let text = snap
                .problem
                .as_ref()
                .map(|_| tr("callaudio.unreachable").to_owned())
                .unwrap_or_else(|| tr("callaudio.checking").to_owned());
            ui.add(egui::Label::new(theme::caption(pal, text)).wrap());
            return;
        };
        self.verdict(ui, pal, &setup);
        match setup.verdict {
            Verdict::MacFallback => self.mac(ui, pal),
            Verdict::Unsupported => {}
            _ => {
                ui.add_space(S2);
                let offered = snap.stage != Stage::Idle
                    || (snap.hear.is_some() && setup.verdict != Verdict::NoBluetooth);
                if offered {
                    self.flow_card(ui, pal, &snap, &setup);
                    ui.add_space(S3);
                }
                // While the guided setup runs, it is the only thing on screen.
                if !snap.stage.active() {
                    for step in &setup.steps {
                        self.step_row(ui, pal, step);
                    }
                    if setup.verdict == Verdict::NoBluetooth {
                        self.buy(ui, pal);
                    }
                    ui.add_space(S3);
                    self.devices(ui, pal, &setup);
                    ui.add_space(S2);
                    self.test(ui, pal, &setup);
                }
            }
        }
        if let Some(p) = &snap.problem {
            ui.add_space(S1);
            theme::blocked_note(ui, pal, p);
        }
    }

    fn flow(&self, e: FlowEvent) {
        if let Some(feed) = &self.feed {
            feed.send(Command::Flow(e));
        }
    }

    fn button(
        &self,
        ui: &mut egui::Ui,
        pal: &Palette,
        label: &str,
        kind: ButtonKind,
        name: &str,
    ) -> bool {
        let r = theme::pill_button(ui, pal, label, kind, true);
        a11y::named(r, name).clicked()
    }

    /// The single guided setup: one primary button, the step it is on, and what to do next.
    fn flow_card(&mut self, ui: &mut egui::Ui, pal: &Palette, snap: &Snapshot, setup: &Setup) {
        let stage = &snap.stage;
        let phone = snap.hear.as_ref().and_then(|h| h.setup.phone.clone());
        if let Some(n) = stage.position() {
            let text = tr_args("callaudio.flow.step", &[("n", &n), ("total", &STEP_COUNT)]);
            progress_bar(ui, pal, n, &text);
            ui.add_space(S1);
        }
        // The sentence for this stage; a polite live region so a screen reader follows the steps.
        let line =
            ui.add(egui::Label::new(RichText::new(stage_text(stage)).color(pal.text)).wrap());
        a11y::live_region(ui.ctx(), line.id, a11y::Live::Polite);
        a11y::describe(
            ui.ctx(),
            line.id,
            &tr_args("callaudio.a11y.flow", &[("state", &stage_text(stage))]),
            None,
        );
        ui.add_space(S1);
        match stage {
            Stage::Idle | Stage::Done => {
                if let Some(name) = &phone {
                    ui.add(
                        egui::Label::new(theme::caption(
                            pal,
                            tr_args("callaudio.flow.phone", &[("phone", name)]),
                        ))
                        .wrap(),
                    );
                    ui.add_space(S1);
                }
                ui.horizontal_wrapped(|ui| {
                    if *stage == Stage::Done
                        && self.button(
                            ui,
                            pal,
                            tr("callaudio.flow.retest"),
                            ButtonKind::Primary,
                            tr("callaudio.a11y.retest"),
                        )
                    {
                        self.flow(FlowEvent::Retest);
                    }
                    let (label, kind) = if phone.is_some() || *stage == Stage::Done {
                        (tr("callaudio.flow.again"), ButtonKind::Plain)
                    } else {
                        (tr("callaudio.flow.start"), ButtonKind::Primary)
                    };
                    if self.button(ui, pal, label, kind, tr("callaudio.a11y.start")) {
                        self.confirm_forget = false;
                        self.flow(FlowEvent::Start);
                    }
                    if phone.is_some()
                        && !self.confirm_forget
                        && self.button(
                            ui,
                            pal,
                            tr("callaudio.flow.forget"),
                            ButtonKind::Plain,
                            tr("callaudio.a11y.forget"),
                        )
                    {
                        self.confirm_forget = true;
                    }
                });
                if phone.is_some() && self.confirm_forget {
                    ui.add_space(S1);
                    let who = phone.clone().unwrap_or_default();
                    ui.add(
                        egui::Label::new(theme::secondary(
                            pal,
                            tr_args("callaudio.flow.forget.ask", &[("phone", &who)]),
                        ))
                        .wrap(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if self.button(
                            ui,
                            pal,
                            tr("callaudio.flow.forget.yes"),
                            ButtonKind::Primary,
                            tr("callaudio.a11y.forget_yes"),
                        ) {
                            self.confirm_forget = false;
                            if let Some(feed) = &self.feed {
                                feed.send(Command::Act(Act::Pair(CallAudioSetupAction::Forget)));
                            }
                        }
                        if self.button(
                            ui,
                            pal,
                            tr("callaudio.flow.forget.keep"),
                            ButtonKind::Plain,
                            tr("callaudio.a11y.forget_keep"),
                        ) {
                            self.confirm_forget = false;
                        }
                    });
                }
            }
            Stage::Confirm { passkey, device } => {
                if let Some(d) = device {
                    ui.add(egui::Label::new(theme::caption(pal, d.as_str())).wrap());
                }
                let code = ui.add(egui::Label::new(
                    RichText::new(passkey.as_str())
                        .monospace()
                        .size(28.0)
                        .color(pal.text),
                ));
                a11y::describe(
                    ui.ctx(),
                    code.id,
                    &tr_args("callaudio.a11y.code", &[("code", passkey)]),
                    Some(passkey),
                );
                ui.add(
                    egui::Label::new(theme::caption(pal, tr("callaudio.flow.confirm.hint"))).wrap(),
                );
                ui.add_space(S1);
                ui.horizontal_wrapped(|ui| {
                    if self.button(
                        ui,
                        pal,
                        tr("callaudio.flow.confirm.yes"),
                        ButtonKind::Primary,
                        tr("callaudio.a11y.code_yes"),
                    ) {
                        self.flow(FlowEvent::Confirm(true));
                    }
                    if self.button(
                        ui,
                        pal,
                        tr("callaudio.flow.confirm.no"),
                        ButtonKind::Plain,
                        tr("callaudio.a11y.code_no"),
                    ) {
                        self.flow(FlowEvent::Confirm(false));
                    }
                });
                self.cancel_button(ui, pal);
            }
            Stage::Devices => {
                if setup.outputs.is_empty() {
                    ui.add(
                        egui::Label::new(theme::secondary(pal, tr("callaudio.flow.devices.none")))
                            .wrap(),
                    );
                } else {
                    self.devices(ui, pal, setup);
                }
                ui.add_space(S1);
                ui.horizontal_wrapped(|ui| {
                    let r = theme::pill_button(
                        ui,
                        pal,
                        tr("callaudio.flow.devices.use"),
                        ButtonKind::Primary,
                        !setup.outputs.is_empty(),
                    );
                    if a11y::named(r, tr("callaudio.a11y.use_devices")).clicked() {
                        self.flow(FlowEvent::UseDevices);
                    }
                    if theme::pill_button(
                        ui,
                        pal,
                        tr("callaudio.flow.cancel"),
                        ButtonKind::Plain,
                        true,
                    )
                    .clicked()
                    {
                        self.flow(FlowEvent::Cancel);
                    }
                });
            }
            Stage::Testing => {
                if let Some(t) = test_text(&setup.test) {
                    ui.add(egui::Label::new(theme::caption(pal, t)).wrap());
                }
                if setup.test.phase != TestPhase::Idle && setup.test.mic_opened != Some(false) {
                    ui.add_space(S1);
                    meter(ui, pal, &setup.test);
                }
                self.cancel_button(ui, pal);
            }
            Stage::Failed(f) => {
                if let Failure::Service(m) = f
                    && !m.is_empty()
                {
                    ui.add(egui::Label::new(theme::caption(pal, m.as_str())).wrap());
                }
                ui.add(egui::Label::new(theme::secondary(pal, hint_text(f.hint()))).wrap());
                ui.add_space(S1);
                ui.horizontal_wrapped(|ui| {
                    let testing_only =
                        matches!(f, Failure::Silent | Failure::NoTone | Failure::NoMicrophone);
                    if testing_only
                        && self.button(
                            ui,
                            pal,
                            tr("callaudio.flow.retest"),
                            ButtonKind::Primary,
                            tr("callaudio.a11y.retest"),
                        )
                    {
                        self.flow(FlowEvent::Retest);
                    }
                    if self.button(
                        ui,
                        pal,
                        tr("callaudio.flow.retry"),
                        if testing_only {
                            ButtonKind::Plain
                        } else {
                            ButtonKind::Primary
                        },
                        tr("callaudio.a11y.start"),
                    ) {
                        self.flow(FlowEvent::Start);
                    }
                });
            }
            Stage::NoAdapter | Stage::PowerOn | Stage::Visible | Stage::Connecting => {
                if *stage == Stage::NoAdapter {
                    self.buy(ui, pal);
                }
                self.cancel_button(ui, pal);
            }
        }
    }

    fn cancel_button(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add_space(S1);
        if self.button(
            ui,
            pal,
            tr("callaudio.flow.cancel"),
            ButtonKind::Plain,
            tr("callaudio.a11y.cancel"),
        ) {
            self.flow(FlowEvent::Cancel);
        }
    }

    fn verdict(&self, ui: &mut egui::Ui, pal: &Palette, s: &Setup) {
        let (color, hollow) = match s.verdict {
            Verdict::Ready => (pal.ok, false),
            Verdict::Todo => (pal.warn, false),
            _ => (pal.text3, true),
        };
        let line = if s.steps.is_empty() {
            verdict_text(s.verdict).to_owned()
        } else {
            format!(
                "{}. {}",
                verdict_text(s.verdict),
                tr_args(
                    "callaudio.progress",
                    &[("done", &s.done_count()), ("total", &s.steps.len())]
                )
            )
        };
        ui.horizontal(|ui| {
            theme::dot(ui, color, hollow);
            ui.add(egui::Label::new(RichText::new(line).color(pal.text)).wrap());
        });
    }

    fn step_row(&mut self, ui: &mut egui::Ui, pal: &Palette, step: &Step) {
        let (color, hollow) = match step.state {
            StepState::Ok => (pal.ok, false),
            StepState::Todo => (pal.warn, false),
            StepState::Waiting | StepState::Unknown => (pal.text3, true),
        };
        let title = step_title(step.id);
        ui.add_space(S1);
        ui.horizontal_top(|ui| {
            theme::dot(ui, color, hollow);
            ui.vertical(|ui| {
                let row = ui.add(
                    egui::Label::new(RichText::new(title).color(
                        if step.state == StepState::Waiting {
                            pal.text3
                        } else {
                            pal.text
                        },
                    ))
                    .wrap(),
                );
                a11y::describe(
                    ui.ctx(),
                    row.id,
                    &tr_args(
                        "callaudio.a11y.step",
                        &[("step", &title), ("state", &state_text(step.state))],
                    ),
                    None,
                );
                ui.add(egui::Label::new(theme::caption(pal, state_text(step.state))).wrap());
                if let Some(fix) = &step.fix
                    && matches!(step.state, StepState::Todo | StepState::Unknown)
                {
                    ui.add(egui::Label::new(theme::secondary(pal, fix_text(fix.kind))).wrap());
                    if let Some(cmd) = &fix.command {
                        ui.horizontal_wrapped(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(cmd.as_str())
                                        .monospace()
                                        .size(12.0)
                                        .color(pal.text2),
                                )
                                .wrap(),
                            );
                            let just = self.copied.is_some_and(|(id, at)| {
                                id == step.id && at.elapsed() < Duration::from_secs(3)
                            });
                            let label = if just {
                                tr("callaudio.copied")
                            } else {
                                tr("callaudio.copy")
                            };
                            let r = theme::pill_button(ui, pal, label, ButtonKind::Plain, true);
                            let r = a11y::named(
                                r,
                                &tr_args("callaudio.a11y.copy", &[("step", &title)]),
                            );
                            if r.clicked() {
                                ui.ctx().copy_text(cmd.clone());
                                self.copied = Some((step.id, Instant::now()));
                                ui.ctx().request_repaint_after(Duration::from_secs(3));
                            }
                        });
                    }
                }
            });
        });
    }

    fn buy(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add_space(S2);
        ui.add(egui::Label::new(theme::secondary(pal, tr("callaudio.buy"))).wrap());
        ui.add_space(S1);
        self.guide_button(ui, pal);
    }

    fn guide_button(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if theme::pill_button(ui, pal, tr("callaudio.guide"), ButtonKind::Plain, true).clicked() {
            let url = guide_url();
            if report::open_in_browser(&url) {
                self.opened_note = None;
            } else {
                ui.ctx().copy_text(url);
                self.opened_note = Some(tr("callaudio.guide.copied").to_owned());
            }
        }
        if let Some(n) = &self.opened_note {
            ui.add(egui::Label::new(theme::caption(pal, n.as_str())).wrap());
        }
    }

    fn mac(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.add_space(S2);
        ui.label(theme::semibold(tr("callaudio.mac.title"), 14.0));
        ui.add(egui::Label::new(theme::secondary(pal, tr("callaudio.mac.body"))).wrap());
        ui.add_space(S1);
        ui.add(egui::Label::new(theme::secondary(pal, tr("callaudio.mac.option1"))).wrap());
        ui.add(egui::Label::new(theme::secondary(pal, tr("callaudio.mac.option2"))).wrap());
        ui.add_space(S1);
        self.guide_button(ui, pal);
    }

    fn devices(&mut self, ui: &mut egui::Ui, pal: &Palette, s: &Setup) {
        if s.outputs.is_empty() && s.inputs.is_empty() {
            return;
        }
        let mut out = s.chosen_output.clone();
        let mut inp = s.chosen_input.clone();
        let pick = |ui: &mut egui::Ui,
                    salt: &str,
                    caption: &str,
                    a11y_name: &str,
                    list: &[Device],
                    cur: &mut Option<String>| {
            ui.label(theme::caption(pal, caption));
            let shown = list
                .iter()
                .find(|d| Some(&d.id) == cur.as_ref())
                .map(device_label)
                .unwrap_or_else(|| tr("callaudio.device.none").to_owned());
            let r = egui::ComboBox::from_id_salt(salt)
                .selected_text(shown)
                .width(ui.available_width().min(320.0))
                .show_ui(ui, |ui| {
                    for d in list {
                        ui.selectable_value(cur, Some(d.id.clone()), device_label(d));
                    }
                });
            a11y::describe(ui.ctx(), r.response.id, a11y_name, None);
        };
        if !s.outputs.is_empty() {
            pick(
                ui,
                "callaudio-out",
                tr("callaudio.output"),
                tr("callaudio.a11y.output"),
                &s.outputs,
                &mut out,
            );
            ui.add_space(S1);
        }
        if !s.inputs.is_empty() {
            pick(
                ui,
                "callaudio-in",
                tr("callaudio.input"),
                tr("callaudio.a11y.input"),
                &s.inputs,
                &mut inp,
            );
        }
        if (out != s.chosen_output || inp != s.chosen_input)
            && let Some(feed) = &self.feed
        {
            feed.send(Command::Act(Act::Choose {
                output: out,
                input: inp,
            }));
        }
    }

    fn test(&mut self, ui: &mut egui::Ui, pal: &Palette, s: &Setup) {
        let running = matches!(s.test.phase, TestPhase::Tone | TestPhase::Listening);
        let label = if running {
            tr("callaudio.test.running")
        } else {
            tr("callaudio.test.button")
        };
        let r = theme::pill_button(ui, pal, label, ButtonKind::Primary, s.can_test && !running);
        let r = a11y::named(r, tr("callaudio.a11y.test"));
        if r.clicked()
            && let Some(feed) = &self.feed
        {
            feed.send(Command::Act(Act::Test));
        }
        ui.add_space(S1);
        let hint = if s.can_test {
            tr_args(
                "callaudio.test.hint",
                &[("seconds", &call_audio::TEST_SECONDS)],
            )
        } else if s.platform == "windows" {
            tr("callaudio.test.not_here").to_owned()
        } else {
            String::new()
        };
        if !hint.is_empty() {
            ui.add(egui::Label::new(theme::caption(pal, hint)).wrap());
        }
        if let Some(t) = test_text(&s.test) {
            ui.add_space(S1);
            let line = ui.add(egui::Label::new(RichText::new(t).color(pal.text)).wrap());
            a11y::live_region(ui.ctx(), line.id, a11y::Live::Polite);
        }
        if s.test.phase != TestPhase::Idle && s.test.mic_opened != Some(false) {
            ui.add_space(S1);
            meter(ui, pal, &s.test);
        }
    }
}

/// Seven segments, the ones reached filled; the words next to it carry the same information.
fn progress_bar(ui: &mut egui::Ui, pal: &Palette, at: usize, text: &str) {
    ui.horizontal(|ui| {
        let w = 18.0;
        let (rect, resp) =
            ui.allocate_exact_size(vec2((w + 3.0) * STEP_COUNT as f32, 6.0), Sense::hover());
        for i in 0..STEP_COUNT {
            let x = rect.left() + (w + 3.0) * i as f32;
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), vec2(w, 6.0));
            ui.painter()
                .rect_filled(r, 3.0, if i < at { pal.ok } else { pal.control });
        }
        a11y::describe(ui.ctx(), resp.id, text, None);
        ui.label(theme::caption(pal, text));
    });
}

/// The level meter: one thin bar per reading, newest on the right, the last second brighter.
fn meter(ui: &mut egui::Ui, pal: &Palette, t: &TestProgress) {
    let total = (call_audio::TEST_SECONDS * call_audio::METER_HZ) as usize;
    let w = ui.available_width().clamp(120.0, 320.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 36.0), Sense::hover());
    let level = t.levels.last().copied().unwrap_or(0);
    a11y::describe(
        ui.ctx(),
        resp.id,
        tr("callaudio.test.meter"),
        Some(&tr_args("callaudio.a11y.meter", &[("level", &level)])),
    );
    let p = ui.painter();
    p.rect_filled(rect, 6.0, pal.control);
    let step = rect.width() / total as f32;
    for (i, l) in t.levels.iter().take(total).enumerate() {
        let h = (f32::from(*l) / 100.0 * (rect.height() - 6.0)).max(1.5);
        let x = rect.left() + step * (i as f32 + 0.5);
        let color: Color32 = if i + 10 >= t.levels.len() {
            pal.ok
        } else {
            pal.text3
        };
        p.line_segment(
            [
                egui::pos2(x, rect.bottom() - 3.0),
                egui::pos2(x, rect.bottom() - 3.0 - h),
            ],
            egui::Stroke::new((step * 0.6).max(1.5), color),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::call_audio_flow::FakeBackend;
    use call_audio::mac_fallback;

    fn ready() -> Setup {
        fixture("ready").unwrap()
    }

    #[test]
    fn every_fixture_exists_and_unknown_names_do_not() {
        for n in [
            "no-bluetooth",
            "connect",
            "ready",
            "testing",
            "mac",
            "windows",
        ] {
            assert!(fixture(n).is_some(), "{n}");
        }
        assert!(fixture("nope").is_none());
    }

    #[test]
    fn every_step_fix_and_state_has_words() {
        for f in FixKind::ALL {
            assert!(!fix_text(f).starts_with("callaudio."), "{f:?}");
        }
        for s in [
            StepId::Adapter,
            StepId::Unblocked,
            StepId::Powered,
            StepId::Paired,
            StepId::Hfp,
            StepId::Headset,
        ] {
            assert!(!step_title(s).starts_with("callaudio."));
        }
        for s in [
            StepState::Ok,
            StepState::Todo,
            StepState::Waiting,
            StepState::Unknown,
        ] {
            assert!(!state_text(s).starts_with("callaudio."));
        }
    }

    #[test]
    fn no_bluetooth_message_is_plain_and_names_the_adapter() {
        let t = fix_text(FixKind::NoAdapter);
        assert!(t.contains("no Bluetooth"));
        assert!(t.contains("USB Bluetooth adapter"));
        assert!(t.contains("answer here and talk on the phone"));
        assert!(tr("callaudio.buy").contains("btusb"));
        assert!(tr("callaudio.buy").contains("5.x"));
        assert!(guide_url().ends_with("docs/call-audio-options.md"));
    }

    #[test]
    fn test_text_follows_the_run() {
        let mut t = TestProgress::default();
        assert_eq!(test_text(&t), None);
        t.phase = TestPhase::Tone;
        assert_eq!(test_text(&t), Some(tr("callaudio.test.tone")));
        t.phase = TestPhase::Listening;
        assert_eq!(test_text(&t), Some(tr("callaudio.test.listen")));
        t.phase = TestPhase::Done;
        t.tone_played = Some(true);
        t.mic_opened = Some(true);
        t.levels = vec![1, 2];
        assert_eq!(test_text(&t), Some(tr("callaudio.test.silent")));
        t.levels = vec![1, 40];
        assert_eq!(test_text(&t), Some(tr("callaudio.test.heard")));
        t.mic_opened = Some(false);
        assert_eq!(test_text(&t), Some(tr("callaudio.test.mic_failed")));
        t.tone_played = Some(false);
        assert_eq!(test_text(&t), Some(tr("callaudio.test.tone_failed")));
    }

    #[test]
    fn device_labels_mark_default_and_bluetooth() {
        let d = Device {
            id: "x".into(),
            label: "Headset".into(),
            bluetooth: true,
            default: true,
        };
        assert_eq!(device_label(&d), "Headset (Bluetooth) (system default)");
    }

    fn draw(panel: &mut CallAudioPanel) -> egui::FullOutput {
        let ctx = egui::Context::default();
        theme::apply(&ctx, bridge_core::AppKind::Calls);
        let pal = theme::palette(bridge_core::AppKind::Calls, false);
        ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| panel.show(ui, &pal, false));
        })
    }

    #[test]
    fn panel_draws_every_fixture_without_panicking() {
        for n in [
            "no-bluetooth",
            "connect",
            "ready",
            "testing",
            "mac",
            "windows",
        ] {
            let mut p = CallAudioPanel::with_feed(Feed::fixed(fixture(n).unwrap()));
            draw(&mut p);
        }
    }

    /// A service that is not there.
    struct Down;
    impl Backend for Down {
        fn observe(&mut self) -> Result<Observation, String> {
            Err("down".into())
        }
        fn act(&mut self, _: &Act) -> Result<Observation, String> {
            Err("down".into())
        }
    }

    fn wait_for(feed: &Feed, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
        for _ in 0..600 {
            let s = feed.snapshot();
            if f(&s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out; last {:?}", feed.snapshot().stage);
    }

    #[test]
    fn panel_without_an_answer_says_it_is_checking() {
        let feed = Feed::spawn_with(Down, || {});
        let mut p = CallAudioPanel::with_feed(feed);
        draw(&mut p);
    }

    #[test]
    fn feed_polls_runs_choices_and_idles_when_unseen() {
        let mut be = FakeBackend::new(true, ready(), vec![]);
        be.hear.steps.clear();
        let feed = Feed::spawn_with(be, || {});
        wait_for(&feed, |s| s.setup.is_some());
        feed.send(Command::Act(Act::Choose {
            output: Some("o2".into()),
            input: None,
        }));
        let s = wait_for(&feed, |s| {
            s.setup
                .as_ref()
                .is_some_and(|s| s.chosen_output.as_deref() == Some("o2"))
        });
        assert!(s.hear.is_some());
        assert_eq!(s.stage, Stage::Idle);
    }

    #[test]
    fn the_primary_button_runs_the_whole_flow_against_the_fake_service() {
        use bridge_core::call_audio_flow::Beat;
        let be = FakeBackend::new(
            true,
            ready(),
            vec![
                Beat::Working,
                Beat::Confirm("246810".into()),
                Beat::Working,
                Beat::Done,
                Beat::Heard(true),
            ],
        );
        let feed = Feed::spawn_with(be, || {});
        wait_for(&feed, |s| s.hear.is_some());
        feed.send(Command::Flow(FlowEvent::Start));
        let s = wait_for(&feed, |s| matches!(s.stage, Stage::Confirm { .. }));
        assert_eq!(
            s.stage,
            Stage::Confirm {
                passkey: "246810".into(),
                device: Some("Phone".into())
            }
        );
        feed.send(Command::Flow(FlowEvent::Confirm(true)));
        wait_for(&feed, |s| s.stage == Stage::Devices);
        feed.send(Command::Flow(FlowEvent::UseDevices));
        wait_for(&feed, |s| s.stage == Stage::Done);
    }

    #[test]
    fn cancel_during_the_flow_returns_to_the_start() {
        use bridge_core::call_audio_flow::Beat;
        let be = FakeBackend::new(true, ready(), vec![Beat::Working]);
        let feed = Feed::spawn_with(be, || {});
        wait_for(&feed, |s| s.hear.is_some());
        feed.send(Command::Flow(FlowEvent::Start));
        wait_for(&feed, |s| s.stage.active());
        feed.send(Command::Flow(FlowEvent::Cancel));
        wait_for(&feed, |s| s.stage == Stage::Idle);
    }

    #[test]
    fn every_flow_fixture_draws_and_has_words() {
        for n in [
            "flow-idle",
            "flow-noadapter",
            "flow-visible",
            "flow-confirm",
            "flow-connecting",
            "flow-devices",
            "flow-testing",
            "flow-done",
            "flow-failed",
            "ready",
            "no-bluetooth",
            "connect",
        ] {
            let snap = fixture_snapshot(n).unwrap_or_else(|| panic!("{n}"));
            assert!(!stage_text(&snap.stage).starts_with("callaudio."), "{n}");
            let mut p = CallAudioPanel::with_feed(Feed::frozen(snap));
            draw(&mut p);
        }
        assert!(fixture_snapshot("nope").is_none());
    }

    #[test]
    fn every_failure_and_hint_has_words() {
        for f in [
            Failure::Service("x".into()),
            Failure::NoAnswer,
            Failure::HandsFree,
            Failure::NoTone,
            Failure::NoMicrophone,
            Failure::Silent,
            Failure::TestStuck,
            Failure::NoDevices,
            Failure::Unsupported(String::new()),
        ] {
            assert!(!failure_title(&f).starts_with("callaudio."), "{f:?}");
            assert!(!hint_text(f.hint()).starts_with("callaudio."), "{f:?}");
        }
    }

    #[test]
    fn the_start_button_is_offered_with_an_adapter_and_not_without() {
        // The words on the screen are the button labels the user will see.
        assert_eq!(tr("callaudio.flow.start"), "Set up call audio");
        assert_eq!(tr("callaudio.flow.again"), "Set up again");
        assert_eq!(tr("callaudio.flow.forget"), "Forget Bluetooth phone");
        let with = fixture_snapshot("flow-idle").unwrap();
        let without = fixture_snapshot("no-bluetooth").unwrap();
        assert!(with.hear.is_some_and(|h| !h.no_bluetooth));
        assert!(without.hear.is_some_and(|h| h.no_bluetooth));
    }

    #[test]
    fn banner_only_for_no_bluetooth_and_hotplug_is_noticed() {
        let ctx = egui::Context::default();
        theme::apply(&ctx, bridge_core::AppKind::Calls);
        let pal = theme::palette(bridge_core::AppKind::Calls, false);
        let mut p = CallAudioPanel::with_feed(Feed::fixed(fixture("no-bluetooth").unwrap()));
        let mut drawn = false;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| drawn = p.banner(ui, &pal));
        });
        assert!(drawn);
        let mut q = CallAudioPanel::with_feed(Feed::fixed(fixture("ready").unwrap()));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| drawn = q.banner(ui, &pal));
        });
        assert!(!drawn);
        assert!(!q.adapter_found, "never missing, so no hotplug card");
        // Missing, then present: the card appears.
        p.current_verdict(&ctx);
        *p.feed.as_ref().unwrap().state.lock().unwrap() = Snapshot {
            setup: fixture("connect"),
            ..Snapshot::default()
        };
        p.current_verdict(&ctx);
        assert!(p.adapter_found);
    }

    #[test]
    fn mac_panel_offers_the_fallbacks_and_never_a_test() {
        let m = mac_fallback();
        assert!(!m.can_test);
        assert!(tr("callaudio.mac.body").contains("iPhones"));
        assert!(tr("callaudio.mac.body").contains("not verified"));
    }
}
