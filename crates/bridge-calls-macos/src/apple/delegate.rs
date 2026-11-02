// SPDX-License-Identifier: GPL-3.0-or-later
//! The Objective-C delegate object that IOBluetooth calls back into.
//!
//! # Ownership and threading
//!
//! * One `HfDelegate` is created per link generation and is owned by [`super::bluetooth::AppleBluetooth`]
//!   for as long as the process lives (retired delegates are parked, never freed early) because
//!   `IOBluetoothHandsFree`'s delegate is **unretained** (`setDelegate:` documentation).
//! * Callbacks may arrive on whichever thread the framework chooses; Apple does not document it.
//!   The handlers therefore touch no framework object and no non-`Sync` Rust state: they convert
//!   the arguments to plain Rust values and push a [`Tagged`] event into a channel. All real work
//!   happens later on the owning thread inside the controller.
//! * A shared `retired` flag is checked before sending, so once a link is retired a late callback is
//!   dropped at the source; the controller additionally drops any event whose generation is not
//!   the active one.
//! * Handlers cannot panic: they use no `unwrap`/`expect`/indexing, and a missing argument becomes
//!   a `Malformed` event. A panic unwinding into the framework would be undefined behaviour.
use crate::model::{
    Generation, IndicatorKind, IoStatus, NativeEvent, Tagged, indicator_event, list_entry_event,
};
use bridge_core::calls::Sensitive;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};
use objc2_io_bluetooth::{
    IOBluetoothHandsFree, IOBluetoothHandsFreeCallDirection, IOBluetoothHandsFreeCallIndex,
    IOBluetoothHandsFreeCallName, IOBluetoothHandsFreeCallNumber, IOBluetoothHandsFreeCallStatus,
    IOBluetoothHandsFreeDelegate, IOBluetoothHandsFreeDevice, IOBluetoothHandsFreeDeviceDelegate,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

/// Plain-Rust state of one delegate. Everything in it is `Send + Sync`.
pub struct DelegateState {
    pub generation: Generation,
    pub sink: Sender<Tagged>,
    pub retired: Arc<AtomicBool>,
}

define_class!(
    // SAFETY:
    // - The superclass `NSObject` has no subclassing requirements.
    // - `HfDelegate` does not implement `Drop`.
    // - No instance state is accessed from any framework callback other than through `ivars()`,
    //   which holds only `Send + Sync` Rust values, so any callback thread is acceptable.
    #[unsafe(super(NSObject))]
    #[name = "GeekBridgeHfpDelegate"]
    #[ivars = DelegateState]
    pub struct HfDelegate;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for HfDelegate {}

    // SAFETY: `IOBluetoothHandsFreeDelegate` has no safety requirements; each selector below is
    // spelled exactly as declared in the protocol and takes (device, NSNumber status).
    unsafe impl IOBluetoothHandsFreeDelegate for HfDelegate {
        #[unsafe(method(handsFree:connected:))]
        fn hf_connected(&self, _device: Option<&IOBluetoothHandsFree>, status: Option<&NSNumber>) {
            self.post(NativeEvent::SlcConnected(io_status(status)));
        }

        #[unsafe(method(handsFree:disconnected:))]
        fn hf_disconnected(
            &self,
            _device: Option<&IOBluetoothHandsFree>,
            status: Option<&NSNumber>,
        ) {
            self.post(NativeEvent::SlcDisconnected(io_status(status)));
        }

        #[unsafe(method(handsFree:scoConnectionOpened:))]
        fn hf_sco_opened(&self, _device: Option<&IOBluetoothHandsFree>, status: Option<&NSNumber>) {
            self.post(NativeEvent::ScoOpened(io_status(status)));
        }

        #[unsafe(method(handsFree:scoConnectionClosed:))]
        fn hf_sco_closed(&self, _device: Option<&IOBluetoothHandsFree>, status: Option<&NSNumber>) {
            self.post(NativeEvent::ScoClosed(io_status(status)));
        }
    }

    // SAFETY: `IOBluetoothHandsFreeDeviceDelegate` has no safety requirements; selectors and
    // argument types match the protocol declaration.
    unsafe impl IOBluetoothHandsFreeDeviceDelegate for HfDelegate {
        #[unsafe(method(handsFree:isServiceAvailable:))]
        fn hf_service(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::Service, int(v)));
        }

        #[unsafe(method(handsFree:isCallActive:))]
        fn hf_call_active(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::Call, int(v)));
        }

        #[unsafe(method(handsFree:callSetupMode:))]
        fn hf_call_setup(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::CallSetup, int(v)));
        }

        #[unsafe(method(handsFree:callHoldState:))]
        fn hf_call_hold(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::CallHeld, int(v)));
        }

        #[unsafe(method(handsFree:signalStrength:))]
        fn hf_signal(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::Signal, int(v)));
        }

        #[unsafe(method(handsFree:isRoaming:))]
        fn hf_roaming(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::Roam, int(v)));
        }

        #[unsafe(method(handsFree:batteryCharge:))]
        fn hf_battery(&self, _d: Option<&IOBluetoothHandsFreeDevice>, v: Option<&NSNumber>) {
            self.post(indicator_event(IndicatorKind::Battery, int(v)));
        }

        #[unsafe(method(handsFree:incomingCallFrom:))]
        fn hf_incoming_from(
            &self,
            _d: Option<&IOBluetoothHandsFreeDevice>,
            number: Option<&NSString>,
        ) {
            self.post(NativeEvent::CallerId(
                number.map(|n| Sensitive::new(n.to_string())),
            ));
        }

        #[unsafe(method(handsFree:ringAttempt:))]
        fn hf_ring(&self, _d: Option<&IOBluetoothHandsFreeDevice>, _v: Option<&NSNumber>) {
            self.post(NativeEvent::Ring);
        }

        #[unsafe(method(handsFree:currentCall:))]
        fn hf_current_call(
            &self,
            _d: Option<&IOBluetoothHandsFreeDevice>,
            call: Option<&NSDictionary>,
        ) {
            self.post(match call {
                Some(dict) => entry_from_dictionary(dict),
                None => NativeEvent::Malformed("call_list_missing_dictionary"),
            });
        }

        #[unsafe(method(handsFree:unhandledResultCode:))]
        fn hf_unhandled(&self, _d: Option<&IOBluetoothHandsFreeDevice>, code: Option<&NSString>) {
            self.post(NativeEvent::UnhandledResult(Sensitive::new(
                code.map(|c| c.to_string()).unwrap_or_default(),
            )));
        }
    }
);

