//! The actor that owns the platform port and the [`Engine`].
//!
//! All COM/WinRT work happens on one dedicated thread: the platform is *created on that thread*
//! (the factory passed to [`CallsService::spawn`] runs there, which is where the native port
//! initialises the multithreaded apartment), used only there, and dropped there. Callers talk to it
//! through a [`CallsHandle`]:
//!
//! * the request queue is bounded; a full queue is [`ServiceError::QueueFull`], never blocking;
//! * every request carries a deadline derived from the engine's [`Timeouts`], and the caller waits
//!   no longer than that;
//! * if the caller gives up while the request is still queued, the request is cancelled and is
//!   *guaranteed not to run* ([`Executed::No`]); if it had already started the answer is
//!   [`Executed::Maybe`], the explicit uncertain state;
//! * one stuck Windows call blocks only this actor, and every later request then times out instead
//!   of piling up.
//!
//! In watch mode the actor forwards operating-system change signals into a coalescing refresh and
//! publishes [`ServiceEvent`]s to bounded subscriber queues. A subscriber that falls behind is sent
//! [`ServiceEvent::CoreReset`] with the full core state instead of silently missing events.
use crate::engine::*;
use crate::model::*;
use crate::platform::{CallsPlatform, Signal, SignalSink};
use bridge_core::calls::{CallCapabilities, CallCommand, CallEvent, CallState};
use serde::Serialize;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const QUEUED: u8 = 0;
const RUNNING: u8 = 1;
const CANCELLED: u8 = 2;

#[derive(Clone, Copy, Debug)]
pub struct ServiceConfig {
    pub engine: EngineConfig,
    /// Pending request limit.
    pub queue: usize,
    /// Per-subscriber event queue limit.
    pub event_queue: usize,
    /// Refresh period while watching (in addition to OS signals).
    pub poll_interval: Duration,
    /// Quiet time after the last OS signal before the refresh it triggers.
    pub debounce: Duration,
    /// Slack added to a request's deadline for queueing and thread wake-up.
    pub margin: Duration,
}
impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            engine: EngineConfig::default(),
            queue: 16,
            event_queue: 64,
            poll_interval: Duration::from_secs(5),
            debounce: Duration::from_millis(250),
            margin: Duration::from_secs(3),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    Refresh,
    Select(TransportDeviceId),
    Deselect,
    RequestAccess,
    Register,
    Unregister,
    Connect,
    Execute(WinCommand),
    ExecuteCore(CallCommand),
    Capabilities,
    Watch(bool),
}
impl Request {
    /// Longest the caller should wait, from the engine's own step budgets.
    fn budget(&self, t: &Timeouts, margin: Duration) -> Duration {
        let read = t.refresh;
        margin
            + match self {
                Self::Refresh | Self::Deselect | Self::Capabilities | Self::Watch(_) => read,
                Self::Select(_) => read * 2,
                Self::RequestAccess => read + t.access,
                Self::Register => read * 2 + t.access + t.step * 2,
                Self::Unregister => read * 2 + t.step * 2,
                Self::Connect => read * 2 + t.connect,
                Self::Execute(_) | Self::ExecuteCore(_) => read * 2 + t.command * 2,
            }
    }
}

#[derive(Clone, Debug)]
pub enum Response {
    Snapshot(Snapshot),
    Access(crate::readiness::AccessState),
    Register(RegisterOutcome),
    Unregister(UnregisterOutcome),
    Connect(ConnectReport),
    Command(Box<CommandReport>),
    Capabilities(CallCapabilities),
    Done,
}

/// Whether a request that the caller gave up on ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Executed {
    /// Cancelled while queued: it never ran and never will.
    No,
    /// It had started; the effect is unknown.
    Maybe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "service_error", rename_all = "snake_case")]
pub enum ServiceError {
    Calls { error: CallsError },
    QueueFull,
    Stopped,
    TimedOut { executed: Executed },
}
impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Calls { error } => write!(f, "{error}"),
            Self::QueueFull => f.write_str("request queue full"),
            Self::Stopped => f.write_str("calls service stopped"),
            Self::TimedOut { executed } => write!(f, "timed out (executed: {executed:?})"),
        }
    }
}
impl std::error::Error for ServiceError {}

