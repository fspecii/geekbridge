// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-neutral vocabulary shared by the orchestration layer, the JSON helper protocol and the
//! Apple bindings. Nothing here touches Objective-C, so it compiles and is tested on every host.
use bridge_core::calls::{MAX_FIELD_LEN, Sensitive};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Version of the helper's JSON line protocol.
pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------- time

/// Monotonic milliseconds from an injectable [`Clock`], so timeouts are deterministic in tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct Millis(pub u64);
impl Millis {
    pub fn saturating_add(self, ms: u64) -> Millis {
        Millis(self.0.saturating_add(ms))
    }
    pub fn since(self, earlier: Millis) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

pub trait Clock {
    fn now(&self) -> Millis;
}

/// Wall-clock-independent monotonic clock (`Instant`-based).
#[derive(Debug)]
pub struct SystemClock(std::time::Instant);
impl SystemClock {
    pub fn new() -> Self {
        Self(std::time::Instant::now())
    }
}
impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}
impl Clock for SystemClock {
    fn now(&self) -> Millis {
        Millis(self.0.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
    }
}

// ---------------------------------------------------------------- identity

/// Monotonic id of one connect attempt. Every native callback is tagged with the generation of the
/// delegate that produced it; anything not matching the active generation is stale and dropped.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressError;
impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not a Bluetooth device address (expected six hex bytes)")
    }
}
impl std::error::Error for AddressError {}

/// Canonical `AA:BB:CC:DD:EE:FF` Bluetooth device address. IOBluetooth prints addresses with
/// dashes in lowercase; both forms are accepted and normalised. `Debug` shows only the last two
/// bytes because the address identifies a person's phone.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct BtAddress(String);
impl BtAddress {
    pub fn parse(text: &str) -> Result<Self, AddressError> {
        let parts: Vec<&str> = text.trim().split([':', '-']).collect();
        if parts.len() != 6
            || parts
                .iter()
                .any(|p| p.len() != 2 || !p.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(AddressError);
        }
        Ok(Self(parts.join(":").to_ascii_uppercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for BtAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BtAddress(..:{})", &self.0[12..])
    }
}
impl<'de> Deserialize<'de> for BtAddress {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        BtAddress::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------- permission and inventory

/// Bluetooth privacy (TCC) state as reported by `CBManager.authorization`, which Apple documents
/// as usable before allocating a manager. Whether it gates *classic* IOBluetooth the same way is
/// not documented; a `Denied` here is treated as blocking, `Allowed` as "not known to block".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BluetoothAuth {
    NotDetermined,
    Restricted,
    Denied,
    Allowed,
    /// Could not be determined (non-macOS host, or the probe is unavailable).
    Unknown,
}
impl BluetoothAuth {
    /// True when native Bluetooth calls must not be attempted.
    pub fn blocks_use(self) -> bool {
        matches!(self, Self::Denied | Self::Restricted)
    }
}

/// Whether macOS's cached SDP data lists the Hands-Free *Audio Gateway* role for the device (the
/// role an Android phone plays). `isHandsFreeAudioGateway` is documented as "obtained from the
/// device's SDP record", so `NotAdvertised` can also mean "never queried".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgClass {
    Advertised,
    NotAdvertised,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhoneRecord {
    pub address: BtAddress,
    pub name: Option<Sensitive>,
    pub paired: bool,
    /// Baseband (ACL) connection present. This is *not* an HFP service-level connection.
    pub acl_connected: bool,
    pub hands_free_gateway: AgClass,
}

/// Read-only inventory result. Producing one opens no connection and sends no command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inventory {
    pub authorization: BluetoothAuth,
    pub phones: Vec<PhoneRecord>,
}
impl Inventory {
    pub fn ag_capable(&self) -> impl Iterator<Item = &PhoneRecord> {
        self.phones
            .iter()
            .filter(|p| p.paired && p.hands_free_gateway == AgClass::Advertised)
    }
    pub fn find(&self, address: &BtAddress) -> Option<&PhoneRecord> {
        self.phones.iter().find(|p| &p.address == address)
    }
}

// ---------------------------------------------------------------- HFP facts

/// Feature bits an Audio Gateway reports (HFP "supported features" bitmap as exposed by
/// `deviceSupportedFeatures`; Apple names the same bits `IOBluetoothHandsFreeAudioGatewayFeatures`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgFeatures(pub u32);
impl AgFeatures {
    pub const THREE_WAY_CALLING: u32 = 1 << 0;
    pub const EC_NR: u32 = 1 << 1;
    pub const VOICE_RECOGNITION: u32 = 1 << 2;
    pub const IN_BAND_RING: u32 = 1 << 3;
    pub const REJECT_CALL: u32 = 1 << 5;
    pub const ENHANCED_CALL_STATUS: u32 = 1 << 6;
    pub const ENHANCED_CALL_CONTROL: u32 = 1 << 7;
    pub const CODEC_NEGOTIATION: u32 = 1 << 9;
    pub fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
    /// A zero bitmap is treated as "not reported", never as "supports nothing".
    pub fn reported(self) -> bool {
        self.0 != 0
    }
}

/// Hands-free-unit (this Mac) features requested before connecting
/// (`IOBluetoothHandsFreeDeviceFeatures`): call waiting / three-way, CLI presentation and enhanced
/// call status. Requested explicitly instead of relying on undocumented framework defaults.
pub const HF_FEATURES_REQUESTED: u32 = (1 << 1) | (1 << 2) | (1 << 5);

/// One standard HFP indicator value reported through the delegate. Values are range checked at
/// the boundary ([`Indicator::call_setup`] etc.); the tracker never sees out-of-range numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "indicator", content = "value", rename_all = "snake_case")]
pub enum Indicator {
    Service(bool),
    /// `call`: 1 when any call is active or held.
    Call(bool),
    /// `callsetup`: 0 none, 1 incoming, 2 outgoing dialing, 3 outgoing alerting.
    CallSetup(u8),
    /// `callheld`: 0 none, 1 active and held, 2 held only.
    CallHeld(u8),
    Signal(u8),
    Roam(bool),
    Battery(u8),
}
impl Indicator {
    pub fn call_setup(v: i64) -> Option<Self> {
        (0..=3).contains(&v).then_some(Self::CallSetup(v as u8))
    }
    pub fn call_held(v: i64) -> Option<Self> {
        (0..=2).contains(&v).then_some(Self::CallHeld(v as u8))
    }
    pub fn call(v: i64) -> Option<Self> {
        (0..=1).contains(&v).then_some(Self::Call(v == 1))
    }
}

/// `+CLCC` call status codes (HFP "enhanced call status").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListStatus {
    Active,
    Held,
    Dialing,
    Alerting,
    Incoming,
    Waiting,
    HeldByResponseAndHold,
}
impl ListStatus {
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            0 => Self::Active,
            1 => Self::Held,
            2 => Self::Dialing,
            3 => Self::Alerting,
            4 => Self::Incoming,
            5 => Self::Waiting,
            6 => Self::HeldByResponseAndHold,
            _ => return None,
        })
    }
}

