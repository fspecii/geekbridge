// SPDX-License-Identifier: GPL-3.0-or-later
//! Scripted ports for host tests. Behaviour is only what a test configures; nothing here claims
//! to reproduce a real IOBluetooth or CoreAudio stack.
use crate::model::{
    AgClass, AgFeatures, BluetoothAuth, BtAddress, Generation, Indicator, IoStatus, NativeEvent,
    PhoneRecord, Tagged,
};
use crate::plan::NativeOp;
use crate::port::{
    AudioDevice, AudioError, AudioPort, AudioTransport, NativeError, NativeErrorKind, NativePort,
    OpenedLink,
};
use bridge_core::calls::Sensitive;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;

/// Scripted IOBluetooth stand-in.
pub struct MockNative {
    pub auth: BluetoothAuth,
    pub phones: Vec<PhoneRecord>,
    pub features: AgFeatures,
    pub fail_open: Option<NativeError>,
    pub default_hf_features: Option<u32>,
    /// Fail the next `perform` with this error.
    pub fail_next_perform: Option<NativeError>,
    pub performed: Vec<(Generation, NativeOp)>,
    pub opened: Vec<(Generation, BtAddress)>,
    pub closed: Vec<Generation>,
    pub retired: HashSet<Generation>,
    pub call_list_requests: u32,
    pub paired_reads: u32,
    sinks: HashMap<Generation, Sender<Tagged>>,
}

impl MockNative {
    pub fn new() -> Self {
        Self {
            auth: BluetoothAuth::Allowed,
            phones: vec![Self::phone(
                "11:22:33:44:55:66",
                "Pixel",
                AgClass::Advertised,
            )],
            features: AgFeatures(AgFeatures::THREE_WAY_CALLING | AgFeatures::REJECT_CALL),
            fail_open: None,
            default_hf_features: None,
            fail_next_perform: None,
            performed: Vec::new(),
            opened: Vec::new(),
            closed: Vec::new(),
            retired: HashSet::new(),
            call_list_requests: 0,
            paired_reads: 0,
            sinks: HashMap::new(),
        }
    }

    pub fn phone(addr: &str, name: &str, ag: AgClass) -> PhoneRecord {
        PhoneRecord {
            address: BtAddress::parse(addr).unwrap(),
            name: Some(Sensitive::new(name)),
            paired: true,
            acl_connected: false,
            hands_free_gateway: ag,
        }
    }

    pub fn default_address() -> BtAddress {
        BtAddress::parse("11:22:33:44:55:66").unwrap()
    }

    /// Simulate a delegate callback exactly as the real delegate would: dropped once retired.
    pub fn emit(&self, generation: Generation, event: NativeEvent) {
        if self.retired.contains(&generation) {
            return;
        }
        self.emit_unchecked(generation, event);
    }

    /// Simulate a callback that was already queued when the link was retired (the race the
    /// orchestrator must survive).
    pub fn emit_unchecked(&self, generation: Generation, event: NativeEvent) {
        if let Some(tx) = self.sinks.get(&generation) {
            let _ = tx.send(Tagged { generation, event });
        }
    }

    pub fn indicator(&self, generation: Generation, i: Indicator) {
        self.emit(generation, NativeEvent::Indicator(i));
    }
    pub fn slc_up(&self, generation: Generation) {
        self.emit(generation, NativeEvent::SlcConnected(IoStatus::SUCCESS));
    }
    pub fn ops(&self) -> Vec<NativeOp> {
        self.performed.iter().map(|(_, o)| o.clone()).collect()
    }
}
impl Default for MockNative {
    fn default() -> Self {
        Self::new()
    }
}

