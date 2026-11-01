//! Explicit readiness: why calling can or cannot work right now.
//!
//! Setup, not the call APIs, is where phone calling fails:
//! contract, package identity, privacy, access, registration, connection, line and audio route
//! each fail independently. This module turns raw facts into an ordered list of [`Blocker`]s so
//! the UI and the engine's gating share one truth. It performs no I/O.
use crate::model::*;
use serde::Serialize;

/// Policy knobs for what this adapter treats as supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// First supported Windows build. Windows 10 is reported unsupported: the only documented way
    /// to use the transport APIs there is a Limited Access Feature unlock whose key this project
    /// does not have and will not borrow.
    pub min_build: u32,
    /// Refuse explicit actions without package identity. Documentation says the restricted
    /// capabilities take effect through identity; set false only to run the "unpackaged" experiment.
    pub require_package_identity: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            min_build: WINDOWS_11_FIRST_BUILD,
            require_package_identity: true,
        }
    }
}

/// What the app learned from its own explicit `RequestAccessAsync` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessState {
    /// Never asked in this session (a probe never asks).
    NotRequested,
    Allowed,
    DeniedByUser,
    DeniedBySystem,
    Unspecified,
    Failed(PlatformErrorKind),
}
impl From<AccessStatus> for AccessState {
    fn from(status: AccessStatus) -> Self {
        match status {
            AccessStatus::Allowed => Self::Allowed,
            AccessStatus::DeniedByUser => Self::DeniedByUser,
            AccessStatus::DeniedBySystem => Self::DeniedBySystem,
            AccessStatus::Unspecified => Self::Unspecified,
        }
    }
}

/// What the app did with `ConnectAsync` in this session. Windows exposes no "is connected" flag;
/// connection is otherwise inferred from the line becoming visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectHistory {
    NotAttempted,
    ReturnedTrue,
    ReturnedFalse,
    /// The deadline passed; the connection may or may not have been made.
    Uncertain,
    Failed(PlatformErrorKind),
}

/// Why an action cannot be attempted now. Variants are ordered roughly by the setup sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "blocker", rename_all = "snake_case")]
pub enum Blocker {
    NotWindows,
    /// The environment read itself failed, so nothing else is known.
    EnvironmentUnreadable {
        kind: PlatformErrorKind,
    },
    /// Windows 10, or a build that could not be determined.
    UnsupportedOs {
        build: Option<u32>,
    },
    /// A required `CallsPhoneContract` major version is absent.
    ContractMissing {
        required_major: u16,
    },
    ContractUnknown,
    PackageIdentityMissing,
    PackageIdentityUnknown,
    /// Settings, Privacy & security, Phone calls (or the missing manifest capability).
    PrivacyBlocked {
        access: PrivacyAccess,
    },
    EnumerationFailed {
        kind: PlatformErrorKind,
    },
    NoPairedTransport,
    DeviceDisabled,
    /// `PhoneLineTransportDevice.FromId` or a registration read failed (for example access denied
    /// or another hands-free stack owns the phone).
    DeviceUnreadable {
        kind: PlatformErrorKind,
    },
    /// `RequestAccessAsync` has not returned `Allowed` in this session.
    AccessNotGranted {
        access: AccessState,
    },
    NotRegistered,
    /// Registered, but reading the phone lines failed, so line state is unknown.
    LinesUnreadable {
        kind: PlatformErrorKind,
    },
    /// Registered but Windows shows no line: the phone is not connected, or the link is coming up.
    NoLine,
    LineNotDialable,
    LineDialabilityUnknown,
}

/// Whether the audio route to this computer is possible. Reported, never assumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioReadiness {
    /// Control is blocked, so audio is moot.
    NotApplicable,
    /// Windows says the call audio can be moved to this computer. Not a measured audio path.
    Routable,
    /// Windows says it cannot (for example a Bluetooth headset holds the audio).
    Blocked,
    /// Windows did not say (older build, or the property could not be read).
    Unreported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readiness {
    /// Environment blockers followed by device blockers, in setup order.
    pub blockers: Vec<Blocker>,
    pub audio: AudioReadiness,
}
impl Readiness {
    /// First thing to fix, if any.
    pub fn primary(&self) -> Option<Blocker> {
        self.blockers.first().copied()
    }
    /// True when nothing blocks reading or controlling calls. `LineNotDialable` and
    /// `LineDialabilityUnknown` only block dialing, see [`Self::dial_blocker`].
    pub fn control_ready(&self) -> bool {
        self.control_blocker().is_none()
    }
    pub fn control_blocker(&self) -> Option<Blocker> {
        self.blockers.iter().copied().find(|b| {
            !matches!(
                b,
                Blocker::LineNotDialable | Blocker::LineDialabilityUnknown
            )
        })
    }
    pub fn dial_blocker(&self) -> Option<Blocker> {
        self.blockers.first().copied()
    }
}

