//! The helper executable's brain: request dispatch and event push over a line sink, generic over
//! the run-loop slice so the same code runs on a Mac (CFRunLoop) and in host tests (no-op).
//!
//! Threading model, which the macOS entry point implements:
//!
//! * the **main thread** owns every Objective-C object and runs the run loop in short slices
//!   ([`RunLoop::run_slice`]); this is where [`Helper::step`] executes, so IOBluetooth is only
//!   ever called there;
//! * a **reader thread** turns stdin into bounded [`Input`] values on a bounded channel;
//! * a **writer thread** owns stdout and drains a bounded channel of finished lines.
//!
//! If the writer cannot keep up the helper fails closed ([`Helper::fatal`]): it stops, tears the
//! link down and restores any owned audio route, rather than blocking the run loop or silently
//! dropping a response.
use crate::controller::{AdapterEvent, Controller, ProbeReport};
use crate::error::AdapterError;
use crate::model::{Clock, PROTOCOL_VERSION};
use crate::port::{AudioPort, NativePort};
use crate::protocol::{Op, Request, Response};
use serde::Serialize;
use std::time::Duration;

/// One unit of input from the reader thread.
#[derive(Debug)]
pub enum Input {
    Request(Request),
    /// Unparseable or oversized input; answered with an error and otherwise ignored.
    Bad(AdapterError),
    Eof,
}

/// Where finished JSON lines go.
pub trait LineSink {
    /// `Err` means the sink is full or closed; the caller must fail closed.
    fn send(&mut self, line: String) -> Result<(), SinkClosed>;
}
#[derive(Debug, PartialEq, Eq)]
pub struct SinkClosed;

impl LineSink for Vec<String> {
    fn send(&mut self, line: String) -> Result<(), SinkClosed> {
        self.push(line);
        Ok(())
    }
}

/// Bounded hand-off to a writer thread.
pub struct ChannelSink(pub std::sync::mpsc::SyncSender<String>);
impl LineSink for ChannelSink {
    fn send(&mut self, line: String) -> Result<(), SinkClosed> {
        self.0.try_send(line).map_err(|_| SinkClosed)
    }
}

/// Run the platform event loop for at most `max`, delivering delegate callbacks.
pub trait RunLoop {
    fn run_slice(&mut self, max: Duration);
}
/// For tests and hosts without a run loop.
pub struct NoRunLoop;
impl RunLoop for NoRunLoop {
    fn run_slice(&mut self, _max: Duration) {}
}

#[derive(Serialize)]
struct Push<'a> {
    v: u32,
    push: &'a AdapterEvent,
}

pub struct Helper<N: NativePort, A: AudioPort, C: Clock, S: LineSink> {
    pub ctl: Controller<N, A, C>,
    sink: S,
    stopping: bool,
    fatal: bool,
}

