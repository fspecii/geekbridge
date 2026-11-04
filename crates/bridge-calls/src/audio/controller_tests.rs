// SPDX-License-Identifier: GPL-3.0-or-later
//! Controller tests over an in-memory audio server and a manual clock.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;
use crate::audio::route::fixtures::*;

#[derive(Default)]
struct State {
    snap: ServerSnapshot,
    log: Vec<String>,
    peaks: BTreeMap<String, f32>,
    down: bool,
}

#[derive(Clone)]
struct Mock(Arc<Mutex<State>>);

impl Mock {
    fn new(snap: ServerSnapshot) -> Self {
        Self(Arc::new(Mutex::new(State {
            snap,
            ..State::default()
        })))
    }
    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }
    fn log(&self) -> Vec<String> {
        self.st().log.clone()
    }
    fn writes(&self) -> Vec<String> {
        self.log()
            .into_iter()
            .filter(|l| !l.starts_with("peak"))
            .collect()
    }
}

impl AudioServer for Mock {
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError> {
        let s = self.st();
        if s.down {
            return Err(ServerError::Unavailable("down".into()));
        }
        Ok(s.snap.clone())
    }
    fn move_sink_input(&mut self, index: u32, sink: &str) -> Result<(), ServerError> {
        let mut s = self.st();
        let to = s
            .snap
            .sinks
            .iter()
            .find(|x| x.name == sink)
            .map(|x| x.index);
        if let Some(i) = s.snap.sink_inputs.iter_mut().find(|i| i.index == index) {
            i.device = to;
        }
        s.log.push(format!("move rx {index} -> {sink}"));
        Ok(())
    }
    fn move_source_output(&mut self, index: u32, source: &str) -> Result<(), ServerError> {
        let mut s = self.st();
        let to = s
            .snap
            .sources
            .iter()
            .find(|x| x.name == source)
            .map(|x| x.index);
        if let Some(o) = s.snap.source_outputs.iter_mut().find(|o| o.index == index) {
            o.device = to;
        }
        s.log.push(format!("move tx {index} -> {source}"));
        Ok(())
    }
    fn set_card_profile(&mut self, card: u32, profile: &str) -> Result<(), ServerError> {
        let mut s = self.st();
        if let Some(c) = s.snap.cards.iter_mut().find(|c| c.index == card) {
            c.active_profile = Some(profile.to_owned());
        }
        if profile == "off" {
            // The voice link closes with the card's nodes.
            s.snap.sink_inputs.clear();
            s.snap.source_outputs.clear();
        }
        s.log.push(format!("profile {card} {profile}"));
        Ok(())
    }
    fn set_source_mute(&mut self, source: &str, mute: bool) -> Result<(), ServerError> {
        let mut s = self.st();
        if let Some(x) = s.snap.sources.iter_mut().find(|x| x.name == source) {
            x.mute = mute;
        }
        s.log.push(format!("mute {source} {mute}"));
        Ok(())
    }
    fn peak(&mut self, t: &PeakTarget, _: Duration) -> Result<Option<f32>, ServerError> {
        let key = match t {
            PeakTarget::SinkInput { .. } => "rx".to_owned(),
            PeakTarget::Source(n) => n.clone(),
        };
        let mut s = self.st();
        s.log.push(format!("peak {key}"));
        Ok(s.peaks.get(&key).copied())
    }
    fn reconnect(&mut self) -> Result<(), ServerError> {
        Ok(())
    }
}

struct Rig {
    mock: Mock,
    clock: Arc<Mutex<Instant>>,
    ctl: CallAudio<Mock>,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new(snap: ServerSnapshot) -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::in_dir(snap, dir)
    }
    fn in_dir(snap: ServerSnapshot, dir: tempfile::TempDir) -> Self {
        let mock = Mock::new(snap);
        let clock = Arc::new(Mutex::new(Instant::now()));
        let m = mock.clone();
        let c = clock.clone();
        let ctl = CallAudio::new(move || Ok(m.clone()), dir.path())
            .with_clock(move || *c.lock().unwrap());
        Self {
            mock,
            clock,
            ctl,
            dir,
        }
    }
    fn advance(&self, ms: u64) {
        *self.clock.lock().unwrap() += Duration::from_millis(ms);
    }
    fn restore_file(&self) -> std::path::PathBuf {
        self.dir.path().join(prefs::RESTORE_FILE)
    }
    fn pick(&self, output: Option<&str>, input: Option<&str>) {
        prefs::save_choice(
            &self.dir.path().join(prefs::CHOICE_FILE),
            &Choice {
                output: output.map(str::to_owned),
                input: input.map(str::to_owned),
            },
        )
        .unwrap();
    }
}

