//! An in-memory [`CallsPlatform`] for tests on any host, including the daemon's.
//!
//! [`MockPlatform`] is a cloneable handle onto shared state, so a test can keep one clone to change
//! what "Windows" reports (a phone appears, a line vanishes) and to read the call log while the
//! engine or service owns another. Every port call is logged; [`MockPlatform::mutations`] lists
//! only the calls that change Windows or phone state, which is what the no-side-effect tests
//! assert on.
use crate::model::*;
use crate::platform::*;
use bridge_core::calls::Sensitive;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Names of port methods that change state (everything else is a read).
pub const MUTATING: &[&str] = &[
    "request_access",
    "register",
    "unregister",
    "connect",
    "dial",
    "call_op",
];

pub struct MockState {
    pub env: EnvironmentFacts,
    pub transports: Vec<TransportRecord>,
    /// Registered flag per transport key.
    pub registered: BTreeMap<String, bool>,
    pub audio_routing: AudioRouting,
    pub lines: Vec<LineRecord>,
    pub calls: BTreeMap<LineId, Vec<CallRecord>>,
    pub access: AccessStatus,
    /// What `connect` returns, and whether the line appears when it does.
    pub connect_result: bool,
    pub line_on_connect: Option<LineRecord>,
    /// `call_op` answers this status instead of applying the operation.
    pub op_status: Option<OperationStatus>,
    /// Inject an error for a method name; consumed only if `sticky_errors` is set.
    pub errors: BTreeMap<&'static str, PlatformError>,
    /// Sleep this long inside `call_op` (to exercise service deadlines).
    pub call_op_delay: Option<Duration>,
    pub log: Vec<String>,
    pub sink: Option<SignalSink>,
    next_call: u32,
}

#[derive(Clone)]
pub struct MockPlatform(Arc<Mutex<MockState>>);

pub fn good_env() -> EnvironmentFacts {
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
            family_name: "GeekBridge.Calls.Dev_test".into(),
        },
        privacy: PrivacyAccess::Allowed,
    }
}

pub fn device_id(n: u8) -> TransportDeviceId {
    TransportDeviceId::new(format!(
        "\\\\?\\BTHENUM#{{0000111e}}_VID&0001#PHONE{n}#{{GUID}}"
    ))
    .unwrap()
}

pub fn line_for(id: &TransportDeviceId, n: u8, can_dial: bool) -> LineRecord {
    LineRecord {
        id: LineId::new(format!("00000000-0000-0000-0000-{n:012}")).unwrap(),
        transport: NativeTransport::Bluetooth,
        transport_device_id: Some(id.clone()),
        can_dial: Some(can_dial),
        name: Some(Sensitive::new("Test Phone")),
    }
}

pub fn call(id: &str, status: NativeCallStatus) -> CallRecord {
    CallRecord {
        id: NativeCallId::new(id).unwrap(),
        status,
        direction: match status {
            NativeCallStatus::Incoming => NativeDirection::Incoming,
            NativeCallStatus::Dialing => NativeDirection::Outgoing,
            _ => NativeDirection::Unknown,
        },
        audio_device: AudioDevice::Remote,
        muted: Some(false),
        number: Some(Sensitive::new("+15550100")),
        name: Some(Sensitive::new("Test Caller")),
        hold_supported: Some(true),
    }
}

impl MockPlatform {
    /// Windows 11 with the right package and privacy state and one paired, enabled, unregistered
    /// phone: the state of a fresh install.
    pub fn fresh() -> Self {
        let id = device_id(1);
        let mut registered = BTreeMap::new();
        registered.insert(id.key().to_owned(), false);
        Self(Arc::new(Mutex::new(MockState {
            env: good_env(),
            transports: vec![TransportRecord {
                id,
                name: Some(Sensitive::new("Test Phone")),
                address: Some(Sensitive::new("aa:bb:cc:dd:ee:ff")),
                enabled: true,
            }],
            registered,
            audio_routing: AudioRouting::CanRouteToLocal,
            lines: Vec::new(),
            calls: BTreeMap::new(),
            access: AccessStatus::Allowed,
            connect_result: true,
            line_on_connect: None,
            op_status: None,
            errors: BTreeMap::new(),
            call_op_delay: None,
            log: Vec::new(),
            sink: None,
            next_call: 0,
        })))
    }

