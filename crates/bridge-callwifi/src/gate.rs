// SPDX-License-Identifier: GPL-3.0-or-later
//! Who may send and accept call audio, and when (EXPERIMENTAL). Pure state, driven by the caller's clock.
//!
//! Three independent consents, all required, none implied by another:
//! 1. **Computer consent, per phone** (`computer_allows`): set on the computer for this one paired phone, off by
//!    default.
//! 2. **Phone switch, per computer** (`phone_switch`): the experimental switch on the phone, off by default, plus
//!    the opt-in screen that explains Wireless debugging.
//! 3. **Explicit per-call start**: a user press on either device during this call (`start` on the phone, a
//!    [`Msg::Request`] from the computer). Nothing starts on its own, ever; a new call needs a new press.
//!
//! A session ends on: either side's stop, the call ending, the link dropping, either consent turning off, or five
//! seconds without a frame. Frames outside the live session, in the wrong direction, or above the rate cap are
//! dropped and counted, never buffered.
use crate::wire::{Answer, Dir, Msg, Offer, Ready, Uplink};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Phone,
    Computer,
}

/// The frames per second a peer may send per direction (50 is nominal; slack for bursts after a stall).
pub const MAX_FRAMES_PER_SEC: u32 = 75;
pub const IDLE_TIMEOUT_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub down: bool,
    pub up: bool,
    pub uplink: Uplink,
    /// Answer received/sent: frames may flow.
    pub live: bool,
    pub last_frame_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Send(Msg),
    /// Show the per-call indicator ("Call audio on <computer>" / "Phone call audio here").
    IndicatorOn,
    IndicatorOff,
    /// Feed this frame to the jitter buffer.
    Accept(crate::wire::Frame),
    Drop(&'static str),
    /// Computer: the phone reported its helper state (shown as the "Listen here" button's availability).
    PeerReady(Ready),
    /// "Speak back" is now on (true) or off (false) for the live session: start or stop the uplink path.
    SpeakState(bool),
}

#[derive(Clone, Debug)]
pub struct Gate {
    pub side: Side,
    pub computer_allows: bool,
    pub phone_switch: bool,
    pub call_active: bool,
    /// Phone: the user's separate "Speak (experimental)" opt-in AND a self-test that found an uplink route.
    pub speak_allowed: bool,
    /// Phone: the uplink route the helper reported (what `speak` would use). `Uplink::None` = none.
    pub uplink_route: Uplink,
    /// Computer: a `speak` request is in flight for this session.
    speak_asked: Option<(u64, bool)>,
    session: Option<Session>,
    /// Computer: the session id of the request it sent, so a phone offer can be matched to a press here.
    requested: Option<u64>,
    bucket_start_ms: u64,
    bucket_count: u32,
    pub dropped: u64,
}

impl Gate {
    pub fn new(side: Side) -> Self {
        Self {
            side,
            computer_allows: false,
            phone_switch: false,
            call_active: false,
            speak_allowed: false,
            uplink_route: Uplink::None,
            speak_asked: None,
            session: None,
            requested: None,
            bucket_start_ms: 0,
            bucket_count: 0,
            dropped: 0,
        }
    }

    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    fn end(&mut self, reason: &str, tell_peer: bool) -> Vec<Action> {
        let mut out = Vec::new();
        if let Some(s) = self.session.take() {
            if tell_peer {
                out.push(Action::Send(Msg::Stop {
                    session: s.id,
                    reason: Some(reason.into()),
                }));
            }
            out.push(Action::IndicatorOff);
        }
        self.requested = None;
        self.speak_asked = None;
        out
    }

    /// The computer user pressed "call audio here" (computer side).
    pub fn request(&mut self, session: u64) -> Vec<Action> {
        if self.side != Side::Computer || !self.computer_allows || self.session.is_some() {
            return vec![Action::Drop("request not allowed now")];
        }
        self.requested = Some(session);
        vec![Action::Send(Msg::Request { session })]
    }

