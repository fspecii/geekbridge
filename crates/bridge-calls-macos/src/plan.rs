//! Command planning: turns a requested operation plus the current call picture into exactly one
//! documented `IOBluetoothHandsFreeDevice` operation, or a refusal.
//!
//! The mapping to native calls uses Apple's header documentation, quoted by paraphrase:
//!
//! | Operation | Native call | Apple's description |
//! | --- | --- | --- |
//! | accept ringing call | `acceptCall` | "Accepts an incoming call." |
//! | reject ringing / end only call | `endCall` | "Hangs up the current call, or rejects an incoming call." |
//! | hold, resume | `holdCall` | puts all active calls on hold and accepts the other (held or waiting) call |
//! | answer waiting call | `holdCall` | same: active call held, waiting call accepted |
//! | reject waiting call, end held call | `releaseHeldCalls` | releases held calls, or sets User Determined User Busy for a waiting call |
//! | dial | `dialNumber:` | calls the number; the gateway decides acceptable formats |
//! | DTMF | `sendDTMF:` | one character of `0-9 # * A-D` |
//!
//! Anything the indicators cannot attribute unambiguously is refused ([`Refusal`]); the planner
//! never guesses which of several calls an operation would hit.
use crate::model::AgFeatures;
use crate::tracker::{SetupKind, SlotKind, Slots};
use bridge_core::calls::{AudioLinkState, CallCommand, CallId, MAX_DIAL_LEN, Sensitive};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How call audio is asked to move to this Mac.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioStrategy {
    /// `transferAudioToComputer` ("moves the audio for current and future calls to a Mac").
    #[default]
    TransferToComputer,
    /// `connectSCO` (opens a SCO link from the hands-free side).
    ConnectSco,
}

/// Adapter-level request. A superset of the core [`CallCommand`]: HFP hold and resume have no
/// core counterpart (core only models "swap").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MacCommand {
    Dial {
        number: Sensitive,
    },
    Answer {
        call: CallId,
    },
    /// Reject a ringing call, cancel a dialing call, or end a live call.
    Hangup {
        call: CallId,
    },
    Hold,
    Resume,
    Swap,
    SendTones {
        tones: Sensitive,
    },
    ConnectAudio {
        #[serde(default)]
        strategy: AudioStrategy,
    },
    DisconnectAudio,
}

impl MacCommand {
    /// Lower a core command. Ringer and microphone mute have no valid public mapping here.
    pub fn from_core(command: &CallCommand) -> Result<Self, Refusal> {
        Ok(match command {
            CallCommand::Answer { call } => Self::Answer { call: call.clone() },
            // IOBluetooth's `endCall` both hangs up and rejects; the planner decides from the
            // call's phase. The core already restricted `Reject` to ringing calls.
            CallCommand::Hangup { call } | CallCommand::Reject { call } => {
                Self::Hangup { call: call.clone() }
            }
            CallCommand::Dial { number } => Self::Dial {
                number: number.clone(),
            },
            CallCommand::SwapCalls => Self::Swap,
            CallCommand::SendTones { tones } => Self::SendTones {
                tones: tones.clone(),
            },
            CallCommand::ConnectAudio => Self::ConnectAudio {
                strategy: AudioStrategy::default(),
            },
            CallCommand::DisconnectAudio => Self::DisconnectAudio,
            CallCommand::MuteRinger => return Err(Refusal::NoPlatformMapping("mute_ringer")),
            CallCommand::SetMicrophoneMute { .. } => {
                return Err(Refusal::NoPlatformMapping("mute_microphone"));
            }
            // `CallCommand` is exhaustive today; a future variant must be mapped deliberately.
            #[allow(unreachable_patterns)]
            _ => return Err(Refusal::NoPlatformMapping("unknown_command")),
        })
    }

    pub fn kind(&self) -> OpKind {
        match self {
            Self::Dial { .. } => OpKind::Dial,
            Self::Answer { .. } => OpKind::Answer,
            Self::Hangup { .. } => OpKind::Hangup,
            Self::Hold => OpKind::Hold,
            Self::Resume => OpKind::Resume,
            Self::Swap => OpKind::Swap,
            Self::SendTones { .. } => OpKind::SendTones,
            Self::ConnectAudio { .. } => OpKind::ConnectAudio,
            Self::DisconnectAudio => OpKind::DisconnectAudio,
        }
    }
}

