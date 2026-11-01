//! PulseAudio / `pipewire-pulse` adapter over libpulse's native API (no `pactl`, no subprocess).
//!
//! Uses the single-threaded libpulse main loop, pumped manually so every wait has a deadline:
//! libpulse's blocking calls and the threaded main loop offer none. The context is connected with
//! `NOAUTOSPAWN`, so connecting can never start a sound server. Reads use only the introspection
//! calls `get_*_info_list` / `get_server_info`; writes are limited to `load_module` and
//! `unload_module`.
//!
//! This adapter has been compiled and its read path run against the host's server, but no module
//! has been loaded through it on a real desktop (see the implementation notes).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::operation::{Operation, State as OpState};
use libpulse_binding::proplist::{Proplist, properties};

use crate::audio::model::{
    CardInfo, CardProfile, ModuleInfo, Props, ServerIdentity, ServerSnapshot, SinkInfo, SourceInfo,
};
use crate::audio::server::{AudioServer, ServerError};

const INVALID_INDEX: u32 = u32::MAX;

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
                    owner_module: s.owner_module,
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
                    owner_module: s.owner_module,
                    monitor_of_sink: s.monitor_of_sink,
                    props: props_of(&s.proplist),
                }
            }
        );
        let modules = collect_list!(
            self,
            "module list",
            get_module_info_list,
            |m: &libpulse_binding::context::introspect::ModuleInfo| {
                ModuleInfo {
                    index: m.index,
                    name: text(&m.name),
                    argument: m.argument.as_deref().map(str::to_string),
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
            modules,
        })
    }

    fn load_module(&mut self, name: &str, argument: &str) -> Result<u32, ServerError> {
        if name.contains('\0') || argument.contains('\0') {
            return Err(ServerError::Rejected {
                what: "load module",
                detail: "NUL in module name or argument".into(),
            });
        }
        let cell = Rc::new(Cell::new(None));
        let c = cell.clone();
        let op = self
            .conn()?
            .context
            .introspect()
            .load_module(name, argument, move |i| c.set(Some(i)));
        let probe = cell.clone();
        self.wait(op, &move || probe.get().is_some(), "load module")?;
        match cell.get() {
            Some(INVALID_INDEX) | None => {
                let errno = self
                    .conn
                    .as_ref()
                    .and_then(|c| c.context.errno().to_string())
                    .unwrap_or_default();
                Err(ServerError::Rejected {
                    what: "load module",
                    detail: errno,
                })
            }
            Some(i) => Ok(i),
        }
    }

    fn unload_module(&mut self, index: u32) -> Result<(), ServerError> {
        let cell = Rc::new(Cell::new(None));
        let c = cell.clone();
        let op = self
            .conn()?
            .context
            .introspect()
            .unload_module(index, move |ok| c.set(Some(ok)));
        let probe = cell.clone();
        self.wait(op, &move || probe.get().is_some(), "unload module")?;
        match cell.get() {
            Some(true) => Ok(()),
            _ => {
                let errno = self
                    .conn
                    .as_ref()
                    .and_then(|c| c.context.errno().to_string())
                    .unwrap_or_default();
                Err(ServerError::Rejected {
                    what: "unload module",
                    detail: errno,
                })
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
