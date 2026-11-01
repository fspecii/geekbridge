//! CoreAudio implementation of [`AudioPort`] using the plain `AudioObject*` property C API.
//!
//! Reads (`devices`, `default_*`) have no side effects. The two setters change the **system
//! default** device for every application and are only reached through
//! [`crate::route::RouteManager`], which requires explicit endpoints and restores only what it
//! changed. No audio streams are opened, so no microphone permission is exercised here; a
//! consumer that captures from the selected input needs its own `NSMicrophoneUsageDescription`.
//!
//! # Safety reasoning for the `unsafe` blocks
//!
//! Every call passes pointers to locals (or to a `Vec` we sized from `AudioObjectGetPropertyDataSize`)
//! that outlive the call, with sizes equal to the buffer sizes, and never retains them. Property
//! data that CoreAudio returns as a `CFStringRef` follows the Create rule (+1), so ownership is
//! adopted immediately with `CFRetained::from_raw` and released on drop.
use crate::port::{AudioDevice, AudioError, AudioPort, AudioTransport};
use bridge_core::calls::Sensitive;
use core::ffi::c_void;
use core::mem::{MaybeUninit, offset_of, size_of};
use core::ptr::NonNull;
use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertyScope, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyStreamConfiguration, kAudioDevicePropertyTransportType,
    kAudioDeviceTransportTypeAggregate, kAudioDeviceTransportTypeBluetooth,
    kAudioDeviceTransportTypeBluetoothLE, kAudioDeviceTransportTypeBuiltIn,
    kAudioDeviceTransportTypeUSB, kAudioDeviceTransportTypeVirtual,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioObjectUnknown,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_foundation::{CFRetained, CFString};

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

#[derive(Debug, Default)]
pub struct AppleAudio;

fn address(
    selector: AudioObjectPropertySelector,
    scope: AudioObjectPropertyScope,
) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn check(op: &str, status: i32) -> Result<(), AudioError> {
    if status == 0 {
        Ok(())
    } else {
        Err(AudioError::new(
            op,
            Some(status),
            "CoreAudio returned an error status",
        ))
    }
}

/// Size in bytes of a property's data.
fn data_size(
    object: AudioObjectID,
    mut addr: AudioObjectPropertyAddress,
    op: &str,
) -> Result<u32, AudioError> {
    let mut size = 0u32;
    // SAFETY: `addr` and `size` are valid for the duration of the call; no qualifier is passed
    // (size 0, null pointer), which the API permits.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    check(op, status)?;
    Ok(size)
}

/// Read a fixed-size plain-old-data property into a `T`.
///
/// # Safety
/// `T` must be a plain-old-data type for which every bit pattern is valid, and must be exactly
/// the type CoreAudio stores for this property.
unsafe fn read_pod<T: Copy>(
    object: AudioObjectID,
    mut addr: AudioObjectPropertyAddress,
    op: &str,
) -> Result<T, AudioError> {
    let mut out = MaybeUninit::<T>::zeroed();
    let mut size = size_of::<T>() as u32;
    // SAFETY: `out` provides `size` writable bytes; `addr`/`size` are valid for the call.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(out.as_mut_ptr().cast::<c_void>()),
        )
    };
    check(op, status)?;
    if size as usize != size_of::<T>() {
        return Err(AudioError::new(op, None, "unexpected property size"));
    }
    // SAFETY: CoreAudio filled exactly `size_of::<T>()` bytes and the caller guarantees any bit
    // pattern is a valid `T`.
    Ok(unsafe { out.assume_init() })
}

fn read_cfstring(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    op: &str,
) -> Result<String, AudioError> {
    let mut addr = address(selector, kAudioObjectPropertyScopeGlobal);
    let mut raw: *mut CFString = core::ptr::null_mut();
    let mut size = size_of::<*mut CFString>() as u32;
    // SAFETY: `raw` is a writable pointer-sized slot and `size` says so.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut raw).cast::<c_void>(),
        )
    };
    check(op, status)?;
    let ptr = NonNull::new(raw).ok_or_else(|| AudioError::new(op, None, "null string"))?;
    // SAFETY: the property follows the Create rule, so we own one retain count on the string.
    let owned: CFRetained<CFString> = unsafe { CFRetained::from_raw(ptr) };
    Ok(owned.to_string())
}

fn channel_count(object: AudioObjectID, scope: AudioObjectPropertyScope) -> u32 {
    let addr = address(kAudioDevicePropertyStreamConfiguration, scope);
    let Ok(size) = data_size(object, addr, "stream_configuration_size") else {
        return 0;
    };
    let header = offset_of!(AudioBufferList, mBuffers);
    if (size as usize) < header {
        return 0;
    }
    // 8-byte aligned storage: `AudioBufferList` contains a pointer.
    let mut storage = vec![0u64; (size as usize).div_ceil(8)];
    let mut io_size = size;
    let mut addr = addr;
    // SAFETY: `storage` has at least `size` writable bytes; `io_size` states `size`.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            NonNull::from(&mut io_size),
            NonNull::new_unchecked(storage.as_mut_ptr().cast::<c_void>()),
        )
    };
    if status != 0 || (io_size as usize) < header {
        return 0;
    }
    let base = storage.as_ptr().cast::<u8>();
    // SAFETY: the first field (`mNumberBuffers`) lies within the `io_size >= header` bytes read.
    let declared = unsafe { base.cast::<u32>().read() } as usize;
    // Never trust the declared count beyond the bytes actually returned.
    let fit = (io_size as usize - header) / size_of::<AudioBuffer>();
    let mut channels = 0u32;
    for i in 0..declared.min(fit) {
        // SAFETY: element `i` lies within the returned bytes (checked by `fit`), the base pointer
        // is 8-aligned and `AudioBuffer` alignment is at most 8.
        let buf = unsafe { base.add(header).cast::<AudioBuffer>().add(i).read() };
        channels = channels.saturating_add(buf.mNumberChannels);
    }
    channels
}