/// One entry delivered by `handsFree:currentCall:` after `currentCallList`. Used only to enrich
/// party details: the framework documents no end-of-list marker, so a complete, ordered call
/// list is never assumed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListEntry {
    pub index: Option<u32>,
    pub status: ListStatus,
    pub incoming: Option<bool>,
    pub number: Option<Sensitive>,
    pub name: Option<Sensitive>,
}

/// IOReturn status carried by connection callbacks. Zero is `kIOReturnSuccess`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IoStatus(pub i32);
impl IoStatus {
    pub const SUCCESS: IoStatus = IoStatus(0);
    pub fn is_success(self) -> bool {
        self.0 == 0
    }
    /// IOKit `IOReturn` values are `0xe00002xx`. These two codes mean the caller lacks the right
    /// to use the device, which on a modern macOS is the usual signature of a missing Bluetooth
    /// TCC grant. The numeric values are recalled from IOKit's public `IOReturn.h` and have not
    /// been checked against an SDK on this host; treat the classification as a hint.
    pub fn suggests_permission_denied(self) -> bool {
        let raw = self.0 as u32;
        raw == 0xe000_02c1 || raw == 0xe000_02e2
    }
    pub fn suggests_timeout(self) -> bool {
        self.0 as u32 == 0xe000_02d6
    }
}

/// Everything a native delegate callback can tell the orchestrator. Produced on whatever thread
/// the framework calls the delegate on; carries no framework objects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeEvent {
    SlcConnected(IoStatus),
    SlcDisconnected(IoStatus),
    ScoOpened(IoStatus),
    ScoClosed(IoStatus),
    Indicator(Indicator),
    /// `incomingCallFrom:` (calling line identification).
    CallerId(Option<Sensitive>),
    /// `ringAttempt:`; evidence the phone is ringing, never used to create a call by itself.
    Ring,
    CurrentCall(ListEntry),
    /// `unhandledResultCode:`; recorded as diagnostics only (the text may hold digits).
    UnhandledResult(Sensitive),
    /// A callback arrived with a payload that failed range checks.
    Malformed(&'static str),
}

