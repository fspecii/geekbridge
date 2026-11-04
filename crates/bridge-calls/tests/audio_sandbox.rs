// SPDX-License-Identifier: GPL-3.0-or-later
//! Call audio end to end against a private PipeWire, WirePlumber, pipewire-pulse and a fake
//! phone. Run through `scripts/e2e/call-audio-sandbox.sh` (it sets everything up and passes
//! `--ignored`); outside the sandbox every test returns at once.
//!
//! Real: PipeWire's bluez5 hands-free code and telephony service, the phone's `bluez_card`, the
//! sound server, GeekBridge's telephony session, call-audio controller and libpulse adapter.
//! Stand-ins: the SCO voice link (two pw-cat streams with the properties PipeWire gives the real
//! ones), the headset/speaker (null sinks) and the microphone (a remap of a null sink).
#![cfg(target_os = "linux")]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bridge_calls::audio::model::ServerSnapshot;
use bridge_calls::audio::{
    AudioServer, BtAddress, CallAudio, Choice, Place, PulseOptions, PulseServer, Want, prefs, route,
};
use bridge_calls::pipewire::{PipeWireBackend, PipeWireConfig};
use bridge_calls::{DeviceRef, Session, hfp_audio};
use bridge_core::calls::{CallCommand, CallPhase};

const ADDR: &str = "AA:BB:CC:00:11:22";

fn in_sandbox() -> bool {
    std::env::var("GEEKBRIDGE_AUDIO_SANDBOX").is_ok_and(|v| v == "1")
}

fn fake(method: &str, arg: Option<&str>) -> zbus::Message {
    let c = zbus::blocking::Connection::system().expect("private system bus");
    let r = match arg {
        Some(a) => c.call_method(
            Some("org.bluez"),
            "/geekbridge/FakePhone",
            Some("org.geekbridge.FakePhone"),
            method,
            &(a,),
        ),
        None => c.call_method(
            Some("org.bluez"),
            "/geekbridge/FakePhone",
            Some("org.geekbridge.FakePhone"),
            method,
            &(),
        ),
    };
    r.expect("fake phone")
}

fn at_log() -> Vec<String> {
    fake("AtLog", None).body().deserialize().unwrap()
}

/// Generous limits: the sandbox shares a busy machine with other builds.
fn pulse_options() -> PulseOptions {
    PulseOptions {
        connect_timeout: Duration::from_secs(20),
        op_timeout: Duration::from_secs(10),
        ..PulseOptions::default()
    }
}

fn pulse() -> PulseServer {
    PulseServer::connect(pulse_options()).expect("sandbox pulse server")
}

fn snap() -> ServerSnapshot {
    pulse().snapshot().unwrap()
}

