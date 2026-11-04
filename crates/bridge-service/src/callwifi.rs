// SPDX-License-Identifier: GPL-3.0-or-later
//! EXPERIMENTAL, off by default: hearing a phone call on this computer over the paired link
//! (`geekbridge.callaudio.*`, `docs/design/call-audio-over-wifi.md`). Bluetooth hands-free stays the primary path.
//!
//! The hub owns one [`Gate`] per phone (the shared three-consent rules from `bridge-callwifi`) and at most one live
//! session in the whole service. What it adds around the gate:
//!
//! * **Listen**: the user's press ([`CallWifi::listen`]) sends `request`; the phone answers with `offer`; the gate
//!   accepts only with this computer's consent for that phone. Frames go through a jitter buffer and an Opus
//!   decoder to the user's chosen output ([`Audio`]; the real one is `pw-cat` on the device `bridge-calls` resolved
//!   from the same pick the Bluetooth path routes to).
//! * **Speak**: a separate step ([`CallWifi::speak`]) that only exists when the phone reported an uplink route
//!   and its own opt-in. The computer's microphone is captured only while it is on.
//! * **Stop** from either side, the call ending, the consent going off, the link dropping, or five quiet seconds.
//!
//! Privacy: no audio is written to disk or logged; the view carries counts only. Packets from a phone whose
//! consent is off are dropped unread.
use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::io::{Read, Write};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Child;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bridge_callwifi::codec::{Decoder, Encoder};
use bridge_callwifi::gate::{Action, Gate, Side};
use bridge_callwifi::jitter::{JitterBuffer, JitterConfig};
use bridge_callwifi::wire::{self, Answer, Dir, FRAME_SAMPLES, Frame, Msg, Ready, Stats};
use bridge_core::call_wifi_view::{CallWifiAction, CallWifiPhone, CallWifiSession, CallWifiView};
use serde_json::{Map, Value};

/// How long a press waits for the phone's offer before giving up.
const OFFER_WAIT: Duration = Duration::from_secs(10);
/// One playback tick.
const TICK: Duration = Duration::from_millis(20);
/// Gate housekeeping, the receiver report and the "is the call still on" check run this often.
const HOUSEKEEPING: Duration = Duration::from_secs(1);