/// A native event with the generation of the delegate that emitted it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tagged {
    pub generation: Generation,
    pub event: NativeEvent,
}

/// Which indicator a delegate callback reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndicatorKind {
    Service,
    Call,
    CallSetup,
    CallHeld,
    Signal,
    Roam,
    Battery,
}

/// Convert a raw callback value to an event, rejecting out-of-range values as `Malformed` rather
/// than letting them into the call model. HFP defines signal and battery on a 0..=5 scale.
pub fn indicator_event(kind: IndicatorKind, raw: Option<i64>) -> NativeEvent {
    let Some(v) = raw else {
        return NativeEvent::Malformed("indicator_missing_value");
    };
    let flag = |label: &'static str| match v {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(NativeEvent::Malformed(label)),
    };
    let level = |label: &'static str| {
        if (0..=5).contains(&v) {
            Ok(v as u8)
        } else {
            Err(NativeEvent::Malformed(label))
        }
    };
    let ind = match kind {
        IndicatorKind::Service => flag("service_out_of_range").map(Indicator::Service),
        IndicatorKind::Roam => flag("roam_out_of_range").map(Indicator::Roam),
        IndicatorKind::Call => {
            Indicator::call(v).ok_or(NativeEvent::Malformed("call_out_of_range"))
        }
        IndicatorKind::CallSetup => {
            Indicator::call_setup(v).ok_or(NativeEvent::Malformed("call_setup_out_of_range"))
        }
        IndicatorKind::CallHeld => {
            Indicator::call_held(v).ok_or(NativeEvent::Malformed("call_held_out_of_range"))
        }
        IndicatorKind::Signal => level("signal_out_of_range").map(Indicator::Signal),
        IndicatorKind::Battery => level("battery_out_of_range").map(Indicator::Battery),
    };
    match ind {
        Ok(i) => NativeEvent::Indicator(i),
        Err(e) => e,
    }
}