impl<N: NativePort, A: AudioPort, C: Clock, S: LineSink> Helper<N, A, C, S> {
    pub fn new(ctl: Controller<N, A, C>, sink: S) -> Self {
        Self {
            ctl,
            sink,
            stopping: false,
            fatal: false,
        }
    }
    pub fn sink(&self) -> &S {
        &self.sink
    }
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }
    pub fn stopping(&self) -> bool {
        self.stopping
    }
    /// The output could not keep up; the helper must tear down and exit.
    pub fn fatal(&self) -> bool {
        self.fatal
    }

    fn emit<T: Serialize>(&mut self, value: &T) {
        match serde_json::to_string(value) {
            Ok(line) => {
                if self.sink.send(line).is_err() {
                    self.fatal = true;
                    self.stopping = true;
                }
            }
            // Serialising our own types cannot fail; if it ever does, say so in-band.
            Err(_) => {
                let _ = self.sink.send(
                    r#"{"v":1,"push":{"kind":"diagnostic","diag":{"diag":"serialize_failed"}}}"#
                        .into(),
                );
            }
        }
    }

    fn respond<T: Serialize>(&mut self, id: u64, result: Result<T, AdapterError>) {
        match result {
            Ok(v) => self.emit(&Response::ok(id, v)),
            Err(e) => self.emit(&Response::<()>::err(id, e)),
        }
    }

    /// Forward queued adapter events as `{"v":1,"push":{...}}` lines.
    pub fn flush_events(&mut self) {
        for ev in self.ctl.drain() {
            self.emit(&Push {
                v: PROTOCOL_VERSION,
                push: &ev,
            });
        }
    }

    pub fn handle(&mut self, input: Input) {
        match input {
            Input::Eof => self.stopping = true,
            Input::Bad(e) => self.emit(&Response::<()>::err(0, e)),
            Input::Request(Request { id, op, .. }) => self.dispatch(id, op),
        }
        self.flush_events();
    }

    fn dispatch(&mut self, id: u64, op: Op) {
        match op {
            Op::Probe => {
                let r: Result<ProbeReport, AdapterError> = Ok(self.ctl.probe());
                self.respond(id, r);
            }
            Op::Inventory => {
                let r = self.ctl.inventory();
                self.respond(id, r);
            }
            Op::Connect { address } => {
                let r = self
                    .ctl
                    .connect(&address)
                    .map(|g| Generation { generation: g });
                self.respond(id, r);
            }
            Op::Disconnect => {
                let r = self.ctl.disconnect();
                self.respond(id, r);
            }
            Op::Command { command } => {
                let r = self.ctl.execute(command).map(|t| Accepted { ticket: t });
                self.respond(id, r);
            }
            Op::CoreCommand { command } => {
                let r = self
                    .ctl
                    .execute_core(&command)
                    .map(|t| Accepted { ticket: t });
                self.respond(id, r);
            }
            Op::AudioDevices => {
                let r = self.ctl.audio_devices();
                self.respond(id, r);
            }
            Op::RouteApply { route } => {
                let r = self.ctl.route_apply(&route);
                self.respond(id, r);
            }
            Op::RouteRestore => {
                let r = self.ctl.route_restore();
                self.respond(id, r);
            }
            Op::Status => {
                let r: Result<_, AdapterError> = Ok(self.ctl.snapshot());
                self.respond(id, r);
            }
            Op::Shutdown => {
                self.stopping = true;
                self.respond(id, Ok::<_, AdapterError>(()));
            }
        }
    }

    /// One iteration of the main loop body: advance the controller and push its events.
    pub fn step(&mut self) {
        self.ctl.pump();
        self.flush_events();
    }

    /// Tear down: disconnect any link, wait (bounded) for it to end, restore owned routes.
    pub fn shutdown(&mut self, run_loop: &mut impl RunLoop) {
        let _ = self.ctl.disconnect();
        let started = self.ctl.now();
        let limit = self.ctl.config().close_timeout_ms + 1_000;
        while self.ctl.phase().is_some() && self.ctl.now().since(started) <= limit {
            run_loop.run_slice(Duration::from_millis(25));
            self.step();
        }
        // A link that never reported back is finalised by the close timeout inside `tick`; a
        // route still owned after that is restored explicitly.
        if self.ctl.routes_mut().has_owned_changes() {
            let _ = self.ctl.route_restore();
        }
        self.flush_events();
    }
}

#[derive(Serialize)]
struct Generation {
    generation: crate::model::Generation,
}
#[derive(Serialize)]
struct Accepted {
    ticket: crate::controller::Ticket,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::Config;
    use crate::mock::{ManualClock, MockAudio, MockNative};
    use crate::model::{Indicator, IoStatus, NativeEvent};
    use crate::protocol::parse_request;
    use crate::route::RouteRequest;
    use serde_json::Value;

    type TestHelper = Helper<MockNative, MockAudio, ManualClock, Vec<String>>;

    fn helper() -> (TestHelper, ManualClock) {
        let clock = ManualClock::new();
        let ctl = Controller::new(
            MockNative::new(),
            MockAudio::standard(),
            clock.clone(),
            Config::default(),
        );
        (Helper::new(ctl, Vec::new()), clock)
    }
    fn send(h: &mut TestHelper, json: &str) {
        let req = parse_request(json).expect("valid request");
        h.handle(Input::Request(req));
    }
    fn lines(h: &mut TestHelper) -> Vec<Value> {
        std::mem::take(h.sink_mut())
            .iter()
            .map(|l| serde_json::from_str(l).expect("every line is JSON"))
            .collect()
    }
    fn responses(v: &[Value]) -> Vec<&Value> {
        v.iter().filter(|x| x.get("ok").is_some()).collect()
    }
    fn pushes(v: &[Value]) -> Vec<&Value> {
        v.iter()
            .filter(|x| x.get("push").is_some())
            .map(|x| &x["push"])
            .collect()
    }