    /// Like [`Self::fresh`] but already registered with a dialable line.
    pub fn connected() -> Self {
        let m = Self::fresh();
        {
            let mut s = m.lock();
            let id = s.transports[0].id.clone();
            s.registered.insert(id.key().to_owned(), true);
            s.lines.push(line_for(&id, 1, true));
        }
        m
    }

    pub fn lock(&self) -> MutexGuard<'_, MockState> {
        self.0.lock().unwrap()
    }
    pub fn with<R>(&self, f: impl FnOnce(&mut MockState) -> R) -> R {
        f(&mut self.lock())
    }

    pub fn first_id(&self) -> TransportDeviceId {
        self.lock().transports[0].id.clone()
    }
    pub fn first_line(&self) -> LineId {
        self.lock().lines[0].id.clone()
    }
    pub fn set_calls(&self, calls: Vec<CallRecord>) {
        let mut s = self.lock();
        let line = s.lines[0].id.clone();
        s.calls.insert(line, calls);
    }
    pub fn log(&self) -> Vec<String> {
        self.lock().log.clone()
    }
    pub fn clear_log(&self) {
        self.lock().log.clear();
    }
    /// Logged calls that change state.
    pub fn mutations(&self) -> Vec<String> {
        self.lock()
            .log
            .iter()
            .filter(|l| MUTATING.contains(&l.split(':').next().unwrap_or("")))
            .cloned()
            .collect()
    }
    /// Deliver an OS notification to the installed sink.
    pub fn fire(&self, signal: Signal) {
        let sink = self.lock().sink.clone();
        if let Some(sink) = sink {
            sink(signal);
        }
    }
}

impl MockState {
    fn note(&mut self, entry: impl Into<String>) {
        self.log.push(entry.into());
    }
    fn injected(&self, method: &'static str) -> Option<PlatformError> {
        self.errors.get(method).copied()
    }
    fn find_call_mut(&mut self, id: &NativeCallId) -> Option<(LineId, usize)> {
        for (line, list) in &self.calls {
            if let Some(i) = list.iter().position(|c| &c.id == id) {
                return Some((line.clone(), i));
            }
        }
        None
    }
}

impl CallsPlatform for MockPlatform {
    fn environment(&mut self, _: Deadline) -> Step<EnvironmentFacts> {
        let mut s = self.lock();
        s.note("environment");
        match s.injected("environment") {
            Some(e) => Err(e),
            None => Ok(s.env.clone()),
        }
    }

    fn list_transports(&mut self, _: Deadline) -> Step<Vec<TransportRecord>> {
        let mut s = self.lock();
        s.note("list_transports");
        match s.injected("list_transports") {
            Some(e) => Err(e),
            None => Ok(s.transports.clone()),
        }
    }

    fn transport_info(&mut self, id: &TransportDeviceId, _: Deadline) -> Step<TransportInfo> {
        let mut s = self.lock();
        s.note("transport_info");
        if let Some(e) = s.injected("transport_info") {
            return Err(e);
        }
        let Some(registered) = s.registered.get(id.key()).copied() else {
            return Err(PlatformError::new(
                PlatformErrorKind::NotFound,
                "PhoneLineTransportDevice.FromId",
            ));
        };
        Ok(TransportInfo {
            transport: Ok(NativeTransport::Bluetooth),
            registered: Ok(registered),
            audio_routing: Ok(s.audio_routing),
            in_band_ringing: Ok(false),
        })
    }

    fn list_lines(&mut self, _: Deadline) -> Step<Vec<LineRecord>> {
        let mut s = self.lock();
        s.note("list_lines");
        match s.injected("list_lines") {
            Some(e) => Err(e),
            None => Ok(s.lines.clone()),
        }
    }

