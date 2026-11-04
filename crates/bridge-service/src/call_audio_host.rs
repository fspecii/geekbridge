// SPDX-License-Identifier: GPL-3.0-or-later
//! Call audio on this computer: the user's choice of headphones and microphone for calls, and
//! the per-call routing that runs on the Bluetooth hands-free thread (Linux, PipeWire).
//!
//! * [`HostAudio`] lives inside the Linux hands-free host (`call_control::linux::LinuxHfp`): every
//!   poll it feeds the phones' call state to `bridge_calls::audio::CallAudio`, which moves the
//!   call's voice to the picked devices, mutes the real microphone, checks that audio flows and
//!   undoes its changes when the call ends (or after a crash).
//! * [`devices`] answers `CallAudioDevices`: what can be picked, the pick, and storing a new one.
//!
//! See `docs/reports/call-audio-pc-headset.md`.

use std::path::Path;

use bridge_core::ipc::{CallAudioDevice, CallAudioDevices, ErrorBody, ErrorCode};

fn err(code: ErrorCode, message: &str) -> ErrorBody {
    ErrorBody {
        code,
        message: message.to_owned(),
    }
}

/// List the devices a call can use and the user's pick; with `set`, store the pick first.
/// Blocking (talks to the sound server).
pub fn devices(
    state_dir: &Path,
    set: Option<(Option<String>, Option<String>)>,
) -> Result<CallAudioDevices, ErrorBody> {
    devices_with(state_dir, set, list)
}

fn devices_with(
    state_dir: &Path,
    set: Option<(Option<String>, Option<String>)>,
    list: impl FnOnce() -> Result<Listing, String>,
) -> Result<CallAudioDevices, ErrorBody> {
    use bridge_calls::audio::prefs;
    let path = state_dir.join(prefs::CHOICE_FILE);
    let mut out = CallAudioDevices::default();
    let listed = list();
    if let Some((output, input)) = set {
        let norm = |v: Option<String>| v.filter(|n| !n.is_empty() && n != "default");
        let choice = prefs::Choice {
            output: norm(output),
            input: norm(input),
        };
        // A name that is not connected right now is refused, so a typo cannot become the pick.
        if let Ok((outs, ins, _, _)) = &listed {
            for (want, pool, what) in [
                (&choice.output, outs, "output"),
                (&choice.input, ins, "microphone"),
            ] {
                if let Some(n) = want
                    && !pool.iter().any(|d| &d.name == n)
                {
                    return Err(err(
                        ErrorCode::NotFound,
                        &format!(
                            "no {what} called {n} is connected; run geekbridge-ctl call audio-devices to list them"
                        ),
                    ));
                }
            }
        }
        prefs::save_choice(&path, &choice).map_err(|e| {
            err(
                ErrorCode::BadRequest,
                &format!("could not store the choice: {e}"),
            )
        })?;
    }
    let choice = prefs::load_choice(&path);
    out.chosen_output = choice.output;
    out.chosen_input = choice.input;
    match listed {
        Ok((outputs, inputs, _, _)) => {
            out.outputs = outputs;
            out.inputs = inputs;
        }
        Err(problem) => out.problem = Some(problem),
    }
    Ok(out)
}

type Listing = (
    Vec<CallAudioDevice>,
    Vec<CallAudioDevice>,
    Option<String>,
    Option<String>,
);

#[cfg(target_os = "linux")]
fn list() -> Result<Listing, String> {
    use bridge_calls::audio::{AudioServer, route};
    let mut server = bridge_calls::audio::connect_platform()
        .map_err(|e| format!("the sound server cannot be reached: {e}"))?;
    let snap = server
        .snapshot()
        .map_err(|e| format!("the sound server could not be read: {e}"))?;
    let l = route::devices(&snap);
    let row = |d: &route::Device| CallAudioDevice {
        name: d.name.clone(),
        description: d.description.clone(),
        is_default: d.is_default,
        bluetooth: d.bluetooth,
        muted: d.muted,
    };
    Ok((
        l.outputs.iter().map(row).collect(),
        l.inputs.iter().map(row).collect(),
        l.default_output,
        l.default_input,
    ))
}

