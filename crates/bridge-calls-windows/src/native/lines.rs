//! Phone lines and calls: snapshot enumeration, dialing and in-call operations.
use super::wait::{await_op, classify};
use crate::model::*;
use crate::platform::Deadline;
use bridge_core::calls::Sensitive;
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use windows::ApplicationModel::Calls::{
    DtmfKey, DtmfToneAudioPlayback, PhoneCall, PhoneCallAudioDevice, PhoneCallDirection,
    PhoneCallManager, PhoneCallOperationStatus, PhoneCallStatus, PhoneLine,
    PhoneLineOperationStatus, PhoneLineTransport, PhoneLineWatcher, PhoneLineWatcherEventArgs,
};
use windows::Foundation::TypedEventHandler;
use windows::core::{GUID, HSTRING, IInspectable};

pub(super) fn guid_text(g: &GUID) -> String {
    format!("{g:?}").to_ascii_lowercase()
}

fn parse_line(line: &LineId, context: &'static str) -> Result<GUID, PlatformError> {
    GUID::try_from(line.as_str())
        .map_err(|_| PlatformError::new(PlatformErrorKind::InvalidArgument, context))
}

fn transport(t: PhoneLineTransport) -> NativeTransport {
    match t {
        PhoneLineTransport::Bluetooth => NativeTransport::Bluetooth,
        PhoneLineTransport::Cellular => NativeTransport::Cellular,
        PhoneLineTransport::VoipApp => NativeTransport::VoipApp,
        other => NativeTransport::Other(other.0),
    }
}

fn line_record(line: &PhoneLine, id: &GUID) -> Option<LineRecord> {
    let transport_device_id = line
        .TransportDeviceId()
        .ok()
        .map(|s| s.to_string_lossy())
        .filter(|s| !s.is_empty())
        .and_then(|s| TransportDeviceId::new(s).ok());
    Some(LineRecord {
        id: LineId::new(guid_text(id)).ok()?,
        transport: transport(line.Transport().ok()?),
        transport_device_id,
        can_dial: line.CanDial().ok(),
        name: line
            .DisplayName()
            .ok()
            .map(|n| n.to_string_lossy())
            .filter(|n| !n.is_empty())
            .map(Sensitive::new),
    })
}

/// One snapshot of the lines visible to this app: start a line watcher, collect what it adds until
/// `EnumerationCompleted`, stop it. Watching is read-only.
pub(super) fn list_lines(deadline: Deadline) -> Step<Vec<LineRecord>> {
    const CTX: &str = "PhoneCallStore.RequestLineWatcher";
    let store = await_op(
        PhoneCallManager::RequestStoreAsync(),
        deadline,
        "PhoneCallManager.RequestStoreAsync",
    )?;
    let watcher = store.RequestLineWatcher().map_err(|e| classify(&e, CTX))?;

    let ids: Arc<Mutex<Vec<GUID>>> = Arc::default();
    let sink = ids.clone();
    let added = watcher
        .LineAdded(&TypedEventHandler::<
            PhoneLineWatcher,
            PhoneLineWatcherEventArgs,
        >::new(move |_, args| {
            if let Some(id) = args.as_ref().and_then(|a| a.LineId().ok())
                && let Ok(mut list) = sink.lock()
                && list.len() < MAX_LINES
            {
                list.push(id);
            }
            Ok(())
        }))
        .map_err(|e| classify(&e, CTX))?;
    let (done_tx, done_rx) = channel::<()>();
    let completed = watcher
        .EnumerationCompleted(&TypedEventHandler::<PhoneLineWatcher, IInspectable>::new(
            move |_, _| {
                let _ = done_tx.send(());
                Ok(())
            },
        ))
        .map_err(|e| classify(&e, CTX))?;

    let started = watcher.Start().map_err(|e| classify(&e, CTX));
    let waited = started.and_then(|()| {
        done_rx
            .recv_timeout(deadline.remaining())
            .map_err(|_| PlatformError::new(PlatformErrorKind::TimedOut, CTX))
    });
    let _ = watcher.Stop();
    let _ = watcher.RemoveLineAdded(added);
    let _ = watcher.RemoveEnumerationCompleted(completed);
    waited?;

    let found: Vec<GUID> = ids.lock().map(|l| l.clone()).unwrap_or_default();
    let mut out = Vec::new();
    for id in found {
        // A line can vanish between being announced and being opened; skip it.
        let Ok(line) = await_op(
            PhoneLine::FromIdAsync(id),
            deadline,
            "PhoneLine.FromIdAsync",
        ) else {
            continue;
        };
        if let Some(record) = line_record(&line, &id) {
            out.push(record);
        }
    }
    Ok(out)
}