fn transport_of(object: AudioObjectID) -> AudioTransport {
    // SAFETY: `kAudioDevicePropertyTransportType` is documented as a `UInt32`.
    let value: Result<u32, _> = unsafe {
        read_pod(
            object,
            address(
                kAudioDevicePropertyTransportType,
                kAudioObjectPropertyScopeGlobal,
            ),
            "transport_type",
        )
    };
    match value {
        Ok(t) if t == kAudioDeviceTransportTypeBluetooth => AudioTransport::Bluetooth,
        Ok(t) if t == kAudioDeviceTransportTypeBluetoothLE => AudioTransport::BluetoothLe,
        Ok(t) if t == kAudioDeviceTransportTypeBuiltIn => AudioTransport::BuiltIn,
        Ok(t) if t == kAudioDeviceTransportTypeUSB => AudioTransport::Usb,
        Ok(t) if t == kAudioDeviceTransportTypeVirtual => AudioTransport::Virtual,
        Ok(t) if t == kAudioDeviceTransportTypeAggregate => AudioTransport::Aggregate,
        _ => AudioTransport::Other,
    }
}

fn device_ids() -> Result<Vec<AudioObjectID>, AudioError> {
    let addr = address(
        kAudioHardwarePropertyDevices,
        kAudioObjectPropertyScopeGlobal,
    );
    let size = data_size(SYSTEM, addr, "devices_size")?;
    let count = size as usize / size_of::<AudioObjectID>();
    let mut ids = vec![kAudioObjectUnknown; count];
    let mut io_size = (count * size_of::<AudioObjectID>()) as u32;
    let mut addr = addr;
    if count == 0 {
        return Ok(ids);
    }
    // SAFETY: `ids` provides `io_size` writable bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            NonNull::from(&mut io_size),
            NonNull::new_unchecked(ids.as_mut_ptr().cast::<c_void>()),
        )
    };
    check("devices", status)?;
    ids.truncate(io_size as usize / size_of::<AudioObjectID>());
    Ok(ids)
}

fn uid_of(id: AudioObjectID) -> Result<String, AudioError> {
    read_cfstring(id, kAudioDevicePropertyDeviceUID, "device_uid")
}

fn default_device(
    selector: AudioObjectPropertySelector,
    op: &str,
) -> Result<Option<String>, AudioError> {
    // SAFETY: default-device properties are documented as an `AudioObjectID` (`UInt32`).
    let id: AudioObjectID = unsafe {
        read_pod(
            SYSTEM,
            address(selector, kAudioObjectPropertyScopeGlobal),
            op,
        )?
    };
    if id == kAudioObjectUnknown {
        return Ok(None);
    }
    uid_of(id).map(Some)
}

fn id_for_uid(uid: &str) -> Result<AudioObjectID, AudioError> {
    for id in device_ids()? {
        if uid_of(id).is_ok_and(|u| u == uid) {
            return Ok(id);
        }
    }
    Err(AudioError::new(
        "find_device",
        None,
        "no device with that uid",
    ))
}

fn set_default(
    selector: AudioObjectPropertySelector,
    uid: &str,
    op: &str,
) -> Result<(), AudioError> {
    let mut id = id_for_uid(uid)?;
    let mut addr = address(selector, kAudioObjectPropertyScopeGlobal);
    // SAFETY: `id` is a readable `u32` and `size_of::<u32>()` is passed as its size.
    let status = unsafe {
        AudioObjectSetPropertyData(
            SYSTEM,
            NonNull::from(&mut addr),
            0,
            core::ptr::null(),
            size_of::<AudioObjectID>() as u32,
            NonNull::from(&mut id).cast::<c_void>(),
        )
    };
    check(op, status)
}

impl AudioPort for AppleAudio {
    fn devices(&mut self) -> Result<Vec<AudioDevice>, AudioError> {
        let mut out = Vec::new();
        for id in device_ids()? {
            // A device can vanish between enumeration and inspection; skip it rather than fail.
            let Ok(uid) = uid_of(id) else { continue };
            let name = read_cfstring(id, kAudioObjectPropertyName, "device_name")
                .ok()
                .map(Sensitive::new);
            out.push(AudioDevice {
                uid,
                name,
                transport: transport_of(id),
                input_channels: channel_count(id, kAudioObjectPropertyScopeInput),
                output_channels: channel_count(id, kAudioObjectPropertyScopeOutput),
            });
        }
        Ok(out)
    }

    fn default_input(&mut self) -> Result<Option<String>, AudioError> {
        default_device(kAudioHardwarePropertyDefaultInputDevice, "default_input")
    }

    fn default_output(&mut self) -> Result<Option<String>, AudioError> {
        default_device(kAudioHardwarePropertyDefaultOutputDevice, "default_output")
    }

    fn set_default_input(&mut self, uid: &str) -> Result<(), AudioError> {
        set_default(
            kAudioHardwarePropertyDefaultInputDevice,
            uid,
            "set_default_input",
        )
    }

    fn set_default_output(&mut self, uid: &str) -> Result<(), AudioError> {
        set_default(
            kAudioHardwarePropertyDefaultOutputDevice,
            uid,
            "set_default_output",
        )
    }
}