fn input(ringing: bool, live: bool, reject: bool) -> PhoneInput {
    PhoneInput {
        address: phone(),
        ringing,
        live,
        transport: Some(TransportState::Idle),
        reject_sco: Some(reject),
    }
}

fn reject(r: bool) -> Request {
    Request::SetRejectSco {
        address: phone(),
        reject: r,
    }
}

#[test]
fn answered_here_goes_to_the_picked_devices_and_everything_is_undone_at_the_end() {
    // The card starts "off" (a user once switched it off): a call here must switch it on.
    let mut r = Rig::new(snapshot("off", false));
    r.pick(Some(HDMI_OUT), None);

    // Ringing: nobody asked yet, so the phone's voice link is refused (don't steal).
    assert_eq!(r.ctl.tick(&[input(true, false, false)]), vec![reject(true)]);
    assert!(
        r.restore_file().exists(),
        "the flag change is recorded first"
    );

    // The user clicks Answer here: accept the voice link before the answer goes out.
    assert_eq!(
        r.ctl.want(phone(), Want::Computer, Some(true)),
        vec![reject(false)]
    );
    // Live, no voice link yet: the card is switched on at once...
    assert!(r.ctl.tick(&[input(false, true, false)]).is_empty());
    assert_eq!(r.mock.writes(), vec!["profile 7 audio-gateway"]);
    // ...and after 1.5 s without a link the phone is asked for it, at most twice.
    r.advance(1600);
    assert_eq!(
        r.ctl.tick(&[input(false, true, false)]),
        vec![Request::Activate { address: phone() }]
    );
    r.advance(500);
    assert!(r.ctl.tick(&[input(false, true, false)]).is_empty());
    assert_eq!(r.ctl.status(phone()).unwrap().place, Place::Connecting);

    // The phone opens the link: WirePlumber put the voice on the defaults; move it to the pick.
    add_call_streams(&mut r.mock.st().snap, Some(65), Some(68));
    let mut i = input(false, true, false);
    i.transport = Some(TransportState::Active);
    r.ctl.tick(std::slice::from_ref(&i));
    let w = r.mock.writes();
    assert!(w.contains(&format!("move rx 201 -> {HDMI_OUT}")), "{w:?}");
    assert!(
        !w.iter().any(|l| l.starts_with("move tx")),
        "the microphone already is the default pick"
    );
    let s = r.ctl.status(phone()).unwrap();
    assert_eq!(
        s.place,
        Place::Computer {
            output: HDMI_OUT.into(),
            input: HEADSET_IN.into(),
            codec: Some("msbc".into())
        }
    );
    assert!(r.ctl.lines()[0].contains("mSBC, wideband 16 kHz"));

    // Mute acts on the real microphone and is reported.
    r.ctl.set_mute(phone(), true).unwrap();
    assert!(r.mock.writes().contains(&format!("mute {HEADSET_IN} true")));
    assert_eq!(r.ctl.status(phone()).unwrap().muted, Some(true));
    // A muted microphone the user asked for is not a problem.
    r.mock.st().peaks.insert("rx".into(), 0.3);
    r.advance(3100);
    r.ctl.tick(std::slice::from_ref(&i));
    assert!(r.ctl.status(phone()).unwrap().problems.is_empty());

    // The call ends: card back off, microphone unmuted, phone flag back, file gone.
    let out = r.ctl.tick(&[input(false, false, false)]);
    assert!(
        out.is_empty(),
        "RejectSCO is already back to its original value"
    );
    let w = r.mock.writes();
    assert!(
        w.ends_with(&[
            "profile 7 off".to_owned(),
            format!("mute {HEADSET_IN} false")
        ]),
        "{w:?}"
    );
    assert!(!r.restore_file().exists());
    assert!(r.ctl.status(phone()).is_none());
}