fn open_line(line: &LineId, deadline: Deadline) -> Result<PhoneLine, PlatformError> {
    let guid = parse_line(line, "PhoneLine.FromIdAsync")?;
    await_op(
        PhoneLine::FromIdAsync(guid),
        deadline,
        "PhoneLine.FromIdAsync",
    )
}

fn call_record(call: &PhoneCall, deadline: Deadline) -> Option<CallRecord> {
    let id = NativeCallId::new(call.CallId().ok()?.to_string_lossy()).ok()?;
    let status = match call.Status().ok()? {
        PhoneCallStatus::Lost => NativeCallStatus::Lost,
        PhoneCallStatus::Incoming => NativeCallStatus::Incoming,
        PhoneCallStatus::Dialing => NativeCallStatus::Dialing,
        PhoneCallStatus::Talking => NativeCallStatus::Talking,
        PhoneCallStatus::Held => NativeCallStatus::Held,
        PhoneCallStatus::Ended => NativeCallStatus::Ended,
        other => NativeCallStatus::Other(other.0),
    };
    let audio_device = match call.AudioDevice() {
        Ok(PhoneCallAudioDevice::LocalDevice) => AudioDevice::Local,
        Ok(PhoneCallAudioDevice::RemoteDevice) => AudioDevice::Remote,
        _ => AudioDevice::Unknown,
    };
    // Number, name, direction and hold support live in the call info, which is its own call.
    let info = await_op(
        call.GetPhoneCallInfoAsync(),
        deadline,
        "PhoneCall.GetPhoneCallInfoAsync",
    )
    .ok();
    let text = |s: windows::core::Result<HSTRING>| {
        s.ok()
            .map(|s| s.to_string_lossy())
            .filter(|s| !s.is_empty())
            .map(Sensitive::new)
    };
    Some(CallRecord {
        id,
        status,
        direction: match info.as_ref().and_then(|i| i.CallDirection().ok()) {
            Some(PhoneCallDirection::Incoming) => NativeDirection::Incoming,
            Some(PhoneCallDirection::Outgoing) => NativeDirection::Outgoing,
            _ => NativeDirection::Unknown,
        },
        audio_device,
        muted: call.IsMuted().ok(),
        number: info.as_ref().and_then(|i| text(i.PhoneNumber())),
        name: info.as_ref().and_then(|i| text(i.DisplayName())),
        hold_supported: info.as_ref().and_then(|i| i.IsHoldSupported().ok()),
    })
}

pub(super) fn list_calls(line: &LineId, deadline: Deadline) -> Step<CallListing> {
    const CTX: &str = "PhoneLine.GetAllActivePhoneCallsAsync";
    let line = open_line(line, deadline)?;
    let result = await_op(line.GetAllActivePhoneCallsAsync(), deadline, CTX)?;
    match result.OperationStatus() {
        Ok(PhoneLineOperationStatus::Succeeded) => {}
        Ok(PhoneLineOperationStatus::TimedOut) => {
            return Err(PlatformError::new(PlatformErrorKind::TimedOut, CTX));
        }
        Ok(_) => return Err(PlatformError::new(PlatformErrorKind::Os, CTX)),
        Err(e) => return Err(classify(&e, CTX)),
    }
    let calls = result
        .AllActivePhoneCalls()
        .map_err(|e| classify(&e, CTX))?;
    let mut listing = CallListing::default();
    for call in calls {
        if listing.calls.len() >= MAX_CALLS_PER_DEVICE {
            listing.truncated = true;
            break;
        }
        if let Some(record) = call_record(&call, deadline) {
            listing.calls.push(record);
        }
    }
    Ok(listing)
}

