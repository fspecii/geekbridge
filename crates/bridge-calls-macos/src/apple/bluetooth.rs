//! IOBluetooth implementation of [`NativePort`].
//!
//! # Ownership and threading
//!
//! [`AppleBluetooth`] holds Objective-C objects (`Retained<IOBluetoothHandsFreeDevice>`), which are
//! not `Send`. It is constructed with a [`MainThreadMarker`], so it can only exist on the process
//! main thread, the thread that runs the `CFRunLoop` in the helper. Every method is called from that
//! thread by the controller; this is a design rule, not something Apple documents as required
//! (Apple does not say which thread or run loop IOBluetooth delivers callbacks on). The delegate
//! ([`super::delegate::HfDelegate`]) is built to tolerate any callback thread.
//!
//! # Lifetime of delegates
//!
//! `IOBluetoothHandsFree`'s delegate is unretained. A delegate is therefore kept alive by its
//! [`Live`] entry while linked and, once retired, by the `parked` list until the process ends: its
//! `retired` flag is set (so callbacks are dropped at the source), the framework's delegate pointer
//! is cleared with `setDelegate:nil`, and the hands-free object is disconnected. The controller
//! caps how many generations a process may create, so parking is bounded.
use super::delegate::{DelegateState, HfDelegate};
use crate::model::{
    AgClass, AgFeatures, BluetoothAuth, BtAddress, Generation, HF_FEATURES_REQUESTED, PhoneRecord,
    Tagged,
};
use crate::plan::NativeOp;
use crate::port::{NativeError, NativeErrorKind, NativePort, OpenedLink};
use bridge_core::calls::Sensitive;
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_core_bluetooth::{CBManager, CBManagerAuthorization};
use objc2_foundation::NSString;
use objc2_io_bluetooth::{IOBluetoothDevice, IOBluetoothHandsFreeDevice};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

struct Live {
    hf: Retained<IOBluetoothHandsFreeDevice>,
    delegate: Retained<HfDelegate>,
    retired: Arc<AtomicBool>,
}

/// A retired link's objects, kept alive so a late framework callback can never reach freed memory.
#[allow(dead_code)]
struct Parked {
    hf: Retained<IOBluetoothHandsFreeDevice>,
    delegate: Retained<HfDelegate>,
}

pub struct AppleBluetooth {
    _main: MainThreadMarker,
    links: HashMap<Generation, Live>,
    parked: Vec<Parked>,
}

impl AppleBluetooth {
    /// Must be created on the main thread (proved by the marker).
    pub fn new(main: MainThreadMarker) -> Self {
        Self {
            _main: main,
            links: HashMap::new(),
            parked: Vec::new(),
        }
    }

    fn link(&self, op: &str, generation: Generation) -> Result<&Live, NativeError> {
        self.links
            .get(&generation)
            .filter(|l| !l.retired.load(Ordering::Acquire))
            .ok_or_else(|| NativeError::new(op, NativeErrorKind::NoSuchLink))
    }

    /// Resolve a paired device by address without any radio activity.
    fn device_for(address: &BtAddress) -> Option<Retained<IOBluetoothDevice>> {
        // IOBluetooth documents the "xx-xx-xx-xx-xx-xx" form.
        let text = NSString::from_str(&address.as_str().replace(':', "-"));
        // SAFETY: `deviceWithAddressString:` takes an NSString and returns a (possibly nil)
        // device object; it performs a lookup/creation of a local object only.
        unsafe { IOBluetoothDevice::deviceWithAddressString(Some(&text)) }
    }
}

fn map_auth(a: CBManagerAuthorization) -> BluetoothAuth {
    match a {
        CBManagerAuthorization::NotDetermined => BluetoothAuth::NotDetermined,
        CBManagerAuthorization::Restricted => BluetoothAuth::Restricted,
        CBManagerAuthorization::Denied => BluetoothAuth::Denied,
        CBManagerAuthorization::AllowedAlways => BluetoothAuth::Allowed,
        _ => BluetoothAuth::Unknown,
    }
}

impl NativePort for AppleBluetooth {
    fn authorization(&mut self) -> BluetoothAuth {
        // SAFETY: `+[CBManager authorization]` is a class property getter with no arguments. Apple
        // documents it for checking status "before allocating CBManager", i.e. it does not prompt.
        map_auth(unsafe { CBManager::authorization_class() })
    }

    fn paired_phones(&mut self) -> Result<Vec<PhoneRecord>, NativeError> {
        // SAFETY: `+[IOBluetoothDevice pairedDevices]` returns nil or an array of IOBluetoothDevice.
        let Some(array) = (unsafe { IOBluetoothDevice::pairedDevices() }) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for object in array.iter() {
            let Some(device) = object.downcast_ref::<IOBluetoothDevice>() else {
                continue;
            };
            // SAFETY: plain getters on a live IOBluetoothDevice. `nameOrAddress` is used instead
            // of `name`, which is declared non-null but can be nil for never-resolved devices.
            let (addr, name, paired, acl, ag) = unsafe {
                (
                    device.addressString(),
                    device.nameOrAddress(),
                    device.isPaired(),
                    device.isConnected(),
                    device.isHandsFreeAudioGateway(),
                )
            };
            let Some(address) = addr.and_then(|a| BtAddress::parse(&a.to_string()).ok()) else {
                continue;
            };
            // `nameOrAddress` falls back to the address text; do not present that as a name.
            let name = name
                .map(|n| n.to_string())
                .filter(|n| {
                    BtAddress::parse(n).map(|a| a != address).unwrap_or(true) && !n.is_empty()
                })
                .map(Sensitive::new);
            out.push(PhoneRecord {
                address,
                name,
                paired,
                acl_connected: acl,
                hands_free_gateway: if ag {
                    AgClass::Advertised
                } else {
                    AgClass::NotAdvertised
                },
            });
        }
        Ok(out)
    }