#[test]
fn answered_on_the_phone_keeps_the_audio_there_but_a_later_phone_pick_still_works() {
    let mut r = Rig::new(snapshot("audio-gateway", false));
    assert_eq!(r.ctl.tick(&[input(true, false, false)]), vec![reject(true)]);
    // Answered on the phone: stays refused for 5 s after going live, never activated.
    assert!(r.ctl.tick(&[input(false, true, true)]).is_empty());
    r.advance(4000);
    assert!(r.ctl.tick(&[input(false, true, true)]).is_empty());
    r.advance(1100);
    assert_eq!(r.ctl.tick(&[input(false, true, true)]), vec![reject(false)]);
    r.advance(5000);
    assert!(r.ctl.tick(&[input(false, true, false)]).is_empty());
    assert!(
        r.mock.writes().is_empty(),
        "nothing touched on the computer"
    );
    assert_eq!(r.ctl.status(phone()).unwrap().place, Place::Phone);
    assert!(r.ctl.status(phone()).unwrap().problems.is_empty());

    // The user then picks Bluetooth on the phone: the audio comes and is routed (no steal).
    add_call_streams(&mut r.mock.st().snap, Some(65), Some(68));
    r.ctl.tick(&[input(false, true, false)]);
    assert!(matches!(
        r.ctl.status(phone()).unwrap().place,
        Place::Computer { .. }
    ));
    r.ctl.tick(&[input(false, false, false)]);
    assert!(!r.restore_file().exists());
}

#[test]
fn use_the_phone_closes_the_link_and_the_card_comes_back_after_the_call() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.ctl.want(phone(), Want::Computer, Some(false));
    r.ctl.tick(&[input(false, true, false)]);
    assert!(matches!(
        r.ctl.status(phone()).unwrap().place,
        Place::Computer { .. }
    ));
    assert_eq!(
        r.ctl.want(phone(), Want::Phone, Some(false)),
        vec![reject(true)]
    );
    r.ctl.tick(&[input(false, true, true)]);
    assert!(r.mock.writes().contains(&"profile 7 off".to_owned()));
    assert_eq!(r.ctl.status(phone()).unwrap().place, Place::Phone);
    // No more profile changes while the link stays closed.
    let n = r.mock.writes().len();
    r.ctl.tick(&[input(false, true, true)]);
    assert_eq!(r.mock.writes().len(), n);
    // End of call: profile and flag restored.
    let out = r.ctl.tick(&[input(false, false, true)]);
    assert_eq!(out, vec![reject(false)]);
    assert_eq!(r.mock.writes().last().unwrap(), "profile 7 audio-gateway");
    assert!(!r.restore_file().exists());
}

#[test]
fn no_voice_link_three_seconds_after_answering_here_is_reported_with_a_fix() {
    let mut r = Rig::new(snapshot("audio-gateway", false));
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    r.advance(2900);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(r.ctl.status(phone()).unwrap().problems.is_empty());
    r.advance(200);
    r.ctl.tick(&[input(false, true, false)]);
    let p = r.ctl.status(phone()).unwrap().problems;
    assert_eq!(p, vec![FlowProblem::NoVoiceLink.message()]);
    assert!(r.ctl.lines().iter().any(|l| l.contains("Samsung")));
}

#[test]
fn silence_an_unlinked_stream_and_a_muted_microphone_are_each_named() {
    let mut snap = snapshot("audio-gateway", false);
    add_call_streams(&mut snap, None, Some(68));
    snap.sources[0].mute = true; // muted on the headset / in the mixer, not by GeekBridge
    let mut r = Rig::new(snap);
    r.ctl.want(phone(), Want::Computer, None);
    // The move fails to link in this mock (it re-links on move): keep rx unlinked by
    // making the target unknown to the mock's move.
    r.ctl.tick(&[input(false, true, false)]);
    r.mock.st().snap.sink_inputs[0].device = None;
    r.mock.st().peaks.insert(HEADSET_IN.into(), 0.0);
    r.advance(3100);
    r.ctl.tick(&[input(false, true, false)]);
    let p = r.ctl.status(phone()).unwrap().problems;
    assert!(p.contains(&FlowProblem::RxUnlinked.message()), "{p:?}");
    assert!(p.iter().any(|l| l.contains("is muted")), "{p:?}");
    // A linked but silent voice stream is named; a microphone with signal is fine.
    r.mock.st().snap.sink_inputs[0].device = Some(65);
    r.mock.st().snap.sources[0].mute = false;
    r.mock.st().peaks.insert("rx".into(), 0.0);
    r.mock.st().peaks.insert(HEADSET_IN.into(), 0.2);
    r.advance(5000);
    r.ctl.tick(&[input(false, true, false)]);
    let p = r.ctl.status(phone()).unwrap().problems;
    assert_eq!(p, vec![FlowProblem::RxSilent.message()]);
    // No more level measurements after the second check.
    let peaks = r
        .mock
        .log()
        .iter()
        .filter(|l| l.starts_with("peak"))
        .count();
    r.advance(10_000);
    r.ctl.tick(&[input(false, true, false)]);
    assert_eq!(
        r.mock
            .log()
            .iter()
            .filter(|l| l.starts_with("peak"))
            .count(),
        peaks
    );
    assert_eq!(
        r.ctl.status(phone()).unwrap().problems,
        vec![FlowProblem::RxSilent.message()],
        "the finding stays until the call ends"
    );
}

