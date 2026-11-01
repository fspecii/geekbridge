//! Call control over the paired GeekBridge/KDE link (`geekbridge.calls.*` packets).
//!
//! This is a complement to the Bluetooth HFP backends, not a replacement. It carries **signalling
//! only**: what the phone's telephony stack is doing and requests to answer, decline or end the
//! call. **Call audio never travels on this link.** It still flows over the phone's Bluetooth HFP
//! connection, and only when this computer is the paired hands-free device. A call answered through
//! this link while no HFP audio link exists therefore stays audible on the phone only; callers
//! surface that as "answered on the phone" ([`crate::calls_view`]).
//!
//! | packet | direction | body |
//! | --- | --- | --- |
//! | [`STATE`] | phone to computer | `state` (`idle`, `ringing`, `offhook`), optional `number`/`name`, `canAnswer`, `canEnd` |
//! | [`REQUEST`] | computer to phone | `requestId`, `action` (`answer`, `reject`, `hangup`) |
//! | [`RESULT`] | phone to computer | `requestId`, `ok`, optional `reason` |
//!
//! The phone decides what it may do: `canAnswer` and `canEnd` are true only while Android's
//! `ANSWER_PHONE_CALLS` permission is granted, and `number`/`name` are present only if the user also
//! allowed reading the caller. Everything parsed here is bounded and strict; a malformed packet is an
//! error, never a guess.
use crate::calls::{
    Applied, CallBackend, CallCapabilities, CallDirection, CallEvent, CallFeature, CallId,
    CallParty, CallPhase, CallState, EndReason, MAX_FIELD_LEN, Sensitive, Support,
    UnsupportedReason,
};
use serde_json::{Map, Value, json};

pub const STATE: &str = "geekbridge.calls.state";
pub const REQUEST: &str = "geekbridge.calls.request";
pub const RESULT: &str = "geekbridge.calls.result";

/// Call id used for the one call the phone reports over this link.
pub const LINK_CALL_ID: &str = "link-current";
/// Longest `requestId` accepted (matches the clipboard request ids).
pub const MAX_REQUEST_ID_BYTES: usize = 64;

pub fn is_calls_kind(kind: &str) -> bool {
    matches!(kind, STATE | REQUEST | RESULT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// A required field is missing or has the wrong type.
    Malformed,
    /// A field is too long, empty or has an unknown value.
    Invalid,
}

pub fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_REQUEST_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

// ---------------------------------------------------------------- phone state

/// Android's coarse telephony state, as reported by the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhoneState {
    Idle,
    Ringing,
    OffHook,
}
impl PhoneState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Ringing => "ringing",
            Self::OffHook => "offhook",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "idle" => Self::Idle,
            "ringing" => Self::Ringing,
            "offhook" => Self::OffHook,
            _ => return None,
        })
    }
}

/// One `geekbridge.calls.state` report.
#[derive(Clone, PartialEq, Eq)]
pub struct LinkCallReport {
    pub state: PhoneState,
    pub number: Option<Sensitive>,
    pub name: Option<Sensitive>,
    /// The phone can accept a ringing call (Android permission granted).
    pub can_answer: bool,
    /// The phone can decline a ringing call or end the current one.
    pub can_end: bool,
}
impl std::fmt::Debug for LinkCallReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkCallReport")
            .field("state", &self.state)
            .field("number", &self.number.as_ref().map(|_| "<redacted>"))
            .field("name", &self.name.as_ref().map(|_| "<redacted>"))
            .field("can_answer", &self.can_answer)
            .field("can_end", &self.can_end)
            .finish()
    }
}

fn opt_text(body: &Map<String, Value>, key: &str) -> Result<Option<Sensitive>, WireError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s))
            if s.len() <= MAX_FIELD_LEN && !s.chars().any(|c| c.is_control()) =>
        {
            Ok(Some(Sensitive::new(s.clone())))
        }
        Some(Value::String(_)) => Err(WireError::Invalid),
        Some(_) => Err(WireError::Malformed),
    }
}
fn opt_bool(body: &Map<String, Value>, key: &str) -> Result<bool, WireError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(WireError::Malformed),
    }
}

