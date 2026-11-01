//! Change notifications. Observation only: handlers forward a payload-free [`Signal`] and the
//! engine re-reads state, because the events carry no call identity worth trusting.
use super::devices::bluetooth_selector;
use super::wait::{await_op, classify};
use crate::model::*;
use crate::platform::{Deadline, Signal, SignalSink};
use std::time::Duration;
use windows::ApplicationModel::Calls::{
    PhoneCallManager, PhoneLineWatcher, PhoneLineWatcherEventArgs,
};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationUpdate, DeviceWatcher};
use windows::Foundation::{EventHandler, TypedEventHandler};
use windows::core::IInspectable;

/// Live subscriptions. Dropping removes every handler and stops the watchers.
pub(super) struct Signals {
    cleanup: Vec<Box<dyn FnOnce()>>,
}

impl Drop for Signals {
    fn drop(&mut self) {
        for undo in self.cleanup.drain(..).rev() {
            undo();
        }
    }
}

impl Signals {
    pub(super) fn start(sink: SignalSink) -> Step<Self> {
        let mut signals = Signals {
            cleanup: Vec::new(),
        };
        let deadline = Deadline::after(Duration::from_secs(5));

        // Any call state change on this PC.
        let s = sink.clone();
        let token =
            PhoneCallManager::CallStateChanged(&EventHandler::<IInspectable>::new(move |_, _| {
                s(Signal::CallsChanged);
                Ok(())
            }))
            .map_err(|e| classify(&e, "PhoneCallManager.CallStateChanged"))?;
        signals.cleanup.push(Box::new(move || {
            let _ = PhoneCallManager::RemoveCallStateChanged(token);
        }));

        // Lines appearing, disappearing or changing.
        let store = await_op(
            PhoneCallManager::RequestStoreAsync(),
            deadline,
            "PhoneCallManager.RequestStoreAsync",
        )?;
        let watcher = store
            .RequestLineWatcher()
            .map_err(|e| classify(&e, "PhoneCallStore.RequestLineWatcher"))?;
        let ctx = "PhoneLineWatcher events";
        let lines = |sink: SignalSink| {
            TypedEventHandler::<PhoneLineWatcher, PhoneLineWatcherEventArgs>::new(move |_, _| {
                sink(Signal::LinesChanged);
                Ok(())
            })
        };
        let added = watcher
            .LineAdded(&lines(sink.clone()))
            .map_err(|e| classify(&e, ctx))?;
        let removed = watcher
            .LineRemoved(&lines(sink.clone()))
            .map_err(|e| classify(&e, ctx))?;
        let updated = watcher
            .LineUpdated(&lines(sink.clone()))
            .map_err(|e| classify(&e, ctx))?;
        watcher.Start().map_err(|e| classify(&e, ctx))?;
        let w = watcher.clone();
        signals.cleanup.push(Box::new(move || {
            let _ = w.Stop();
            let _ = w.RemoveLineAdded(added);
            let _ = w.RemoveLineRemoved(removed);
            let _ = w.RemoveLineUpdated(updated);
        }));

        // Phones appearing or disappearing as phone-line transports.
        let devices: DeviceWatcher =
            DeviceInformation::CreateWatcherAqsFilter(&bluetooth_selector()?)
                .map_err(|e| classify(&e, "DeviceInformation.CreateWatcher"))?;
        let ctx = "DeviceWatcher events";
        let s = sink.clone();
        let dev_added = devices
            .Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(
                move |_, _| {
                    s(Signal::TransportsChanged);
                    Ok(())
                },
            ))
            .map_err(|e| classify(&e, ctx))?;
        let update = |sink: SignalSink| {
            TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, _| {
                sink(Signal::TransportsChanged);
                Ok(())
            })
        };
        let dev_removed = devices
            .Removed(&update(sink.clone()))
            .map_err(|e| classify(&e, ctx))?;
        let dev_updated = devices
            .Updated(&update(sink))
            .map_err(|e| classify(&e, ctx))?;
        devices.Start().map_err(|e| classify(&e, ctx))?;
        signals.cleanup.push(Box::new(move || {
            let _ = devices.Stop();
            let _ = devices.RemoveAdded(dev_added);
            let _ = devices.RemoveRemoved(dev_removed);
            let _ = devices.RemoveUpdated(dev_updated);
        }));

        Ok(signals)
    }
}
