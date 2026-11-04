// SPDX-License-Identifier: GPL-3.0-or-later
//! PulseAudio / `pipewire-pulse` adapter over libpulse's native API (no `pactl`, no subprocess).
//!
//! Uses the single-threaded libpulse main loop, pumped manually so every wait has a deadline:
//! libpulse's blocking calls and the threaded main loop offer none. The context is connected with
//! `NOAUTOSPAWN`, so connecting can never start a sound server. Reads use only the introspection
//! calls `get_*_info_list` / `get_server_info`; writes are limited to moving a stream, switching
//! a card profile and muting a source. The level meter opens a short-lived record stream.
//!
//! Exercised against a private `pipewire` + `pipewire-pulse` + `wireplumber` instance by
//! `scripts/e2e/call-audio-sandbox.sh` (see `tests/audio_sandbox.rs`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OpState};
use libpulse_binding::proplist::{Proplist, properties};

use crate::audio::model::{
    CardInfo, CardProfile, Props, ServerIdentity, ServerSnapshot, SinkInfo, SourceInfo, StreamInfo,
};
use crate::audio::server::{AudioServer, PeakTarget, ServerError};

const INVALID_INDEX: u32 = u32::MAX;

fn index(i: u32) -> Option<u32> {
    (i != INVALID_INDEX).then_some(i)
}

/// Server identity plus the default sink and source names.
type IdentityAndDefaults = (ServerIdentity, Option<String>, Option<String>);

#[derive(Clone, Debug)]
pub struct PulseOptions {
    /// `None`: the default server of the user's session. Never autospawned.
    pub server: Option<String>,
    pub client_name: String,
    pub connect_timeout: Duration,
    /// Deadline for each individual server round trip.
    pub op_timeout: Duration,
}

impl Default for PulseOptions {
    fn default() -> Self {
        Self {
            server: None,
            client_name: "GeekBridge".into(),
            connect_timeout: Duration::from_secs(3),
            op_timeout: Duration::from_secs(3),
        }
    }
}

struct Connection {
    mainloop: Mainloop,
    context: Context,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.context.disconnect();
    }
}

pub struct PulseServer {
    opts: PulseOptions,
    conn: Option<Connection>,
}

// SAFETY: libpulse objects are not `Send` because the C library is not thread-safe, but a
// `PulseServer` is never shared or cloned and every method takes `&mut self`: it is used by one
// thread at a time. Moving the whole value (main loop, context and all pending state) to another
// thread while no call is in progress is sound for the single-threaded main loop, which keeps no
// thread-local state.
unsafe impl Send for PulseServer {}

fn props_of(p: &libpulse_binding::proplist::Proplist) -> Props {
    p.iter()
        .filter_map(|k| p.get_str(&k).map(|v| (k, v)))
        .collect()
}

fn text(s: &Option<std::borrow::Cow<'_, str>>) -> String {
    s.as_deref().unwrap_or_default().to_string()
}

/// Run an introspection list call and collect converted items.
macro_rules! collect_list {
    ($self:ident, $what:expr, $method:ident, $convert:expr) => {{
        let out = Rc::new(RefCell::new(Vec::new()));
        let done = Rc::new(Cell::new(false));
        let failed = Rc::new(Cell::new(false));
        let (o, d, f) = (out.clone(), done.clone(), failed.clone());
        let convert = $convert;
        let op = $self
            .conn()?
            .context
            .introspect()
            .$method(move |r| match r {
                ListResult::Item(i) => o.borrow_mut().push(convert(i)),
                ListResult::End => d.set(true),
                ListResult::Error => {
                    f.set(true);
                    d.set(true);
                }
            });
        $self.wait(op, &|| done.get(), $what)?;
        if failed.get() {
            return Err(ServerError::Rejected {
                what: $what,
                detail: "the server failed the request".into(),
            });
        }
        let items = out.borrow().clone();
        items
    }};
}

impl PulseServer {
    pub fn connect(opts: PulseOptions) -> Result<Self, ServerError> {
        let conn = Self::open(&opts)?;
        Ok(Self {
            opts,
            conn: Some(conn),
        })
    }