#[derive(Clone, Debug)]
pub enum ServiceEvent {
    /// A refresh completed (only the latest is published when several ran back to back).
    Snapshot(Arc<Snapshot>),
    /// New `bridge-core` events plus the capabilities they were produced under.
    Core {
        events: Vec<CallEvent>,
        capabilities: CallCapabilities,
    },
    /// The subscriber missed events; this is the whole core state to resume from.
    CoreReset { state: CallState },
}

enum Msg {
    Request(Envelope),
    Signal,
    Stop,
}

struct Envelope {
    request: Request,
    reply: SyncSender<Result<Response, CallsError>>,
    deadline: Instant,
    state: Arc<AtomicU8>,
}

struct Subscriber {
    tx: SyncSender<ServiceEvent>,
    lagged: bool,
}
type Subscribers = Arc<Mutex<Vec<Subscriber>>>;

/// Cheap, cloneable client of the actor.
#[derive(Clone)]
pub struct CallsHandle {
    tx: SyncSender<Msg>,
    subs: Subscribers,
    timeouts: Timeouts,
    margin: Duration,
    event_queue: usize,
}

/// Owns the actor thread; dropping it stops the actor and joins the thread.
pub struct CallsService {
    handle: CallsHandle,
    join: Option<JoinHandle<()>>,
}

impl CallsService {
    /// Start the actor. `factory` runs on the actor thread and builds the platform there; its error
    /// (for example COM initialisation failing) is returned from here.
    pub fn spawn<P, F>(factory: F, cfg: ServiceConfig) -> Result<Self, PlatformError>
    where
        P: CallsPlatform + 'static,
        F: FnOnce() -> Result<P, PlatformError> + Send + 'static,
    {
        let (tx, rx) = sync_channel::<Msg>(cfg.queue.max(1));
        let (ready_tx, ready_rx) = sync_channel::<Result<(), PlatformError>>(1);
        let subs: Subscribers = Arc::default();
        let actor_subs = subs.clone();
        let sink_tx = tx.clone();
        let join = std::thread::Builder::new()
            .name("geekbridge-calls-windows".into())
            .spawn(move || {
                let platform = match factory() {
                    Ok(p) => {
                        let _ = ready_tx.send(Ok(()));
                        p
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                run(
                    Engine::new(platform, cfg.engine),
                    rx,
                    sink_tx,
                    actor_subs,
                    cfg,
                );
            })
            .map_err(|_| PlatformError::new(PlatformErrorKind::Os, "spawn calls service thread"))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                handle: CallsHandle {
                    tx,
                    subs,
                    timeouts: cfg.engine.timeouts,
                    margin: cfg.margin,
                    event_queue: cfg.event_queue.max(1),
                },
                join: Some(join),
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => Err(PlatformError::new(
                PlatformErrorKind::Os,
                "calls service thread exited during start",
            )),
        }
    }

    pub fn handle(&self) -> CallsHandle {
        self.handle.clone()
    }
}
impl Drop for CallsService {
    fn drop(&mut self) {
        // Best effort: if the queue is full the actor is busy or stuck. In the stuck case the
        // thread cannot be joined, so it is detached rather than hanging the caller.
        if self.handle.tx.try_send(Msg::Stop).is_ok()
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
    }
}

impl CallsHandle {
    fn call(&self, request: Request) -> Result<Response, ServiceError> {
        let budget = request.budget(&self.timeouts, self.margin);
        let (reply, reply_rx) = sync_channel(1);
        let state = Arc::new(AtomicU8::new(QUEUED));
        let envelope = Envelope {
            request,
            reply,
            deadline: Instant::now() + budget,
            state: state.clone(),
        };
        match self.tx.try_send(Msg::Request(envelope)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(ServiceError::QueueFull),
            Err(TrySendError::Disconnected(_)) => return Err(ServiceError::Stopped),
        }
        match reply_rx.recv_timeout(budget) {
            Ok(result) => result.map_err(|error| ServiceError::Calls { error }),
            Err(RecvTimeoutError::Disconnected) => Err(ServiceError::Stopped),
            Err(RecvTimeoutError::Timeout) => {
                if state
                    .compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                    || state.load(Ordering::Acquire) == CANCELLED
                {
                    return Err(ServiceError::TimedOut {
                        executed: Executed::No,
                    });
                }
                // Started: take a reply that raced the timeout rather than reporting uncertainty.
                match reply_rx.try_recv() {
                    Ok(result) => result.map_err(|error| ServiceError::Calls { error }),
                    Err(_) => Err(ServiceError::TimedOut {
                        executed: Executed::Maybe,
                    }),
                }
            }
        }
    }