impl LinkCallReport {
    pub fn idle(can_answer: bool, can_end: bool) -> Self {
        Self {
            state: PhoneState::Idle,
            number: None,
            name: None,
            can_answer,
            can_end,
        }
    }

    pub fn body(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("state".into(), json!(self.state.as_str()));
        if let Some(n) = &self.number {
            m.insert("number".into(), json!(n.expose()));
        }
        if let Some(n) = &self.name {
            m.insert("name".into(), json!(n.expose()));
        }
        m.insert("canAnswer".into(), json!(self.can_answer));
        m.insert("canEnd".into(), json!(self.can_end));
        m
    }

    pub fn parse(body: &Map<String, Value>) -> Result<Self, WireError> {
        let state = body
            .get("state")
            .and_then(Value::as_str)
            .ok_or(WireError::Malformed)?;
        let state = PhoneState::parse(state).ok_or(WireError::Invalid)?;
        let mut report = Self {
            state,
            number: opt_text(body, "number")?,
            name: opt_text(body, "name")?,
            can_answer: opt_bool(body, "canAnswer")?,
            can_end: opt_bool(body, "canEnd")?,
        };
        if state == PhoneState::Idle {
            // An idle phone has no party; never keep a stale number around.
            report.number = None;
            report.name = None;
        }
        Ok(report)
    }

    /// Declared capabilities for this phone. Observation is declared once a report arrived; control
    /// is declared only for what the phone said it may do. Audio and dialling are not in this
    /// protocol.
    pub fn capabilities(&self) -> CallCapabilities {
        let mut caps = CallCapabilities::unsupported(
            CallBackend::GeekBridgeLink,
            UnsupportedReason::NotInProtocol,
        );
        caps.set(CallFeature::CallStateEvents, Support::Declared);
        // Whether the number is shown depends on what the user allowed on the phone; the party is
        // simply absent otherwise, so the feature is declared only after one was seen.
        if self.number.is_some() || self.name.is_some() {
            caps.set(CallFeature::CallerIdentity, Support::Declared);
        }
        let allowed = |ok: bool| {
            if ok {
                Support::Declared
            } else {
                Support::Unsupported {
                    reason: UnsupportedReason::RemoteDoesNotAdvertise,
                }
            }
        };
        caps.set(CallFeature::Answer, allowed(self.can_answer));
        caps.set(CallFeature::Hangup, allowed(self.can_end));
        caps
    }

    /// Fold this report into `state` as the matching [`CallEvent`]s. Android reports one current
    /// call (no hold, no waiting call), under [`LINK_CALL_ID`].
    pub fn apply(&self, state: &mut CallState) -> Applied {
        use crate::calls::LinkState;
        let id = CallId::new(LINK_CALL_ID);
        let mut result = state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        let party = CallParty {
            number: self.number.clone(),
            name: self.name.clone(),
        };
        let event = match self.state {
            PhoneState::Idle => {
                if !state.calls.contains_key(&id) {
                    return result;
                }
                CallEvent::CallEnded {
                    id,
                    reason: EndReason::Unspecified,
                }
            }
            PhoneState::Ringing => CallEvent::CallUpdated {
                id,
                direction: CallDirection::Incoming,
                phase: CallPhase::Incoming,
                party,
            },
            PhoneState::OffHook => CallEvent::CallUpdated {
                id,
                direction: CallDirection::Unknown,
                phase: CallPhase::Active,
                party,
            },
        };
        match state.apply(event) {
            Applied::Unchanged => result,
            other => {
                result = other;
                result
            }
        }
    }
}

// ---------------------------------------------------------------- requests

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
    Answer,
    Reject,
    Hangup,
}
impl LinkAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Reject => "reject",
            Self::Hangup => "hangup",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "answer" => Self::Answer,
            "reject" => Self::Reject,
            "hangup" => Self::Hangup,
            _ => return None,
        })
    }
}