    fn list_calls(&mut self, line: &LineId, _: Deadline) -> Step<CallListing> {
        let mut s = self.lock();
        s.note("list_calls");
        if let Some(e) = s.injected("list_calls") {
            return Err(e);
        }
        Ok(CallListing {
            calls: s.calls.get(line).cloned().unwrap_or_default(),
            truncated: false,
        })
    }

    fn start_signals(&mut self, sink: SignalSink) -> Step<()> {
        let mut s = self.lock();
        s.note("start_signals");
        s.sink = Some(sink);
        Ok(())
    }
    fn stop_signals(&mut self) {
        let mut s = self.lock();
        s.note("stop_signals");
        s.sink = None;
    }

    fn request_access(&mut self, _: &TransportDeviceId, _: Deadline) -> Step<AccessStatus> {
        let mut s = self.lock();
        s.note("request_access");
        match s.injected("request_access") {
            Some(e) => Err(e),
            None => Ok(s.access),
        }
    }

    fn register(&mut self, id: &TransportDeviceId) -> Step<()> {
        let mut s = self.lock();
        s.note("register");
        if let Some(e) = s.injected("register") {
            return Err(e);
        }
        s.registered.insert(id.key().to_owned(), true);
        Ok(())
    }

    fn unregister(&mut self, id: &TransportDeviceId) -> Step<()> {
        let mut s = self.lock();
        s.note("unregister");
        if let Some(e) = s.injected("unregister") {
            return Err(e);
        }
        s.registered.insert(id.key().to_owned(), false);
        s.lines
            .retain(|l| l.transport_device_id.as_ref() != Some(id));
        Ok(())
    }

    fn connect(&mut self, _: &TransportDeviceId, _: Deadline) -> Step<bool> {
        let mut s = self.lock();
        s.note("connect");
        if let Some(e) = s.injected("connect") {
            return Err(e);
        }
        if s.connect_result
            && let Some(line) = s.line_on_connect.take()
        {
            s.lines.push(line);
        }
        Ok(s.connect_result)
    }

    fn dial(&mut self, line: &LineId, number: &str, _: Deadline) -> Step<DialResult> {
        let mut s = self.lock();
        // The number is deliberately not logged.
        s.note(format!("dial:len={}", number.len()));
        if let Some(e) = s.injected("dial") {
            return Err(e);
        }
        s.next_call += 1;
        let mut record = call(
            &format!("dialed-{}", s.next_call),
            NativeCallStatus::Dialing,
        );
        record.number = Some(Sensitive::new(number));
        s.calls
            .entry(line.clone())
            .or_default()
            .push(record.clone());
        Ok(DialResult {
            status: s.op_status.unwrap_or(OperationStatus::Succeeded),
            call: Some(record),
        })
    }

    fn call_op(&mut self, id: &NativeCallId, op: CallOp, _: Deadline) -> Step<OperationStatus> {
        let delay = self.lock().call_op_delay;
        if let Some(d) = delay {
            std::thread::sleep(d);
        }
        let mut s = self.lock();
        s.note(format!("call_op:{}", op.name()));
        if let Some(e) = s.injected("call_op") {
            return Err(e);
        }
        if let Some(status) = s.op_status {
            return Ok(status);
        }
        let Some((line, i)) = s.find_call_mut(id) else {
            return Err(PlatformError::new(
                PlatformErrorKind::NotFound,
                "PhoneCall.GetFromId",
            ));
        };
        let list = s.calls.get_mut(&line).unwrap();
        match op {
            CallOp::Accept | CallOp::Resume => list[i].status = NativeCallStatus::Talking,
            CallOp::Reject | CallOp::End => {
                list.remove(i);
            }
            CallOp::Hold => list[i].status = NativeCallStatus::Held,
            CallOp::Mute => list[i].muted = Some(true),
            CallOp::Unmute => list[i].muted = Some(false),
            CallOp::Dtmf(_) => {}
            CallOp::Route(d) => list[i].audio_device = d,
        }
        Ok(OperationStatus::Succeeded)
    }
}