/// A process tree that is killed on drop.
struct Kids(Vec<Child>, Arc<AtomicBool>);
impl Drop for Kids {
    fn drop(&mut self) {
        self.1.store(true, Ordering::SeqCst);
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn props(name: &str) -> String {
    format!(
        "{{ \"api.bluez5.address\": \"{ADDR}\", \"api.bluez5.profile\": \"headset-audio-gateway\", \"api.bluez5.codec\": \"msbc\", \"device.api\": \"bluez5\", \"node.name\": \"{name}\", \"media.name\": \"{name}\" }}"
    )
}

/// Play `amplitude` sine (0.0 = digital silence) into a pw-cat playback stream.
fn player(args: &[&str], amplitude: f32, stop: Arc<AtomicBool>) -> Child {
    let mut child = Command::new("pw-cat")
        .args([
            "--playback",
            "--raw",
            "--format",
            "f32",
            "--rate",
            "16000",
            "--channels",
            "1",
        ])
        .args(args)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("pw-cat");
    let mut stdin = child.stdin.take().unwrap();
    std::thread::spawn(move || {
        let mut n = 0u64;
        while !stop.load(Ordering::SeqCst) {
            let mut buf = Vec::with_capacity(1600 * 4);
            for _ in 0..1600 {
                let v =
                    amplitude * ((n as f32) * 2.0 * std::f32::consts::PI * 440.0 / 16000.0).sin();
                buf.extend_from_slice(&v.to_le_bytes());
                n += 1;
            }
            if stdin.write_all(&buf).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(90));
        }
    });
    child
}

/// The two streams PipeWire creates while the phone's voice link is open.
fn voice_link(amplitude: f32) -> Kids {
    let stop = Arc::new(AtomicBool::new(false));
    let rx = player(
        &["-P", &props("bluez_input.AA_BB_CC_00_11_22.0")],
        amplitude,
        stop.clone(),
    );
    let tx = Command::new("pw-cat")
        .args([
            "--record",
            "--raw",
            "--format",
            "f32",
            "--rate",
            "16000",
            "--channels",
            "1",
            "-P",
        ])
        .arg(props("bluez_output.AA_BB_CC_00_11_22.1"))
        .arg("-")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("pw-cat");
    Kids(vec![rx, tx], stop)
}

/// Someone talking into the sandbox microphone.
fn talking() -> Kids {
    let stop = Arc::new(AtomicBool::new(false));
    let c = player(&["--target", "sbx_mic_feed"], 0.5, stop.clone());
    Kids(vec![c], stop)
}

struct Rig {
    session: Session,
    audio: CallAudio<PulseServer>,
    state: std::path::PathBuf,
}

impl Rig {
    fn new() -> Self {
        let state = std::path::PathBuf::from(std::env::var("GEEKBRIDGE_SANDBOX_STATE").unwrap());
        let _ = std::fs::remove_file(state.join(prefs::RESTORE_FILE));
        let backend = PipeWireBackend::connect(PipeWireConfig::default()).expect("telephony");
        let mut session = Session::new(Box::new(backend));
        let end = Instant::now() + Duration::from_secs(10);
        let ours = |s: &Session| {
            s.phones()
                .any(|(d, _, _)| d.address.expose().eq_ignore_ascii_case(ADDR))
        };
        while !ours(&session) && Instant::now() < end {
            session.pump(Duration::from_millis(200)).unwrap();
        }
        assert!(
            session.phones().next().is_some(),
            "the fake phone registered"
        );
        let audio = CallAudio::new(|| PulseServer::connect(pulse_options()), &state);
        Self {
            session,
            audio,
            state,
        }
    }
    fn device(&self) -> DeviceRef {
        self.session
            .phones()
            .find(|(d, _, _)| d.address.expose().eq_ignore_ascii_case(ADDR))
            .expect("the sandbox phone")
            .0
            .clone()
    }
    fn phone(&self) -> BtAddress {
        BtAddress::parse(ADDR).unwrap()
    }
    fn reject_sco(&self) -> Option<bool> {
        self.session
            .phone(&self.device())
            .and_then(|p| p.transport.as_ref())
            .and_then(|t| t.reject_sco)
    }
    fn phases(&self) -> Vec<CallPhase> {
        self.session
            .state(&self.device())
            .map(|s| s.calls.values().map(|c| c.phase).collect())
            .unwrap_or_default()
    }
    /// Poll like the service does until `f` holds (or fail after `secs`).
    fn until(&mut self, secs: u64, what: &str, f: impl Fn(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let _ = self.session.pump(Duration::from_millis(100));
            let errs = hfp_audio::tick(&self.session, &mut self.audio);
            assert!(errs.is_empty(), "{errs:?}");
            if f(self) {
                return;
            }
            assert!(
                Instant::now() < end,
                "timed out waiting for: {what}\nlines: {:?}\nat: {:?}",
                self.audio.lines(),
                at_log()
            );
        }
    }
    fn pump_for(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.until(5, "a poll", |_| true);
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn pick(&self, output: Option<&str>, input: Option<&str>) {
        prefs::save_choice(
            &self.state.join(prefs::CHOICE_FILE),
            &Choice {
                output: output.map(str::to_owned),
                input: input.map(str::to_owned),
            },
        )
        .unwrap();
    }
    fn ring_and_answer_here(&mut self) {
        fake("Ring", Some("+15550100"));
        self.until(5, "ringing", |r| r.phases().contains(&CallPhase::Incoming));
        // Ringing and nobody asked: the voice link is refused so answering on the phone keeps it there.
        self.until(5, "RejectSCO=true while ringing", |r| {
            r.reject_sco() == Some(true)
        });
        let d = self.device();
        let errs = hfp_audio::want(&self.session, &mut self.audio, &d, Want::Computer);
        assert!(errs.is_empty(), "{errs:?}");
        let call = self
            .session
            .state(&d)
            .unwrap()
            .calls
            .keys()
            .next()
            .unwrap()
            .clone();
        self.session
            .execute(&d, &CallCommand::Answer { call })
            .unwrap();
        self.until(5, "answered", |r| r.phases() == vec![CallPhase::Active]);
        assert!(at_log().iter().any(|l| l == "ATA"));
        assert_eq!(
            self.reject_sco(),
            Some(false),
            "accepted before ATA went out"
        );
    }
    fn hang_up_remote(&mut self) {
        fake("RemoteHangup", None);
        self.until(5, "call ended", |r| r.phases().is_empty());
    }
}

fn card_profile() -> Option<String> {
    snap()
        .cards
        .iter()
        .find(|c| c.name.starts_with("bluez_card"))
        .and_then(|c| c.active_profile.clone())
}

fn stream_targets() -> (Option<String>, Option<String>) {
    let s = snap();
    let v = route::phone_voice(&s, BtAddress::parse(ADDR).unwrap());
    let sink = v.rx.and_then(|x| x.device).and_then(|i| {
        s.sinks
            .iter()
            .find(|k| k.index == i)
            .map(|k| k.name.clone())
    });
    let source = v.tx.and_then(|x| x.device).and_then(|i| {
        s.sources
            .iter()
            .find(|k| k.index == i)
            .map(|k| k.name.clone())
    });
    (sink, source)
}

fn source_muted(name: &str) -> bool {
    snap().sources.iter().any(|s| s.name == name && s.mute)
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn a_call_answered_here_plays_on_the_picked_devices_mutes_the_real_mic_and_restores_everything() {
    if !in_sandbox() {
        return;
    }
    let mut r = Rig::new();
    assert_eq!(
        card_profile().as_deref(),
        Some("audio-gateway"),
        "PipeWire's profile for a phone"
    );
    // The user once switched the phone's card off; a call answered here must switch it on.
    pulse()
        .set_card_profile(
            snap()
                .cards
                .iter()
                .find(|c| c.name.starts_with("bluez_card"))
                .unwrap()
                .index,
            "off",
        )
        .unwrap();
    r.pick(Some("sbx_speaker"), Some("sbx_mic"));
    let _mic = talking();

    r.ring_and_answer_here();
    r.until(5, "card switched on", |_| {
        card_profile().as_deref() == Some("audio-gateway")
    });
    // No voice link after 1.5 s: the phone is asked for it (HFP AT+BCC from PipeWire).
    r.until(6, "voice link requested", |_| {
        at_log().iter().any(|l| l == "AT+BCC")
    });
    // No SCO can open in the sandbox, so after 3 s the missing link is reported with a fix.
    r.until(6, "no-voice-link problem", |r| {
        r.audio
            .lines()
            .iter()
            .any(|l| l.contains("did not open the Bluetooth voice link"))
    });

    // The phone opens the link: WirePlumber links the streams to the defaults, GeekBridge moves them.
    let _voice = voice_link(0.5);
    r.until(8, "routed to the pick", |_| {
        stream_targets() == (Some("sbx_speaker".into()), Some("sbx_mic".into()))
    });
    let s = r.audio.status(r.phone()).unwrap();
    assert_eq!(
        s.place,
        Place::Computer {
            output: "sbx_speaker".into(),
            input: "sbx_mic".into(),
            codec: Some("msbc".into())
        }
    );
    // Audio flows both ways, so after the checks no problem is left.
    r.pump_for(3500);
    let s = r.audio.status(r.phone()).unwrap();
    assert!(s.problems.is_empty(), "{:?}", s.problems);

    // Mute acts on the real microphone.
    r.audio.set_mute(r.phone(), true).unwrap();
    assert!(source_muted("sbx_mic"));
    assert_eq!(r.audio.status(r.phone()).unwrap().muted, Some(true));
    assert!(
        std::fs::read_to_string(r.state.join(prefs::RESTORE_FILE))
            .unwrap()
            .contains("sbx_mic")
    );

    // Changing the pick mid-call moves the voice at the next poll.
    r.pick(Some("sbx_headset_out"), Some("sbx_mic"));
    r.audio.reload_choice();
    r.until(5, "moved to the new pick", |_| {
        stream_targets().0.as_deref() == Some("sbx_headset_out")
    });

    // The far end hangs up: microphone unmuted, card back off, phone flag back, nothing left.
    r.hang_up_remote();
    r.until(5, "restored", |_| {
        !source_muted("sbx_mic") && card_profile().as_deref() == Some("off")
    });
    assert_eq!(r.reject_sco(), Some(false));
    assert!(!r.state.join(prefs::RESTORE_FILE).exists());
    assert!(r.audio.lines().is_empty());
    let card = snap()
        .cards
        .iter()
        .find(|c| c.name.starts_with("bluez_card"))
        .unwrap()
        .index;
    pulse().set_card_profile(card, "audio-gateway").unwrap();
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn answered_on_the_phone_stays_there_and_use_the_phone_sends_it_back() {
    if !in_sandbox() {
        return;
    }
    let mut r = Rig::new();
    r.pick(None, None);
    let bcc = |log: Vec<String>| log.iter().filter(|l| *l == "AT+BCC").count();
    let asked_before = bcc(at_log());
    fake("Ring", Some("+15550101"));
    r.until(5, "RejectSCO=true while ringing", |r| {
        r.reject_sco() == Some(true)
    });
    fake("RemoteAnswer", None);
    r.until(5, "active", |r| r.phases() == vec![CallPhase::Active]);
    // Kept refused for a few seconds after the answer, then handed back.
    r.pump_for(2000);
    assert_eq!(r.reject_sco(), Some(true));
    r.until(8, "RejectSCO handed back", |r| {
        r.reject_sco() == Some(false)
    });
    assert_eq!(bcc(at_log()), asked_before, "never asked for the audio");
    assert_eq!(r.audio.status(r.phone()).unwrap().place, Place::Phone);

    // "Use computer audio" on that call, then "use the phone".
    let d = r.device();
    hfp_audio::want(&r.session, &mut r.audio, &d, Want::Computer);
    let _voice = voice_link(0.5);
    r.until(6, "routed to the defaults", |_| {
        stream_targets() == (Some("sbx_headset_out".into()), Some("sbx_mic".into()))
    });
    let errs = hfp_audio::want(&r.session, &mut r.audio, &d, Want::Phone);
    assert!(errs.is_empty());
    r.until(5, "link closed (card off)", |_| {
        card_profile().as_deref() == Some("off")
    });
    assert_eq!(r.reject_sco(), Some(true));
    assert_eq!(r.audio.status(r.phone()).unwrap().place, Place::Phone);
    r.hang_up_remote();
    r.until(5, "card restored", |_| {
        card_profile().as_deref() == Some("audio-gateway")
    });
    r.until(5, "flag restored", |r| r.reject_sco() == Some(false));
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn silence_from_the_phone_and_a_muted_microphone_are_reported_within_seconds() {
    if !in_sandbox() {
        return;
    }
    let mut r = Rig::new();
    r.pick(None, Some("sbx_mic"));
    pulse().set_source_mute("sbx_mic", true).unwrap();
    r.ring_and_answer_here();
    let _voice = voice_link(0.0);
    r.until(8, "silence and mute named", |r| {
        let l = r.audio.lines();
        l.iter().any(|l| l.contains("digital silence")) && l.iter().any(|l| l.contains("is muted"))
    });
    pulse().set_source_mute("sbx_mic", false).unwrap();
    r.hang_up_remote();
    // GeekBridge did not mute it, so it does not unmute or remute it.
    assert!(!r.state.join(prefs::RESTORE_FILE).exists());
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn a_crash_mid_call_is_undone_at_the_next_start() {
    if !in_sandbox() {
        return;
    }
    let mut r = Rig::new();
    r.pick(None, Some("sbx_mic"));
    r.ring_and_answer_here();
    let voice = voice_link(0.5);
    r.until(6, "routed", |r| {
        stream_targets().1.as_deref() == Some("sbx_mic")
            && matches!(
                r.audio.status(r.phone()).map(|s| s.place),
                Some(Place::Computer { .. })
            )
    });
    r.audio.set_mute(r.phone(), true).unwrap();
    assert!(source_muted("sbx_mic"));
    // The process dies: no release runs. The call ends while it is down.
    let Rig {
        session,
        audio,
        state,
    } = r;
    drop(audio); // no undo runs: CallAudio has no Drop-time cleanup, like a killed process
    drop(voice);
    drop(session);
    fake("RemoteHangup", None);
    // Next start: the restore file is replayed before anything else.
    assert!(state.join(prefs::RESTORE_FILE).exists());
    let backend = PipeWireBackend::connect(PipeWireConfig::default()).unwrap();
    let mut session = Session::new(Box::new(backend));
    session.refresh().unwrap();
    let mut audio = CallAudio::new(|| PulseServer::connect(pulse_options()), &state);
    let end = Instant::now() + Duration::from_secs(5);
    while (source_muted("sbx_mic") || state.join(prefs::RESTORE_FILE).exists())
        && Instant::now() < end
    {
        let _ = session.pump(Duration::from_millis(100));
        hfp_audio::tick(&session, &mut audio);
    }
    assert!(!source_muted("sbx_mic"), "unmuted after the crash");
    assert!(!state.join(prefs::RESTORE_FILE).exists());
    let d = session
        .phones()
        .find(|(d, _, _)| d.address.expose().eq_ignore_ascii_case(ADDR))
        .unwrap()
        .0
        .clone();
    assert_eq!(
        session
            .phone(&d)
            .unwrap()
            .transport
            .as_ref()
            .unwrap()
            .reject_sco,
        Some(false),
        "phone flag restored"
    );
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn zz_the_phone_leaving_mid_call_ends_cleanly() {
    if !in_sandbox() {
        return;
    }
    let mut r = Rig::new();
    r.pick(None, Some("sbx_mic"));
    r.ring_and_answer_here();
    let voice = voice_link(0.5);
    r.until(6, "routed", |r| {
        stream_targets().1.as_deref() == Some("sbx_mic")
            && matches!(
                r.audio.status(r.phone()).map(|s| s.place),
                Some(Place::Computer { .. })
            )
    });
    r.audio.set_mute(r.phone(), true).unwrap();
    // Out of range: RFCOMM closes, the voice streams go with it.
    fake("Disconnect", None);
    drop(voice);
    r.until(10, "phone gone and microphone unmuted", |r| {
        !r.session
            .phones()
            .any(|(d, _, _)| d.address.expose().eq_ignore_ascii_case(ADDR))
            && !source_muted("sbx_mic")
    });
    assert!(r.audio.lines().is_empty());
    assert!(!r.state.join(prefs::RESTORE_FILE).exists());
}

#[test]
#[ignore = "needs scripts/e2e/call-audio-sandbox.sh"]
fn y_setup_pairs_trusts_and_connects_a_new_phone_with_the_code_confirmed() {
    use bridge_calls::setup::{self, Event};
    if !in_sandbox() {
        return;
    }
    let mut shown = Vec::new();
    let mut steps = Vec::new();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let opts = setup::Options {
        phone_name: Some("setup phone".into()),
        timeout: Duration::from_secs(20),
        ..setup::Options::default()
    };
    let r = setup::run(
        &opts,
        |e| match e {
            Event::Step(s) => steps.push(s),
            Event::Confirm {
                device,
                passkey,
                reply,
            } => {
                shown.push((device, passkey));
                reply.send(true).unwrap();
            }
        },
        &cancel,
    )
    .unwrap_or_else(|e| panic!("setup failed: {e}; steps: {steps:?}"));
    assert_eq!(r.address, "AA:BB:CC:00:33:44");
    assert_eq!(r.name, "Setup Phone");
    assert_eq!(shown, vec![("Setup Phone".to_owned(), "482910".to_owned())]);
    assert!(
        steps
            .iter()
            .any(|s| s.contains("visible as a Bluetooth device")),
        "{steps:?}"
    );
    // PipeWire's telephony now has the new phone as a hands-free audio gateway.
    let backend = PipeWireBackend::connect(PipeWireConfig::default()).unwrap();
    let mut session = Session::new(Box::new(backend));
    let end = Instant::now() + Duration::from_secs(10);
    let has = |s: &Session| {
        s.phones()
            .any(|(d, _, _)| d.address.expose().eq_ignore_ascii_case("AA:BB:CC:00:33:44"))
    };
    while !has(&session) && Instant::now() < end {
        let _ = session.pump(Duration::from_millis(200));
    }
    assert!(
        has(&session),
        "the new phone's hands-free profile is connected"
    );
    // Running it again finds it paired and only reconnects; reconnect() is quiet when connected.
    assert!(setup::reconnect(&setup::Bus::default(), "AA:BB:CC:00:33:44").unwrap());
    // Setup leaves the computer invisible again.
    let c = zbus::blocking::Connection::system().unwrap();
    let v: zbus::zvariant::OwnedValue = c
        .call_method(
            Some("org.bluez"),
            "/org/bluez/hci0",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.bluez.Adapter1", "Discoverable"),
        )
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    assert!(!bool::try_from(v).unwrap(), "invisible again after setup");
}