fn op_status(s: PhoneCallOperationStatus) -> OperationStatus {
    match s {
        PhoneCallOperationStatus::Succeeded => OperationStatus::Succeeded,
        PhoneCallOperationStatus::OtherFailure => OperationStatus::OtherFailure,
        PhoneCallOperationStatus::TimedOut => OperationStatus::TimedOut,
        PhoneCallOperationStatus::ConnectionLost => OperationStatus::ConnectionLost,
        PhoneCallOperationStatus::InvalidCallState => OperationStatus::InvalidCallState,
        other => OperationStatus::Other(other.0),
    }
}

pub(super) fn dial(line: &LineId, number: &str, deadline: Deadline) -> Step<DialResult> {
    const CTX: &str = "PhoneLine.DialWithResultAsync";
    let line = open_line(line, deadline)?;
    // The display name is left empty: GeekBridge supplies contact names itself.
    let result = await_op(
        line.DialWithResultAsync(&HSTRING::from(number), &HSTRING::new()),
        deadline,
        CTX,
    )?;
    let status = result
        .DialCallStatus()
        .map(op_status)
        .map_err(|e| classify(&e, CTX))?;
    let call = result
        .DialedCall()
        .ok()
        .and_then(|c| call_record(&c, deadline));
    Ok(DialResult { status, call })
}

pub(super) fn call_op(id: &NativeCallId, op: CallOp, deadline: Deadline) -> Step<OperationStatus> {
    const CTX: &str = "PhoneCall.GetFromId";
    // Resolved afresh on every operation: no handle outlives the call it was issued for.
    let call = PhoneCall::GetFromId(&HSTRING::from(id.as_str())).map_err(|e| classify(&e, CTX))?;
    let status = match op {
        CallOp::Accept => await_op(
            call.AcceptIncomingAsync(),
            deadline,
            "PhoneCall.AcceptIncomingAsync",
        ),
        CallOp::Reject => await_op(
            call.RejectIncomingAsync(),
            deadline,
            "PhoneCall.RejectIncomingAsync",
        ),
        CallOp::End => await_op(call.EndAsync(), deadline, "PhoneCall.EndAsync"),
        CallOp::Hold => await_op(call.HoldAsync(), deadline, "PhoneCall.HoldAsync"),
        CallOp::Resume => await_op(
            call.ResumeFromHoldAsync(),
            deadline,
            "PhoneCall.ResumeFromHoldAsync",
        ),
        CallOp::Mute => await_op(call.MuteAsync(), deadline, "PhoneCall.MuteAsync"),
        CallOp::Unmute => await_op(call.UnmuteAsync(), deadline, "PhoneCall.UnmuteAsync"),
        CallOp::Dtmf(tone) => {
            let key = match tone {
                DtmfTone::Digit(0) => DtmfKey::D0,
                DtmfTone::Digit(1) => DtmfKey::D1,
                DtmfTone::Digit(2) => DtmfKey::D2,
                DtmfTone::Digit(3) => DtmfKey::D3,
                DtmfTone::Digit(4) => DtmfKey::D4,
                DtmfTone::Digit(5) => DtmfKey::D5,
                DtmfTone::Digit(6) => DtmfKey::D6,
                DtmfTone::Digit(7) => DtmfKey::D7,
                DtmfTone::Digit(8) => DtmfKey::D8,
                DtmfTone::Digit(9) => DtmfKey::D9,
                DtmfTone::Star => DtmfKey::Star,
                DtmfTone::Pound => DtmfKey::Pound,
                DtmfTone::Digit(_) => {
                    return Err(PlatformError::new(
                        PlatformErrorKind::InvalidArgument,
                        "DtmfKey",
                    ));
                }
            };
            // The phone generates the tone; the PC does not play it locally.
            await_op(
                call.SendDtmfKeyAsync(key, DtmfToneAudioPlayback::DoNotPlay),
                deadline,
                "PhoneCall.SendDtmfKeyAsync",
            )
        }
        CallOp::Route(to) => {
            let device = match to {
                AudioDevice::Local => PhoneCallAudioDevice::LocalDevice,
                AudioDevice::Remote => PhoneCallAudioDevice::RemoteDevice,
                AudioDevice::Unknown => {
                    return Err(PlatformError::new(
                        PlatformErrorKind::InvalidArgument,
                        "PhoneCall.ChangeAudioDeviceAsync",
                    ));
                }
            };
            await_op(
                call.ChangeAudioDeviceAsync(device),
                deadline,
                "PhoneCall.ChangeAudioDeviceAsync",
            )
        }
    }?;
    Ok(op_status(status))
}
