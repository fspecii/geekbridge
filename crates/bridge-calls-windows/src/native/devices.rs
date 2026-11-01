//! Phone-line transport devices: enumeration, state reads and the explicit register/connect actions.
use super::wait::{await_op, classify};
use crate::model::*;
use crate::platform::Deadline;
use bridge_core::calls::Sensitive;
use windows::ApplicationModel::Calls::{
    PhoneLineTransport, PhoneLineTransportDevice, TransportDeviceAudioRoutingStatus,
};
use windows::Devices::Enumeration::{DeviceAccessStatus, DeviceInformation};
use windows::Foundation::IReference;
use windows::core::{HSTRING, Interface};
use windows_collections::IIterable;

/// Interface property that carries the phone's Bluetooth address. Windows does not always fill
/// the endpoint's own address in this context, so it is requested explicitly.
const ADDRESS_PROPERTY: &str = "System.DeviceInterface.Bluetooth.DeviceAddress";

pub(super) fn hstring(id: &TransportDeviceId) -> HSTRING {
    HSTRING::from(id.as_str())
}

fn open(
    id: &TransportDeviceId,
    context: &'static str,
) -> Result<PhoneLineTransportDevice, PlatformError> {
    PhoneLineTransportDevice::FromId(&hstring(id)).map_err(|e| classify(&e, context))
}

pub(super) fn bluetooth_selector() -> Result<HSTRING, PlatformError> {
    PhoneLineTransportDevice::GetDeviceSelectorForPhoneLineTransport(PhoneLineTransport::Bluetooth)
        .map_err(|e| classify(&e, "PhoneLineTransportDevice.GetDeviceSelector"))
}

pub(super) fn list_transports(deadline: Deadline) -> Step<Vec<TransportRecord>> {
    let selector = bluetooth_selector()?;
    let wanted: IIterable<HSTRING> = vec![HSTRING::from(ADDRESS_PROPERTY)].into();
    let found = await_op(
        DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties(&selector, &wanted),
        deadline,
        "DeviceInformation.FindAllAsync",
    )?;
    let mut out = Vec::new();
    for info in found {
        if out.len() > MAX_DEVICES {
            // One extra entry lets the engine see that the list was truncated.
            break;
        }
        let Ok(raw_id) = info.Id() else { continue };
        let Ok(id) = TransportDeviceId::new(raw_id.to_string_lossy()) else {
            continue;
        };
        let name = info
            .Name()
            .ok()
            .map(|n| n.to_string_lossy())
            .filter(|n| !n.is_empty())
            .map(Sensitive::new);
        let address = info
            .Properties()
            .ok()
            .and_then(|props| props.Lookup(&HSTRING::from(ADDRESS_PROPERTY)).ok())
            .and_then(|value| value.cast::<IReference<HSTRING>>().ok())
            .and_then(|r| r.Value().ok())
            .map(|a| a.to_string_lossy())
            .filter(|a| !a.is_empty())
            .map(Sensitive::new);
        out.push(TransportRecord {
            id,
            name,
            address,
            enabled: info.IsEnabled().unwrap_or(true),
        });
    }
    Ok(out)
}

pub(super) fn transport_info(id: &TransportDeviceId) -> Step<TransportInfo> {
    let device = open(id, "PhoneLineTransportDevice.FromId")?;
    Ok(TransportInfo {
        transport: device
            .Transport()
            .map(|t| match t {
                PhoneLineTransport::Bluetooth => NativeTransport::Bluetooth,
                PhoneLineTransport::Cellular => NativeTransport::Cellular,
                PhoneLineTransport::VoipApp => NativeTransport::VoipApp,
                other => NativeTransport::Other(other.0),
            })
            .map_err(|e| classify(&e, "PhoneLineTransportDevice.Transport")),
        registered: device
            .IsRegistered()
            .map_err(|e| classify(&e, "PhoneLineTransportDevice.IsRegistered")),
        // Present only on builds with the newer contract; a failure here is "not reported".
        audio_routing: device
            .AudioRoutingStatus()
            .map(|s| match s {
                TransportDeviceAudioRoutingStatus::CanRouteToLocalDevice => {
                    AudioRouting::CanRouteToLocal
                }
                TransportDeviceAudioRoutingStatus::CannotRouteToLocalDevice => {
                    AudioRouting::CannotRouteToLocal
                }
                _ => AudioRouting::Unknown,
            })
            .map_err(|e| classify(&e, "PhoneLineTransportDevice.AudioRoutingStatus")),
        in_band_ringing: device
            .InBandRingingEnabled()
            .map_err(|e| classify(&e, "PhoneLineTransportDevice.InBandRingingEnabled")),
    })
}

pub(super) fn request_access(id: &TransportDeviceId, deadline: Deadline) -> Step<AccessStatus> {
    let device = open(id, "PhoneLineTransportDevice.FromId")?;
    let status = await_op(
        device.RequestAccessAsync(),
        deadline,
        "PhoneLineTransportDevice.RequestAccessAsync",
    )?;
    Ok(match status {
        DeviceAccessStatus::Allowed => AccessStatus::Allowed,
        DeviceAccessStatus::DeniedByUser => AccessStatus::DeniedByUser,
        DeviceAccessStatus::DeniedBySystem => AccessStatus::DeniedBySystem,
        _ => AccessStatus::Unspecified,
    })
}

pub(super) fn register(id: &TransportDeviceId) -> Step<()> {
    open(id, "PhoneLineTransportDevice.FromId")?
        .RegisterApp()
        .map_err(|e| classify(&e, "PhoneLineTransportDevice.RegisterApp"))
}

pub(super) fn unregister(id: &TransportDeviceId) -> Step<()> {
    open(id, "PhoneLineTransportDevice.FromId")?
        .UnregisterApp()
        .map_err(|e| classify(&e, "PhoneLineTransportDevice.UnregisterApp"))
}

pub(super) fn connect(id: &TransportDeviceId, deadline: Deadline) -> Step<bool> {
    let device = open(id, "PhoneLineTransportDevice.FromId")?;
    await_op(
        device.ConnectAsync(),
        deadline,
        "PhoneLineTransportDevice.ConnectAsync",
    )
}