#[test]
fn a_crash_leftover_is_undone_at_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut snap = snapshot("audio-gateway", false);
    snap.sources[0].mute = true;
    prefs::save_restore(
        &dir.path().join(prefs::RESTORE_FILE),
        &Restore {
            cards: vec![CardRestore {
                card: "bluez_card.AA_BB_CC_00_11_22".into(),
                previous: "off".into(),
                set_to: "audio-gateway".into(),
            }],
            mutes: vec![MuteRestore {
                source: HEADSET_IN.into(),
                was_muted: false,
                set_to: true,
            }],
            reject: vec![RejectRestore {
                address: PHONE.into(),
                previous: false,
            }],
        },
    )
    .unwrap();
    let mut r = Rig::in_dir(snap, dir);
    let out = r.ctl.tick(&[input(false, false, true)]);
    assert_eq!(out, vec![reject(false)]);
    assert_eq!(
        r.mock.writes(),
        vec![
            "profile 7 off".to_owned(),
            format!("mute {HEADSET_IN} false")
        ]
    );
    assert!(!r.restore_file().exists());
    // Only once.
    assert!(r.ctl.tick(&[input(false, false, false)]).is_empty());
}

#[test]
fn a_change_someone_else_made_since_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    prefs::save_restore(
        &dir.path().join(prefs::RESTORE_FILE),
        &Restore {
            cards: vec![CardRestore {
                card: "bluez_card.AA_BB_CC_00_11_22".into(),
                previous: "off".into(),
                set_to: "something-else".into(),
            }],
            ..Restore::default()
        },
    )
    .unwrap();
    let mut r = Rig::in_dir(snapshot("audio-gateway", false), dir);
    r.ctl.tick(&[]);
    assert!(r.mock.writes().is_empty());
    assert!(!r.restore_file().exists());
}

#[test]
fn the_phone_leaving_mid_call_ends_the_route_without_requests() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.ctl.want(phone(), Want::Computer, Some(true));
    r.ctl.tick(&[input(false, true, false)]);
    r.ctl.set_mute(phone(), true).unwrap();
    // Out of range: the phone, its card and its streams are gone.
    {
        let mut st = r.mock.st();
        st.snap.cards.clear();
        st.snap.sink_inputs.clear();
        st.snap.source_outputs.clear();
    }
    let out = r.ctl.tick(&[]);
    assert!(
        out.is_empty(),
        "nothing can be sent to a phone that is gone"
    );
    assert!(
        r.mock.writes().last().unwrap().contains("false"),
        "the microphone is unmuted again"
    );
    assert!(!r.restore_file().exists());
    assert!(r.ctl.lines().is_empty());
}

#[test]
fn an_unreachable_sound_server_is_a_line_not_a_silent_failure() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.mock.st().down = true;
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    let l = r.ctl.lines();
    assert!(
        l.iter()
            .any(|l| l.contains("Reading the sound server failed")),
        "{l:?}"
    );
    // It comes back: routed, the line is gone.
    r.mock.st().down = false;
    r.advance(2500);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(matches!(
        r.ctl.status(phone()).unwrap().place,
        Place::Computer { .. }
    ));
}

#[test]
fn pulseaudio_without_pipewire_is_reported_and_left_alone() {
    let mut snap = snapshot("audio-gateway", true);
    snap.identity.server_name = "pulseaudio".into();
    let mut r = Rig::new(snap);
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(r.mock.writes().is_empty());
    assert_eq!(
        r.ctl.status(phone()).unwrap().problems,
        vec![FlowProblem::NotPipeWire.message()]
    );
}

#[test]
fn a_missing_pick_falls_back_to_the_default_and_says_so() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.pick(Some("alsa_output.unplugged"), None);
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    let s = r.ctl.status(phone()).unwrap();
    assert!(matches!(&s.place, Place::Computer { output, .. } if output == HEADSET_OUT));
    assert!(s.problems[0].contains("alsa_output.unplugged"));
    // Re-picking during the call moves the voice at the next poll.
    r.pick(Some(HDMI_OUT), None);
    r.ctl.reload_choice();
    r.ctl.tick(&[input(false, true, false)]);
    assert!(
        r.mock
            .writes()
            .contains(&format!("move rx 201 -> {HDMI_OUT}"))
    );
}