/// Send one packet to a phone (`device id`, kind, body). False when it could not be queued.
pub type Sender = Arc<dyn Fn(&str, &'static str, Map<String, Value>) -> bool + Send + Sync>;
/// Is a call live on this phone (not just ringing)?
pub type InCall = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// PCM out to the speakers (16 kHz mono).
pub trait Playback: Send {
    fn write(&mut self, pcm: &[i16]) -> std::io::Result<()>;
    fn close(&mut self);
}

/// PCM in from the microphone (16 kHz mono); blocks for one frame.
pub trait Capture: Send {
    fn read(&mut self, pcm: &mut [i16]) -> std::io::Result<()>;
    fn close(&mut self);
}

/// The computer's audio. A trait so the hub is tested without a sound server.
pub trait Audio: Send + Sync {
    /// `Err(why)`: this computer cannot play the audio at all.
    fn supported(&self) -> Result<(), String>;
    /// The chosen output and microphone, for the view (descriptions, not device names).
    fn describe(&self) -> (Option<String>, Option<String>);
    fn open_playback(&self) -> Result<Box<dyn Playback>, String>;
    fn open_capture(&self) -> Result<Box<dyn Capture>, String>;
}

// ------------------------------------------------------------------------------------------ the system audio

/// Linux with PipeWire: `pw-cat` on the device the user chose for calls (`bridge_calls::audio::endpoint`).
pub struct PipeWireAudio {
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    state_dir: Option<PathBuf>,
    /// Whether `pw-cat` runs, looked up once (the call card asks every second).
    support: std::sync::OnceLock<Result<(), String>>,
}

impl PipeWireAudio {
    pub fn new(state_dir: Option<PathBuf>) -> Self {
        Self {
            state_dir,
            support: std::sync::OnceLock::new(),
        }
    }

    #[cfg(target_os = "linux")]
    fn device(
        &self,
        role: bridge_calls::audio::Role,
    ) -> Result<bridge_calls::audio::Endpoint, String> {
        let dir = self
            .state_dir
            .as_deref()
            .ok_or("call audio needs a state directory")?;
        bridge_calls::audio::endpoint(dir, role)
            .map_err(|e| format!("the sound server cannot be reached: {e}"))?
            .ok_or_else(|| match role {
                bridge_calls::audio::Role::Output => {
                    "this computer has no speaker or headphones".to_owned()
                }
                bridge_calls::audio::Role::Input => "this computer has no microphone".to_owned(),
            })
    }
}

fn have_pw_cat() -> bool {
    Command::new("pw-cat")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(target_os = "linux")]
fn pw_cat(mode: &str, target: &str) -> Result<Child, String> {
    let mut c = Command::new("pw-cat");
    c.args([
        mode,
        "--raw",
        "--rate",
        "16000",
        "--channels",
        "1",
        "--format",
        "s16",
        "--latency",
        "40ms",
    ])
    .args(["--media-role", "Communication", "--target", target, "-"]);
    c.stderr(Stdio::null());
    if mode == "-p" {
        c.stdin(Stdio::piped()).stdout(Stdio::null());
    } else {
        c.stdin(Stdio::null()).stdout(Stdio::piped());
    }
    c.spawn().map_err(|e| format!("pw-cat cannot run ({e})"))
}

impl Audio for PipeWireAudio {
    fn supported(&self) -> Result<(), String> {
        self.support
            .get_or_init(|| {
                if !cfg!(target_os = "linux") {
                    Err("playing call audio over Wi-Fi works on Linux with PipeWire only".into())
                } else if !have_pw_cat() {
                    Err("pw-cat (PipeWire's tools) is not installed".into())
                } else {
                    Ok(())
                }
            })
            .clone()
    }

    fn describe(&self) -> (Option<String>, Option<String>) {
        #[cfg(target_os = "linux")]
        {
            use bridge_calls::audio::Role;
            (
                self.device(Role::Output).ok().map(|d| d.description),
                self.device(Role::Input).ok().map(|d| d.description),
            )
        }
        #[cfg(not(target_os = "linux"))]
        (None, None)
    }

    #[cfg(target_os = "linux")]
    fn open_playback(&self) -> Result<Box<dyn Playback>, String> {
        let dev = self.device(bridge_calls::audio::Role::Output)?;
        let mut child = pw_cat("-p", &dev.name)?;
        let stdin = child.stdin.take().ok_or("pw-cat has no input")?;
        set_nonblocking(&stdin);
        Ok(Box::new(PwPlayback {
            child,
            stdin: Some(stdin),
        }))
    }
    #[cfg(not(target_os = "linux"))]
    fn open_playback(&self) -> Result<Box<dyn Playback>, String> {
        Err("not available on this system".into())
    }

    #[cfg(target_os = "linux")]
    fn open_capture(&self) -> Result<Box<dyn Capture>, String> {
        let dev = self.device(bridge_calls::audio::Role::Input)?;
        let mut child = pw_cat("-r", &dev.name)?;
        let out = child.stdout.take().ok_or("pw-cat has no output")?;
        Ok(Box::new(PwCapture { child, out }))
    }
    #[cfg(not(target_os = "linux"))]
    fn open_capture(&self) -> Result<Box<dyn Capture>, String> {
        Err("not available on this system".into())
    }
}

/// A sound output that does not take its samples (suspended, not linked, stalled) must not block the playback worker
/// in a full pipe: the worker is what stops pw-cat at the end of a session. Frames the pipe cannot take are dropped.
#[cfg(target_os = "linux")]
fn set_nonblocking(w: &std::process::ChildStdin) {
    use std::os::fd::AsRawFd;
    let fd = w.as_raw_fd();
    // SAFETY: fcntl on a descriptor this process owns, with plain flag arguments.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

#[cfg(target_os = "linux")]
struct PwPlayback {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
}

#[cfg(target_os = "linux")]
impl Playback for PwPlayback {
    fn write(&mut self, pcm: &[i16]) -> std::io::Result<()> {
        let mut bytes = Vec::with_capacity(pcm.len() * 2);
        for s in pcm {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        match self.stdin.as_mut() {
            Some(w) => match w.write(&bytes) {
                // A full pipe (the output is not taking samples): this 20 ms frame is dropped, never waited for.
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
                // A partial write would leave half a frame in the stream: the rest is dropped too.
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            },
            None => Err(std::io::ErrorKind::BrokenPipe.into()),
        }
    }
    fn close(&mut self) {
        self.stdin = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
impl Drop for PwPlayback {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(target_os = "linux")]
struct PwCapture {
    child: Child,
    out: std::process::ChildStdout,
}

#[cfg(target_os = "linux")]
impl Capture for PwCapture {
    fn read(&mut self, pcm: &mut [i16]) -> std::io::Result<()> {
        let mut bytes = vec![0u8; pcm.len() * 2];
        self.out.read_exact(&mut bytes)?;
        let (pairs, _) = bytes.as_chunks::<2>();
        for (s, b) in pcm.iter_mut().zip(pairs) {
            *s = i16::from_le_bytes(*b);
        }
        Ok(())
    }
    fn close(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_os = "linux")]
impl Drop for PwCapture {
    fn drop(&mut self) {
        self.close();
    }
}

// ------------------------------------------------------------------------------------------------ the hub

struct Phone {
    name: String,
    gate: Gate,
    ready: Option<Ready>,
}

/// The one live (or starting) session.
struct Live {
    device: String,
    epoch: u64,
    /// Session id of an unanswered press, until the offer arrives.
    asked: Option<(u64, Instant)>,
    jitter: JitterBuffer,
    frames: u64,
    started: Instant,
    speaking: bool,
    /// Stops the microphone thread.
    mic_stop: Option<Arc<AtomicBool>>,
}

#[derive(Default)]
struct Inner {
    phones: BTreeMap<String, Phone>,
    live: Option<Live>,
    epoch: u64,
    /// A press the user withdrew before the phone offered: an offer that crossed the stop is declined.
    withdrawn: Option<u64>,
}

pub struct CallWifi {
    inner: Mutex<Inner>,
    send: Sender,
    in_call: InCall,
    audio: Arc<dyn Audio>,
    t0: Instant,
}

fn session_id() -> u64 {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    (u64::from_le_bytes(b) & ((1 << 52) - 1)) | 1
}

impl CallWifi {
    pub fn new(send: Sender, in_call: InCall, audio: Arc<dyn Audio>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            send,
            in_call,
            audio,
            t0: Instant::now(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    // ------------------------------------------------------------------ links and consent

    /// A trusted link is up. `speaks`: the phone declared the callaudio packets and accepts ours.
    pub fn link_up(&self, id: &str, name: &str, speaks: bool, allowed: bool) {
        let mut g = self.lock();
        if !speaks {
            g.phones.remove(id);
            return;
        }
        let mut gate = Gate::new(Side::Computer);
        gate.set_consent(allowed, false);
        let name = name.chars().filter(|c| !c.is_control()).take(80).collect();
        g.phones.insert(
            id.to_owned(),
            Phone {
                name,
                gate,
                ready: None,
            },
        );
    }

    /// The link is gone: whatever was running ends, whatever the phone said about its helper is forgotten.
    pub fn link_down(self: &Arc<Self>, id: &str) {
        let mut g = self.lock();
        if let Some(p) = g.phones.get_mut(id) {
            let actions = p.gate.link_lost();
            self.run(&mut g, id, actions);
        }
        self.end_live(&mut g, id);
        g.phones.remove(id);
    }

    /// This computer's consent for `id` changed (`Feature::CallAudioWifi`). Off ends the session at once.
    pub fn set_allowed(self: &Arc<Self>, id: &str, allowed: bool) {
        let mut g = self.lock();
        let Some(p) = g.phones.get_mut(id) else {
            return;
        };
        let actions = p.gate.set_consent(allowed, false);
        if !allowed {
            p.ready = None;
        }
        self.run(&mut g, id, actions);
        if !allowed {
            self.end_live(&mut g, id);
        }
    }

    // ---------------------------------------------------------------------- packets from the phone

    /// A `geekbridge.callaudio.*` packet from a negotiated, trusted phone. False only when it is malformed.
    /// Without this computer's consent for the phone it is dropped unread.
    pub fn on_packet(self: &Arc<Self>, id: &str, kind: &str, body: &Map<String, Value>) -> bool {
        let mut g = self.lock();
        let Some(phone) = g.phones.get_mut(id) else {
            return true;
        };
        // Without the consent everything is dropped unread, except an offer: the gate declines it with
        // `no_consent`, so a phone whose user pressed start is told why nothing plays.
        if !phone.gate.computer_allows && kind != wire::OFFER {
            return true;
        }
        let Ok(msg) = wire::parse(kind, body) else {
            return false;
        };
        if let Msg::Offer(o) = &msg
            && g.withdrawn == Some(o.session)
        {
            g.withdrawn = None;
            let declined = Msg::Answer(Answer {
                session: o.session,
                accept: false,
                down: false,
                up: false,
                reason: Some("stopped".into()),
            });
            self.transmit(id, &declined);
            return true;
        }
        // One session in the whole service: a second phone's offer is declined, not queued.
        if let Msg::Offer(o) = &msg
            && g.live.as_ref().is_some_and(|l| l.device != id)
        {
            let busy = Msg::Answer(Answer {
                session: o.session,
                accept: false,
                down: false,
                up: false,
                reason: Some("busy".into()),
            });
            self.transmit(id, &busy);
            return true;
        }
        let now = self.now_ms();
        let Some(phone) = g.phones.get_mut(id) else {
            return true;
        };
        let actions = phone.gate.on_msg(msg, now);
        self.run(&mut g, id, actions);
        true
    }

    fn transmit(&self, id: &str, msg: &Msg) {
        let _ = (self.send)(id, msg.kind(), wire::body(msg));
    }

    /// Carry out what a gate decided. `g` is the locked state.
    fn run(self: &Arc<Self>, g: &mut Inner, id: &str, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Send(m) => self.transmit(id, &m),
                Action::IndicatorOn => self.go_live(g, id),
                Action::IndicatorOff => self.end_live(g, id),
                Action::Accept(f) => {
                    let now = self.now_ms();
                    if let Some(l) = g.live.as_mut().filter(|l| l.device == id) {
                        l.frames += 1;
                        l.jitter.push(f.seq, f.opus, now);
                    }
                }
                Action::Drop(_) => {}
                Action::PeerReady(r) => {
                    if let Some(p) = g.phones.get_mut(id) {
                        p.ready = Some(r);
                    }
                }
                Action::SpeakState(on) => self.set_speaking(g, id, on),
            }
        }
    }

    /// The gate says the session is live (the phone's offer was accepted): start playing.
    fn go_live(self: &Arc<Self>, g: &mut Inner, id: &str) {
        if let Some(l) = g.live.as_mut().filter(|l| l.device == id) {
            l.asked = None;
            l.started = Instant::now();
            return;
        }
        // The phone's own press started it. Same worker, same rules.
        self.start_worker(g, id, None);
    }

    fn start_worker(self: &Arc<Self>, g: &mut Inner, id: &str, asked: Option<(u64, Instant)>) {
        g.epoch += 1;
        let epoch = g.epoch;
        crate::call_trace::record(
            "wifi",
            &format!(
                "session started phone={} by={}",
                crate::call_trace::hash(id),
                if asked.is_some() { "computer" } else { "phone" }
            ),
        );
        g.live = Some(Live {
            device: id.to_owned(),
            epoch,
            asked,
            jitter: JitterBuffer::new(JitterConfig::default()),
            frames: 0,
            started: Instant::now(),
            speaking: false,
            mic_stop: None,
        });
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name("geekbridge-callwifi".into())
            .spawn(move || me.worker(epoch));
        if spawned.is_err() {
            g.live = None;
        }
    }

    /// Forget the live session of `id` (the worker and the microphone thread notice and stop).
    fn end_live(&self, g: &mut Inner, id: &str) {
        if g.live.as_ref().is_some_and(|l| l.device == id)
            && let Some(l) = g.live.take()
            && let Some(stop) = l.mic_stop
        {
            stop.store(true, Ordering::Relaxed);
        }
    }

    fn set_speaking(self: &Arc<Self>, g: &mut Inner, id: &str, on: bool) {
        let Some(l) = g.live.as_mut().filter(|l| l.device == id) else {
            return;
        };
        if let Some(stop) = l.mic_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        l.speaking = false;
        if !on {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        l.mic_stop = Some(stop.clone());
        l.speaking = true;
        let (me, epoch, who) = (Arc::clone(self), l.epoch, id.to_owned());
        if std::thread::Builder::new()
            .name("geekbridge-callwifi-mic".into())
            .spawn(move || me.mic_loop(&who, epoch, &stop))
            .is_err()
        {
            l.speaking = false;
            l.mic_stop = None;
        }
    }

    // ------------------------------------------------------------------------------- the user's presses

    /// "Listen here (Wi-Fi, experimental)" for `id`'s current call.
    pub fn listen(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let result = self.listen_inner(id);
        crate::call_trace::record(
            "wifi",
            &format!(
                "listen phone={}: {}",
                crate::call_trace::hash(id),
                result
                    .as_ref()
                    .map_or_else(|e| format!("refused: {e}"), |()| "asked".into())
            ),
        );
        result
    }

    fn listen_inner(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.audio.supported()?;
        let mut g = self.lock();
        if g.live.is_some() {
            return Err("call audio over Wi-Fi is already running; stop it first".into());
        }
        let phone = g
            .phones
            .get_mut(id)
            .ok_or("that phone is not connected, or its app has no Wi-Fi call audio")?;
        if !phone.gate.computer_allows {
            return Err(format!(
                "Wi-Fi call audio is off for this phone; turn it on: geekbridge-ctl consent {id} call_audio_wifi on"
            ));
        }
        match &phone.ready {
            Some(r) if r.ready && r.listen => {}
            Some(r) => {
                return Err(format!(
                    "the phone's helper is not ready to listen (its self-test says: {})",
                    r.test.as_str()
                ));
            }
            None => return Err("the phone has not said its helper is ready; open Settings > Experimental on the phone".into()),
        }
        if !(self.in_call)(id) {
            return Err("there is no live call on this phone".into());
        }
        phone.gate.set_call_active(true);
        let session = session_id();
        let actions = phone.gate.request(session);
        if !actions.iter().any(|a| matches!(a, Action::Send(_))) {
            return Err("cannot start now".into());
        }
        self.run(&mut g, id, actions);
        self.start_worker(&mut g, id, Some((session, Instant::now())));
        Ok(())
    }

    /// Stop listening and speaking at once.
    pub fn stop(self: &Arc<Self>) {
        let mut g = self.lock();
        let Some(id) = g.live.as_ref().map(|l| l.device.clone()) else {
            return;
        };
        self.stop_locked(&mut g, &id, "user");
    }

    fn stop_locked(self: &Arc<Self>, g: &mut Inner, id: &str, reason: &str) {
        crate::call_trace::record(
            "wifi",
            &format!("stop phone={} reason={reason}", crate::call_trace::hash(id)),
        );
        let asked = g.live.as_ref().and_then(|l| l.asked).map(|(s, _)| s);
        let actions = g
            .phones
            .get_mut(id)
            .map(|p| p.gate.stop())
            .unwrap_or_default();
        let told = actions.iter().any(|a| matches!(a, Action::Send(_)));
        self.run(g, id, actions);
        if let (false, Some(session)) = (told, asked) {
            g.withdrawn = Some(session);
            // Pressed but the phone never offered: tell it the request is withdrawn.
            self.transmit(
                id,
                &Msg::Stop {
                    session,
                    reason: Some(reason.into()),
                },
            );
        }
        self.end_live(g, id);
    }

    /// "Speak (experimental)" on or off.
    pub fn speak(self: &Arc<Self>, on: bool) -> Result<(), String> {
        let result = self.speak_inner(on);
        crate::call_trace::record(
            "wifi",
            &format!(
                "speak={on}: {}",
                result
                    .as_ref()
                    .map_or_else(|e| format!("refused: {e}"), |()| "ok".into())
            ),
        );
        result
    }

    fn speak_inner(self: &Arc<Self>, on: bool) -> Result<(), String> {
        let mut g = self.lock();
        let id = g
            .live
            .as_ref()
            .map(|l| l.device.clone())
            .ok_or("nothing is playing; press Listen here first")?;
        let phone = g.phones.get_mut(&id).ok_or("the phone is gone")?;
        if on {
            self.audio.supported()?;
            if !phone.ready.as_ref().is_some_and(|r| r.speak) {
                return Err("this phone reports no way to speak into the call".into());
            }
        }
        let actions = phone.gate.ask_speak(on);
        if actions.iter().any(|a| matches!(a, Action::Drop(_))) {
            return Err("speaking cannot be changed right now".into());
        }
        if !on {
            // Off is immediate on this side; the phone confirms after.
            self.set_speaking(&mut g, &id, false);
        }
        self.run(&mut g, &id, actions);
        Ok(())
    }

    /// The control API entry.
    pub fn act(self: &Arc<Self>, action: CallWifiAction) -> Result<CallWifiView, String> {
        match action {
            CallWifiAction::Status => {}
            CallWifiAction::Listen { device_id } => self.listen(&device_id)?,
            CallWifiAction::Stop => self.stop(),
            CallWifiAction::Speak { on } => self.speak(on)?,
        }
        Ok(self.view(true))
    }

    // ------------------------------------------------------------------------------------------- the view

    /// What the control API and the call card show. `devices`: also name the chosen headphones and microphone
    /// (reads the sound server; the call card's once-a-second poll leaves it out).
    pub fn view(&self, devices: bool) -> CallWifiView {
        let supported = self.audio.supported();
        let (output, input) = if devices && supported.is_ok() {
            self.audio.describe()
        } else {
            (None, None)
        };
        let g = self.lock();
        let phones = g
            .phones
            .iter()
            .map(|(id, p)| {
                let r = p.ready.as_ref();
                CallWifiPhone {
                    device_id: id.clone(),
                    name: p.name.clone(),
                    connected: true,
                    allowed: p.gate.computer_allows,
                    ready: p.gate.computer_allows && r.is_some_and(|r| r.ready),
                    listen: p.gate.computer_allows && r.is_some_and(|r| r.ready && r.listen),
                    speak: p.gate.computer_allows
                        && r.is_some_and(|r| r.ready && r.listen && r.speak),
                    test: r.map_or("not_run", |r| r.test.as_str()).to_owned(),
                    in_call: (self.in_call)(id),
                }
            })
            .collect();
        let session = g.live.as_ref().and_then(|l| {
            let p = g.phones.get(&l.device)?;
            let stats = l.jitter.stats();
            Some(CallWifiSession {
                device_id: l.device.clone(),
                name: p.name.clone(),
                state: if p.gate.session().is_some_and(|s| s.live) {
                    "live"
                } else {
                    "starting"
                }
                .into(),
                speaking: l.speaking && p.gate.session().is_some_and(|s| s.up),
                frames: stats.got,
                lost: stats.lost,
                depth_ms: l.jitter.depth_ms(),
                seconds: l.started.elapsed().as_secs(),
            })
        });
        CallWifiView {
            supported: supported.is_ok(),
            problem: supported.err(),
            phones,
            session,
            output,
            input,
        }
    }

    // ------------------------------------------------------------------------------------- the workers

    /// Plays the far party while the session `epoch` lives; also waits for the offer and does the housekeeping.
    fn worker(self: Arc<Self>, epoch: u64) {
        let mut decoder = Decoder::new().ok();
        let mut player: Option<Box<dyn Playback>> = None;
        let mut next = Instant::now();
        let mut last_housekeeping = Instant::now();
        loop {
            next += TICK;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > Duration::from_millis(200) {
                next = now; // we were stalled: do not burst to catch up
            }
            let slot = {
                let mut g = self.lock();
                let Some(l) = g.live.as_mut().filter(|l| l.epoch == epoch) else {
                    break;
                };
                let id = l.device.clone();
                let live = g
                    .phones
                    .get(&id)
                    .is_some_and(|p| p.gate.session().is_some_and(|s| s.live));
                if let Some((_, since)) = g.live.as_ref().and_then(|l| l.asked)
                    && !live
                    && since.elapsed() > OFFER_WAIT
                {
                    self.stop_locked(&mut g, &id, "timeout");
                    break;
                }
                if last_housekeeping.elapsed() >= HOUSEKEEPING {
                    last_housekeeping = Instant::now();
                    if self.housekeeping(&mut g, &id, epoch) {
                        break;
                    }
                }
                let Some(l) = g.live.as_mut().filter(|l| l.epoch == epoch) else {
                    break;
                };
                if !live {
                    continue;
                }
                l.jitter.pop()
            };
            if player.is_none() {
                match self.audio.open_playback() {
                    Ok(p) => player = Some(p),
                    Err(_) => {
                        let mut g = self.lock();
                        if let Some(id) = g
                            .live
                            .as_ref()
                            .filter(|l| l.epoch == epoch)
                            .map(|l| l.device.clone())
                        {
                            self.stop_locked(&mut g, &id, "no_output");
                        }
                        break;
                    }
                }
            }
            let (Some(p), Some(d)) = (player.as_mut(), decoder.as_mut()) else {
                break;
            };
            if p.write(d.render(&slot)).is_err() {
                let mut g = self.lock();
                if let Some(id) = g
                    .live
                    .as_ref()
                    .filter(|l| l.epoch == epoch)
                    .map(|l| l.device.clone())
                {
                    self.stop_locked(&mut g, &id, "output_closed");
                }
                break;
            }
        }
        if let Some(mut p) = player {
            p.close();
        }
        // The decoder's 150 kB of state and the pipes' buffers go back to the OS with the session.
        drop(decoder);
        crate::memtrim::trim_heap();
    }

    /// Once a second: end a quiet session, end it when the call is over, send the receiver report.
    /// True when the session ended.
    fn housekeeping(self: &Arc<Self>, g: &mut Inner, id: &str, epoch: u64) -> bool {
        let now = self.now_ms();
        let tick = g
            .phones
            .get_mut(id)
            .map(|p| p.gate.tick(now))
            .unwrap_or_default();
        self.run(g, id, tick);
        if !g.live.as_ref().is_some_and(|l| l.epoch == epoch) {
            return true;
        }
        if !(self.in_call)(id) {
            let actions = g
                .phones
                .get_mut(id)
                .map(|p| p.gate.set_call_active(false))
                .unwrap_or_default();
            self.run(g, id, actions);
            self.end_live(g, id);
            return true;
        }
        if let (Some(l), Some(p)) = (g.live.as_ref(), g.phones.get(id))
            && let Some(s) = p.gate.session().filter(|s| s.live)
        {
            let st = l.jitter.stats();
            self.transmit(
                id,
                &Msg::Stats(Stats {
                    session: s.id,
                    dir: Dir::Down,
                    got: st.got,
                    lost: st.lost,
                    late: st.late,
                    depth_ms: l.jitter.depth_ms(),
                }),
            );
        }
        false
    }

    /// Captures the microphone and sends it to the phone while speaking is on (and only then).
    fn mic_loop(self: &Arc<Self>, id: &str, epoch: u64, stop: &AtomicBool) {
        let (Ok(mut enc), Ok(mut cap)) = (Encoder::new(), self.audio.open_capture()) else {
            let mut g = self.lock();
            self.set_speaking(&mut g, id, false);
            return;
        };
        let mut pcm = vec![0i16; FRAME_SAMPLES];
        let (mut seq, mut ts) = (0u32, 0u32);
        while !stop.load(Ordering::Relaxed) {
            if cap.read(&mut pcm).is_err() {
                break;
            }
            let Ok(opus) = enc.encode(&pcm) else { continue };
            let g = self.lock();
            // Off is set under this same lock, so a frame read before the press never goes out after it.
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let now = self.now_ms();
            let Some(sess) = g
                .live
                .as_ref()
                .filter(|l| l.epoch == epoch && l.device == id)
                .and_then(|_| g.phones.get(id))
                .filter(|p| p.gate.may_send(Dir::Up))
                .and_then(|p| p.gate.session().map(|s| s.id))
            else {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            };
            self.transmit(
                id,
                &Msg::Frame(Frame {
                    session: sess,
                    dir: Dir::Up,
                    seq,
                    ts,
                    cap: now as u32,
                    opus,
                }),
            );
            seq = seq.wrapping_add(1);
            ts = ts.wrapping_add(FRAME_SAMPLES as u32);
        }
        cap.close();
        let mut g = self.lock();
        // The microphone is off now; make sure nothing says otherwise.
        if g.live
            .as_ref()
            .is_some_and(|l| l.epoch == epoch && l.device == id)
            && stop.load(Ordering::Relaxed)
        {
            return;
        }
        self.set_speaking(&mut g, id, false);
        if let Some(p) = g.phones.get_mut(id) {
            let a = p.gate.ask_speak(false);
            self.run(&mut g, id, a);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_callwifi::selftest::Verdict;
    use bridge_callwifi::tone::{Tone, dominant_hz, rms};
    use bridge_callwifi::wire::{Offer, Uplink};
    use std::sync::atomic::AtomicUsize;

    type Sent = Arc<Mutex<Vec<(String, &'static str, Map<String, Value>)>>>;

    #[derive(Default)]
    struct Fake {
        played: Arc<Mutex<Vec<i16>>>,
        opened: AtomicUsize,
        closed: Arc<AtomicUsize>,
        mic_opened: AtomicUsize,
        unsupported: bool,
    }

    struct FakePlay(Arc<Mutex<Vec<i16>>>, Arc<AtomicUsize>);
    impl Playback for FakePlay {
        fn write(&mut self, pcm: &[i16]) -> std::io::Result<()> {
            self.0.lock().unwrap().extend_from_slice(pcm);
            Ok(())
        }
        fn close(&mut self) {}
    }
    impl Drop for FakePlay {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }
    struct FakeMic(Tone);
    impl Capture for FakeMic {
        fn read(&mut self, pcm: &mut [i16]) -> std::io::Result<()> {
            std::thread::sleep(Duration::from_millis(20));
            pcm.copy_from_slice(&self.0.frame());
            Ok(())
        }
        fn close(&mut self) {}
    }
    impl Audio for Fake {
        fn supported(&self) -> Result<(), String> {
            if self.unsupported {
                Err("no pw-cat".into())
            } else {
                Ok(())
            }
        }
        fn describe(&self) -> (Option<String>, Option<String>) {
            (Some("Headphones".into()), Some("Headset mic".into()))
        }
        fn open_playback(&self) -> Result<Box<dyn Playback>, String> {
            self.opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakePlay(self.played.clone(), self.closed.clone())))
        }
        fn open_capture(&self) -> Result<Box<dyn Capture>, String> {
            self.mic_opened.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeMic(Tone::new(440.0, 8000.0))))
        }
    }

    struct Rig {
        hub: Arc<CallWifi>,
        sent: Sent,
        call: Arc<AtomicBool>,
        audio: Arc<Fake>,
    }

    fn rig_with(fake: Fake) -> Rig {
        let sent: Sent = Arc::default();
        let call = Arc::new(AtomicBool::new(true));
        let audio = Arc::new(fake);
        let (s, c) = (sent.clone(), call.clone());
        let hub = CallWifi::new(
            Arc::new(move |id, kind, body| {
                s.lock().unwrap().push((id.to_owned(), kind, body));
                true
            }),
            Arc::new(move |_| c.load(Ordering::SeqCst)),
            audio.clone(),
        );
        Rig {
            hub,
            sent,
            call,
            audio,
        }
    }

    fn rig() -> Rig {
        rig_with(Fake::default())
    }

    fn feed(h: &Arc<CallWifi>, id: &str, m: &Msg) -> bool {
        h.on_packet(id, m.kind(), &wire::body(m))
    }

    fn ready(listen: bool, speak: bool) -> Msg {
        Msg::Ready(Ready {
            ready: true,
            listen,
            speak,
            test: if listen {
                Verdict::Works
            } else {
                Verdict::Silent
            },
        })
    }

    fn kinds(r: &Rig) -> Vec<&'static str> {
        r.sent.lock().unwrap().iter().map(|s| s.1).collect()
    }

    fn wait(what: &str, mut ok: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn session_of(r: &Rig, kind: &str) -> u64 {
        let g = r.sent.lock().unwrap();
        let (_, _, b) = g.iter().rev().find(|s| s.1 == kind).expect("sent");
        b["session"].as_u64().unwrap()
    }

    /// A phone that is linked, allowed and has told us it is ready.
    fn phone(r: &Rig, listen: bool, speak: bool) {
        r.hub.link_up("p1", "Fold", true, true);
        assert!(feed(&r.hub, "p1", &ready(listen, speak)));
    }

    /// Press Listen, let the "phone" offer, return the session id.
    fn go_live(r: &Rig) -> u64 {
        r.hub.listen("p1").unwrap();
        let s = session_of(r, wire::REQUEST);
        feed(
            &r.hub,
            "p1",
            &Msg::Offer(Offer {
                session: s,
                down: true,
                up: false,
                uplink: Uplink::TelephonyTx,
            }),
        );
        assert_eq!(kinds(r).last(), Some(&wire::ANSWER));
        s
    }

    fn send_tone(r: &Rig, s: u64, frames: u32) {
        let (mut enc, mut tone) = (Encoder::new().unwrap(), Tone::new(440.0, 8000.0));
        for seq in 0..frames {
            let opus = enc.encode(&tone.frame()).unwrap();
            feed(
                &r.hub,
                "p1",
                &Msg::Frame(Frame {
                    session: s,
                    dir: Dir::Down,
                    seq,
                    ts: seq * 320,
                    cap: 0,
                    opus,
                }),
            );
            // Paced like the phone's 20 ms frames: the jitter buffer holds at most a few, so a burst is shed.
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn listening_needs_consent_a_ready_helper_a_live_call_and_a_working_player() {
        let r = rig();
        assert!(r.hub.listen("p1").unwrap_err().contains("not connected"));
        r.hub.link_up("p1", "Fold", true, false);
        let e = r.hub.listen("p1").unwrap_err();
        assert!(e.contains("consent p1 call_audio_wifi on"), "{e}");
        r.hub.set_allowed("p1", true);
        assert!(r.hub.listen("p1").unwrap_err().contains("not said"));
        feed(&r.hub, "p1", &ready(false, false));
        assert!(r.hub.listen("p1").unwrap_err().contains("silent"));
        feed(&r.hub, "p1", &ready(true, false));
        r.call.store(false, Ordering::SeqCst);
        assert!(r.hub.listen("p1").unwrap_err().contains("no live call"));
        assert!(kinds(&r).is_empty(), "nothing was sent: {:?}", kinds(&r));
        let off = rig_with(Fake {
            unsupported: true,
            ..Fake::default()
        });
        off.hub.link_up("p1", "Fold", true, true);
        assert_eq!(off.hub.listen("p1").unwrap_err(), "no pw-cat");
        let v = off.hub.view(true);
        assert!(!v.supported && v.problem.as_deref() == Some("no pw-cat"));
    }

    #[test]
    fn a_press_an_offer_and_frames_play_the_far_party_and_stop_ends_it() {
        let r = rig();
        phone(&r, true, false);
        let v = r.hub.view(true);
        assert!(v.phones[0].listen && !v.phones[0].speak && v.phones[0].in_call);
        assert_eq!(v.output.as_deref(), Some("Headphones"));
        let s = go_live(&r);
        assert_eq!(kinds(&r), [wire::REQUEST, wire::ANSWER]);
        assert_eq!(r.hub.view(false).session.as_ref().unwrap().state, "live");
        send_tone(&r, s, 30);
        wait("the tone to be played", || {
            let p = r.audio.played.lock().unwrap();
            p.len() >= 320 * 20 && p.windows(1600).step_by(1600).any(|c| rms(c) > 1000.0)
        });
        {
            let p = r.audio.played.lock().unwrap();
            let loud: Vec<i16> = p
                .iter()
                .copied()
                .skip_while(|s| s.abs() < 500)
                .take(3200)
                .collect();
            let hz = dominant_hz(&loud, 16_000);
            assert!((hz - 440.0).abs() < 25.0, "dominant {hz} Hz");
        }
        assert!(r.hub.view(false).session.unwrap().frames >= 30);
        r.hub.stop();
        assert_eq!(kinds(&r).last(), Some(&wire::STOP));
        assert!(r.hub.view(false).session.is_none());
        wait("the player to close", || {
            r.audio.closed.load(Ordering::SeqCst) == 1
        });
        // A frame after the stop is dropped, never played or buffered.
        let n = r.audio.played.lock().unwrap().len();
        send_tone(&r, s, 5);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(r.audio.played.lock().unwrap().len(), n);
    }

    #[test]
    fn the_phone_can_start_it_and_stop_it() {
        let r = rig();
        phone(&r, true, false);
        feed(
            &r.hub,
            "p1",
            &Msg::Offer(Offer {
                session: 9,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
        );
        assert_eq!(r.hub.view(false).session.unwrap().state, "live");
        wait("the player to open", || {
            r.audio.opened.load(Ordering::SeqCst) == 1
        });
        feed(
            &r.hub,
            "p1",
            &Msg::Stop {
                session: 9,
                reason: Some("user".into()),
            },
        );
        assert!(r.hub.view(false).session.is_none());
        wait("the player to close", || {
            r.audio.closed.load(Ordering::SeqCst) == 1
        });
        // No Stop echoed back to a phone that stopped it.
        assert!(!kinds(&r).contains(&wire::STOP));
    }

    #[test]
    fn withdrawing_consent_or_losing_the_link_or_the_call_ends_it() {
        let r = rig();
        phone(&r, true, false);
        go_live(&r);
        r.hub.set_allowed("p1", false);
        assert!(r.hub.view(false).session.is_none());
        assert_eq!(kinds(&r).last(), Some(&wire::STOP));
        // And the phone's readiness is forgotten with the consent.
        assert!(!r.hub.view(false).phones[0].ready);
        r.hub.set_allowed("p1", true);
        feed(&r.hub, "p1", &ready(true, false));
        go_live(&r);
        r.hub.link_down("p1");
        assert!(r.hub.view(false).session.is_none() && r.hub.view(false).phones.is_empty());
        r.hub.link_up("p1", "Fold", true, true);
        feed(&r.hub, "p1", &ready(true, false));
        go_live(&r);
        r.call.store(false, Ordering::SeqCst);
        wait("the call end to stop it", || {
            r.hub.view(false).session.is_none()
        });
        assert_eq!(kinds(&r).last(), Some(&wire::STOP));
    }

    #[test]
    fn without_consent_packets_are_dropped_unread_and_an_offer_is_declined() {
        let r = rig();
        r.hub.link_up("p1", "Fold", true, false);
        assert!(feed(&r.hub, "p1", &ready(true, true)));
        assert!(!r.hub.view(false).phones[0].ready);
        // Even malformed input is not looked at.
        assert!(r.hub.on_packet("p1", wire::FRAME, &Map::new()));
        feed(
            &r.hub,
            "p1",
            &Msg::Offer(Offer {
                session: 3,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
        );
        let g = r.sent.lock().unwrap();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].1, wire::ANSWER);
        assert_eq!(g[0].2["accept"], false);
        assert_eq!(g[0].2["reason"], "no_consent");
        assert!(r.hub.view(false).session.is_none());
    }

    #[test]
    fn malformed_packets_from_a_consenting_phone_are_reported_and_change_nothing() {
        let r = rig();
        phone(&r, true, false);
        assert!(!r.hub.on_packet("p1", wire::OFFER, &Map::new()));
        assert!(!r.hub.on_packet("p1", wire::FRAME, &Map::new()));
        assert!(
            !r.hub
                .on_packet("p1", "geekbridge.callaudio.nonsense", &Map::new())
        );
        // An unknown phone is ignored quietly.
        assert!(r.hub.on_packet("nobody", wire::READY, &Map::new()));
        assert!(r.hub.view(false).session.is_none());
    }

    #[test]
    fn a_second_phone_is_declined_while_one_session_runs() {
        let r = rig();
        phone(&r, true, false);
        r.hub.link_up("p2", "Pixel", true, true);
        go_live(&r);
        feed(
            &r.hub,
            "p2",
            &Msg::Offer(Offer {
                session: 77,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
        );
        let g = r.sent.lock().unwrap();
        let last = g.last().unwrap();
        assert_eq!((last.0.as_str(), last.1), ("p2", wire::ANSWER));
        assert_eq!(last.2["reason"], "busy");
        drop(g);
        assert_eq!(r.hub.view(false).session.unwrap().device_id, "p1");
        assert!(r.hub.listen("p2").unwrap_err().contains("already running"));
    }

    #[test]
    fn a_press_the_phone_never_answers_gives_up() {
        let r = rig();
        phone(&r, true, false);
        r.hub.listen("p1").unwrap();
        assert_eq!(r.hub.view(false).session.unwrap().state, "starting");
        // Stopping before the offer withdraws the request.
        r.hub.stop();
        assert_eq!(kinds(&r), [wire::REQUEST, wire::STOP]);
        assert!(r.hub.view(false).session.is_none());
        // A late offer for that session is then declined, not played.
        let s = session_of(&r, wire::REQUEST);
        feed(
            &r.hub,
            "p1",
            &Msg::Offer(Offer {
                session: s,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(r.audio.opened.load(Ordering::SeqCst), 0);
        assert!(r.hub.view(false).session.is_none());
        assert_eq!(
            r.sent.lock().unwrap().last().unwrap().2["reason"],
            "stopped"
        );
    }

    #[test]
    fn speaking_is_a_separate_step_only_with_an_uplink_and_only_while_on() {
        // Phone reports no uplink route: refused here, nothing sent, microphone never opened.
        let r = rig();
        phone(&r, true, false);
        go_live(&r);
        assert!(r.hub.speak(true).unwrap_err().contains("no way to speak"));
        assert_eq!(r.audio.mic_opened.load(Ordering::SeqCst), 0);
        r.hub.stop();

        let r = rig();
        phone(&r, true, true);
        assert!(r.hub.view(false).phones[0].speak);
        let s = go_live(&r);
        assert_eq!(
            r.audio.mic_opened.load(Ordering::SeqCst),
            0,
            "listening alone never opens the microphone"
        );
        r.hub.speak(true).unwrap();
        assert_eq!(kinds(&r).last(), Some(&wire::SPEAK));
        assert_eq!(
            r.audio.mic_opened.load(Ordering::SeqCst),
            0,
            "not before the phone agrees"
        );
        feed(
            &r.hub,
            "p1",
            &Msg::Speak {
                session: s,
                on: true,
            },
        );
        assert!(r.hub.view(false).session.unwrap().speaking);
        wait("microphone frames", || {
            r.sent
                .lock()
                .unwrap()
                .iter()
                .filter(|m| m.1 == wire::FRAME)
                .count()
                >= 5
        });
        let g = r.sent.lock().unwrap();
        let f = g.iter().find(|m| m.1 == wire::FRAME).unwrap();
        assert_eq!(f.2["dir"], "up");
        assert_eq!(f.2["session"], s);
        drop(g);
        // Off: the microphone thread stops and no more frames follow.
        r.hub.speak(false).unwrap();
        let frames = || kinds(&r).iter().filter(|k| **k == wire::FRAME).count();
        // Off takes effect under the hub lock: at most nothing more is sent. Allow a slow runner to settle.
        std::thread::sleep(Duration::from_millis(100));
        let n = frames();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(frames(), n);
        assert!(!r.hub.view(false).session.unwrap().speaking);
        r.hub.stop();
    }

    #[test]
    fn the_phone_switching_speaking_off_stops_the_microphone() {
        let r = rig();
        phone(&r, true, true);
        let s = go_live(&r);
        r.hub.speak(true).unwrap();
        feed(
            &r.hub,
            "p1",
            &Msg::Speak {
                session: s,
                on: true,
            },
        );
        wait("microphone frames", || kinds(&r).contains(&wire::FRAME));
        // The phone's own opt-in went off.
        feed(
            &r.hub,
            "p1",
            &Msg::Speak {
                session: s,
                on: false,
            },
        );
        assert!(!r.hub.view(false).session.unwrap().speaking);
        std::thread::sleep(Duration::from_millis(100));
        let n = kinds(&r).iter().filter(|k| **k == wire::FRAME).count();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(kinds(&r).iter().filter(|k| **k == wire::FRAME).count(), n);
        r.hub.stop();
    }

    #[test]
    fn actions_return_the_view() {
        let r = rig();
        phone(&r, true, false);
        let v = r.hub.act(CallWifiAction::Status).unwrap();
        assert_eq!(v.phones.len(), 1);
        assert!(r.hub.act(CallWifiAction::Speak { on: true }).is_err());
        let v = r
            .hub
            .act(CallWifiAction::Listen {
                device_id: "p1".into(),
            })
            .unwrap();
        assert_eq!(v.session.unwrap().state, "starting");
        let v = r.hub.act(CallWifiAction::Stop).unwrap();
        assert!(v.session.is_none());
    }
}