impl HfDelegate {
    /// Create a delegate for one link generation.
    pub fn new(state: DelegateState) -> Retained<Self> {
        let this = Self::alloc().set_ivars(state);
        // SAFETY: `NSObject`'s `init` has the signature `-> instancetype` with no arguments.
        unsafe { msg_send![super(this), init] }
    }

    /// View as the protocol object `setDelegate:` expects. The caller must keep `self` alive for
    /// as long as the framework may call it.
    pub fn as_protocol(&self) -> &ProtocolObject<dyn IOBluetoothHandsFreeDelegate> {
        ProtocolObject::from_ref(self)
    }

    fn post(&self, event: NativeEvent) {
        let st = self.ivars();
        if st.retired.load(Ordering::Acquire) {
            return;
        }
        // A closed channel means the controller is gone; nothing useful to do from a callback.
        let _ = st.sink.send(Tagged {
            generation: st.generation,
            event,
        });
    }
}

/// `NSNumber` carrying an `IOReturn`; a missing number is reported as a generic failure (-1).
fn io_status(n: Option<&NSNumber>) -> IoStatus {
    IoStatus(n.map(|n| n.intValue()).unwrap_or(-1))
}

fn int(n: Option<&NSNumber>) -> Option<i64> {
    n.map(|n| n.integerValue() as i64)
}

/// Read one `currentCall:` dictionary using the documented key constants.
fn entry_from_dictionary(dict: &NSDictionary) -> NativeEvent {
    // SAFETY: the key constants are immutable framework-defined `NSString *` globals; reading them
    // has no preconditions beyond the framework being linked.
    let (k_index, k_dir, k_status, k_number, k_name) = unsafe {
        (
            IOBluetoothHandsFreeCallIndex,
            IOBluetoothHandsFreeCallDirection,
            IOBluetoothHandsFreeCallStatus,
            IOBluetoothHandsFreeCallNumber,
            IOBluetoothHandsFreeCallName,
        )
    };
    let number_of = |key: Option<&'static NSString>| -> Option<i64> {
        let key = key?;
        let value = dict.objectForKey(key.as_ref())?;
        value
            .downcast_ref::<NSNumber>()
            .map(|n| n.integerValue() as i64)
    };
    let string_of = |key: Option<&'static NSString>| -> Option<String> {
        let key = key?;
        let value = dict.objectForKey(key.as_ref())?;
        value.downcast_ref::<NSString>().map(|s| s.to_string())
    };
    let number = string_of(k_number);
    let name = string_of(k_name);
    list_entry_event(
        number_of(k_index),
        number_of(k_status),
        number_of(k_dir),
        number.as_deref(),
        name.as_deref(),
    )
}