    /// The computer user pressed "Speak (experimental)" (or turned it off) during a live session.
    pub fn ask_speak(&mut self, on: bool) -> Vec<Action> {
        match (&self.session, self.side) {
            (Some(s), Side::Computer) if s.live && s.down && self.speak_asked.is_none() => {
                let id = s.id;
                self.speak_asked = Some((id, on));
                vec![Action::Send(Msg::Speak { session: id, on })]
            }
            _ => vec![Action::Drop("speak not allowed now")],
        }
    }

    /// The phone user pressed start, or a request arrived (phone side). `uplink` is what the helper reported.
    pub fn start(&mut self, session: u64, uplink: Uplink) -> Vec<Action> {
        if self.side != Side::Phone
            || !self.phone_switch
            || !self.call_active
            || self.session.is_some()
        {
            return vec![Action::Drop("start not allowed now")];
        }
        // Listening only: speaking back is a separate, later step (`speak`).
        self.session = Some(Session {
            id: session,
            down: true,
            up: false,
            uplink,
            live: false,
            last_frame_ms: 0,
        });
        vec![Action::Send(Msg::Offer(Offer {
            session,
            down: true,
            up: false,
            uplink,
        }))]
    }

    pub fn set_call_active(&mut self, on: bool) -> Vec<Action> {
        self.call_active = on;
        if on {
            Vec::new()
        } else {
            self.end("call_ended", true)
        }
    }

    pub fn set_consent(&mut self, computer_allows: bool, phone_switch: bool) -> Vec<Action> {
        self.computer_allows = computer_allows;
        self.phone_switch = phone_switch;
        let mine = match self.side {
            Side::Phone => phone_switch,
            Side::Computer => computer_allows,
        };
        if mine {
            Vec::new()
        } else {
            self.end("consent_off", true)
        }
    }

    /// Phone: the user's "Speak (experimental)" opt-in changed, or the self-test result did. Turning it off ends
    /// speaking back at once; listening goes on.
    pub fn set_speak_allowed(&mut self, on: bool) -> Vec<Action> {
        self.speak_allowed = on;
        if on {
            return Vec::new();
        }
        match self.session.as_mut() {
            Some(s) if s.up => {
                s.up = false;
                vec![
                    Action::Send(Msg::Speak {
                        session: s.id,
                        on: false,
                    }),
                    Action::SpeakState(false),
                ]
            }
            _ => Vec::new(),
        }
    }

    pub fn link_lost(&mut self) -> Vec<Action> {
        self.end("link_lost", false)
    }

    pub fn stop(&mut self) -> Vec<Action> {
        self.end("user", true)
    }

