//! Apple-framework bindings (macOS only): IOBluetooth hands-free calls, CoreAudio defaults and the
//! main-thread run loop. All `unsafe` in the crate lives in this module tree; each block carries a
//! safety argument. The platform-independent logic (state machine, planning, protocol) is in the
//! parent modules and is exercised by host tests; this layer is only type-checked off-Mac.
pub mod audio;
pub mod bluetooth;
pub mod delegate;
pub mod runloop;

use crate::controller::{Config, Controller};
use crate::helper::{ChannelSink, Helper, Input, RunLoop};
use crate::model::SystemClock;
use crate::protocol::{Line, MAX_LINE, parse_request};
use crate::{AdapterError, cli};
use objc2::MainThreadMarker;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

pub use audio::AppleAudio;
pub use bluetooth::AppleBluetooth;
pub use runloop::MainRunLoop;

pub type AppleController = Controller<AppleBluetooth, AppleAudio, SystemClock>;

/// Build the production controller. Requires the main thread (proved by the marker).
pub fn controller(main: MainThreadMarker, cfg: Config) -> AppleController {
    Controller::new(
        AppleBluetooth::new(main),
        AppleAudio,
        SystemClock::new(),
        cfg,
    )
}

/// Default action: one read-only probe printed as JSON, then exit.
pub fn probe(cfg: Config) -> ExitCode {
    let Some(main) = MainThreadMarker::new() else {
        eprintln!("geekbridge-macos-calls: must run on the main thread");
        return ExitCode::from(2);
    };
    let mut ctl = controller(main, cfg);
    let report = ctl.probe();
    let blocked = report.blocked.is_some();
    println!(
        "{}",
        serde_json::to_string(&report).unwrap_or_else(|_| "{}".into())
    );
    cli::exit_for_probe(blocked)
}

/// Long-running helper: bounded JSON lines on stdin/stdout, run loop on this (main) thread.
pub fn serve(cfg: Config) -> ExitCode {
    let Some(main) = MainThreadMarker::new() else {
        eprintln!("geekbridge-macos-calls: must run on the main thread");
        return ExitCode::from(2);
    };
    let (in_tx, in_rx) = sync_channel::<Input>(64);
    let (out_tx, out_rx) = sync_channel::<String>(1024);

    // Reader: stdin -> bounded parsed requests. Owns no Objective-C objects.
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        loop {
            let input = match crate::protocol::read_bounded_line(&mut lock, MAX_LINE) {
                Ok(Line::Text(t)) if t.trim().is_empty() => continue,
                Ok(Line::Text(t)) => match parse_request(&t) {
                    Ok(r) => Input::Request(r),
                    Err(e) => Input::Bad(e),
                },
                Ok(Line::TooLong) => Input::Bad(AdapterError::Protocol {
                    detail: "request line too long".into(),
                }),
                Ok(Line::Eof) | Err(_) => {
                    let _ = in_tx.send(Input::Eof);
                    return;
                }
            };
            if in_tx.send(input).is_err() {
                return;
            }
        }
    });

    // Writer: finished lines -> stdout. A broken pipe ends it; the main loop then fails closed.
    let writer = std::thread::spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for line in out_rx {
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                return;
            }
        }
    });

    let ctl = controller(main, cfg);
    let mut helper = Helper::new(ctl, ChannelSink(out_tx));
    let mut run_loop = MainRunLoop::new(main);
    while !helper.stopping() {
        run_loop.run_slice(Duration::from_millis(25));
        while let Ok(input) = in_rx.try_recv() {
            helper.handle(input);
        }
        helper.step();
    }
    helper.shutdown(&mut run_loop);
    let failed = helper.fatal();
    drop(helper);
    let _ = writer.join();
    ExitCode::from(u8::from(failed))
}