    pub fn refresh(&self) -> Result<Snapshot, ServiceError> {
        match self.call(Request::Refresh)? {
            Response::Snapshot(s) => Ok(s),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn select(&self, id: &TransportDeviceId) -> Result<Snapshot, ServiceError> {
        match self.call(Request::Select(id.clone()))? {
            Response::Snapshot(s) => Ok(s),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn deselect(&self) -> Result<(), ServiceError> {
        self.call(Request::Deselect).map(|_| ())
    }
    pub fn request_access(&self) -> Result<crate::readiness::AccessState, ServiceError> {
        match self.call(Request::RequestAccess)? {
            Response::Access(a) => Ok(a),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn register(&self) -> Result<RegisterOutcome, ServiceError> {
        match self.call(Request::Register)? {
            Response::Register(r) => Ok(r),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn unregister(&self) -> Result<UnregisterOutcome, ServiceError> {
        match self.call(Request::Unregister)? {
            Response::Unregister(r) => Ok(r),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn connect(&self) -> Result<ConnectReport, ServiceError> {
        match self.call(Request::Connect)? {
            Response::Connect(r) => Ok(r),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn execute(&self, command: WinCommand) -> Result<CommandReport, ServiceError> {
        match self.call(Request::Execute(command))? {
            Response::Command(r) => Ok(*r),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn execute_core(&self, command: &CallCommand) -> Result<CommandReport, ServiceError> {
        match self.call(Request::ExecuteCore(command.clone()))? {
            Response::Command(r) => Ok(*r),
            _ => Err(ServiceError::Stopped),
        }
    }
    pub fn capabilities(&self) -> Result<CallCapabilities, ServiceError> {
        match self.call(Request::Capabilities)? {
            Response::Capabilities(c) => Ok(c),
            _ => Err(ServiceError::Stopped),
        }
    }
    /// Start or stop OS change notifications and periodic refresh.
    pub fn watch(&self, on: bool) -> Result<(), ServiceError> {
        self.call(Request::Watch(on)).map(|_| ())
    }
    /// A bounded stream of events produced by refreshes.
    pub fn subscribe(&self) -> Receiver<ServiceEvent> {
        let (tx, rx) = sync_channel(self.event_queue);
        self.subs
            .lock()
            .unwrap()
            .push(Subscriber { tx, lagged: false });
        rx
    }
}

// ------------------------------------------------------------------ the actor

fn run<P: CallsPlatform>(
    mut engine: Engine<P>,
    rx: Receiver<Msg>,
    signal_tx: SyncSender<Msg>,
    subs: Subscribers,
    cfg: ServiceConfig,
) {
    let mut watching = false;
    let mut dirty_since: Option<Instant> = None;
    let mut last_published: Option<Snapshot> = None;
    let mut last_poll = Instant::now();

    loop {
        let wait = match (dirty_since, watching) {
            (Some(at), _) => (at + cfg.debounce).saturating_duration_since(Instant::now()),
            (None, true) => {
                (last_poll + cfg.poll_interval).saturating_duration_since(Instant::now())
            }
            (None, false) => Duration::from_secs(3600),
        };
        match rx.recv_timeout(wait) {
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Msg::Signal) => {
                dirty_since.get_or_insert_with(Instant::now);
                continue;
            }
            Ok(Msg::Request(env)) => {
                // Too old to be what the caller still wants: cancel it ourselves. If the caller
                // already cancelled, the exchange fails and the state is already CANCELLED.
                if Instant::now() > env.deadline {
                    let _ = env.state.compare_exchange(
                        QUEUED,
                        CANCELLED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                    continue;
                }
                if env
                    .state
                    .compare_exchange(QUEUED, RUNNING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    continue;
                }
                let result = handle_request(&mut engine, env.request, &signal_tx, &mut watching);
                let _ = env.reply.send(result);
            }
            Err(RecvTimeoutError::Timeout) => {
                if dirty_since.take().is_some() || watching {
                    engine.refresh();
                    last_poll = Instant::now();
                }
            }
        }
        publish(&mut engine, &subs, &mut last_published);
    }
    engine.platform_mut().stop_signals();
}

fn handle_request<P: CallsPlatform>(
    engine: &mut Engine<P>,
    request: Request,
    signal_tx: &SyncSender<Msg>,
    watching: &mut bool,
) -> Result<Response, CallsError> {
    Ok(match request {
        Request::Refresh => Response::Snapshot(engine.refresh()),
        Request::Select(id) => Response::Snapshot(engine.select(&id)?),
        Request::Deselect => {
            engine.deselect();
            Response::Done
        }
        Request::RequestAccess => Response::Access(engine.request_access()?),
        Request::Register => Response::Register(engine.register()?),
        Request::Unregister => Response::Unregister(engine.unregister()?),
        Request::Connect => Response::Connect(engine.connect()?),
        Request::Execute(c) => Response::Command(Box::new(engine.execute(&c)?)),
        Request::ExecuteCore(c) => Response::Command(Box::new(engine.execute_core(&c)?)),
        Request::Capabilities => Response::Capabilities(engine.capabilities()),
        Request::Watch(on) => {
            if on {
                let tx = signal_tx.clone();
                // Signals carry no payload worth keeping; a full queue already has a refresh due.
                let sink: SignalSink = Arc::new(move |_: Signal| {
                    let _ = tx.try_send(Msg::Signal);
                });
                engine.platform_mut().start_signals(sink)?;
                engine.refresh();
            } else {
                engine.platform_mut().stop_signals();
            }
            *watching = on;
            Response::Done
        }
    })
}

fn publish<P: CallsPlatform>(
    engine: &mut Engine<P>,
    subs: &Subscribers,
    last: &mut Option<Snapshot>,
) {
    let events = engine.take_core_events();
    // A periodic refresh that found nothing new is not an event.
    let snapshot = engine
        .last_snapshot()
        .filter(|s| {
            last.as_ref().is_none_or(|prev| {
                let mut same_seq: Snapshot = (*s).clone();
                same_seq.seq = prev.seq;
                same_seq != *prev
            })
        })
        .cloned()
        .map(Arc::new);
    if events.is_empty() && snapshot.is_none() {
        return;
    }
    if let Some(s) = &snapshot {
        *last = Some((**s).clone());
    }
    let capabilities = (!events.is_empty()).then(|| engine.capabilities());
    let state = engine.core_state().clone();
    let mut subs = subs.lock().unwrap();
    subs.retain_mut(|sub| {
        if sub.lagged {
            match sub.tx.try_send(ServiceEvent::CoreReset {
                state: state.clone(),
            }) {
                Ok(()) => sub.lagged = false,
                Err(TrySendError::Full(_)) => return true,
                Err(TrySendError::Disconnected(_)) => return false,
            }
            // The reset already contains these events.
            if let Some(s) = &snapshot {
                let _ = sub.tx.try_send(ServiceEvent::Snapshot(s.clone()));
            }
            return true;
        }
        let mut outgoing = Vec::new();
        if let Some(s) = &snapshot {
            outgoing.push(ServiceEvent::Snapshot(s.clone()));
        }
        if let Some(capabilities) = &capabilities {
            outgoing.push(ServiceEvent::Core {
                events: events.clone(),
                capabilities: capabilities.clone(),
            });
        }
        for event in outgoing {
            match sub.tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(ServiceEvent::Core { .. })) => sub.lagged = true,
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        true
    });
}
