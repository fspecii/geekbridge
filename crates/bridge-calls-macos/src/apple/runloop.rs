// SPDX-License-Identifier: GPL-3.0-or-later
//! The process main thread's `CFRunLoop`, driven in short slices.
//!
//! IOBluetooth delivers delegate callbacks through run loops; Apple does not say which one, so the
//! helper owns the main thread's loop and pumps it continuously. Slicing (rather than a blocking
//! `CFRunLoopRun`) lets the same thread also service the request queue and the controller's timers
//! without extra wake-up plumbing. The cost is that a queued request waits at most one slice.
use crate::helper::RunLoop;
use objc2::MainThreadMarker;
use objc2_core_foundation::{CFRunLoop, CFRunLoopRunResult, kCFRunLoopDefaultMode};
use std::time::Duration;

pub struct MainRunLoop {
    _main: MainThreadMarker,
}

impl MainRunLoop {
    pub fn new(main: MainThreadMarker) -> Self {
        Self { _main: main }
    }
}

impl RunLoop for MainRunLoop {
    fn run_slice(&mut self, max: Duration) {
        // SAFETY: reading an immutable framework-defined global constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let result = CFRunLoop::run_in_mode(mode, max.as_secs_f64(), false);
        // With no sources or timers registered in the mode the call returns at once. Sleep out the
        // slice so an idle helper does not spin a core.
        if result == CFRunLoopRunResult::Finished {
            std::thread::sleep(max);
        }
    }
}