#[test]
fn mute_without_a_route_is_refused_plainly() {
    let mut r = Rig::new(snapshot("audio-gateway", false));
    assert!(r.ctl.set_mute(phone(), true).is_err());
    r.ctl.tick(&[input(false, true, false)]);
    assert!(
        r.ctl
            .set_mute(phone(), true)
            .unwrap_err()
            .contains("not on this computer")
    );
}

#[test]
fn a_mute_is_undone_even_after_the_audio_went_back_to_the_phone() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.ctl.want(phone(), Want::Computer, Some(false));
    r.ctl.tick(&[input(false, true, false)]);
    r.ctl.set_mute(phone(), true).unwrap();
    r.ctl.want(phone(), Want::Phone, Some(false));
    r.ctl.tick(&[input(false, true, true)]);
    assert_eq!(r.ctl.status(phone()).unwrap().place, Place::Phone);
    r.ctl.tick(&[input(false, false, true)]);
    let w = r.mock.writes();
    assert!(w.contains(&format!("mute {HEADSET_IN} false")), "{w:?}");
    assert!(!r.restore_file().exists());
}

#[test]
fn a_card_change_for_a_phone_that_left_is_dropped_from_the_restore_file() {
    let mut r = Rig::new(snapshot("off", false));
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(r.restore_file().exists());
    r.mock.st().snap.cards.clear();
    r.ctl.tick(&[]);
    assert!(!r.restore_file().exists());
}

#[test]
fn a_sound_server_outage_at_the_end_of_a_call_is_replayed_later() {
    let mut r = Rig::new(snapshot("audio-gateway", true));
    r.ctl.want(phone(), Want::Computer, None);
    r.ctl.tick(&[input(false, true, false)]);
    r.ctl.set_mute(phone(), true).unwrap();
    r.mock.st().down = true;
    r.ctl.tick(&[input(false, false, false)]);
    assert!(r.restore_file().exists(), "kept for later");
    r.mock.st().down = false;
    r.advance(2500);
    r.ctl.tick(&[input(false, false, false)]);
    assert!(!r.restore_file().exists());
    assert!(
        r.mock
            .writes()
            .contains(&format!("mute {HEADSET_IN} false"))
    );
}

#[test]
fn silence_in_the_first_seconds_of_a_call_is_not_reported_until_it_lasts() {
    // Dialling: no ringback yet, the phone sends true digital silence at the first check.
    let mut snap = snapshot("audio-gateway", false);
    add_call_streams(&mut snap, Some(65), Some(68));
    let mut r = Rig::new(snap);
    r.ctl.want(phone(), Want::Computer, None);
    r.mock.st().peaks.insert("rx".into(), 0.0);
    r.mock.st().peaks.insert(HEADSET_IN.into(), 0.0);
    r.ctl.tick(&[input(false, true, false)]);
    r.advance(3100);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(
        r.mock.log().iter().any(|l| l == "peak rx"),
        "measured at the first check"
    );
    assert_eq!(
        r.ctl.status(phone()).unwrap().problems,
        Vec::<String>::new()
    );
    // The far end answers and talks: heard at the second check, nothing ever reported.
    r.mock.st().peaks.insert("rx".into(), 0.3);
    r.mock.st().peaks.insert(HEADSET_IN.into(), 0.1);
    r.advance(5000);
    r.ctl.tick(&[input(false, true, false)]);
    assert_eq!(
        r.ctl.status(phone()).unwrap().problems,
        Vec::<String>::new()
    );
    // Still silent at the second check: then it is named, with the dongle hint.
    let mut snap = snapshot("audio-gateway", false);
    add_call_streams(&mut snap, Some(65), Some(68));
    let mut r = Rig::new(snap);
    r.ctl.want(phone(), Want::Computer, None);
    r.mock.st().peaks.insert("rx".into(), 0.0);
    r.mock.st().peaks.insert(HEADSET_IN.into(), 0.1);
    r.ctl.tick(&[input(false, true, false)]);
    r.advance(3100);
    r.ctl.tick(&[input(false, true, false)]);
    assert!(r.ctl.status(phone()).unwrap().problems.is_empty());
    r.advance(5000);
    r.ctl.tick(&[input(false, true, false)]);
    assert_eq!(
        r.ctl.status(phone()).unwrap().problems,
        vec![FlowProblem::RxSilent.message()]
    );
}