#[cfg(not(target_os = "linux"))]
fn list() -> Result<Listing, String> {
    Err("picking call-audio devices here is only implemented on Linux; Windows and macOS use the system's communication device".into())
}

/// `Request::HearCalls` / `Request::CallAudioPair`. Blocking.
pub fn hear_calls_request(
    state_dir: &Path,
    action: Option<bridge_core::ipc::CallAudioSetupAction>,
    default_phone: Option<String>,
) -> Result<bridge_core::ipc::HearCalls, ErrorBody> {
    #[cfg(target_os = "linux")]
    {
        if let Some(a) = action {
            host::SetupRunner::get(state_dir).act(a, default_phone)?;
        }
        Ok(host::hear_calls(state_dir))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state_dir, default_phone);
        if action.is_some() {
            return Err(err(
                ErrorCode::Unsupported,
                "the guided call-audio setup is for Linux; on Windows and macOS pair the phone in the system's Bluetooth settings",
            ));
        }
        Ok(bridge_core::ipc::HearCalls {
            headline: "Pair the phone in the system's Bluetooth settings; calls then use the system's communication device.".into(),
            ..Default::default()
        })
    }
}

/// Text for `geekbridge-ctl call audio-devices`.
pub fn describe(d: &CallAudioDevices) -> String {
    let mut s = String::new();
    if let Some(p) = &d.problem {
        s.push_str(&format!("problem: {p}\n"));
    }
    for (title, list, chosen) in [
        ("Speakers / headphones", &d.outputs, &d.chosen_output),
        ("Microphones", &d.inputs, &d.chosen_input),
    ] {
        s.push_str(&format!(
            "{title} (calls use: {}):\n",
            chosen.as_deref().map_or_else(
                || "the system default".to_owned(),
                |n| list
                    .iter()
                    .find(|x| x.name == *n)
                    .map_or(format!("{n} (not connected: the default is used)"), |x| x
                        .description
                        .clone())
            )
        ));
        for x in list {
            let mut tags = Vec::new();
            if chosen.as_deref() == Some(x.name.as_str()) {
                tags.push("picked");
            }
            if x.is_default {
                tags.push("system default");
            }
            if x.bluetooth {
                tags.push("bluetooth");
            }
            if x.muted {
                tags.push("muted");
            }
            s.push_str(&format!(
                "  {} {}\n      {}\n",
                if chosen.as_deref() == Some(x.name.as_str()) {
                    "*"
                } else {
                    " "
                },
                x.description,
                if tags.is_empty() {
                    x.name.clone()
                } else {
                    format!("{} [{}]", x.name, tags.join(", "))
                }
            ));
        }
    }
    for l in &d.call {
        s.push_str(&format!("now: {l}\n"));
    }
    s.push_str(
        "Pick: geekbridge-ctl call audio-devices --output NAME --input NAME (NAME or default)\n",
    );
    s
}

#[cfg(target_os = "linux")]
pub use host::{HostAudio, SetupRunner, hear_calls};