pub fn request_body(request_id: &str, action: LinkAction) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("requestId".into(), json!(request_id));
    m.insert("action".into(), json!(action.as_str()));
    m
}

pub fn parse_request(body: &Map<String, Value>) -> Result<(String, LinkAction), WireError> {
    let id = body
        .get("requestId")
        .and_then(Value::as_str)
        .ok_or(WireError::Malformed)?;
    if !valid_request_id(id) {
        return Err(WireError::Invalid);
    }
    let action = body
        .get("action")
        .and_then(Value::as_str)
        .ok_or(WireError::Malformed)?;
    Ok((
        id.to_owned(),
        LinkAction::parse(action).ok_or(WireError::Invalid)?,
    ))
}

// ---------------------------------------------------------------- results

/// Why the phone did not carry out a request. Fixed vocabulary, never free text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkFailure {
    /// `ANSWER_PHONE_CALLS` is not granted on the phone.
    PermissionMissing,
    /// There was no ringing (answer, reject) or live (hangup) call.
    NoSuchCall,
    /// Android refused or the call changed in between.
    Failed,
}
impl LinkFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PermissionMissing => "permission_missing",
            Self::NoSuchCall => "no_such_call",
            Self::Failed => "failed",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "permission_missing" => Self::PermissionMissing,
            "no_such_call" => Self::NoSuchCall,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::PermissionMissing => "The phone has not allowed GeekBridge to answer calls.",
            Self::NoSuchCall => "The call had already changed on the phone.",
            Self::Failed => "The phone could not do that.",
        }
    }
}

pub fn result_body(request_id: &str, outcome: Result<(), LinkFailure>) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("requestId".into(), json!(request_id));
    m.insert("ok".into(), json!(outcome.is_ok()));
    if let Err(reason) = outcome {
        m.insert("reason".into(), json!(reason.as_str()));
    }
    m
}