/// Blockers that apply to every device.
pub fn environment_blockers(env: &EnvironmentFacts, policy: &Policy) -> Vec<Blocker> {
    let mut out = Vec::new();
    if env.os.platform == OsPlatform::NotWindows {
        return vec![Blocker::NotWindows];
    }
    if env.os.build.is_none_or(|b| b < policy.min_build) {
        out.push(Blocker::UnsupportedOs {
            build: env.os.build,
        });
    }
    for (present, major) in [(env.contract.v5, 5u16), (env.contract.v6, 6u16)] {
        match present {
            Some(true) => {}
            Some(false) => out.push(Blocker::ContractMissing {
                required_major: major,
            }),
            None => {
                if !out.contains(&Blocker::ContractUnknown) {
                    out.push(Blocker::ContractUnknown);
                }
            }
        }
    }
    if policy.require_package_identity {
        match env.package {
            PackageIdentity::Present { .. } => {}
            PackageIdentity::Absent => out.push(Blocker::PackageIdentityMissing),
            PackageIdentity::Unknown { .. } => out.push(Blocker::PackageIdentityUnknown),
        }
    }
    if matches!(
        env.privacy,
        PrivacyAccess::DeniedByUser
            | PrivacyAccess::DeniedBySystem
            | PrivacyAccess::UserPromptRequired
            | PrivacyAccess::NotDeclaredByApp
    ) {
        out.push(Blocker::PrivacyBlocked {
            access: env.privacy,
        });
    }
    out
}

/// What was observed about one device in a refresh.
pub struct DeviceObservation<'a> {
    pub enabled: bool,
    pub info: &'a Step<TransportInfo>,
    /// The line chosen for this transport, if any.
    pub line: Option<&'a LineRecord>,
    /// Set when the line listing failed (then `line` carries no information).
    pub lines_error: Option<PlatformErrorKind>,
    pub access: AccessState,
}

/// Blockers specific to one device, assuming the environment ones are empty.
pub fn device_blockers(obs: &DeviceObservation<'_>) -> Vec<Blocker> {
    let mut out = Vec::new();
    if !obs.enabled {
        out.push(Blocker::DeviceDisabled);
    }
    let info = match obs.info {
        Ok(info) => info,
        Err(e) => {
            out.push(Blocker::DeviceUnreadable { kind: e.kind });
            return out;
        }
    };
    let registered = match &info.registered {
        Ok(r) => *r,
        Err(e) => {
            out.push(Blocker::DeviceUnreadable { kind: e.kind });
            return out;
        }
    };
    if !registered {
        if obs.access != AccessState::Allowed {
            out.push(Blocker::AccessNotGranted { access: obs.access });
        }
        out.push(Blocker::NotRegistered);
        return out;
    }
    if let Some(kind) = obs.lines_error {
        out.push(Blocker::LinesUnreadable { kind });
        return out;
    }
    match obs.line {
        None => out.push(Blocker::NoLine),
        Some(line) => match line.can_dial {
            Some(true) => {}
            Some(false) => out.push(Blocker::LineNotDialable),
            None => out.push(Blocker::LineDialabilityUnknown),
        },
    }
    out
}