/// A Bluetooth adapter appeared while the service ran (hotplug); cleared once a phone is set up.
#[cfg(target_os = "linux")]
static ADAPTER_APPEARED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "linux")]
mod host {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant, SystemTime};

    use bridge_calls::audio::{
        AudioStatus, BtAddress, CallAudio, Place, PulseOptions, PulseServer, Want, prefs,
    };
    use bridge_calls::{DeviceRef, Session, hear, hfp_audio, setup};
    use bridge_core::ipc::{
        CallAudioSetupAction, CallAudioSetupState, ErrorBody, ErrorCode, HearCalls,
    };

    use super::{ADAPTER_APPEARED, err};

    const HOTPLUG_EVERY: Duration = Duration::from_secs(3);
    const RECONNECT_FIRST: Duration = Duration::from_secs(60);
    const RECONNECT_MAX: Duration = Duration::from_secs(600);

    /// The call-audio controller of the Linux hands-free host, plus adapter hotplug and the
    /// automatic reconnect of the phone set up for calls.
    pub struct HostAudio {
        audio: CallAudio<PulseServer>,
        state_dir: PathBuf,
        choice: PathBuf,
        choice_stamp: Option<SystemTime>,
        /// Telephony request failures of the last poll.
        errors: Vec<String>,
        adapters: Option<usize>,
        last_hotplug: Option<Instant>,
        links: Vec<String>,
        reconnect_at: Option<Instant>,
        reconnect_wait: Duration,
        reconnecting: Arc<AtomicBool>,
        reconnect_note: Arc<Mutex<Option<String>>>,
    }

    fn stamp(p: &Path) -> Option<SystemTime> {
        std::fs::metadata(p).and_then(|m| m.modified()).ok()
    }

    fn notify(title: &str, body: &str) {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "GeekBridge", "-i", "bluetooth", title, body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|mut c| std::thread::spawn(move || c.wait()));
    }

    impl HostAudio {
        pub fn new(state_dir: &Path) -> Self {
            let choice = state_dir.join(prefs::CHOICE_FILE);
            Self {
                audio: CallAudio::new(|| PulseServer::connect(PulseOptions::default()), state_dir),
                state_dir: state_dir.to_path_buf(),
                choice_stamp: stamp(&choice),
                choice,
                errors: Vec::new(),
                adapters: None,
                last_hotplug: None,
                links: Vec::new(),
                reconnect_at: Some(Instant::now() + Duration::from_secs(5)),
                reconnect_wait: RECONNECT_FIRST,
                reconnecting: Arc::new(AtomicBool::new(false)),
                reconnect_note: Arc::new(Mutex::new(None)),
            }
        }

        /// One poll, after the session was pumped.
        pub fn tick(&mut self, session: &Session) {
            let s = stamp(&self.choice);
            if s != self.choice_stamp {
                self.choice_stamp = s;
                self.audio.reload_choice();
            }
            self.errors = hfp_audio::tick(session, &mut self.audio);
            self.hotplug();
            self.maybe_reconnect(session);
        }

        /// A Bluetooth adapter plugged in: say so once and offer the setup.
        fn hotplug(&mut self) {
            let now = Instant::now();
            if self
                .last_hotplug
                .is_some_and(|t| now.duration_since(t) < HOTPLUG_EVERY)
            {
                return;
            }
            self.last_hotplug = Some(now);
            let n = hear::adapters_in(&hear::bt_sysfs()).len();
            let before = self.adapters.replace(n);
            if before == Some(0) && n > 0 {
                let set_up = setup::load_remembered(&self.state_dir).is_some();
                if set_up {
                    // A phone is already set up: just try to reconnect it soon.
                    self.reconnect_at = Some(now + Duration::from_secs(3));
                    self.reconnect_wait = RECONNECT_FIRST;
                } else {
                    ADAPTER_APPEARED.store(true, Ordering::SeqCst);
                    notify(
                        "Bluetooth adapter found",
                        "Set up call audio to talk through this computer: GeekBridge Settings > Calls, or run geekbridge-ctl call setup",
                    );
                }
            }
        }

        /// The phone set up for calls is not connected for hands-free: reconnect it when its
        /// GeekBridge link comes up, and otherwise now and then (backing off to 10 minutes).
        fn maybe_reconnect(&mut self, session: &Session) {
            let Some(r) = setup::load_remembered(&self.state_dir) else {
                return;
            };
            if self.adapters == Some(0) || self.reconnecting.load(Ordering::SeqCst) {
                return;
            }
            let connected = session
                .phones()
                .any(|(d, _, _)| d.address.expose().eq_ignore_ascii_case(&r.address));
            if connected {
                self.reconnect_wait = RECONNECT_FIRST;
                self.reconnect_at = None;
                *self
                    .reconnect_note
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                return;
            }
            let now = Instant::now();
            let due = self.reconnect_at.is_none_or(|t| now >= t);
            if !due {
                return;
            }
            self.reconnect_at = Some(now + self.reconnect_wait);
            self.reconnect_wait = (self.reconnect_wait * 2).min(RECONNECT_MAX);
            let flag = self.reconnecting.clone();
            let note = self.reconnect_note.clone();
            flag.store(true, Ordering::SeqCst);
            let _ = std::thread::Builder::new()
                .name("geekbridge-bt-reconnect".into())
                .spawn(move || {
                    let r2 = setup::reconnect(&setup::Bus::default(), &r.address);
                    *note.lock().unwrap_or_else(|e| e.into_inner()) = match r2 {
                        Ok(_) => None,
                        Err(e) => Some(format!("{} is not connected for calls ({e}).", r.name)),
                    };
                    flag.store(false, Ordering::SeqCst);
                });
        }

        /// The GeekBridge links that are up; a newly connected set-up phone triggers a reconnect.
        pub fn set_link_phones(&mut self, names: Vec<String>) {
            if let Some(r) = setup::load_remembered(&self.state_dir) {
                let theirs = |n: &String| {
                    r.geekbridge_name
                        .as_deref()
                        .is_some_and(|g| g.eq_ignore_ascii_case(n))
                        || n.eq_ignore_ascii_case(&r.name)
                };
                let was = self.links.iter().any(theirs);
                let is = names.iter().any(theirs);
                if is && !was {
                    self.reconnect_at = Some(Instant::now());
                    self.reconnect_wait = RECONNECT_FIRST;
                }
            }
            self.links = names;
        }

        pub fn want(&mut self, session: &Session, device: &DeviceRef, want: Want) {
            let errs = hfp_audio::want(session, &mut self.audio, device, want);
            self.errors.extend(errs);
        }

        pub fn status(&self, device: &DeviceRef) -> Option<AudioStatus> {
            self.audio
                .status(BtAddress::from_sensitive(&device.address)?)
        }

        /// The call's audio is on this computer.
        pub fn routed(&self, device: &DeviceRef) -> bool {
            matches!(
                self.status(device).map(|s| s.place),
                Some(Place::Computer { .. })
            )
        }

        pub fn set_mute(&mut self, device: &DeviceRef, mute: bool) -> Result<(), String> {
            let addr = BtAddress::from_sensitive(&device.address)
                .ok_or("this phone has no Bluetooth address")?;
            self.audio.set_mute(addr, mute)
        }

        pub fn lines(&self) -> Vec<String> {
            let mut l = self.audio.lines();
            l.extend(self.errors.iter().cloned());
            if let Some(n) = self
                .reconnect_note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                l.push(n);
            }
            l
        }

        pub fn shutdown(&mut self, session: &Session) {
            let _ = hfp_audio::shutdown(session, &mut self.audio);
        }
    }

    // ------------------------------------------------------------------ guided setup

    /// The one guided setup of this service (pairing runs on its own thread).
    pub struct SetupRunner {
        state_dir: PathBuf,
        state: Arc<Mutex<CallAudioSetupState>>,
        reply: Arc<Mutex<Option<mpsc::Sender<bool>>>>,
        cancel: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
    }

    impl SetupRunner {
        /// The runner for `state_dir` (one per process).
        pub fn get(state_dir: &Path) -> &'static SetupRunner {
            static RUNNER: OnceLock<SetupRunner> = OnceLock::new();
            RUNNER.get_or_init(|| SetupRunner {
                state_dir: state_dir.to_path_buf(),
                state: Arc::new(Mutex::new(CallAudioSetupState {
                    phase: "idle".into(),
                    ..CallAudioSetupState::default()
                })),
                reply: Arc::new(Mutex::new(None)),
                cancel: Arc::new(AtomicBool::new(false)),
                running: Arc::new(AtomicBool::new(false)),
            })
        }

        pub fn state(&self) -> CallAudioSetupState {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone();
            s.phone = setup::load_remembered(&self.state_dir).map(|r| r.name);
            s
        }

        /// `default_phone`: the GeekBridge phone's name, used when the user gave none.
        pub fn act(
            &self,
            action: CallAudioSetupAction,
            default_phone: Option<String>,
        ) -> Result<(), ErrorBody> {
            match action {
                CallAudioSetupAction::Start { phone } => self.start(phone.or(default_phone)),
                CallAudioSetupAction::Confirm { accept } => {
                    let tx = self.reply.lock().unwrap_or_else(|e| e.into_inner()).take();
                    match tx {
                        Some(tx) => {
                            let _ = tx.send(accept);
                            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
                            s.phase = "working".into();
                            s.passkey = None;
                            Ok(())
                        }
                        None => Err(err(
                            ErrorCode::NotFound,
                            "no pairing code is waiting for a confirmation",
                        )),
                    }
                }
                CallAudioSetupAction::Cancel => {
                    self.cancel.store(true, Ordering::SeqCst);
                    if let Some(tx) = self.reply.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        let _ = tx.send(false);
                    }
                    Ok(())
                }
                CallAudioSetupAction::FixConfig => self.fix_config(),
            }
        }

        fn log(state: &Mutex<CallAudioSetupState>, phase: &str, line: String) {
            let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = phase.into();
            s.message = line.clone();
            s.log.push(line);
        }

        fn start(&self, phone: Option<String>) -> Result<(), ErrorBody> {
            if self.running.swap(true, Ordering::SeqCst) {
                return Err(err(
                    ErrorCode::Busy,
                    "the call-audio setup is already running",
                ));
            }
            self.cancel.store(false, Ordering::SeqCst);
            *self.state.lock().unwrap_or_else(|e| e.into_inner()) = CallAudioSetupState {
                phase: "working".into(),
                ..CallAudioSetupState::default()
            };
            let (state, reply, cancel, running, dir) = (
                self.state.clone(),
                self.reply.clone(),
                self.cancel.clone(),
                self.running.clone(),
                self.state_dir.clone(),
            );
            let spawned = std::thread::Builder::new()
                .name("geekbridge-call-setup".into())
                .spawn(move || {
                    let opts = setup::Options {
                        phone_name: phone.clone(),
                        ..setup::Options::default()
                    };
                    let r = setup::run(
                        &opts,
                        |e| match e {
                            setup::Event::Step(l) => Self::log(&state, "working", l),
                            setup::Event::Confirm { device, passkey, reply: tx } => {
                                *reply.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
                                let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
                                s.phase = "confirm".into();
                                s.message = format!(
                                    "Does {device} show {passkey}? Confirm on the phone and here."
                                );
                                let line = s.message.clone();
                                s.log.push(line);
                                s.passkey = Some(passkey);
                                s.device = Some(device);
                            }
                        },
                        &cancel,
                    );
                    match r {
                        Ok(mut rem) => {
                            rem.geekbridge_name = phone;
                            let saved = setup::save_remembered(&dir, &rem);
                            ADAPTER_APPEARED.store(false, Ordering::SeqCst);
                            Self::log(
                                &state,
                                "done",
                                match saved {
                                    Ok(()) => format!(
                                        "{} is set up for calls. Answer a call on this computer to talk through its headphones and microphone.",
                                        rem.name
                                    ),
                                    Err(e) => format!(
                                        "{} is paired, but GeekBridge could not remember it ({e}); it will not reconnect by itself.",
                                        rem.name
                                    ),
                                },
                            );
                        }
                        Err(e) => Self::log(&state, "failed", e),
                    }
                    reply.lock().unwrap_or_else(|e| e.into_inner()).take();
                    running.store(false, Ordering::SeqCst);
                });
            if spawned.is_err() {
                self.running.store(false, Ordering::SeqCst);
                return Err(err(ErrorCode::Failed, "could not start the setup"));
            }
            Ok(())
        }

        /// Write GeekBridge's WirePlumber fragment and restart WirePlumber (user level, no sudo).
        fn fix_config(&self) -> Result<(), ErrorBody> {
            let home = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .ok_or_else(|| {
                    err(
                        ErrorCode::Failed,
                        "no HOME to write the WirePlumber fragment in",
                    )
                })?;
            let cfg = hear::read_wp_config(Some(&home), Path::new("/etc"), Path::new("/usr/share"));
            match hear::write_wp_dropin(&home, &cfg) {
                Ok(None) => {
                    Self::log(
                        &self.state,
                        "done",
                        "The WirePlumber configuration needs no change.".into(),
                    );
                    Ok(())
                }
                Ok(Some(path)) => {
                    // GEEKBRIDGE_WP_RESTART=0: write only (the sandbox, tests).
                    let restarted = std::env::var("GEEKBRIDGE_WP_RESTART").as_deref() != Ok("0")
                        && std::process::Command::new("systemctl")
                            .args(["--user", "restart", "wireplumber.service"])
                            .status()
                            .is_ok_and(|s| s.success());
                    Self::log(
                        &self.state,
                        "done",
                        format!(
                            "Wrote {} (your own files are unchanged; delete it to undo).{}",
                            path.display(),
                            if restarted {
                                " WirePlumber restarted."
                            } else {
                                " Restart WirePlumber: systemctl --user restart wireplumber"
                            }
                        ),
                    );
                    Ok(())
                }
                Err(e) => Err(err(
                    ErrorCode::Failed,
                    &format!("could not write the WirePlumber fragment: {e}"),
                )),
            }
        }
    }

    /// The checklist plus the setup state, for `Request::HearCalls`. Blocking (a few seconds).
    pub fn hear_calls(state_dir: &Path) -> HearCalls {
        let steps = hear::evaluate(&hear::gather(state_dir));
        let runner = SetupRunner::get(state_dir);
        let offer =
            ADAPTER_APPEARED.load(Ordering::SeqCst) && setup::load_remembered(state_dir).is_none();
        hear::to_ipc(&steps, runner.state(), offer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_marks_the_pick_the_default_and_a_missing_pick() {
        let dev = |n: &str, d: &str, def: bool| CallAudioDevice {
            name: n.into(),
            description: d.into(),
            is_default: def,
            bluetooth: false,
            muted: false,
        };
        let d = CallAudioDevices {
            outputs: vec![dev("hx", "HyperX", true), dev("hdmi", "HDMI", false)],
            inputs: vec![dev("mic", "HyperX Mono", true)],
            chosen_output: Some("hdmi".into()),
            chosen_input: Some("gone".into()),
            call: vec!["Call audio: on the phone.".into()],
            problem: None,
        };
        let t = describe(&d);
        assert!(t.contains("Speakers / headphones (calls use: HDMI):"));
        assert!(t.contains("* HDMI\n      hdmi [picked]"));
        assert!(t.contains("hx [system default]"));
        assert!(t.contains("Microphones (calls use: gone (not connected: the default is used)):"));
        assert!(t.contains("now: Call audio: on the phone."));
        assert!(t.contains("--output NAME --input NAME"));
    }

    fn fake() -> Result<Listing, String> {
        let dev = |n: &str| CallAudioDevice {
            name: n.into(),
            description: n.to_uppercase(),
            is_default: false,
            bluetooth: false,
            muted: false,
        };
        Ok((vec![dev("hx"), dev("hdmi")], vec![dev("mic")], None, None))
    }

    #[test]
    fn storing_a_pick_validates_it_and_default_clears_it() {
        use bridge_calls::audio::{Choice, prefs};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(prefs::CHOICE_FILE);
        let d = devices_with(dir.path(), Some((Some("hdmi".into()), None)), fake).unwrap();
        assert_eq!(d.chosen_output.as_deref(), Some("hdmi"));
        assert_eq!(d.outputs.len(), 2);
        // A device that is not connected is refused and the old pick stays.
        let e = devices_with(dir.path(), Some((Some("typo".into()), None)), fake).unwrap_err();
        assert!(e.message.contains("no output called typo"));
        assert_eq!(prefs::load_choice(&path).output.as_deref(), Some("hdmi"));
        // "default" and empty mean no pick.
        devices_with(
            dir.path(),
            Some((Some("default".into()), Some(String::new()))),
            fake,
        )
        .unwrap();
        assert_eq!(prefs::load_choice(&path), Choice::default());
        // No sound server: the pick is still stored and the problem is reported.
        let d = devices_with(dir.path(), Some((Some("hx".into()), None)), || {
            Err("down".into())
        })
        .unwrap();
        assert_eq!(d.problem.as_deref(), Some("down"));
        assert_eq!(d.chosen_output.as_deref(), Some("hx"));
    }
}