    fn open(opts: &PulseOptions) -> Result<Connection, ServerError> {
        let unavailable = |m: &str| ServerError::Unavailable(m.to_string());
        let mut props =
            Proplist::new().ok_or_else(|| unavailable("cannot allocate property list"))?;
        props
            .set_str(properties::APPLICATION_NAME, &opts.client_name)
            .map_err(|()| unavailable("cannot set client name"))?;
        let mainloop = Mainloop::new().ok_or_else(|| unavailable("cannot create main loop"))?;
        let mut context = Context::new_with_proplist(&mainloop, &opts.client_name, &props)
            .ok_or_else(|| unavailable("cannot create context"))?;
        context
            .connect(opts.server.as_deref(), FlagSet::NOAUTOSPAWN, None)
            .map_err(|e| ServerError::Unavailable(format!("connect: {e}")))?;
        let mut conn = Connection { mainloop, context };
        let deadline = Instant::now() + opts.connect_timeout;
        loop {
            match conn.context.get_state() {
                State::Ready => return Ok(conn),
                State::Failed | State::Terminated => {
                    return Err(unavailable("no audio server answered"));
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                return Err(unavailable("timed out connecting to the audio server"));
            }
            Self::turn(&mut conn.mainloop)?;
        }
    }

    /// One non-blocking main-loop turn; sleeps briefly when idle.
    fn turn(mainloop: &mut Mainloop) -> Result<(), ServerError> {
        match mainloop.iterate(false) {
            IterateResult::Success(0) => std::thread::sleep(Duration::from_millis(2)),
            IterateResult::Success(_) => {}
            IterateResult::Quit(_) | IterateResult::Err(_) => {
                return Err(ServerError::ConnectionLost("main loop stopped".into()));
            }
        }
        Ok(())
    }

    fn conn(&mut self) -> Result<&mut Connection, ServerError> {
        match self.conn.as_mut() {
            Some(c) if !matches!(c.context.get_state(), State::Failed | State::Terminated) => Ok(c),
            _ => Err(ServerError::ConnectionLost("not connected".into())),
        }
    }

    /// Pump until `done`, the deadline, or connection loss; cancel the operation on timeout.
    fn wait<T: ?Sized>(
        &mut self,
        mut op: Operation<T>,
        done: &dyn Fn() -> bool,
        what: &'static str,
    ) -> Result<(), ServerError> {
        let timeout = self.opts.op_timeout;
        let conn = self.conn()?;
        let deadline = Instant::now() + timeout;
        loop {
            if done() || op.get_state() == OpState::Done {
                return Ok(());
            }
            if op.get_state() == OpState::Cancelled
                || matches!(conn.context.get_state(), State::Failed | State::Terminated)
            {
                return Err(ServerError::ConnectionLost(format!(
                    "connection closed during {what}"
                )));
            }
            if Instant::now() >= deadline {
                op.cancel();
                return Err(ServerError::Timeout { what });
            }
            Self::turn(&mut conn.mainloop)?;
        }
    }

    fn server_identity(&mut self) -> Result<IdentityAndDefaults, ServerError> {
        let cell: Rc<RefCell<Option<IdentityAndDefaults>>> = Rc::default();
        let c = cell.clone();
        let op = self.conn()?.context.introspect().get_server_info(move |i| {
            let own = |s: &Option<std::borrow::Cow<'_, str>>| s.as_deref().map(str::to_string);
            *c.borrow_mut() = Some((
                ServerIdentity {
                    server_name: text(&i.server_name),
                    server_version: text(&i.server_version),
                    host_name: text(&i.host_name),
                    cookie: i.cookie,
                },
                own(&i.default_sink_name),
                own(&i.default_source_name),
            ));
        });
        let probe = cell.clone();
        self.wait(op, &move || probe.borrow().is_some(), "server info")?;
        let v = cell.borrow_mut().take();
        v.ok_or_else(|| ServerError::Rejected {
            what: "server info",
            detail: "empty reply".into(),
        })
    }
}

impl PulseServer {
    /// Wait for a write whose callback reports success, mapping refusal to `Rejected`.
    fn ack(
        &mut self,
        what: &'static str,
        start: impl FnOnce(
            &mut libpulse_binding::context::introspect::Introspector,
            Box<dyn FnMut(bool)>,
        ) -> Operation<dyn FnMut(bool)>,
    ) -> Result<(), ServerError> {
        let cell = Rc::new(Cell::new(None));
        let c = cell.clone();
        let mut introspect = self.conn()?.context.introspect();
        let op = start(&mut introspect, Box::new(move |ok| c.set(Some(ok))));
        let probe = cell.clone();
        self.wait(op, &move || probe.get().is_some(), what)?;
        match cell.get() {
            Some(true) => Ok(()),
            _ => {
                let errno = self
                    .conn
                    .as_ref()
                    .and_then(|c| c.context.errno().to_string())
                    .unwrap_or_default();
                Err(ServerError::Rejected {
                    what,
                    detail: errno,
                })
            }
        }
    }