    /// Advances time on every slice so bounded waits terminate.
    struct TickingLoop(ManualClock);
    impl RunLoop for TickingLoop {
        fn run_slice(&mut self, _max: Duration) {
            self.0.advance(100);
        }
    }

    #[test]
    fn probe_answers_without_touching_a_phone() {
        let (mut h, _) = helper();
        send(&mut h, r#"{"v":1,"id":1,"op":"probe"}"#);
        let out = lines(&mut h);
        let r = responses(&out);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0]["ok"], true);
        assert_eq!(r[0]["id"], 1);
        assert_eq!(r[0]["result"]["ag_capable"], 1);
        assert_eq!(
            r[0]["result"]["capabilities"]["planes"]["control"]["status"],
            "unknown"
        );
        let n = h.ctl.native_mut();
        assert!(n.opened.is_empty() && n.performed.is_empty());
    }

    #[test]
    fn connect_command_and_events_flow_end_to_end() {
        let (mut h, _) = helper();
        send(
            &mut h,
            r#"{"v":1,"id":2,"op":"connect","address":"11-22-33-44-55-66"}"#,
        );
        let out = lines(&mut h);
        assert_eq!(responses(&out)[0]["result"]["generation"], 1);
        assert!(
            pushes(&out)
                .iter()
                .any(|p| p["kind"] == "link" && p["notice"]["state"] == "connecting")
        );

        let g = h.ctl.generation().unwrap();
        h.ctl.native_mut().slc_up(g);
        h.ctl.native_mut().indicator(g, Indicator::CallSetup(1));
        h.step();
        let out = lines(&mut h);
        let p = pushes(&out);
        assert!(
            p.iter()
                .any(|p| p["kind"] == "link" && p["notice"]["state"] == "up")
        );
        assert!(
            p.iter()
                .any(|p| p["kind"] == "call" && p["event"]["type"] == "call_updated")
        );

        send(
            &mut h,
            r#"{"v":1,"id":3,"op":"command","command":{"type":"answer","call":"mac-g1-c1"}}"#,
        );
        let out = lines(&mut h);
        assert_eq!(responses(&out)[0]["ok"], true);
        assert_eq!(responses(&out)[0]["result"]["ticket"], 1);
        h.ctl.native_mut().indicator(g, Indicator::Call(true));
        h.ctl.native_mut().indicator(g, Indicator::CallSetup(0));
        h.step();
        let out = lines(&mut h);
        assert!(
            pushes(&out)
                .iter()
                .any(|p| p["kind"] == "command" && p["outcome"]["result"] == "confirmed")
        );
    }

    #[test]
    fn core_commands_are_accepted_verbatim_and_unmappable_ones_refused() {
        let (mut h, _) = helper();
        send(
            &mut h,
            r#"{"v":1,"id":1,"op":"connect","address":"11:22:33:44:55:66"}"#,
        );
        let g = h.ctl.generation().unwrap();
        h.ctl.native_mut().slc_up(g);
        h.ctl.native_mut().indicator(g, Indicator::Call(true));
        h.step();
        lines(&mut h);
        send(
            &mut h,
            r#"{"v":1,"id":2,"op":"core_command","command":{"type":"set_microphone_mute","muted":true}}"#,
        );
        send(
            &mut h,
            r#"{"v":1,"id":3,"op":"core_command","command":{"type":"send_tones","tones":"12"}}"#,
        );
        let out = lines(&mut h);
        let r = responses(&out);
        assert_eq!(r[0]["ok"], false);
        assert_eq!(r[0]["error"]["error"], "refused");
        assert_eq!(r[0]["error"]["refusal"]["reason"], "no_platform_mapping");
        assert_eq!(r[1]["ok"], true);
    }

    #[test]
    fn errors_are_structured_and_do_not_end_the_helper() {
        let (mut h, _) = helper();
        send(
            &mut h,
            r#"{"v":1,"id":4,"op":"command","command":{"type":"hold"}}"#,
        );
        h.handle(Input::Bad(AdapterError::Protocol {
            detail: "request line too long".into(),
        }));
        let out = lines(&mut h);
        let r = responses(&out);
        assert_eq!(r[0]["error"]["error"], "no_active_link");
        assert_eq!(r[1]["ok"], false);
        assert_eq!(r[1]["error"]["error"], "protocol");
        assert!(!h.stopping());
    }

    #[test]
    fn eof_and_shutdown_request_stop() {
        let (mut h, _) = helper();
        h.handle(Input::Eof);
        assert!(h.stopping());
        let (mut h, _) = helper();
        send(&mut h, r#"{"v":1,"id":9,"op":"shutdown"}"#);
        assert!(h.stopping());
        assert_eq!(responses(&lines(&mut h))[0]["ok"], true);
    }

    #[test]
    fn a_full_output_sink_fails_closed() {
        struct Full;
        impl LineSink for Full {
            fn send(&mut self, _line: String) -> Result<(), SinkClosed> {
                Err(SinkClosed)
            }
        }
        let clock = ManualClock::new();
        let ctl = Controller::new(
            MockNative::new(),
            MockAudio::standard(),
            clock,
            Config::default(),
        );
        let mut h = Helper::new(ctl, Full);
        h.handle(Input::Request(
            parse_request(r#"{"v":1,"id":1,"op":"probe"}"#).unwrap(),
        ));
        assert!(h.fatal() && h.stopping());
    }

    #[test]
    fn shutdown_disconnects_and_restores_owned_route() {
        let (mut h, clock) = helper();
        send(
            &mut h,
            r#"{"v":1,"id":1,"op":"connect","address":"11:22:33:44:55:66"}"#,
        );
        let g = h.ctl.generation().unwrap();
        h.ctl.native_mut().slc_up(g);
        h.step();
        let req = RouteRequest {
            session: "s".into(),
            input_uid: "bt-in".into(),
            output_uid: "bt-out".into(),
        };
        h.ctl.route_apply(&req).unwrap();
        assert_eq!(
            h.ctl
                .routes_mut()
                .port_mut()
                .default_output()
                .unwrap()
                .as_deref(),
            Some("bt-out")
        );
        lines(&mut h);

        // The phone never answers the disconnect: the bounded wait still ends the link and
        // the route is put back.
        h.shutdown(&mut TickingLoop(clock));
        assert_eq!(h.ctl.phase(), None);
        assert_eq!(h.ctl.native_mut().closed, vec![g]);
        assert_eq!(
            h.ctl
                .routes_mut()
                .port_mut()
                .default_output()
                .unwrap()
                .as_deref(),
            Some("builtin-out")
        );
        assert_eq!(
            h.ctl
                .routes_mut()
                .port_mut()
                .default_input()
                .unwrap()
                .as_deref(),
            Some("builtin-in")
        );
        let out = lines(&mut h);
        let p = pushes(&out);
        assert!(p.iter().any(|p| p["kind"] == "route_restored"));
        assert!(
            p.iter()
                .any(|p| p["kind"] == "link" && p["notice"]["state"] == "down")
        );
    }

    #[test]
    fn shutdown_with_orderly_disconnect_callback() {
        let (mut h, clock) = helper();
        send(
            &mut h,
            r#"{"v":1,"id":1,"op":"connect","address":"11:22:33:44:55:66"}"#,
        );
        let g = h.ctl.generation().unwrap();
        h.ctl.native_mut().slc_up(g);
        h.step();
        h.ctl
            .native_mut()
            .emit(g, NativeEvent::SlcDisconnected(IoStatus::SUCCESS));
        // Not yet processed; shutdown pumps it.
        h.shutdown(&mut TickingLoop(clock));
        assert_eq!(h.ctl.phase(), None);
    }
}
