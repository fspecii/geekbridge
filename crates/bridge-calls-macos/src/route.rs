//! Explicit, reversible call-audio routing through CoreAudio default devices.
//!
//! macOS decides whether HFP voice appears as a CoreAudio device at all; this crate cannot
//! make it appear and does not tap or inject PCM. The only thing it can do on top of
//! `transferAudioToComputer`/`connectSCO` is move the **system default** input/output to endpoints
//! the caller named. That affects every application, so:
//!
//! * nothing is ever changed unless the caller passes a [`RouteRequest`] naming both endpoints
//!   and a session id (there is no "pick the Bluetooth device for me");
//! * only changes this manager actually made are *owned*; a direction that already pointed at the
//!   requested device is left alone and never "restored";
//! * [`RouteManager::restore`] sets the previous default back only if the current default is still
//!   the endpoint we set — if the user or another app changed it since, we leave it alone;
//! * a failure halfway through [`RouteManager::apply`] rolls back what was just changed.
//!
//! None of this establishes that audio flows. See [`AudioProof`].
use crate::port::{AudioDevice, AudioError, AudioPort};
use serde::{Deserialize, Serialize};

/// Evidence ladder for call audio. The crate stops at [`AudioProof::RouteApplied`].
/// There is deliberately no "duplex verified" level: that needs a human or a loopback test on
/// real hardware (record from the Bluetooth input, play to its output, compare), which no code in
/// this crate performs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioProof {
    #[default]
    None,
    /// `scoConnectionOpened` with success status, or `transferAudio*` issued and SCO reported.
    ScoLinkReported,
    /// A duplex Bluetooth CoreAudio device is visible after the SCO link opened.
    RouteDeviceVisible,
    /// Defaults were moved to caller-chosen endpoints. Still no proof that audio is audible.
    RouteApplied,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteRequest {
    /// Caller-chosen id (the call session); required so ownership is attributable.
    pub session: String,
    pub input_uid: String,
    pub output_uid: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteError {
    EmptyRequest,
    /// A route owned by another session is active; restore it first.
    Busy {
        owner: String,
    },
    AlreadyApplied,
    EndpointMissing {
        which: Direction,
    },
    /// The named endpoint exists but has no channels in the needed direction.
    WrongDirection {
        which: Direction,
    },
    Audio {
        error: AudioError,
    },
    NothingToRestore,
}
impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for RouteError {}
impl From<AudioError> for RouteError {
    fn from(error: AudioError) -> Self {
        Self::Audio { error }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Input,
    Output,
}

/// One default-device change this manager made and therefore may undo.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Owned {
    set_uid: String,
    previous_uid: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Applied {
    session: String,
    input: Option<Owned>,
    output: Option<Owned>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreOutcome {
    Restored,
    /// The default is no longer the endpoint we set; it was changed by someone else.
    SkippedChangedExternally,
    /// The device that used to be the default has disappeared.
    SkippedPreviousGone,
    /// This direction was never changed by us.
    NotOwned,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub session: String,
    pub input: RestoreOutcome,
    pub output: RestoreOutcome,
}
impl RestoreReport {
    pub fn complete(&self) -> bool {
        self.input != RestoreOutcome::Failed && self.output != RestoreOutcome::Failed
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteReport {
    pub session: String,
    pub input_changed: bool,
    pub output_changed: bool,
}

pub struct RouteManager<A: AudioPort> {
    port: A,
    applied: Option<Applied>,
}

impl<A: AudioPort> RouteManager<A> {
    pub fn new(port: A) -> Self {
        Self {
            port,
            applied: None,
        }
    }
    pub fn port_mut(&mut self) -> &mut A {
        &mut self.port
    }
    pub fn owner(&self) -> Option<&str> {
        self.applied.as_ref().map(|a| a.session.as_str())
    }
    pub fn has_owned_changes(&self) -> bool {
        self.applied
            .as_ref()
            .is_some_and(|a| a.input.is_some() || a.output.is_some())
    }

    /// Read-only listing for the caller's endpoint choice.
    pub fn devices(&mut self) -> Result<Vec<AudioDevice>, RouteError> {
        Ok(self.port.devices()?)
    }

    pub fn apply(&mut self, req: &RouteRequest) -> Result<RouteReport, RouteError> {
        if req.session.is_empty() || req.input_uid.is_empty() || req.output_uid.is_empty() {
            return Err(RouteError::EmptyRequest);
        }
        if let Some(a) = &self.applied {
            return Err(if a.session == req.session {
                RouteError::AlreadyApplied
            } else {
                RouteError::Busy {
                    owner: a.session.clone(),
                }
            });
        }
        let devices = self.port.devices()?;
        let find = |uid: &str| devices.iter().find(|d| d.uid == uid);
        match find(&req.input_uid) {
            None => {
                return Err(RouteError::EndpointMissing {
                    which: Direction::Input,
                });
            }
            Some(d) if !d.has_input() => {
                return Err(RouteError::WrongDirection {
                    which: Direction::Input,
                });
            }
            Some(_) => {}
        }
        match find(&req.output_uid) {
            None => {
                return Err(RouteError::EndpointMissing {
                    which: Direction::Output,
                });
            }
            Some(d) if !d.has_output() => {
                return Err(RouteError::WrongDirection {
                    which: Direction::Output,
                });
            }
            Some(_) => {}
        }
        let prev_in = self.port.default_input()?;
        let prev_out = self.port.default_output()?;

        let mut input = None;
        if prev_in.as_deref() != Some(req.input_uid.as_str()) {
            self.port.set_default_input(&req.input_uid)?;
            input = Some(Owned {
                set_uid: req.input_uid.clone(),
                previous_uid: prev_in,
            });
        }
        let mut output = None;
        if prev_out.as_deref() != Some(req.output_uid.as_str()) {
            if let Err(e) = self.port.set_default_output(&req.output_uid) {
                // Undo the input change made a moment ago; best effort, reported via the error.
                if let Some(prev) = input.as_ref().and_then(|o| o.previous_uid.as_ref()) {
                    let _ = self.port.set_default_input(prev);
                }
                return Err(e.into());
            }
            output = Some(Owned {
                set_uid: req.output_uid.clone(),
                previous_uid: prev_out,
            });
        }
        let report = RouteReport {
            session: req.session.clone(),
            input_changed: input.is_some(),
            output_changed: output.is_some(),
        };
        self.applied = Some(Applied {
            session: req.session.clone(),
            input,
            output,
        });
        Ok(report)
    }

    /// Undo the route this manager owns, if any. On a `Failed` direction the ownership record is
    /// kept so a later call can retry; directions that completed are dropped from it.
    pub fn restore(&mut self) -> Result<RestoreReport, RouteError> {
        let Some(mut applied) = self.applied.take() else {
            return Err(RouteError::NothingToRestore);
        };
        let devices = self.port.devices().unwrap_or_default();
        let output = Self::restore_one(
            &mut self.port,
            applied.output.as_ref(),
            &devices,
            Direction::Output,
        );
        let input = Self::restore_one(
            &mut self.port,
            applied.input.as_ref(),
            &devices,
            Direction::Input,
        );
        if output != RestoreOutcome::Failed {
            applied.output = None;
        }
        if input != RestoreOutcome::Failed {
            applied.input = None;
        }
        let report = RestoreReport {
            session: applied.session.clone(),
            input,
            output,
        };
        if applied.input.is_some() || applied.output.is_some() {
            self.applied = Some(applied);
        }
        Ok(report)
    }

    fn restore_one(
        port: &mut A,
        owned: Option<&Owned>,
        devices: &[AudioDevice],
        dir: Direction,
    ) -> RestoreOutcome {
        let Some(owned) = owned else {
            return RestoreOutcome::NotOwned;
        };
        let current = match dir {
            Direction::Input => port.default_input(),
            Direction::Output => port.default_output(),
        };
        match current {
            Err(_) => return RestoreOutcome::Failed,
            Ok(cur) if cur.as_deref() != Some(owned.set_uid.as_str()) => {
                return RestoreOutcome::SkippedChangedExternally;
            }
            Ok(_) => {}
        }
        let Some(prev) = &owned.previous_uid else {
            return RestoreOutcome::SkippedPreviousGone;
        };
        if !devices.iter().any(|d| &d.uid == prev) {
            return RestoreOutcome::SkippedPreviousGone;
        }
        let result = match dir {
            Direction::Input => port.set_default_input(prev),
            Direction::Output => port.set_default_output(prev),
        };
        if result.is_ok() {
            RestoreOutcome::Restored
        } else {
            RestoreOutcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockAudio;

    fn req(session: &str, i: &str, o: &str) -> RouteRequest {
        RouteRequest {
            session: session.into(),
            input_uid: i.into(),
            output_uid: o.into(),
        }
    }
    fn manager() -> RouteManager<MockAudio> {
        RouteManager::new(MockAudio::standard())
    }

    #[test]
    fn nothing_changes_without_an_explicit_request() {
        let mut m = manager();
        assert!(m.devices().unwrap().len() >= 3);
        assert_eq!(m.port_mut().set_calls(), 0);
        assert!(!m.has_owned_changes());
    }

    #[test]
    fn rejects_empty_and_unknown_and_wrong_direction_endpoints_without_touching_defaults() {
        let mut m = manager();
        assert_eq!(
            m.apply(&req("", "bt-in", "bt-out")),
            Err(RouteError::EmptyRequest)
        );
        assert_eq!(
            m.apply(&req("s", "ghost", "bt-out")),
            Err(RouteError::EndpointMissing {
                which: Direction::Input
            })
        );
        assert_eq!(
            m.apply(&req("s", "bt-in", "mic-only")),
            Err(RouteError::WrongDirection {
                which: Direction::Output
            })
        );
        assert_eq!(
            m.apply(&req("s", "spk-only", "bt-out")),
            Err(RouteError::WrongDirection {
                which: Direction::Input
            })
        );
        assert_eq!(m.port_mut().set_calls(), 0);
    }

    #[test]
    fn apply_then_restore_returns_previous_defaults() {
        let mut m = manager();
        let before = (
            m.port_mut().default_input().unwrap(),
            m.port_mut().default_output().unwrap(),
        );
        let r = m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        assert!(r.input_changed && r.output_changed);
        assert_eq!(
            m.port_mut().default_input().unwrap().as_deref(),
            Some("bt-in")
        );
        let rep = m.restore().unwrap();
        assert_eq!(
            (rep.input, rep.output),
            (RestoreOutcome::Restored, RestoreOutcome::Restored)
        );
        let after = (
            m.port_mut().default_input().unwrap(),
            m.port_mut().default_output().unwrap(),
        );
        assert_eq!(before, after);
        assert!(m.owner().is_none());
    }

    #[test]
    fn already_selected_direction_is_not_owned_and_never_restored() {
        let mut m = manager();
        m.port_mut().force_default_output("bt-out");
        let r = m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        assert!(r.input_changed && !r.output_changed);
        let rep = m.restore().unwrap();
        assert_eq!(rep.output, RestoreOutcome::NotOwned);
        // The output the user had selected before we started is untouched.
        assert_eq!(
            m.port_mut().default_output().unwrap().as_deref(),
            Some("bt-out")
        );
    }

    #[test]
    fn external_change_is_respected_on_restore() {
        let mut m = manager();
        m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        // The user switches output to the built-in speakers while the route is active.
        m.port_mut().force_default_output("builtin-out");
        let rep = m.restore().unwrap();
        assert_eq!(rep.output, RestoreOutcome::SkippedChangedExternally);
        assert_eq!(rep.input, RestoreOutcome::Restored);
        assert_eq!(
            m.port_mut().default_output().unwrap().as_deref(),
            Some("builtin-out")
        );
    }

    #[test]
    fn disappeared_previous_device_is_not_resurrected() {
        let mut m = manager();
        m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        m.port_mut().remove_device("builtin-in");
        let rep = m.restore().unwrap();
        assert_eq!(rep.input, RestoreOutcome::SkippedPreviousGone);
    }

    #[test]
    fn second_session_is_refused_while_a_route_is_owned() {
        let mut m = manager();
        m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        assert_eq!(
            m.apply(&req("s2", "bt-in", "bt-out")),
            Err(RouteError::Busy { owner: "s1".into() })
        );
        assert_eq!(
            m.apply(&req("s1", "bt-in", "bt-out")),
            Err(RouteError::AlreadyApplied)
        );
    }

    #[test]
    fn output_failure_rolls_back_input_change() {
        let mut m = manager();
        m.port_mut().fail_next_set_output();
        let before = m.port_mut().default_input().unwrap();
        let err = m.apply(&req("s1", "bt-in", "bt-out")).unwrap_err();
        assert!(matches!(err, RouteError::Audio { .. }));
        assert_eq!(m.port_mut().default_input().unwrap(), before);
        assert!(!m.has_owned_changes());
    }

    #[test]
    fn failed_restore_keeps_ownership_for_retry() {
        let mut m = manager();
        m.apply(&req("s1", "bt-in", "bt-out")).unwrap();
        m.port_mut().fail_next_set_output();
        let rep = m.restore().unwrap();
        assert_eq!(rep.output, RestoreOutcome::Failed);
        assert_eq!(rep.input, RestoreOutcome::Restored);
        assert!(!rep.complete());
        assert!(m.has_owned_changes());
        let rep = m.restore().unwrap();
        assert_eq!(rep.output, RestoreOutcome::Restored);
        assert_eq!(rep.input, RestoreOutcome::NotOwned);
        assert!(!m.has_owned_changes());
    }

    #[test]
    fn restore_without_a_route_is_an_error() {
        let mut m = manager();
        assert_eq!(m.restore(), Err(RouteError::NothingToRestore));
    }

    #[test]
    fn proof_ladder_has_no_duplex_level_and_orders_by_strength() {
        assert!(AudioProof::None < AudioProof::ScoLinkReported);
        assert!(AudioProof::RouteDeviceVisible < AudioProof::RouteApplied);
        let names: Vec<String> = [
            AudioProof::None,
            AudioProof::ScoLinkReported,
            AudioProof::RouteDeviceVisible,
            AudioProof::RouteApplied,
        ]
        .iter()
        .map(|p| serde_json::to_string(p).unwrap())
        .collect();
        assert!(names.iter().all(|n| !n.contains("duplex")));
    }
}