    fn checked(name: &str, what: &'static str) -> Result<(), ServerError> {
        if name.is_empty() || name.contains('\0') {
            return Err(ServerError::Rejected {
                what,
                detail: "empty name or NUL in name".into(),
            });
        }
        Ok(())
    }

    /// Record float32 mono samples from `device` (optionally only one sink input's samples) for
    /// `window` and return the highest absolute value.
    fn measure(
        &mut self,
        device: &str,
        monitor_of: Option<u32>,
        window: Duration,
    ) -> Result<f32, ServerError> {
        use libpulse_binding::def::BufferAttr;
        use libpulse_binding::sample::{Format, Spec};
        use libpulse_binding::stream::{FlagSet as SFlags, PeekResult, State as SState, Stream};

        let spec = Spec {
            format: Format::F32le,
            channels: 1,
            rate: 8000,
        };
        let op_timeout = self.opts.op_timeout;
        let conn = self.conn()?;
        let mut props = Proplist::new().ok_or_else(|| ServerError::Rejected {
            what: "level meter",
            detail: "cannot allocate property list".into(),
        })?;
        let _ = props.set_str(properties::MEDIA_ROLE, "abstract");
        let _ = props.set_str("geekbridge.purpose", "call-audio-level");
        let mut stream = Stream::new_with_proplist(
            &mut conn.context,
            "GeekBridge call audio level",
            &spec,
            None,
            &mut props,
        )
        .ok_or_else(|| ServerError::Rejected {
            what: "level meter",
            detail: "cannot create record stream".into(),
        })?;
        if let Some(i) = monitor_of {
            stream
                .set_monitor_stream(i)
                .map_err(|e| ServerError::Rejected {
                    what: "level meter",
                    detail: e.to_string().unwrap_or_default(),
                })?;
        }
        // ~20 ms fragments so a short window still sees several.
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 640,
        };
        stream
            .connect_record(
                Some(device),
                Some(&attr),
                SFlags::DONT_MOVE | SFlags::ADJUST_LATENCY,
            )
            .map_err(|e| ServerError::Rejected {
                what: "level meter",
                detail: e.to_string().unwrap_or_default(),
            })?;
        let deadline = Instant::now() + op_timeout;
        loop {
            match stream.get_state() {
                SState::Ready => break,
                SState::Failed | SState::Terminated => {
                    let detail = conn.context.errno().to_string().unwrap_or_default();
                    return Err(ServerError::Rejected {
                        what: "level meter",
                        detail,
                    });
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                let _ = stream.disconnect();
                return Err(ServerError::Timeout {
                    what: "level meter",
                });
            }
            Self::turn(&mut conn.mainloop)?;
        }
        let end = Instant::now() + window;
        let mut peak = 0f32;
        while Instant::now() < end {
            Self::turn(&mut conn.mainloop)?;
            loop {
                match stream.peek() {
                    Ok(PeekResult::Empty) | Err(_) => break,
                    Ok(PeekResult::Hole(_)) => {
                        let _ = stream.discard();
                    }
                    Ok(PeekResult::Data(bytes)) => {
                        for c in bytes.as_chunks::<4>().0 {
                            let v = f32::from_le_bytes(*c).abs();
                            if v.is_finite() && v > peak {
                                peak = v;
                            }
                        }
                        let _ = stream.discard();
                    }
                }
            }
            if matches!(stream.get_state(), SState::Failed | SState::Terminated) {
                break;
            }
        }
        let _ = stream.disconnect();
        Ok(peak.min(1.0))
    }
}

fn stream_name(name: &Option<std::borrow::Cow<'_, str>>, props: &Props) -> String {
    props
        .get(crate::audio::model::PROP_NODE_NAME)
        .cloned()
        .unwrap_or_else(|| text(name))
}

impl AudioServer for PulseServer {
    fn snapshot(&mut self) -> Result<ServerSnapshot, ServerError> {
        let (identity, default_sink, default_source) = self.server_identity()?;
        let cards = collect_list!(
            self,
            "card list",
            get_card_info_list,
            |c: &libpulse_binding::context::introspect::CardInfo| {
                CardInfo {
                    index: c.index,
                    name: text(&c.name),
                    props: props_of(&c.proplist),
                    profiles: c
                        .profiles
                        .iter()
                        .map(|p| CardProfile {
                            name: text(&p.name),
                            available: p.available,
                        })
                        .collect(),
                    active_profile: c.active_profile.as_ref().map(|p| text(&p.name)),
                }
            }
        );
        let sinks = collect_list!(
            self,
            "sink list",
            get_sink_info_list,
            |s: &libpulse_binding::context::introspect::SinkInfo| {
                SinkInfo {
                    index: s.index,
                    name: text(&s.name),
                    description: text(&s.description),
                    card: s.card,
                    mute: s.mute,
                    props: props_of(&s.proplist),
                }
            }
        );
        let sources = collect_list!(
            self,
            "source list",
            get_source_info_list,
            |s: &libpulse_binding::context::introspect::SourceInfo| {
                SourceInfo {
                    index: s.index,
                    name: text(&s.name),
                    description: text(&s.description),
                    card: s.card,
                    mute: s.mute,
                    monitor_of_sink: s.monitor_of_sink,
                    props: props_of(&s.proplist),
                }
            }
        );
        let sink_inputs = collect_list!(
            self,
            "playback stream list",
            get_sink_input_info_list,
            |s: &libpulse_binding::context::introspect::SinkInputInfo| {
                let props = props_of(&s.proplist);
                StreamInfo {
                    index: s.index,
                    name: stream_name(&s.name, &props),
                    device: index(s.sink),
                    corked: s.corked,
                    mute: s.mute,
                    props,
                }
            }
        );
        let source_outputs = collect_list!(
            self,
            "capture stream list",
            get_source_output_info_list,
            |s: &libpulse_binding::context::introspect::SourceOutputInfo| {
                let props = props_of(&s.proplist);
                StreamInfo {
                    index: s.index,
                    name: stream_name(&s.name, &props),
                    device: index(s.source),
                    corked: s.corked,
                    mute: s.mute,
                    props,
                }
            }
        );
        Ok(ServerSnapshot {
            identity,
            default_sink,
            default_source,
            cards,
            sinks,
            sources,
            sink_inputs,
            source_outputs,
        })
    }