    /// Housekeeping: end a session that has gone quiet.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        match &self.session {
            Some(s) if s.live && now_ms.saturating_sub(s.last_frame_ms) > IDLE_TIMEOUT_MS => {
                self.end("timeout", true)
            }
            _ => Vec::new(),
        }
    }

    /// May a locally produced frame in `dir` be sent now?
    pub fn may_send(&self, dir: Dir) -> bool {
        let ok_dir = matches!(
            (self.side, dir),
            (Side::Phone, Dir::Down) | (Side::Computer, Dir::Up)
        );
        ok_dir
            && self.session.as_ref().is_some_and(|s| {
                s.live
                    && match dir {
                        Dir::Down => s.down,
                        Dir::Up => s.up,
                    }
            })
    }

    fn rate_ok(&mut self, now_ms: u64) -> bool {
        if now_ms.saturating_sub(self.bucket_start_ms) >= 1000 {
            self.bucket_start_ms = now_ms;
            self.bucket_count = 0;
        }
        self.bucket_count += 1;
        self.bucket_count <= MAX_FRAMES_PER_SEC
    }

    fn drop(&mut self, why: &'static str) -> Vec<Action> {
        self.dropped += 1;
        vec![Action::Drop(why)]
    }

    /// A packet from the peer.
    pub fn on_msg(&mut self, msg: Msg, now_ms: u64) -> Vec<Action> {
        match (self.side, msg) {
            (Side::Phone, Msg::Request { session }) => {
                // The computer user's press is the per-call start; it still needs the phone switch and a call.
                let route = self.uplink_route;
                let mut out = self.start(session, route);
                if matches!(out.first(), Some(Action::Drop(_))) {
                    out = vec![Action::Send(Msg::Stop {
                        session,
                        reason: Some("not_available".into()),
                    })];
                }
                out
            }
            (Side::Computer, Msg::Offer(o)) => {
                if !self.computer_allows || self.session.is_some() {
                    return vec![Action::Send(Msg::Answer(Answer {
                        session: o.session,
                        accept: false,
                        down: false,
                        up: false,
                        reason: Some(
                            if self.computer_allows {
                                "busy"
                            } else {
                                "no_consent"
                            }
                            .into(),
                        ),
                    }))];
                }
                self.requested = None;
                self.session = Some(Session {
                    id: o.session,
                    down: o.down,
                    up: o.up,
                    uplink: o.uplink,
                    live: true,
                    last_frame_ms: now_ms,
                });
                vec![
                    Action::Send(Msg::Answer(Answer {
                        session: o.session,
                        accept: true,
                        down: o.down,
                        up: o.up,
                        reason: None,
                    })),
                    Action::IndicatorOn,
                ]
            }
            (Side::Phone, Msg::Answer(a)) => match &mut self.session {
                Some(s) if s.id == a.session && !s.live => {
                    if a.accept {
                        s.down &= a.down;
                        s.up &= a.up;
                        s.live = true;
                        s.last_frame_ms = now_ms;
                        vec![Action::IndicatorOn]
                    } else {
                        self.session = None;
                        Vec::new()
                    }
                }
                _ => self.drop("answer for no pending offer"),
            },
            (_, Msg::Frame(f)) => {
                let want = match self.side {
                    Side::Phone => Dir::Up,
                    Side::Computer => Dir::Down,
                };
                let ok = self.session.as_ref().is_some_and(|s| {
                    s.live
                        && s.id == f.session
                        && f.dir == want
                        && match want {
                            Dir::Down => s.down,
                            Dir::Up => s.up,
                        }
                });
                if !ok {
                    return self.drop("frame outside the session");
                }
                if !self.rate_ok(now_ms) {
                    return self.drop("frame rate");
                }
                if let Some(s) = self.session.as_mut() {
                    s.last_frame_ms = now_ms;
                }
                vec![Action::Accept(f)]
            }
            (_, Msg::Stop { session, .. }) => {
                if self.session.as_ref().is_some_and(|s| s.id == session) {
                    self.end("peer", false)
                } else {
                    Vec::new()
                }
            }
            (Side::Phone, Msg::Speak { session, on }) => {
                let allowed = self.speak_allowed;
                match self.session.as_mut() {
                    Some(s) if s.live && s.id == session => {
                        s.up = on && allowed && s.uplink != Uplink::None;
                        let granted = s.up;
                        vec![
                            Action::Send(Msg::Speak {
                                session,
                                on: granted,
                            }),
                            Action::SpeakState(granted),
                        ]
                    }
                    _ => self.drop("speak outside the session"),
                }
            }
            (Side::Computer, Msg::Speak { session, on }) => {
                match (self.session.as_mut(), self.speak_asked) {
                    (Some(s), Some((id, _))) if s.live && s.id == session && id == session => {
                        s.up = on;
                        self.speak_asked = None;
                        vec![Action::SpeakState(on)]
                    }
                    // The phone may always switch speaking off by itself (its opt-in went off); never on.
                    (Some(s), _) if s.live && s.id == session && !on => {
                        s.up = false;
                        vec![Action::SpeakState(false)]
                    }
                    _ => self.drop("unasked speak answer"),
                }
            }
            (Side::Computer, Msg::Ready(r)) => vec![Action::PeerReady(r)],
            (_, Msg::Stats(_)) => Vec::new(),
            _ => self.drop("not for this side"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::Frame;

    fn frame(session: u64, dir: Dir, seq: u32) -> Msg {
        Msg::Frame(Frame {
            session,
            dir,
            seq,
            ts: seq * 320,
            cap: 0,
            opus: vec![1],
        })
    }

    fn sent(a: &[Action]) -> Vec<Msg> {
        a.iter()
            .filter_map(|x| match x {
                Action::Send(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    /// Phone and computer gates wired to each other; returns them with a live, listen-only session 7.
    fn listen_pair() -> (Gate, Gate) {
        let mut p = Gate::new(Side::Phone);
        let mut c = Gate::new(Side::Computer);
        p.set_consent(false, true);
        c.set_consent(true, false);
        p.set_call_active(true);
        p.uplink_route = Uplink::TelephonyTx;
        let offer = sent(&p.on_msg(Msg::Request { session: 7 }, 0)).remove(0);
        assert!(matches!(&offer, Msg::Offer(o) if o.down && !o.up));
        let ans = c.on_msg(offer, 0);
        assert!(ans.contains(&Action::IndicatorOn));
        let a = sent(&ans).remove(0);
        assert_eq!(p.on_msg(a, 0), vec![Action::IndicatorOn]);
        assert!(p.may_send(Dir::Down) && !p.may_send(Dir::Up));
        assert!(!c.may_send(Dir::Up) && !c.may_send(Dir::Down));
        (p, c)
    }

    /// The same, with speaking back switched on through the `speak` handshake.
    fn live_pair() -> (Gate, Gate) {
        let (mut p, mut c) = listen_pair();
        p.set_speak_allowed(true);
        let ask = sent(&c.ask_speak(true)).remove(0);
        let reply = p.on_msg(ask, 1);
        assert!(reply.contains(&Action::SpeakState(true)));
        let back = sent(&reply).remove(0);
        assert_eq!(c.on_msg(back, 1), vec![Action::SpeakState(true)]);
        assert!(p.may_send(Dir::Down) && !p.may_send(Dir::Up));
        assert!(c.may_send(Dir::Up) && !c.may_send(Dir::Down));
        (p, c)
    }

    #[test]
    fn listening_never_carries_the_computers_voice() {
        let (mut p, mut c) = listen_pair();
        // The phone drops mic frames in a listen-only session; the computer may not send them.
        assert!(matches!(
            p.on_msg(frame(7, Dir::Up, 1), 10)[0],
            Action::Drop(_)
        ));
        assert!(!c.may_send(Dir::Up));
        // Speak is refused unless the phone's own opt-in is on, however the computer asks.
        let ask = sent(&c.ask_speak(true)).remove(0);
        let reply = p.on_msg(ask, 2);
        assert!(reply.contains(&Action::SpeakState(false)));
        let back = sent(&reply).remove(0);
        assert!(matches!(back, Msg::Speak { on: false, .. }));
        c.on_msg(back, 2);
        assert!(!c.may_send(Dir::Up) && !p.may_send(Dir::Up));
        // An unasked or foreign speak answer changes nothing.
        assert!(matches!(
            c.on_msg(
                Msg::Speak {
                    session: 7,
                    on: true
                },
                3
            )[0],
            Action::Drop(_)
        ));
        assert!(!c.may_send(Dir::Up));
    }

    #[test]
    fn speak_stops_when_the_phone_opt_in_goes_off() {
        let (mut p, mut c) = live_pair();
        let out = p.set_speak_allowed(false);
        assert!(out.contains(&Action::SpeakState(false)));
        assert!(!p.may_send(Dir::Up));
        // The phone's frame handling now drops mic frames at once.
        assert!(matches!(
            p.on_msg(frame(7, Dir::Up, 9), 20)[0],
            Action::Drop(_)
        ));
        let back = sent(&out).remove(0);
        // The computer follows the phone's "off" without having asked, but never an unasked "on".
        assert_eq!(c.on_msg(back, 21), vec![Action::SpeakState(false)]);
        assert!(!c.may_send(Dir::Up));
    }

    #[test]
    fn ready_reaches_only_the_computer() {
        use crate::selftest::Verdict;
        let r = crate::wire::Ready {
            ready: true,
            listen: true,
            speak: false,
            test: Verdict::Works,
        };
        let mut c = Gate::new(Side::Computer);
        assert_eq!(
            c.on_msg(Msg::Ready(r.clone()), 0),
            vec![Action::PeerReady(r.clone())]
        );
        let mut p = Gate::new(Side::Phone);
        assert!(matches!(p.on_msg(Msg::Ready(r), 0)[0], Action::Drop(_)));
    }

    #[test]
    fn nothing_starts_without_all_three_consents() {
        let mut p = Gate::new(Side::Phone);
        p.set_call_active(true);
        assert!(
            matches!(p.start(1, Uplink::None)[0], Action::Drop(_)),
            "switch off"
        );
        p.set_consent(false, true);
        p.set_call_active(false);
        assert!(
            matches!(p.start(1, Uplink::None)[0], Action::Drop(_)),
            "no call"
        );
        // Computer without consent refuses an offer and never shows the indicator.
        let mut c = Gate::new(Side::Computer);
        let a = c.on_msg(
            Msg::Offer(Offer {
                session: 1,
                down: true,
                up: false,
                uplink: Uplink::None,
            }),
            0,
        );
        assert_eq!(a.len(), 1);
        assert!(
            matches!(&sent(&a)[0], Msg::Answer(x) if !x.accept && x.reason.as_deref() == Some("no_consent"))
        );
        assert!(matches!(c.request(2)[0], Action::Drop(_)));
        // A request reaching a phone whose switch is off is answered with a stop, nothing else.
        let mut q = Gate::new(Side::Phone);
        q.set_call_active(true);
        assert!(matches!(
            &sent(&q.on_msg(Msg::Request { session: 3 }, 0))[0],
            Msg::Stop { .. }
        ));
        assert!(q.session().is_none());
    }

    #[test]
    fn frames_flow_only_inside_the_live_session_and_direction() {
        let (mut p, mut c) = live_pair();
        assert!(matches!(
            c.on_msg(frame(7, Dir::Down, 1), 10)[0],
            Action::Accept(_)
        ));
        assert!(
            matches!(c.on_msg(frame(8, Dir::Down, 1), 10)[0], Action::Drop(_)),
            "other session"
        );
        assert!(
            matches!(c.on_msg(frame(7, Dir::Up, 1), 10)[0], Action::Drop(_)),
            "wrong way"
        );
        assert!(matches!(
            p.on_msg(frame(7, Dir::Up, 1), 10)[0],
            Action::Accept(_)
        ));
        assert!(matches!(
            p.on_msg(frame(7, Dir::Down, 1), 10)[0],
            Action::Drop(_)
        ));
    }

    #[test]
    fn the_rate_cap_holds() {
        let (_, mut c) = live_pair();
        let accepted = (0..200)
            .filter(|i| matches!(c.on_msg(frame(7, Dir::Down, *i), 100)[0], Action::Accept(_)))
            .count();
        assert_eq!(accepted, MAX_FRAMES_PER_SEC as usize);
        assert!(matches!(
            c.on_msg(frame(7, Dir::Down, 999), 1200)[0],
            Action::Accept(_)
        ));
    }

    #[test]
    fn every_ending_turns_the_indicator_off() {
        let (mut p, mut c) = live_pair();
        let a = p.set_call_active(false);
        assert!(a.contains(&Action::IndicatorOff));
        let stop = sent(&a).remove(0);
        assert_eq!(c.on_msg(stop, 0), vec![Action::IndicatorOff]);
        assert!(!c.may_send(Dir::Up));

        let (mut p, mut c) = live_pair();
        assert!(c.set_consent(false, false).contains(&Action::IndicatorOff));
        assert!(p.link_lost().contains(&Action::IndicatorOff));

        let (_, mut c) = live_pair();
        assert!(c.tick(IDLE_TIMEOUT_MS - 1).is_empty());
        assert!(c.tick(IDLE_TIMEOUT_MS + 1).contains(&Action::IndicatorOff));
    }

    #[test]
    fn a_computer_press_starts_a_listen_only_offer_on_the_phone() {
        let mut p = Gate::new(Side::Phone);
        p.set_consent(false, true);
        p.set_call_active(true);
        let mut c = Gate::new(Side::Computer);
        c.set_consent(true, false);
        let req = sent(&c.request(11)).remove(0);
        let offer = sent(&p.on_msg(req, 0)).remove(0);
        assert!(matches!(&offer, Msg::Offer(o) if o.session == 11 && o.down && !o.up));
        // A stray answer is dropped.
        let mut q = Gate::new(Side::Phone);
        assert!(matches!(
            q.on_msg(
                Msg::Answer(Answer {
                    session: 1,
                    accept: true,
                    down: true,
                    up: false,
                    reason: None
                }),
                0
            )[0],
            Action::Drop(_)
        ));
    }
}