pub fn parse_result(
    body: &Map<String, Value>,
) -> Result<(String, Result<(), LinkFailure>), WireError> {
    let id = body
        .get("requestId")
        .and_then(Value::as_str)
        .ok_or(WireError::Malformed)?;
    if !valid_request_id(id) {
        return Err(WireError::Invalid);
    }
    let ok = body
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or(WireError::Malformed)?;
    if ok {
        return Ok((id.to_owned(), Ok(())));
    }
    let reason = match body.get("reason").and_then(Value::as_str) {
        Some(r) => LinkFailure::parse(r).unwrap_or(LinkFailure::Failed),
        None => LinkFailure::Failed,
    };
    Ok((id.to_owned(), Err(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calls::CommandRejection;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn report_round_trips_and_idle_forgets_the_party() {
        let r = LinkCallReport {
            state: PhoneState::Ringing,
            number: Some(Sensitive::new("+15550100")),
            name: Some(Sensitive::new("Ada")),
            can_answer: true,
            can_end: true,
        };
        assert_eq!(LinkCallReport::parse(&r.body()), Ok(r.clone()));
        assert!(!format!("{r:?}").contains("5550100"));
        let idle = LinkCallReport::parse(&obj(
            json!({"state":"idle","number":"123","canAnswer":true}),
        ))
        .unwrap();
        assert_eq!(idle.number, None);
    }

    #[test]
    fn malformed_reports_are_errors_not_guesses() {
        for bad in [
            json!({}),
            json!({"state": 3}),
            json!({"state":"dialing"}),
            json!({"state":"ringing","number": 5}),
            json!({"state":"ringing","number":"a".repeat(MAX_FIELD_LEN + 1)}),
            json!({"state":"ringing","name":"x\u{7}y"}),
            json!({"state":"ringing","canAnswer":"yes"}),
        ] {
            assert!(LinkCallReport::parse(&obj(bad.clone())).is_err(), "{bad}");
        }
    }

    #[test]
    fn capabilities_follow_what_the_phone_may_do() {
        let none = LinkCallReport::idle(false, false).capabilities();
        assert_eq!(
            none.support(CallFeature::CallStateEvents),
            Support::Declared
        );
        assert!(!none.support(CallFeature::Answer).may_attempt());
        assert!(!none.support(CallFeature::Hangup).may_attempt());
        let full = LinkCallReport::idle(true, true).capabilities();
        assert_eq!(full.support(CallFeature::Answer), Support::Declared);
        assert_eq!(full.support(CallFeature::Hangup), Support::Declared);
        for f in [
            CallFeature::Dial,
            CallFeature::AudioDownlink,
            CallFeature::AudioUplink,
            CallFeature::MuteMicrophone,
            CallFeature::MuteRinger,
        ] {
            assert!(!full.support(f).may_attempt(), "{f:?}");
        }
    }

    #[test]
    fn ring_answer_idle_sequence_drives_the_reducer() {
        let mut s = CallState::default();
        let ring = LinkCallReport {
            state: PhoneState::Ringing,
            number: Some(Sensitive::new("555")),
            name: None,
            can_answer: true,
            can_end: true,
        };
        assert_eq!(ring.apply(&mut s), Applied::Changed);
        assert_eq!(ring.apply(&mut s), Applied::Unchanged);
        let id = CallId::new(LINK_CALL_ID);
        assert!(s.calls[&id].phase.is_ringing());
        let caps = ring.capabilities();
        assert_eq!(
            s.check_command(
                &caps,
                &crate::calls::CallCommand::Answer { call: id.clone() }
            ),
            Ok(())
        );
        let talking = LinkCallReport {
            state: PhoneState::OffHook,
            number: None,
            name: Some(Sensitive::new("Ada")),
            ..ring.clone()
        };
        assert_eq!(talking.apply(&mut s), Applied::Changed);
        assert_eq!(s.calls[&id].phase, CallPhase::Active);
        assert_eq!(s.calls[&id].direction, CallDirection::Incoming);
        // The number learnt while ringing survives the number-less offhook report.
        assert!(s.calls[&id].party.number.is_some());
        assert_eq!(
            s.check_command(
                &caps,
                &crate::calls::CallCommand::Reject { call: id.clone() }
            ),
            Err(CommandRejection::InvalidState)
        );
        LinkCallReport::idle(true, true).apply(&mut s);
        assert!(s.calls.is_empty());
        assert_eq!(s.recent_ended.back().unwrap().id, id);
    }

    #[test]
    fn declined_ring_is_not_answered() {
        let mut s = CallState::default();
        let mut r = LinkCallReport::idle(true, true);
        r.state = PhoneState::Ringing;
        r.apply(&mut s);
        LinkCallReport::idle(true, true).apply(&mut s);
        assert_eq!(
            s.recent_ended.back().unwrap().reason,
            EndReason::NotAnswered
        );
    }

    #[test]
    fn requests_and_results_round_trip_and_reject_bad_ids() {
        for a in [LinkAction::Answer, LinkAction::Reject, LinkAction::Hangup] {
            assert_eq!(
                parse_request(&request_body("r-1", a)),
                Ok(("r-1".to_owned(), a))
            );
        }
        assert!(parse_request(&request_body("bad id!", LinkAction::Answer)).is_err());
        assert!(parse_request(&obj(json!({"requestId":"a","action":"dial"}))).is_err());
        assert_eq!(
            parse_result(&result_body("r-1", Ok(()))),
            Ok(("r-1".into(), Ok(())))
        );
        assert_eq!(
            parse_result(&result_body("r-1", Err(LinkFailure::PermissionMissing))),
            Ok(("r-1".into(), Err(LinkFailure::PermissionMissing)))
        );
        // An unknown reason degrades to the generic failure.
        assert_eq!(
            parse_result(&obj(json!({"requestId":"a","ok":false,"reason":"zzz"}))),
            Ok(("a".into(), Err(LinkFailure::Failed)))
        );
    }
}