pub fn assess(
    env: &EnvironmentFacts,
    policy: &Policy,
    device: Option<&DeviceObservation<'_>>,
) -> Readiness {
    let mut blockers = environment_blockers(env, policy);
    if blockers.is_empty()
        && let Some(obs) = device
    {
        blockers.extend(device_blockers(obs));
    }
    let audio = match device {
        Some(obs)
            if blockers.iter().all(|b| {
                matches!(
                    b,
                    Blocker::LineNotDialable | Blocker::LineDialabilityUnknown
                )
            }) =>
        {
            match obs.info {
                Ok(TransportInfo {
                    audio_routing: Ok(AudioRouting::CanRouteToLocal),
                    ..
                }) => AudioReadiness::Routable,
                Ok(TransportInfo {
                    audio_routing: Ok(AudioRouting::CannotRouteToLocal),
                    ..
                }) => AudioReadiness::Blocked,
                _ => AudioReadiness::Unreported,
            }
        }
        _ => AudioReadiness::NotApplicable,
    };
    Readiness { blockers, audio }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn good_env() -> EnvironmentFacts {
        EnvironmentFacts {
            os: OsFacts {
                platform: OsPlatform::Windows,
                build: Some(26100),
            },
            contract: ContractFacts {
                v5: Some(true),
                v6: Some(true),
                v7: Some(true),
            },
            package: PackageIdentity::Present {
                family_name: "GeekBridge.Calls.Dev_x".into(),
            },
            privacy: PrivacyAccess::Allowed,
        }
    }

    #[test]
    fn good_environment_has_no_blockers() {
        assert!(environment_blockers(&good_env(), &Policy::default()).is_empty());
    }

    #[test]
    fn windows_10_is_unsupported_not_unlocked() {
        let mut env = good_env();
        env.os.build = Some(19045);
        assert_eq!(
            environment_blockers(&env, &Policy::default()),
            vec![Blocker::UnsupportedOs { build: Some(19045) }]
        );
    }

    #[test]
    fn each_environment_failure_is_its_own_state() {
        let mut env = good_env();
        env.contract.v6 = Some(false);
        env.package = PackageIdentity::Absent;
        env.privacy = PrivacyAccess::DeniedByUser;
        assert_eq!(
            environment_blockers(&env, &Policy::default()),
            vec![
                Blocker::ContractMissing { required_major: 6 },
                Blocker::PackageIdentityMissing,
                Blocker::PrivacyBlocked {
                    access: PrivacyAccess::DeniedByUser
                },
            ]
        );
    }

    #[test]
    fn unknown_privacy_is_not_treated_as_a_denial() {
        let mut env = good_env();
        env.privacy = PrivacyAccess::Unavailable;
        assert!(environment_blockers(&env, &Policy::default()).is_empty());
    }

    #[test]
    fn unpackaged_experiment_can_be_allowed_by_policy() {
        let mut env = good_env();
        env.package = PackageIdentity::Absent;
        let policy = Policy {
            require_package_identity: false,
            ..Policy::default()
        };
        assert!(environment_blockers(&env, &policy).is_empty());
    }

    #[test]
    fn not_windows_short_circuits() {
        assert_eq!(
            environment_blockers(&EnvironmentFacts::not_windows(), &Policy::default()),
            vec![Blocker::NotWindows]
        );
    }

    fn info(registered: bool, routing: AudioRouting) -> Step<TransportInfo> {
        Ok(TransportInfo {
            transport: Ok(NativeTransport::Bluetooth),
            registered: Ok(registered),
            audio_routing: Ok(routing),
            in_band_ringing: Ok(false),
        })
    }
    fn line(can_dial: Option<bool>) -> LineRecord {
        LineRecord {
            id: LineId::new("0f0f0f0f-0000-0000-0000-000000000001").unwrap(),
            transport: NativeTransport::Bluetooth,
            transport_device_id: None,
            can_dial,
            name: None,
        }
    }

    #[test]
    fn unregistered_device_lists_access_then_registration() {
        let info = info(false, AudioRouting::Unknown);
        let obs = DeviceObservation {
            enabled: true,
            info: &info,
            line: None,
            lines_error: None,
            access: AccessState::NotRequested,
        };
        let r = assess(&good_env(), &Policy::default(), Some(&obs));
        assert_eq!(
            r.blockers,
            vec![
                Blocker::AccessNotGranted {
                    access: AccessState::NotRequested
                },
                Blocker::NotRegistered
            ]
        );
        assert_eq!(r.audio, AudioReadiness::NotApplicable);
    }

    #[test]
    fn registered_without_line_is_no_line() {
        let info = info(true, AudioRouting::Unknown);
        let obs = DeviceObservation {
            enabled: true,
            info: &info,
            line: None,
            lines_error: None,
            access: AccessState::NotRequested,
        };
        let r = assess(&good_env(), &Policy::default(), Some(&obs));
        assert_eq!(r.blockers, vec![Blocker::NoLine]);
        assert!(!r.control_ready());
    }

    #[test]
    fn not_dialable_blocks_dial_but_not_call_control() {
        let info = info(true, AudioRouting::CanRouteToLocal);
        let l = line(Some(false));
        let obs = DeviceObservation {
            enabled: true,
            info: &info,
            line: Some(&l),
            lines_error: None,
            access: AccessState::Allowed,
        };
        let r = assess(&good_env(), &Policy::default(), Some(&obs));
        assert!(r.control_ready());
        assert_eq!(r.dial_blocker(), Some(Blocker::LineNotDialable));
        assert_eq!(r.audio, AudioReadiness::Routable);
    }

    #[test]
    fn audio_route_blocked_is_reported_honestly() {
        let info = info(true, AudioRouting::CannotRouteToLocal);
        let l = line(Some(true));
        let obs = DeviceObservation {
            enabled: true,
            info: &info,
            line: Some(&l),
            lines_error: None,
            access: AccessState::Allowed,
        };
        let r = assess(&good_env(), &Policy::default(), Some(&obs));
        assert!(r.control_ready());
        assert_eq!(r.audio, AudioReadiness::Blocked);
    }

    #[test]
    fn unreadable_device_is_typed() {
        let info: Step<TransportInfo> = Err(PlatformError::new(
            PlatformErrorKind::AccessDenied,
            "PhoneLineTransportDevice.FromId",
        ));
        let obs = DeviceObservation {
            enabled: true,
            info: &info,
            line: None,
            lines_error: None,
            access: AccessState::NotRequested,
        };
        let r = assess(&good_env(), &Policy::default(), Some(&obs));
        assert_eq!(
            r.blockers,
            vec![Blocker::DeviceUnreadable {
                kind: PlatformErrorKind::AccessDenied
            }]
        );
    }
}