    fn open_link(
        &mut self,
        address: &BtAddress,
        generation: Generation,
        sink: Sender<Tagged>,
    ) -> Result<OpenedLink, NativeError> {
        const OP: &str = "open_link";
        let device = Self::device_for(address)
            .ok_or_else(|| NativeError::new(OP, NativeErrorKind::DeviceNotFound))?;
        // SAFETY: getter on a live object.
        if !unsafe { device.isPaired() } {
            return Err(NativeError::new(OP, NativeErrorKind::NotPaired));
        }
        let retired = Arc::new(AtomicBool::new(false));
        let delegate = HfDelegate::new(DelegateState {
            generation,
            sink,
            retired: retired.clone(),
        });
        // SAFETY: `initWithDevice:delegate:` takes an IOBluetoothDevice and an object implementing
        // the delegate protocols (HfDelegate does). The delegate is unretained by the framework;
        // it is stored in `Live`/`parked` for the rest of the process.
        let hf = unsafe {
            IOBluetoothHandsFreeDevice::initWithDevice_delegate(
                IOBluetoothHandsFreeDevice::alloc(),
                Some(&device),
                Some(&delegate),
            )
        }
        .ok_or_else(|| NativeError::new(OP, NativeErrorKind::InitFailed))?;
        // SAFETY: simple property accessors/mutators and `connect` on a live object. Features are
        // requested explicitly (before `connect`) instead of relying on framework defaults.
        let default_features = unsafe {
            let default = hf.supportedFeatures();
            hf.setSupportedFeatures(HF_FEATURES_REQUESTED);
            hf.connect();
            default
        };
        self.links.insert(
            generation,
            Live {
                hf,
                delegate,
                retired,
            },
        );
        Ok(OpenedLink {
            default_hf_features: Some(default_features),
        })
    }

    fn gateway_features(&mut self, generation: Generation) -> Result<AgFeatures, NativeError> {
        let live = self.link("gateway_features", generation)?;
        // SAFETY: getter on a live object.
        Ok(AgFeatures(unsafe { live.hf.deviceSupportedFeatures() }))
    }

    fn perform(&mut self, generation: Generation, op: &NativeOp) -> Result<(), NativeError> {
        let live = self.link("perform", generation)?;
        // SAFETY: getter on a live object.
        if !unsafe { live.hf.isConnected() } {
            return Err(NativeError::other(
                "perform",
                "service-level connection is not up",
            ));
        }
        let hf = &live.hf;
        // SAFETY (all arms): each is a documented `IOBluetoothHandsFreeDevice` method called on a
        // live object from the owning thread; string arguments are freshly created NSStrings that
        // outlive the call. Results arrive later through the delegate.
        unsafe {
            match op {
                NativeOp::Dial(number) => hf.dialNumber(Some(&NSString::from_str(number))),
                NativeOp::AcceptCall => hf.acceptCall(),
                NativeOp::EndCall => hf.endCall(),
                NativeOp::HoldCall => hf.holdCall(),
                NativeOp::ReleaseHeldCalls => hf.releaseHeldCalls(),
                NativeOp::SendDtmf(chars) => {
                    for c in chars {
                        hf.sendDTMF(Some(&NSString::from_str(&c.to_string())));
                    }
                }
                NativeOp::ConnectSco => hf.connectSCO(),
                NativeOp::DisconnectSco => hf.disconnectSCO(),
                NativeOp::TransferAudioToComputer => hf.transferAudioToComputer(),
                NativeOp::TransferAudioToPhone => hf.transferAudioToPhone(),
            }
        }
        Ok(())
    }

    fn request_call_list(&mut self, generation: Generation) -> Result<(), NativeError> {
        let live = self.link("request_call_list", generation)?;
        // SAFETY: documented method; entries are delivered through `handsFree:currentCall:`.
        unsafe { live.hf.currentCallList() };
        Ok(())
    }

    fn close_link(&mut self, generation: Generation) -> Result<(), NativeError> {
        let live = self.link("close_link", generation)?;
        // SAFETY: documented method; `disconnected:` is delivered through the delegate.
        unsafe { live.hf.disconnect() };
        Ok(())
    }

    fn retire_link(&mut self, generation: Generation) {
        let Some(live) = self.links.remove(&generation) else {
            return;
        };
        // Order matters: stop our own forwarding, detach the framework's pointer, then make sure
        // the connection is closed. The objects stay alive in `parked`.
        live.retired.store(true, Ordering::Release);
        // SAFETY: clearing the delegate is documented ("Pass nil to remove an existing delegate");
        // `disconnect` is only issued if the framework still reports a connection.
        unsafe {
            live.hf.setDelegate(None);
            if live.hf.isConnected() {
                live.hf.disconnect();
            }
        }
        self.parked.push(Parked {
            hf: live.hf,
            delegate: live.delegate,
        });
    }
}