    fn move_sink_input(&mut self, index: u32, sink: &str) -> Result<(), ServerError> {
        Self::checked(sink, "moving the call's playback stream")?;
        self.ack("moving the call's playback stream", |i, cb| {
            i.move_sink_input_by_name(index, sink, Some(cb))
        })
    }

    fn move_source_output(&mut self, index: u32, source: &str) -> Result<(), ServerError> {
        Self::checked(source, "moving the call's capture stream")?;
        self.ack("moving the call's capture stream", |i, cb| {
            i.move_source_output_by_name(index, source, Some(cb))
        })
    }

    fn set_card_profile(&mut self, card: u32, profile: &str) -> Result<(), ServerError> {
        Self::checked(profile, "switching the card profile")?;
        self.ack("switching the card profile", |i, cb| {
            i.set_card_profile_by_index(card, profile, Some(cb))
        })
    }

    fn set_source_mute(&mut self, source: &str, mute: bool) -> Result<(), ServerError> {
        Self::checked(source, "muting the microphone")?;
        self.ack("muting the microphone", |i, cb| {
            i.set_source_mute_by_name(source, mute, Some(cb))
        })
    }

    fn peak(&mut self, target: &PeakTarget, window: Duration) -> Result<Option<f32>, ServerError> {
        match target {
            PeakTarget::SinkInput { index, sink } => {
                Self::checked(sink, "level meter")?;
                let monitor = format!("{sink}.monitor");
                self.measure(&monitor, Some(*index), window).map(Some)
            }
            PeakTarget::Source(name) => {
                Self::checked(name, "level meter")?;
                self.measure(name, None, window).map(Some)
            }
        }
    }

    fn reconnect(&mut self) -> Result<(), ServerError> {
        // Drop the old connection first so a half-dead context cannot linger.
        self.conn = None;
        self.conn = Some(Self::open(&self.opts)?);
        Ok(())
    }
}
