//! Explicit "not supported here" ports for hosts without macOS Bluetooth. They let the daemon
//! construct the same [`Controller`](crate::controller::Controller) everywhere; every operation
//! then reports [`AdapterError::UnsupportedHost`](crate::error::AdapterError::UnsupportedHost)
//! instead of silently doing nothing.
use crate::model::{AgFeatures, BluetoothAuth, BtAddress, Generation, PhoneRecord, Tagged};
use crate::plan::NativeOp;
use crate::port::{
    AudioDevice, AudioError, AudioPort, NativeError, NativeErrorKind, NativePort, OpenedLink,
};
use std::sync::mpsc::Sender;

fn nope(op: &str) -> NativeError {
    NativeError::other(
        op,
        format!(
            "macOS Bluetooth is not available on {}",
            std::env::consts::OS
        ),
    )
}

#[derive(Debug, Default)]
pub struct UnsupportedNative;

impl NativePort for UnsupportedNative {
    fn host_supported(&self) -> bool {
        false
    }
    fn authorization(&mut self) -> BluetoothAuth {
        BluetoothAuth::Unknown
    }
    fn paired_phones(&mut self) -> Result<Vec<PhoneRecord>, NativeError> {
        Err(nope("paired_phones"))
    }
    fn open_link(
        &mut self,
        _address: &BtAddress,
        _generation: Generation,
        _sink: Sender<Tagged>,
    ) -> Result<OpenedLink, NativeError> {
        Err(nope("open_link"))
    }
    fn gateway_features(&mut self, _g: Generation) -> Result<AgFeatures, NativeError> {
        Err(NativeError::new(
            "gateway_features",
            NativeErrorKind::NoSuchLink,
        ))
    }
    fn perform(&mut self, _g: Generation, _op: &NativeOp) -> Result<(), NativeError> {
        Err(NativeError::new("perform", NativeErrorKind::NoSuchLink))
    }
    fn request_call_list(&mut self, _g: Generation) -> Result<(), NativeError> {
        Err(NativeError::new(
            "request_call_list",
            NativeErrorKind::NoSuchLink,
        ))
    }
    fn close_link(&mut self, _g: Generation) -> Result<(), NativeError> {
        Err(NativeError::new("close_link", NativeErrorKind::NoSuchLink))
    }
    fn retire_link(&mut self, _g: Generation) {}
}

#[derive(Debug, Default)]
pub struct UnsupportedAudio;

fn audio_nope(op: &str) -> AudioError {
    AudioError::new(op, None, "CoreAudio is not available on this host")
}

impl AudioPort for UnsupportedAudio {
    fn devices(&mut self) -> Result<Vec<AudioDevice>, AudioError> {
        Err(audio_nope("devices"))
    }
    fn default_input(&mut self) -> Result<Option<String>, AudioError> {
        Err(audio_nope("default_input"))
    }
    fn default_output(&mut self) -> Result<Option<String>, AudioError> {
        Err(audio_nope("default_output"))
    }
    fn set_default_input(&mut self, _uid: &str) -> Result<(), AudioError> {
        Err(audio_nope("set_default_input"))
    }
    fn set_default_output(&mut self, _uid: &str) -> Result<(), AudioError> {
        Err(audio_nope("set_default_output"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::{Config, Controller};
    use crate::error::AdapterError;
    use crate::mock::ManualClock;
    use crate::model::BtAddress;
    use crate::plan::MacCommand;
    use bridge_core::calls::{CallFeature, Support};

    fn ctl() -> Controller<UnsupportedNative, UnsupportedAudio, ManualClock> {
        Controller::new(
            UnsupportedNative,
            UnsupportedAudio,
            ManualClock::new(),
            Config::default(),
        )
    }
    fn unsupported() -> AdapterError {
        AdapterError::UnsupportedHost {
            os: std::env::consts::OS,
        }
    }

    #[test]
    fn every_entry_point_reports_unsupported_host() {
        let mut c = ctl();
        assert_eq!(c.inventory(), Err(unsupported()));
        assert_eq!(
            c.connect(&BtAddress::parse("11:22:33:44:55:66").unwrap()),
            Err(unsupported())
        );
        let probe = c.probe();
        assert_eq!(probe.blocked, Some(unsupported()));
        assert!(probe.phones.is_empty());
        for f in CallFeature::ALL {
            assert!(matches!(
                probe.capabilities.core.support(f),
                Support::Unsupported { .. }
            ));
        }
        assert_eq!(c.execute(MacCommand::Hold), Err(AdapterError::NoActiveLink));
        assert_eq!(c.disconnect(), Err(AdapterError::NoActiveLink));
        assert!(c.audio_devices().is_err());
    }
}