impl NativePort for MockNative {
    fn authorization(&mut self) -> BluetoothAuth {
        self.auth
    }
    fn paired_phones(&mut self) -> Result<Vec<PhoneRecord>, NativeError> {
        self.paired_reads += 1;
        Ok(self.phones.clone())
    }
    fn open_link(
        &mut self,
        address: &BtAddress,
        generation: Generation,
        sink: Sender<Tagged>,
    ) -> Result<OpenedLink, NativeError> {
        if let Some(e) = self.fail_open.take() {
            return Err(e);
        }
        self.sinks.insert(generation, sink);
        self.opened.push((generation, address.clone()));
        Ok(OpenedLink {
            default_hf_features: self.default_hf_features,
        })
    }
    fn gateway_features(&mut self, _generation: Generation) -> Result<AgFeatures, NativeError> {
        Ok(self.features)
    }
    fn perform(&mut self, generation: Generation, op: &NativeOp) -> Result<(), NativeError> {
        if !self.sinks.contains_key(&generation) || self.retired.contains(&generation) {
            return Err(NativeError::new("perform", NativeErrorKind::NoSuchLink));
        }
        if let Some(e) = self.fail_next_perform.take() {
            return Err(e);
        }
        self.performed.push((generation, op.clone()));
        Ok(())
    }
    fn request_call_list(&mut self, _generation: Generation) -> Result<(), NativeError> {
        self.call_list_requests += 1;
        Ok(())
    }
    fn close_link(&mut self, generation: Generation) -> Result<(), NativeError> {
        self.closed.push(generation);
        Ok(())
    }
    fn retire_link(&mut self, generation: Generation) {
        self.retired.insert(generation);
    }
}

/// Scripted CoreAudio stand-in with real default-device bookkeeping.
pub struct MockAudio {
    devices: Vec<AudioDevice>,
    default_in: Option<String>,
    default_out: Option<String>,
    sets: u32,
    fail_set_output: bool,
}

impl MockAudio {
    pub fn standard() -> Self {
        let dev = |uid: &str, t, i, o| AudioDevice {
            uid: uid.into(),
            name: Some(Sensitive::new(uid)),
            transport: t,
            input_channels: i,
            output_channels: o,
        };
        Self {
            devices: vec![
                dev("builtin-in", AudioTransport::BuiltIn, 1, 0),
                dev("builtin-out", AudioTransport::BuiltIn, 0, 2),
                dev("bt-in", AudioTransport::Bluetooth, 1, 0),
                dev("bt-out", AudioTransport::Bluetooth, 0, 1),
                dev("mic-only", AudioTransport::Usb, 1, 0),
                dev("spk-only", AudioTransport::Usb, 0, 1),
            ],
            default_in: Some("builtin-in".into()),
            default_out: Some("builtin-out".into()),
            sets: 0,
            fail_set_output: false,
        }
    }
    pub fn with_duplex_bluetooth(mut self, uid: &str) -> Self {
        self.devices.push(AudioDevice {
            uid: uid.into(),
            name: None,
            transport: AudioTransport::Bluetooth,
            input_channels: 1,
            output_channels: 1,
        });
        self
    }
    pub fn set_calls(&self) -> u32 {
        self.sets
    }
    pub fn force_default_output(&mut self, uid: &str) {
        self.default_out = Some(uid.into());
    }
    pub fn remove_device(&mut self, uid: &str) {
        self.devices.retain(|d| d.uid != uid);
    }
    pub fn fail_next_set_output(&mut self) {
        self.fail_set_output = true;
    }
}

impl AudioPort for MockAudio {
    fn devices(&mut self) -> Result<Vec<AudioDevice>, AudioError> {
        Ok(self.devices.clone())
    }
    fn default_input(&mut self) -> Result<Option<String>, AudioError> {
        Ok(self.default_in.clone())
    }
    fn default_output(&mut self) -> Result<Option<String>, AudioError> {
        Ok(self.default_out.clone())
    }
    fn set_default_input(&mut self, uid: &str) -> Result<(), AudioError> {
        self.sets += 1;
        self.default_in = Some(uid.into());
        Ok(())
    }
    fn set_default_output(&mut self, uid: &str) -> Result<(), AudioError> {
        self.sets += 1;
        if std::mem::take(&mut self.fail_set_output) {
            return Err(AudioError::new(
                "set_default_output",
                Some(-1),
                "scripted failure",
            ));
        }
        self.default_out = Some(uid.into());
        Ok(())
    }
}

/// Test clock the test advances by hand. Clones share the same time.
#[derive(Clone, Default)]
pub struct ManualClock(std::rc::Rc<std::cell::Cell<u64>>);
impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn advance(&self, ms: u64) {
        self.0.set(self.0.get() + ms);
    }
}
impl crate::model::Clock for ManualClock {
    fn now(&self) -> crate::model::Millis {
        crate::model::Millis(self.0.get())
    }
}
