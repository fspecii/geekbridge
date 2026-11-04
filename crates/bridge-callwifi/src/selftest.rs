// SPDX-License-Identifier: GPL-3.0-or-later
//! What the phone's self-test ("Test with a call") found, in plain words. Pure: the phone measures, this decides.
//!
//! The helper opens the call-downlink source twice: once with **no call** (the control) and once **during a call**.
//! A real phone is silent or refuses without a call; an emulator (and possibly a quirky HAL) produces a loud signal
//! either way, which proves nothing about the far party. The Kotlin twin (`SelfTestVerdict.kt`) is checked against
//! the same vectors.

/// RMS (of 16-bit samples) below which a capture counts as silence (about -60 dBFS).
pub const SILENT_RMS: u32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Never run on this phone (or the helper is not running).
    NotRun,
    /// Audio of the far party arrives (silent without a call, loud with one).
    Works,
    /// The source opens but delivers silence during a call: this phone blocks it (a Samsung build may do so).
    Silent,
    /// Android refused the capture (permission or policy).
    Refused,
    /// Loud even with no call: the phone's sound path produces a test signal, so the result proves nothing.
    Unsure,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::Works => "works",
            Self::Silent => "silent",
            Self::Refused => "refused",
            Self::Unsure => "unsure",
        }
    }
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "not_run" => Self::NotRun,
            "works" => Self::Works,
            "silent" => Self::Silent,
            "refused" => Self::Refused,
            "unsure" => Self::Unsure,
            _ => return None,
        })
    }
    /// May "Listen here" be offered? Only when the far party's audio was proven to arrive.
    pub fn can_listen(self) -> bool {
        self == Self::Works
    }
}

/// One capture attempt, numbers only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capture {
    /// The source opened and delivered frames. False = refused (exception, failed init, policy).
    pub opened: bool,
    pub rms: u32,
    pub silenced: bool,
}

/// Decide from the two captures. `call` is the one taken during a call.
pub fn classify(control: Option<Capture>, call: Option<Capture>) -> Verdict {
    let Some(call) = call else {
        return Verdict::NotRun;
    };
    if !call.opened {
        return Verdict::Refused;
    }
    if call.silenced || call.rms < SILENT_RMS {
        return Verdict::Silent;
    }
    if let Some(c) = control
        && c.opened
        && !c.silenced
        && c.rms >= SILENT_RMS
        // Loud with no call, at least half as loud as with one: not the far party.
        && u64::from(c.rms) * 2 >= u64::from(call.rms)
    {
        return Verdict::Unsure;
    }
    Verdict::Works
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cap(opened: bool, rms: u32, silenced: bool) -> Capture {
        Capture {
            opened,
            rms,
            silenced,
        }
    }

    /// The same table is in `SelfTestVerdictTest.kt`.
    #[test]
    fn vectors() {
        let quiet = Some(cap(true, 2, false));
        assert_eq!(classify(quiet, None), Verdict::NotRun);
        assert_eq!(
            classify(quiet, Some(cap(false, 0, false))),
            Verdict::Refused
        );
        assert_eq!(classify(quiet, Some(cap(true, 5, false))), Verdict::Silent);
        assert_eq!(
            classify(quiet, Some(cap(true, 4000, true))),
            Verdict::Silent
        );
        assert_eq!(
            classify(quiet, Some(cap(true, 4000, false))),
            Verdict::Works
        );
        assert_eq!(classify(None, Some(cap(true, 4000, false))), Verdict::Works);
        // Control refused or silent: fine. Control loud: unsure.
        assert_eq!(
            classify(Some(cap(false, 0, false)), Some(cap(true, 4000, false))),
            Verdict::Works
        );
        assert_eq!(
            classify(Some(cap(true, 15000, false)), Some(cap(true, 15000, false))),
            Verdict::Unsure
        );
        assert_eq!(
            classify(Some(cap(true, 1000, false)), Some(cap(true, 4000, false))),
            Verdict::Works
        );
    }

    #[test]
    fn names_round_trip() {
        for v in [
            Verdict::NotRun,
            Verdict::Works,
            Verdict::Silent,
            Verdict::Refused,
            Verdict::Unsure,
        ] {
            assert_eq!(Verdict::from_name(v.as_str()), Some(v));
        }
        assert_eq!(Verdict::from_name("WORKS"), None);
        assert!(!Verdict::Unsure.can_listen());
    }
}