/// Operation kinds, for per-operation capability reporting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Dial,
    Answer,
    Hangup,
    Hold,
    Resume,
    Swap,
    SendTones,
    ConnectAudio,
    DisconnectAudio,
}
impl OpKind {
    pub const ALL: [OpKind; 9] = [
        Self::Dial,
        Self::Answer,
        Self::Hangup,
        Self::Hold,
        Self::Resume,
        Self::Swap,
        Self::SendTones,
        Self::ConnectAudio,
        Self::DisconnectAudio,
    ];
    /// Call-control operations share one in-flight slot; audio operations have their own.
    pub fn is_audio(self) -> bool {
        matches!(self, Self::ConnectAudio | Self::DisconnectAudio)
    }
}

/// The single native operation a plan resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeOp {
    Dial(String),
    AcceptCall,
    EndCall,
    HoldCall,
    ReleaseHeldCalls,
    SendDtmf(Vec<char>),
    ConnectSco,
    DisconnectSco,
    TransferAudioToComputer,
    TransferAudioToPhone,
}

/// The observable effect that confirms an operation. The framework offers no completion callback
/// for these operations, so confirmation can only come from later indicators or SCO callbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// A call exists in setup or active state after a dial.
    DialStarted,
    Active(CallId),
    Held(CallId),
    Gone(CallId),
    ScoOpened,
    ScoClosed,
    /// No observable effect exists (DTMF). Resolved as "dispatched", never "confirmed".
    Unobservable,
}
impl Expect {
    /// Evaluate against the committed call picture and SCO state.
    pub fn satisfied(&self, slots: &Slots, sco_open: bool) -> bool {
        match self {
            Self::DialStarted => slots.live() > 0,
            Self::Active(id) => slots.kind_of(id) == Some(SlotKind::Active),
            Self::Held(id) => slots.kind_of(id) == Some(SlotKind::Held),
            Self::Gone(id) => slots.kind_of(id).is_none(),
            Self::ScoOpened => sco_open,
            Self::ScoClosed => !sco_open,
            Self::Unobservable => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub op: NativeOp,
    pub expect: Expect,
    /// A repeated attempt could place a second call or repeat a side effect; never auto-retried.
    pub not_idempotent: bool,
}

/// Why a command was refused before reaching the framework.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", content = "detail", rename_all = "snake_case")]
pub enum Refusal {
    /// No call indicator has been delivered yet, so "no calls" cannot be told from "unknown".
    CallStateUnknown,
    /// More than one call is live and the operation would act on an unidentified one.
    AmbiguousCalls,
    /// A previous transition could not be attributed to specific calls.
    IdentityUncertain,
    NoSuchCall,
    NotRinging,
    CallsAlreadyLive,
    NoActiveCall,
    NoHeldCall,
    /// Swapping an active and a held call produces no indicator change that identifies which is
    /// which afterwards, so the adapter will not do it.
    SwapNotObservable,
    /// The gateway did not report the capability (or reported no features at all).
    FeatureUnknown(&'static str),
    FeatureNotAdvertised(&'static str),
    InvalidArgument,
    AudioAlreadyConnected,
    AudioNotConnected,
    /// No valid public IOBluetooth operation corresponds to this request.
    NoPlatformMapping(&'static str),
}
impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CallStateUnknown => f.write_str("call state not yet reported by the phone"),
            Self::AmbiguousCalls => f.write_str("several calls are live; the target is ambiguous"),
            Self::IdentityUncertain => {
                f.write_str("call identities are uncertain after an unattributed transition")
            }
            Self::NoSuchCall => f.write_str("no such live call"),
            Self::NotRinging => f.write_str("the call is not ringing"),
            Self::CallsAlreadyLive => f.write_str("a call is already live"),
            Self::NoActiveCall => f.write_str("no active call"),
            Self::NoHeldCall => f.write_str("no held call"),
            Self::SwapNotObservable => {
                f.write_str("swapping active and held calls cannot be tracked via indicators")
            }
            Self::FeatureUnknown(w) => write!(f, "phone has not reported support for {w}"),
            Self::FeatureNotAdvertised(w) => write!(f, "phone does not advertise {w}"),
            Self::InvalidArgument => f.write_str("invalid argument"),
            Self::AudioAlreadyConnected => f.write_str("call audio is already connected"),
            Self::AudioNotConnected => f.write_str("call audio is not connected"),
            Self::NoPlatformMapping(w) => write!(f, "no public macOS API for {w}"),
        }
    }
}
impl std::error::Error for Refusal {}

/// Everything the planner may look at.
#[derive(Clone, Copy, Debug)]
pub struct PlanInput<'a> {
    pub slots: &'a Slots,
    pub identity_uncertain: bool,
    pub indicators_observed: bool,
    pub ag: AgFeatures,
    pub audio: AudioLinkState,
    pub last_audio_strategy: Option<AudioStrategy>,
}

fn need_three_way(ag: AgFeatures) -> Result<(), Refusal> {
    if !ag.reported() {
        Err(Refusal::FeatureUnknown("three-way calling"))
    } else if !ag.has(AgFeatures::THREE_WAY_CALLING) {
        Err(Refusal::FeatureNotAdvertised("three-way calling"))
    } else {
        Ok(())
    }
}

fn plan_of(op: NativeOp, expect: Expect) -> Plan {
    Plan {
        op,
        expect,
        not_idempotent: false,
    }
}

pub fn plan(cmd: &MacCommand, input: &PlanInput<'_>) -> Result<Plan, Refusal> {
    let s = input.slots;
    // Audio commands do not depend on the call picture.
    match cmd {
        MacCommand::ConnectAudio { strategy } => {
            if input.audio != AudioLinkState::Disconnected {
                return Err(Refusal::AudioAlreadyConnected);
            }
            let op = match strategy {
                AudioStrategy::TransferToComputer => NativeOp::TransferAudioToComputer,
                AudioStrategy::ConnectSco => NativeOp::ConnectSco,
            };
            return Ok(plan_of(op, Expect::ScoOpened));
        }
        MacCommand::DisconnectAudio => {
            // Only an established link can be taken down; a link still opening is neither
            // connected nor safely cancellable through the public API.
            if !matches!(input.audio, AudioLinkState::Connected { .. }) {
                return Err(Refusal::AudioNotConnected);
            }
            let op = match input.last_audio_strategy {
                Some(AudioStrategy::TransferToComputer) => NativeOp::TransferAudioToPhone,
                _ => NativeOp::DisconnectSco,
            };
            return Ok(plan_of(op, Expect::ScoClosed));
        }
        _ => {}
    }
    if !input.indicators_observed {
        return Err(Refusal::CallStateUnknown);
    }
    let multi = s.live() > 1;
    if multi && input.identity_uncertain && !matches!(cmd, MacCommand::SendTones { .. }) {
        return Err(Refusal::IdentityUncertain);
    }
    match cmd {
        MacCommand::Dial { number } => {
            let n = number.expose();
            if n.is_empty()
                || n.len() > MAX_DIAL_LEN
                || !n.chars().all(|c| "0123456789+*#".contains(c))
            {
                return Err(Refusal::InvalidArgument);
            }
            if s.live() > 0 {
                return Err(Refusal::CallsAlreadyLive);
            }
            Ok(Plan {
                not_idempotent: true,
                ..plan_of(NativeOp::Dial(n.to_owned()), Expect::DialStarted)
            })
        }
        MacCommand::Answer { call } => {
            let kind = s.kind_of(call).ok_or(Refusal::NoSuchCall)?;
            if kind != SlotKind::Setup(SetupKind::Incoming) {
                return Err(Refusal::NotRinging);
            }
            match (s.active.is_some(), s.held.is_some()) {
                (false, _) => Ok(plan_of(NativeOp::AcceptCall, Expect::Active(call.clone()))),
                (true, false) => {
                    need_three_way(input.ag)?;
                    // Waiting call: hold the active call and accept the waiting one.
                    Ok(plan_of(NativeOp::HoldCall, Expect::Active(call.clone())))
                }
                (true, true) => Err(Refusal::AmbiguousCalls),
            }
        }
        MacCommand::Hangup { call } => {
            let kind = s.kind_of(call).ok_or(Refusal::NoSuchCall)?;
            let gone = Expect::Gone(call.clone());
            match kind {
                SlotKind::Setup(SetupKind::Incoming) => {
                    match (s.active.is_some(), s.held.is_some()) {
                        (false, _) => Ok(plan_of(NativeOp::EndCall, gone)),
                        (true, false) => {
                            need_three_way(input.ag)?;
                            Ok(plan_of(NativeOp::ReleaseHeldCalls, gone))
                        }
                        (true, true) => Err(Refusal::AmbiguousCalls),
                    }
                }
                SlotKind::Setup(_) | SlotKind::Active => {
                    if s.live() == 1 {
                        Ok(plan_of(NativeOp::EndCall, gone))
                    } else {
                        Err(Refusal::AmbiguousCalls)
                    }
                }
                SlotKind::Held => {
                    if s.setup.is_some() {
                        return Err(Refusal::AmbiguousCalls);
                    }
                    need_three_way(input.ag)?;
                    Ok(plan_of(NativeOp::ReleaseHeldCalls, gone))
                }
            }
        }
        MacCommand::Hold => {
            let a = s.active.as_ref().ok_or(Refusal::NoActiveCall)?;
            if s.held.is_some() || s.setup.is_some() {
                // `holdCall` would also accept the other call; refuse rather than pick for the user.
                return Err(Refusal::AmbiguousCalls);
            }
            need_three_way(input.ag)?;
            Ok(plan_of(NativeOp::HoldCall, Expect::Held(a.id.clone())))
        }
        MacCommand::Resume => {
            let h = s.held.as_ref().ok_or(Refusal::NoHeldCall)?;
            if s.active.is_some() || s.setup.is_some() {
                return Err(Refusal::AmbiguousCalls);
            }
            need_three_way(input.ag)?;
            Ok(plan_of(NativeOp::HoldCall, Expect::Active(h.id.clone())))
        }
        MacCommand::Swap => {
            let h = s.held.as_ref().ok_or(Refusal::NoHeldCall)?;
            if s.active.is_some() {
                return Err(Refusal::SwapNotObservable);
            }
            if s.setup.is_some() {
                return Err(Refusal::AmbiguousCalls);
            }
            need_three_way(input.ag)?;
            Ok(plan_of(NativeOp::HoldCall, Expect::Active(h.id.clone())))
        }
        MacCommand::SendTones { tones } => {
            let t = tones.expose();
            if t.is_empty()
                || t.len() > MAX_DIAL_LEN
                || !t.chars().all(|c| "0123456789*#ABCD".contains(c))
            {
                return Err(Refusal::InvalidArgument);
            }
            if s.active.is_none() {
                return Err(Refusal::NoActiveCall);
            }
            Ok(plan_of(
                NativeOp::SendDtmf(t.chars().collect()),
                Expect::Unobservable,
            ))
        }
        MacCommand::ConnectAudio { .. } | MacCommand::DisconnectAudio => {
            unreachable!("handled above")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Indicator;
    use crate::model::Millis;
    use crate::tracker::CallTracker;

    const FEATURES_3WAY: AgFeatures = AgFeatures(AgFeatures::THREE_WAY_CALLING);

    fn tracker(script: &[Indicator]) -> CallTracker {
        let mut tr = CallTracker::new("m");
        for (i, ind) in script.iter().enumerate() {
            tr.on_indicator(*ind, Millis(i as u64));
        }
        tr.tick(Millis(10_000));
        tr
    }
    fn input<'a>(tr: &'a CallTracker, ag: AgFeatures) -> PlanInput<'a> {
        PlanInput {
            slots: tr.slots(),
            identity_uncertain: tr.identity_uncertain(),
            indicators_observed: tr.indicators_observed(),
            ag,
            audio: AudioLinkState::Disconnected,
            last_audio_strategy: None,
        }
    }
    fn id(s: &str) -> CallId {
        CallId::new(s)
    }

    #[test]
    fn refuses_call_control_before_any_indicator() {
        let tr = tracker(&[]);
        let r = plan(
            &MacCommand::Dial {
                number: Sensitive::new("123"),
            },
            &input(&tr, FEATURES_3WAY),
        );
        assert_eq!(r, Err(Refusal::CallStateUnknown));
    }

    #[test]
    fn dial_when_idle_is_not_idempotent_and_validates() {
        let tr = tracker(&[Indicator::CallSetup(0)]);
        let p = plan(
            &MacCommand::Dial {
                number: Sensitive::new("+15551234"),
            },
            &input(&tr, AgFeatures(0)),
        )
        .unwrap();
        assert_eq!(p.op, NativeOp::Dial("+15551234".into()));
        assert!(p.not_idempotent);
        assert_eq!(
            plan(
                &MacCommand::Dial {
                    number: Sensitive::new("12a")
                },
                &input(&tr, AgFeatures(0))
            ),
            Err(Refusal::InvalidArgument)
        );
        assert_eq!(
            plan(
                &MacCommand::Dial {
                    number: Sensitive::new("")
                },
                &input(&tr, AgFeatures(0))
            ),
            Err(Refusal::InvalidArgument)
        );
    }

    #[test]
    fn dial_refused_when_a_call_is_live() {
        let tr = tracker(&[Indicator::Call(true)]);
        assert_eq!(
            plan(
                &MacCommand::Dial {
                    number: Sensitive::new("1")
                },
                &input(&tr, FEATURES_3WAY)
            ),
            Err(Refusal::CallsAlreadyLive)
        );
    }

    #[test]
    fn single_incoming_accept_and_reject_need_no_three_way() {
        let tr = tracker(&[Indicator::CallSetup(1)]);
        let i = input(&tr, AgFeatures(0));
        let a = plan(&MacCommand::Answer { call: id("m-c1") }, &i).unwrap();
        assert_eq!(a.op, NativeOp::AcceptCall);
        assert_eq!(a.expect, Expect::Active(id("m-c1")));
        let r = plan(&MacCommand::Hangup { call: id("m-c1") }, &i).unwrap();
        assert_eq!(r.op, NativeOp::EndCall);
        assert_eq!(r.expect, Expect::Gone(id("m-c1")));
    }

    #[test]
    fn answering_unknown_or_non_ringing_call_is_refused() {
        let tr = tracker(&[Indicator::Call(true)]);
        let i = input(&tr, FEATURES_3WAY);
        assert_eq!(
            plan(&MacCommand::Answer { call: id("nope") }, &i),
            Err(Refusal::NoSuchCall)
        );
        assert_eq!(
            plan(&MacCommand::Answer { call: id("m-c1") }, &i),
            Err(Refusal::NotRinging)
        );
    }

    #[test]
    fn waiting_call_answer_holds_active_and_needs_three_way() {
        let tr = tracker(&[Indicator::Call(true), Indicator::CallSetup(1)]);
        let waiting = id("m-c2");
        assert_eq!(
            plan(
                &MacCommand::Answer {
                    call: waiting.clone()
                },
                &input(&tr, AgFeatures(0))
            ),
            Err(Refusal::FeatureUnknown("three-way calling"))
        );
        assert_eq!(
            plan(
                &MacCommand::Answer {
                    call: waiting.clone()
                },
                &input(&tr, AgFeatures(AgFeatures::EC_NR))
            ),
            Err(Refusal::FeatureNotAdvertised("three-way calling"))
        );
        let p = plan(
            &MacCommand::Answer {
                call: waiting.clone(),
            },
            &input(&tr, FEATURES_3WAY),
        )
        .unwrap();
        assert_eq!(p.op, NativeOp::HoldCall);
        assert_eq!(p.expect, Expect::Active(waiting.clone()));
        // Rejecting the waiting call releases it without touching the active call.
        let r = plan(
            &MacCommand::Hangup {
                call: waiting.clone(),
            },
            &input(&tr, FEATURES_3WAY),
        )
        .unwrap();
        assert_eq!(r.op, NativeOp::ReleaseHeldCalls);
    }

    #[test]
    fn hangup_with_several_calls_refuses_active_but_can_release_held() {
        // c1 held, c2 active.
        let tr = tracker(&[Indicator::Call(true), Indicator::CallHeld(1)]);
        let i = input(&tr, FEATURES_3WAY);
        assert_eq!(
            plan(&MacCommand::Hangup { call: id("m-c2") }, &i),
            Err(Refusal::AmbiguousCalls)
        );
        let p = plan(&MacCommand::Hangup { call: id("m-c1") }, &i).unwrap();
        assert_eq!(p.op, NativeOp::ReleaseHeldCalls);
    }

    #[test]
    fn hangup_sole_active_and_sole_dialing_use_end_call() {
        let tr = tracker(&[Indicator::Call(true)]);
        assert_eq!(
            plan(
                &MacCommand::Hangup { call: id("m-c1") },
                &input(&tr, AgFeatures(0))
            )
            .unwrap()
            .op,
            NativeOp::EndCall
        );
        let tr = tracker(&[Indicator::CallSetup(2)]);
        assert_eq!(
            plan(
                &MacCommand::Hangup { call: id("m-c1") },
                &input(&tr, AgFeatures(0))
            )
            .unwrap()
            .op,
            NativeOp::EndCall
        );
    }

    #[test]
    fn hold_resume_swap_semantics() {
        let single = tracker(&[Indicator::Call(true)]);
        let p = plan(&MacCommand::Hold, &input(&single, FEATURES_3WAY)).unwrap();
        assert_eq!(
            (p.op, p.expect),
            (NativeOp::HoldCall, Expect::Held(id("m-c1")))
        );
        assert_eq!(
            plan(&MacCommand::Hold, &input(&single, AgFeatures(0))),
            Err(Refusal::FeatureUnknown("three-way calling"))
        );
        assert_eq!(
            plan(&MacCommand::Resume, &input(&single, FEATURES_3WAY)),
            Err(Refusal::NoHeldCall)
        );

        let held_only = tracker(&[Indicator::Call(true), Indicator::CallHeld(2)]);
        let r = plan(&MacCommand::Resume, &input(&held_only, FEATURES_3WAY)).unwrap();
        assert_eq!(r.expect, Expect::Active(id("m-c1")));
        // Core "swap" with only a held call is a resume.
        let s = plan(&MacCommand::Swap, &input(&held_only, FEATURES_3WAY)).unwrap();
        assert_eq!(s.op, NativeOp::HoldCall);
        assert_eq!(
            plan(&MacCommand::Hold, &input(&held_only, FEATURES_3WAY)),
            Err(Refusal::NoActiveCall)
        );

        let both = tracker(&[Indicator::Call(true), Indicator::CallHeld(1)]);
        assert_eq!(
            plan(&MacCommand::Swap, &input(&both, FEATURES_3WAY)),
            Err(Refusal::SwapNotObservable)
        );
        assert_eq!(
            plan(&MacCommand::Hold, &input(&both, FEATURES_3WAY)),
            Err(Refusal::AmbiguousCalls)
        );
        assert_eq!(
            plan(&MacCommand::Resume, &input(&both, FEATURES_3WAY)),
            Err(Refusal::AmbiguousCalls)
        );
    }

    #[test]
    fn uncertain_identity_refuses_everything_but_tones() {
        let mut tr = CallTracker::new("m");
        for (i, ind) in [
            Indicator::Call(true),
            Indicator::CallHeld(1),
            Indicator::CallHeld(1),
        ]
        .iter()
        .enumerate()
        {
            tr.on_indicator(*ind, Millis(i as u64));
        }
        assert!(tr.identity_uncertain());
        let i = input(&tr, FEATURES_3WAY);
        assert_eq!(
            plan(&MacCommand::Hangup { call: id("m-c1") }, &i),
            Err(Refusal::IdentityUncertain)
        );
        assert_eq!(plan(&MacCommand::Hold, &i), Err(Refusal::IdentityUncertain));
        assert!(
            plan(
                &MacCommand::SendTones {
                    tones: Sensitive::new("1#")
                },
                &i
            )
            .is_ok()
        );
    }

    #[test]
    fn tones_need_an_active_call_and_valid_characters() {
        let held = tracker(&[Indicator::Call(true), Indicator::CallHeld(2)]);
        assert_eq!(
            plan(
                &MacCommand::SendTones {
                    tones: Sensitive::new("1")
                },
                &input(&held, FEATURES_3WAY)
            ),
            Err(Refusal::NoActiveCall)
        );
        let active = tracker(&[Indicator::Call(true)]);
        let ok = plan(
            &MacCommand::SendTones {
                tones: Sensitive::new("12*#AD"),
            },
            &input(&active, AgFeatures(0)),
        )
        .unwrap();
        assert_eq!(ok.expect, Expect::Unobservable);
        assert_eq!(
            plan(
                &MacCommand::SendTones {
                    tones: Sensitive::new("1x")
                },
                &input(&active, AgFeatures(0))
            ),
            Err(Refusal::InvalidArgument)
        );
        assert_eq!(
            plan(
                &MacCommand::SendTones {
                    tones: Sensitive::new("1".repeat(33))
                },
                &input(&active, AgFeatures(0))
            ),
            Err(Refusal::InvalidArgument)
        );
    }

    #[test]
    fn audio_plans_follow_strategy_and_state() {
        let tr = tracker(&[]);
        let mut i = input(&tr, AgFeatures(0));
        let c = plan(
            &MacCommand::ConnectAudio {
                strategy: AudioStrategy::default(),
            },
            &i,
        )
        .unwrap();
        assert_eq!(
            (c.op, c.expect),
            (NativeOp::TransferAudioToComputer, Expect::ScoOpened)
        );
        let c = plan(
            &MacCommand::ConnectAudio {
                strategy: AudioStrategy::ConnectSco,
            },
            &i,
        )
        .unwrap();
        assert_eq!(c.op, NativeOp::ConnectSco);
        assert_eq!(
            plan(&MacCommand::DisconnectAudio, &i),
            Err(Refusal::AudioNotConnected)
        );
        i.audio = AudioLinkState::Connected {
            codec: bridge_core::calls::AudioCodec::Unknown,
        };
        assert_eq!(
            plan(
                &MacCommand::ConnectAudio {
                    strategy: AudioStrategy::default()
                },
                &i
            ),
            Err(Refusal::AudioAlreadyConnected)
        );
        i.last_audio_strategy = Some(AudioStrategy::TransferToComputer);
        assert_eq!(
            plan(&MacCommand::DisconnectAudio, &i).unwrap().op,
            NativeOp::TransferAudioToPhone
        );
        i.last_audio_strategy = Some(AudioStrategy::ConnectSco);
        assert_eq!(
            plan(&MacCommand::DisconnectAudio, &i).unwrap().op,
            NativeOp::DisconnectSco
        );
    }

    #[test]
    fn core_mute_commands_have_no_mapping() {
        assert_eq!(
            MacCommand::from_core(&CallCommand::MuteRinger),
            Err(Refusal::NoPlatformMapping("mute_ringer"))
        );
        assert_eq!(
            MacCommand::from_core(&CallCommand::SetMicrophoneMute { muted: true }),
            Err(Refusal::NoPlatformMapping("mute_microphone"))
        );
        assert_eq!(
            MacCommand::from_core(&CallCommand::SwapCalls),
            Ok(MacCommand::Swap)
        );
    }

    #[test]
    fn expectations_evaluate_against_slots() {
        let tr = tracker(&[Indicator::Call(true)]);
        let s = tr.slots();
        assert!(Expect::Active(id("m-c1")).satisfied(s, false));
        assert!(!Expect::Held(id("m-c1")).satisfied(s, false));
        assert!(!Expect::Gone(id("m-c1")).satisfied(s, false));
        assert!(Expect::Gone(id("zzz")).satisfied(s, false));
        assert!(Expect::ScoOpened.satisfied(s, true));
        assert!(Expect::ScoClosed.satisfied(s, false));
    }
}