/// Build a call-list entry from the raw `currentCall:` dictionary values. `direction` follows
/// HFP `+CLCC`: 0 mobile originated (outgoing), 1 mobile terminated (incoming). A missing or
/// unknown status makes the entry unusable, so it is reported as malformed.
pub fn list_entry_event(
    index: Option<i64>,
    status: Option<i64>,
    direction: Option<i64>,
    number: Option<&str>,
    name: Option<&str>,
) -> NativeEvent {
    let Some(status) = status.and_then(ListStatus::from_code) else {
        return NativeEvent::Malformed("call_list_status");
    };
    let non_empty = |s: Option<&str>| {
        s.filter(|t| !t.is_empty())
            .map(|t| Sensitive::new(bound_field(t)))
    };
    NativeEvent::CurrentCall(ListEntry {
        index: index.and_then(|i| u32::try_from(i).ok()),
        status,
        incoming: match direction {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        number: non_empty(number),
        name: non_empty(name),
    })
}

/// Truncate untrusted text from the phone to the core's field limit at a char boundary.
pub fn bound_field(text: &str) -> String {
    if text.len() <= MAX_FIELD_LEN {
        return text.to_owned();
    }
    let mut end = MAX_FIELD_LEN;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_accepts_both_separators_and_normalises() {
        let a = BtAddress::parse("aa-bb-0c-dd-ee-0f").unwrap();
        assert_eq!(a.as_str(), "AA:BB:0C:DD:EE:0F");
        assert_eq!(a, BtAddress::parse("AA:BB:0C:DD:EE:0F").unwrap());
        assert!(BtAddress::parse("aa-bb-0c-dd-ee").is_err());
        assert!(BtAddress::parse("aa-bb-0c-dd-ee-0g").is_err());
        assert!(BtAddress::parse("aabbccddeeff").is_err());
        assert!(BtAddress::parse("aa:bb:cc:dd:ee:fff").is_err());
    }

    #[test]
    fn address_debug_does_not_leak_full_address() {
        let a = BtAddress::parse("11:22:33:44:55:66").unwrap();
        let shown = format!("{a:?}");
        assert!(!shown.contains("11:22"), "{shown}");
        assert!(shown.contains("55:66"));
    }

    #[test]
    fn address_deserialisation_validates() {
        assert!(serde_json::from_str::<BtAddress>("\"11-22-33-44-55-66\"").is_ok());
        assert!(serde_json::from_str::<BtAddress>("\"nope\"").is_err());
    }

    #[test]
    fn raw_indicator_values_are_validated_at_the_boundary() {
        use IndicatorKind::*;
        assert_eq!(
            indicator_event(CallSetup, Some(2)),
            NativeEvent::Indicator(Indicator::CallSetup(2))
        );
        assert_eq!(
            indicator_event(Call, Some(1)),
            NativeEvent::Indicator(Indicator::Call(true))
        );
        assert_eq!(
            indicator_event(Signal, Some(5)),
            NativeEvent::Indicator(Indicator::Signal(5))
        );
        assert_eq!(
            indicator_event(Service, Some(0)),
            NativeEvent::Indicator(Indicator::Service(false))
        );
        assert!(matches!(
            indicator_event(CallSetup, Some(9)),
            NativeEvent::Malformed(_)
        ));
        assert!(matches!(
            indicator_event(Battery, Some(6)),
            NativeEvent::Malformed(_)
        ));
        assert!(matches!(
            indicator_event(Roam, Some(2)),
            NativeEvent::Malformed(_)
        ));
        assert!(matches!(
            indicator_event(CallHeld, None),
            NativeEvent::Malformed(_)
        ));
        assert!(matches!(
            indicator_event(Call, Some(-1)),
            NativeEvent::Malformed(_)
        ));
    }

    #[test]
    fn list_entry_conversion_handles_missing_and_bad_fields() {
        let e = list_entry_event(Some(1), Some(4), Some(1), Some("+1555"), Some(""));
        match e {
            NativeEvent::CurrentCall(l) => {
                assert_eq!(l.status, ListStatus::Incoming);
                assert_eq!(l.incoming, Some(true));
                assert_eq!(l.index, Some(1));
                assert!(l.number.is_some());
                assert!(l.name.is_none(), "empty name is absent");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            list_entry_event(None, None, None, None, None),
            NativeEvent::Malformed(_)
        ));
        assert!(matches!(
            list_entry_event(None, Some(9), None, None, None),
            NativeEvent::Malformed(_)
        ));
        match list_entry_event(Some(-3), Some(0), Some(7), None, None) {
            NativeEvent::CurrentCall(l) => assert_eq!((l.index, l.incoming), (None, None)),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn indicators_are_range_checked() {
        assert_eq!(Indicator::call_setup(3), Some(Indicator::CallSetup(3)));
        assert_eq!(Indicator::call_setup(4), None);
        assert_eq!(Indicator::call_setup(-1), None);
        assert_eq!(Indicator::call_held(2), Some(Indicator::CallHeld(2)));
        assert_eq!(Indicator::call_held(3), None);
        assert_eq!(Indicator::call(1), Some(Indicator::Call(true)));
        assert_eq!(Indicator::call(2), None);
    }

    #[test]
    fn zero_feature_bitmap_is_not_reported() {
        assert!(!AgFeatures(0).reported());
        assert!(AgFeatures(AgFeatures::THREE_WAY_CALLING).has(AgFeatures::THREE_WAY_CALLING));
        assert!(!AgFeatures(AgFeatures::EC_NR).has(AgFeatures::THREE_WAY_CALLING));
    }

    #[test]
    fn io_status_permission_hint() {
        assert!(IoStatus(0xe000_02e2_u32 as i32).suggests_permission_denied());
        assert!(!IoStatus::SUCCESS.suggests_permission_denied());
        assert!(IoStatus(0xe000_02d6_u32 as i32).suggests_timeout());
    }

    #[test]
    fn bound_field_respects_char_boundaries() {
        let long = "é".repeat(200);
        let b = bound_field(&long);
        assert!(b.len() <= MAX_FIELD_LEN);
        assert!(b.chars().all(|c| c == 'é'));
    }

    #[test]
    fn inventory_filters_ag_capable_paired_only() {
        let rec = |n: u8, paired, ag| PhoneRecord {
            address: BtAddress::parse(&format!("00:00:00:00:00:0{n}")).unwrap(),
            name: None,
            paired,
            acl_connected: false,
            hands_free_gateway: ag,
        };
        let inv = Inventory {
            authorization: BluetoothAuth::Allowed,
            phones: vec![
                rec(1, true, AgClass::Advertised),
                rec(2, true, AgClass::NotAdvertised),
                rec(3, false, AgClass::Advertised),
            ],
        };
        assert_eq!(inv.ag_capable().count(), 1);
        assert!(
            inv.find(&BtAddress::parse("00:00:00:00:00:02").unwrap())
                .is_some()
        );
    }
}
