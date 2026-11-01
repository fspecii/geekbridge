//! Transactional start/stop of the session-owned modules that carry call audio.
//!
//! Ownership rules (the reason this file is long):
//!
//! * Nothing is loaded until [`AudioRouteManager::start`] is called with a plan. Discovery,
//!   inventory, assessment and preparation are read-only.
//! * A module is *owned* only if this session loaded it (the server returned its index) or, after
//!   an unknown-outcome failure, if it carries this session's tag. Before every unload the module
//!   at that index is re-read and must still be the same module with the same tag. A user's
//!   module that reuses an index (servers restart counters) is never unloaded.
//! * The manager never changes defaults, moves streams, changes card profiles or writes config:
//!   [`AudioServer`] has no such operation.
//! * Failure at any step unloads, in reverse order, exactly what the failed start loaded. If an
//!   unload fails the module is kept as *cleanup debt* and retried by [`AudioRouteManager::stop`],
//!   [`AudioRouteManager::recover`] and on drop; a new start is refused while debt remains.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::audio::inventory::{AudioInventory, OrphanModule, inventory, orphans_in};
use crate::audio::model::{BtAddress, ModuleInfo, ServerIdentity, ServerSnapshot};
use crate::audio::plan::{
    Assessment, Blocker, ControlEvidence, DeclaredFeasibility, RoutePlan, RouteRequest, assess,
};
use crate::audio::server::{AudioServer, ServerError};
use crate::audio::spec::{MODULE_ECHO_CANCEL, MODULE_LOOPBACK, SessionId, session_in_argument};
use crate::model::TransportState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleRole {
    EchoCancel,
    LoopbackRx,
    LoopbackTx,
}

impl ModuleRole {
    fn module(self) -> &'static str {
        match self {
            Self::EchoCancel => MODULE_ECHO_CANCEL,
            Self::LoopbackRx | Self::LoopbackTx => MODULE_LOOPBACK,
        }
    }
}

/// A module this session loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OwnedModule {
    pub index: u32,
    pub module: &'static str,
    pub role: ModuleRole,
    pub session: SessionId,
    /// Connection epoch in which `index` was issued. Indices are trusted only within it, or
    /// after a reconnect that proved the same server instance.
    #[serde(skip)]
    epoch: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum EchoFailure {
    LoadRejected {
        detail: String,
    },
    NodesDidNotAppear,
    /// The nodes exist but are owned by a different module.
    NodesNotOurs,
}

/// What happened to echo cancellation. A fallback is always reported, never silent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "echo", rename_all = "snake_case")]
pub enum EchoOutcome {
    NotRequested,
    Active { source: String, sink: String },
    FellBack { reason: EchoFailure },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Requested,
    /// The call layer reported the phone link down.
    LinkLost,
    /// The call layer reported the voice transport no longer active.
    TransportDown,
    /// The phone's card, profile or voice endpoints are gone or replaced.
    PhoneAudioLost,
    LocalEndpointLost,
    /// One of our modules vanished (loopbacks unload themselves when an endpoint disappears).
    ModuleVanished,
    /// The server restarted (or could not be proven the same instance) across a reconnect.
    ServerRestarted,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UntouchedModule {
    pub module: OwnedModule,
    pub reason: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedUnload {
    pub module: OwnedModule,
    pub error: String,
}

/// What a teardown did. `pending` modules are still loaded (or may be) and remain cleanup debt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeardownReport {
    pub unloaded: Vec<OwnedModule>,
    pub already_gone: Vec<OwnedModule>,
    /// Not unloaded because the module at that index is no longer provably ours.
    pub left_untouched: Vec<UntouchedModule>,
    /// Tagged modules of this session found by the post-failure sweep, not by index.
    pub swept: Vec<OwnedModule>,
    pub failed: Vec<FailedUnload>,
    pub server_error: Option<ServerError>,
    pub pending: Vec<OwnedModule>,
}

impl TeardownReport {
    pub fn is_clean(&self) -> bool {
        self.pending.is_empty() && self.server_error.is_none()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopReport {
    pub session: Option<SessionId>,
    pub reason: StopReason,
    pub teardown: TeardownReport,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StartReport {
    pub session: SessionId,
    pub echo: EchoOutcome,
    pub modules: Vec<OwnedModule>,
    /// Declared state only: modules are loaded and the endpoints exist. Whether audio is audible
    /// in both directions is not known here.
    pub hardware_verified: crate::audio::plan::HardwareEvidence,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("a route is already active")]
    AlreadyActive,
    #[error("{0} module(s) from an earlier session could not be unloaded yet")]
    CleanupPending(usize),
    #[error(transparent)]
    Server(#[from] ServerError),
    #[error("plan is stale: {0}")]
    StalePlan(&'static str),
    #[error("route is not feasible: {0:?}")]
    NotFeasible(Vec<Blocker>),
    #[error("echo cancellation was required but unavailable: {0:?}")]
    EchoRequiredUnavailable(EchoFailure),
    #[error("{0:?} module could not be loaded: {1}")]
    ModuleLoad(ModuleRole, ServerError),
    #[error("a device disappeared while the route was being built: {0}")]
    PeerDisappeared(&'static str),
    #[error("a module vanished while the route was being built: {0:?}")]
    ModuleVanished(ModuleRole),
}

/// A failed start. `cleanup` lists what was undone.
#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
pub struct StartFailure {
    pub cause: StartError,
    pub cleanup: Box<TeardownReport>,
}

/// The running route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActiveRoute {
    pub session: SessionId,
    pub phone: BtAddress,
    pub plan: Box<RoutePlan>,
    pub echo: EchoOutcome,
    pub modules: Vec<OwnedModule>,
    pub server: ServerIdentity,
}

#[derive(Debug)]
pub enum Health {
    NoRoute,
    Healthy,
    /// The server could not be read. The route is kept; call [`AudioRouteManager::recover`].
    ServerUnreachable(ServerError),
    Ended(Box<StopReport>),
}

#[derive(Debug)]
pub enum RecoverRoute {
    NoRoute,
    /// The same server instance still holds our modules; nothing was reloaded.
    Adopted,
    Ended(Box<StopReport>),
}

#[derive(Debug)]
pub struct RecoverReport {
    pub route: RecoverRoute,
    pub debt_remaining: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct StartOptions {
    pub echo_ready_timeout: Duration,
    pub poll_interval: Duration,
}

impl Default for StartOptions {
    fn default() -> Self {
        Self {
            echo_ready_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(50),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Ownership {
    Ours,
    /// The server reports no argument string for the module, so the tag cannot be checked.
    Unverifiable,
    Missing,
    NotOurs(&'static str),
}

fn classify(own: &OwnedModule, modules: &[ModuleInfo]) -> Ownership {
    let Some(m) = modules.iter().find(|m| m.index == own.index) else {
        return Ownership::Missing;
    };
    if m.name != own.module {
        return Ownership::NotOurs("index now belongs to a different module");
    }
    match m.argument.as_deref() {
        Some(a) if !a.is_empty() => {
            if own.session.tags(own.module, a) {
                Ownership::Ours
            } else {
                Ownership::NotOurs("module at this index does not carry our session tag")
            }
        }
        _ => Ownership::Unverifiable,
    }
}

fn role_of(module: &str, argument: &str) -> ModuleRole {
    if module == MODULE_ECHO_CANCEL {
        ModuleRole::EchoCancel
    } else if argument.contains("geekbridge.path=tx") {
        ModuleRole::LoopbackTx
    } else {
        ModuleRole::LoopbackRx
    }
}

fn default_session_ids() -> Box<dyn FnMut() -> SessionId + Send> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    Box::new(|| {
        let mut h = DefaultHasher::new();
        std::process::id().hash(&mut h);
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
            .hash(&mut h);
        COUNTER.fetch_add(1, Ordering::Relaxed).hash(&mut h);
        SessionId::from_u64(h.finish())
    })
}

/// Owns at most one active call-audio route on one audio server.
///
/// Single-threaded and blocking (`&mut self`); a service wraps it in a dedicated thread or
/// `spawn_blocking`. Dropping it unloads whatever it still owns (bounded, best effort).
pub struct AudioRouteManager<S: AudioServer> {
    server: S,
    options: StartOptions,
    next_session: Box<dyn FnMut() -> SessionId + Send>,
    epoch: u64,
    last_identity: Option<ServerIdentity>,
    active: Option<ActiveRoute>,
    debt: Vec<OwnedModule>,
}

impl<S: AudioServer> AudioRouteManager<S> {
    pub fn new(server: S) -> Self {
        Self::with_options(server, StartOptions::default())
    }

    pub fn with_options(server: S, options: StartOptions) -> Self {
        Self {
            server,
            options,
            next_session: default_session_ids(),
            epoch: 0,
            last_identity: None,
            active: None,
            debt: Vec::new(),
        }
    }

    /// Replace the session-id source (deterministic tests).
    pub fn with_session_ids(mut self, f: impl FnMut() -> SessionId + Send + 'static) -> Self {
        self.next_session = Box::new(f);
        self
    }

    pub fn active(&self) -> Option<&ActiveRoute> {
        self.active.as_ref()
    }

    /// Modules whose unload failed and will be retried.
    pub fn cleanup_debt(&self) -> &[OwnedModule] {
        &self.debt
    }

    fn read(&mut self) -> Result<ServerSnapshot, ServerError> {
        let snap = self.server.snapshot()?;
        self.last_identity = Some(snap.identity.clone());
        Ok(snap)
    }

    fn live_sessions(&self) -> Vec<SessionId> {
        self.active
            .iter()
            .map(|a| a.session.clone())
            .chain(self.debt.iter().map(|m| m.session.clone()))
            .collect()
    }

    // ------------------------------------------------------------ read-only

    /// Read-only inventory: phones (A2DP and voice endpoints reported separately), local devices,
    /// and orphaned GeekBridge modules.
    pub fn inventory(&mut self) -> Result<AudioInventory, ServerError> {
        let snap = self.read()?;
        Ok(inventory(&snap, &self.live_sessions()))
    }

    /// Declared and hardware-verified feasibility for `req`. Read-only.
    pub fn assess(&mut self, req: &RouteRequest) -> Result<Assessment, ServerError> {
        Ok(assess(&self.read()?, req))
    }

    /// Like [`Self::assess`] but returns the plan or the reasons there is none.
    pub fn prepare(
        &mut self,
        req: &RouteRequest,
    ) -> Result<Result<RoutePlan, Vec<Blocker>>, ServerError> {
        Ok(match self.assess(req)?.declared {
            DeclaredFeasibility::Ready { plan } => Ok(*plan),
            DeclaredFeasibility::Waiting { blockers }
            | DeclaredFeasibility::Refused { blockers } => Err(blockers),
        })
    }

    // ------------------------------------------------------------ start

    /// Build the route. Loads, in order: optional echo canceller, RX loopback, TX loopback.
    /// The only entry point that changes the server.
    pub fn start(&mut self, plan: &RoutePlan) -> Result<StartReport, StartFailure> {
        let fail = |cause: StartError| StartFailure {
            cause,
            cleanup: Box::default(),
        };
        if self.active.is_some() {
            return Err(fail(StartError::AlreadyActive));
        }
        if !self.debt.is_empty() {
            self.retry_cleanup();
            if !self.debt.is_empty() {
                return Err(fail(StartError::CleanupPending(self.debt.len())));
            }
        }
        let snap = self.read().map_err(|e| fail(e.into()))?;
        if snap.identity != plan.server {
            return Err(fail(StartError::StalePlan(
                "the audio server instance changed",
            )));
        }
        match assess(&snap, &plan.request).declared {
            DeclaredFeasibility::Ready { plan: fresh } if fresh.same_targets(plan) => {}
            DeclaredFeasibility::Ready { .. } => {
                return Err(fail(StartError::StalePlan(
                    "devices changed since the plan was prepared",
                )));
            }
            DeclaredFeasibility::Waiting { blockers }
            | DeclaredFeasibility::Refused { blockers } => {
                return Err(fail(StartError::NotFeasible(blockers)));
            }
        }

        let session = (self.next_session)();
        let mut loaded: Vec<OwnedModule> = Vec::new();
        let mut with_echo = plan.request.echo.method().is_some();
        let mut echo_outcome = EchoOutcome::NotRequested;

        if with_echo {
            let specs = plan.specs(&session, true);
            let echo = specs.echo.expect("echo requested");
            let echo_failure = match self.load(
                &session,
                ModuleRole::EchoCancel,
                &echo.module_argument(),
                &mut loaded,
            ) {
                Err(e) if e.is_connection_loss() => {
                    return Err(self.abort(
                        &session,
                        loaded,
                        StartError::ModuleLoad(ModuleRole::EchoCancel, e),
                    ));
                }
                Err(e) => Some(EchoFailure::LoadRejected {
                    detail: e.to_string(),
                }),
                Ok(()) => {
                    match self.wait_echo_nodes(&echo.source_name, &echo.sink_name, loaded[0].index)
                    {
                        Ok(None) => None,
                        Ok(Some(f)) => Some(f),
                        Err(e) => {
                            return Err(self.abort(&session, loaded, StartError::Server(e)));
                        }
                    }
                }
            };
            match echo_failure {
                None => {
                    echo_outcome = EchoOutcome::Active {
                        source: echo.source_name,
                        sink: echo.sink_name,
                    };
                }
                Some(f) => {
                    // Remove the half-built canceller before deciding anything else.
                    let report =
                        self.teardown(std::mem::take(&mut loaded), std::slice::from_ref(&session));
                    if !report.is_clean() {
                        self.debt.extend(report.pending.clone());
                        return Err(StartFailure {
                            cause: StartError::EchoRequiredUnavailable(f),
                            cleanup: Box::new(report),
                        });
                    }
                    if matches!(
                        plan.request.echo,
                        crate::audio::plan::EchoPolicy::Required(_)
                    ) {
                        return Err(StartFailure {
                            cause: StartError::EchoRequiredUnavailable(f),
                            cleanup: Box::new(report),
                        });
                    }
                    with_echo = false;
                    echo_outcome = EchoOutcome::FellBack { reason: f };
                }
            }
        }

        let specs = plan.specs(&session, with_echo);
        if let Err(e) = self.load(
            &session,
            ModuleRole::LoopbackRx,
            &specs.rx.module_argument(&session),
            &mut loaded,
        ) {
            return Err(self.abort(
                &session,
                loaded,
                StartError::ModuleLoad(ModuleRole::LoopbackRx, e),
            ));
        }
        if let Err(e) = self.load(
            &session,
            ModuleRole::LoopbackTx,
            &specs.tx.module_argument(&session),
            &mut loaded,
        ) {
            return Err(self.abort(
                &session,
                loaded,
                StartError::ModuleLoad(ModuleRole::LoopbackTx, e),
            ));
        }

        // Final check: the peers are still there and nothing we loaded has already unloaded.
        match self.read() {
            Err(e) => return Err(self.abort(&session, loaded, StartError::Server(e))),
            Ok(snap) => {
                let epoch = self.epoch;
                if let Some(gone) = loaded
                    .iter()
                    .find(|m| !Self::usable(m, classify(m, &snap.modules), epoch))
                {
                    let role = gone.role;
                    return Err(self.abort(&session, loaded, StartError::ModuleVanished(role)));
                }
                match assess(&snap, &plan.request).declared {
                    DeclaredFeasibility::Ready { plan: fresh } if fresh.same_targets(plan) => {}
                    _ => {
                        return Err(self.abort(
                            &session,
                            loaded,
                            StartError::PeerDisappeared(
                                "phone or local endpoint changed during start",
                            ),
                        ));
                    }
                }
            }
        }

        self.active = Some(ActiveRoute {
            session: session.clone(),
            phone: plan.phone.address,
            plan: Box::new(plan.clone()),
            echo: echo_outcome.clone(),
            modules: loaded.clone(),
            server: plan.server.clone(),
        });
        Ok(StartReport {
            session,
            echo: echo_outcome,
            modules: loaded,
            hardware_verified: crate::audio::plan::HardwareEvidence::unverified(),
        })
    }

    fn load(
        &mut self,
        session: &SessionId,
        role: ModuleRole,
        argument: &str,
        loaded: &mut Vec<OwnedModule>,
    ) -> Result<(), ServerError> {
        let index = self.server.load_module(role.module(), argument)?;
        loaded.push(OwnedModule {
            index,
            module: role.module(),
            role,
            session: session.clone(),
            epoch: self.epoch,
        });
        Ok(())
    }

    /// Wait (bounded) until the echo canceller's virtual nodes exist and are owned by `index`.
    fn wait_echo_nodes(
        &mut self,
        source: &str,
        sink: &str,
        index: u32,
    ) -> Result<Option<EchoFailure>, ServerError> {
        let deadline = Instant::now() + self.options.echo_ready_timeout;
        loop {
            let snap = self.read()?;
            let src = snap.sources.iter().find(|s| s.name == source);
            let snk = snap.sinks.iter().find(|s| s.name == sink);
            if let (Some(src), Some(snk)) = (src, snk) {
                let ours = |o: Option<u32>| o.is_none() || o == Some(index);
                return Ok((!(ours(src.owner_module) && ours(snk.owner_module)))
                    .then_some(EchoFailure::NodesNotOurs));
            }
            if Instant::now() >= deadline {
                return Ok(Some(EchoFailure::NodesDidNotAppear));
            }
            std::thread::sleep(self.options.poll_interval);
        }
    }

    /// Roll back a failed start: unload what it loaded plus anything carrying its tag.
    fn abort(
        &mut self,
        session: &SessionId,
        loaded: Vec<OwnedModule>,
        cause: StartError,
    ) -> StartFailure {
        let report = self.teardown(loaded, std::slice::from_ref(session));
        self.debt.extend(report.pending.clone());
        StartFailure {
            cause,
            cleanup: Box::new(report),
        }
    }

    fn usable(own: &OwnedModule, o: Ownership, epoch: u64) -> bool {
        match o {
            Ownership::Ours => true,
            Ownership::Unverifiable => own.epoch == epoch,
            Ownership::Missing | Ownership::NotOurs(_) => false,
        }
    }

    // ------------------------------------------------------------ teardown

    /// Unload `mods` in reverse load order, each only after re-confirming ownership, then sweep
    /// `sessions`' tagged modules that are not in `mods`. Never touches anything else.
    fn teardown(&mut self, mods: Vec<OwnedModule>, sessions: &[SessionId]) -> TeardownReport {
        let mut report = TeardownReport::default();
        if mods.is_empty() && sessions.is_empty() {
            return report;
        }
        let snap = match self.read() {
            Ok(s) => s,
            Err(e) => {
                report.server_error = Some(e);
                report.pending = mods;
                return report;
            }
        };
        let epoch = self.epoch;
        let mut lost = false;
        for m in mods.iter().rev() {
            if lost {
                report.pending.push(m.clone());
                continue;
            }
            match classify(m, &snap.modules) {
                Ownership::Missing => report.already_gone.push(m.clone()),
                Ownership::NotOurs(reason) => report.left_untouched.push(UntouchedModule {
                    module: m.clone(),
                    reason,
                }),
                o @ (Ownership::Ours | Ownership::Unverifiable) => {
                    if !Self::usable(m, o, epoch) {
                        report.left_untouched.push(UntouchedModule {
                            module: m.clone(),
                            reason: "module arguments are not reported and the index is not trusted across a reconnect",
                        });
                        continue;
                    }
                    match self.server.unload_module(m.index) {
                        Ok(()) => report.unloaded.push(m.clone()),
                        Err(e) => {
                            lost = e.is_connection_loss();
                            report.failed.push(FailedUnload {
                                module: m.clone(),
                                error: e.to_string(),
                            });
                            if lost {
                                report.server_error = Some(e);
                            }
                            report.pending.push(m.clone());
                        }
                    }
                }
            }
        }
        if lost {
            return report;
        }
        // Sweep: tagged modules of these sessions that we hold no index for (a load whose
        // outcome was unknown after a timeout).
        let known: Vec<u32> = mods.iter().map(|m| m.index).collect();
        let mut stray: Vec<OwnedModule> = Vec::new();
        for m in &snap.modules {
            if known.contains(&m.index) {
                continue;
            }
            let Some(arg) = m.argument.as_deref() else {
                continue;
            };
            let Some(sid) = session_in_argument(&m.name, arg) else {
                continue;
            };
            if !sessions.contains(&sid) {
                continue;
            }
            let module = if m.name == MODULE_ECHO_CANCEL {
                MODULE_ECHO_CANCEL
            } else {
                MODULE_LOOPBACK
            };
            stray.push(OwnedModule {
                index: m.index,
                module,
                role: role_of(&m.name, arg),
                session: sid,
                epoch,
            });
        }
        for m in stray.into_iter().rev() {
            match self.server.unload_module(m.index) {
                Ok(()) => report.swept.push(m),
                Err(e) => {
                    report.failed.push(FailedUnload {
                        module: m.clone(),
                        error: e.to_string(),
                    });
                    report.pending.push(m);
                    if e.is_connection_loss() {
                        report.server_error = Some(e);
                        break;
                    }
                }
            }
        }
        report
    }

    /// Retry unloading modules left from earlier failures.
    pub fn retry_cleanup(&mut self) -> TeardownReport {
        let debt = std::mem::take(&mut self.debt);
        let sessions: Vec<SessionId> = debt.iter().map(|m| m.session.clone()).collect();
        let report = self.teardown(debt, &sessions);
        self.debt = report.pending.clone();
        report
    }

    /// Stop the active route and unload only what this session created. Safe to call with no
    /// route, repeatedly, and when the phone or server is already gone.
    pub fn stop(&mut self, reason: StopReason) -> StopReport {
        let Some(active) = self.active.take() else {
            let teardown = if self.debt.is_empty() {
                TeardownReport::default()
            } else {
                self.retry_cleanup()
            };
            return StopReport {
                session: None,
                reason,
                teardown,
            };
        };
        let report = self.teardown(active.modules, std::slice::from_ref(&active.session));
        self.debt.extend(report.pending.clone());
        StopReport {
            session: Some(active.session),
            reason,
            teardown: report,
        }
    }

    // ------------------------------------------------------------ peer events

    /// The call layer says the phone's link is down. Stops the route only if it belongs to
    /// `phone`; another phone's route is untouched.
    pub fn notify_link_lost(&mut self, phone: BtAddress) -> Option<StopReport> {
        (self.active.as_ref()?.phone == phone).then(|| self.stop(StopReason::LinkLost))
    }

    /// The call layer reports the transport of `phone`. Anything but `active` ends its route.
    pub fn notify_transport(
        &mut self,
        phone: BtAddress,
        evidence: ControlEvidence,
    ) -> Option<StopReport> {
        (self.active.as_ref()?.phone == phone && evidence.transport != TransportState::Active)
            .then(|| self.stop(StopReason::TransportDown))
    }

    fn health(&self, snap: &ServerSnapshot, active: &ActiveRoute) -> Result<(), StopReason> {
        if active
            .modules
            .iter()
            .any(|m| !Self::usable(m, classify(m, &snap.modules), self.epoch))
        {
            return Err(StopReason::ModuleVanished);
        }
        match assess(snap, &active.plan.request).declared {
            DeclaredFeasibility::Ready { plan } if plan.same_targets(&active.plan) => Ok(()),
            DeclaredFeasibility::Ready { .. } => Err(StopReason::PhoneAudioLost),
            DeclaredFeasibility::Waiting { blockers }
            | DeclaredFeasibility::Refused { blockers } => {
                if blockers.iter().any(|b| {
                    matches!(
                        b,
                        Blocker::LocalEndpointMissing { .. } | Blocker::LocalEndpointInvalid { .. }
                    )
                }) {
                    Err(StopReason::LocalEndpointLost)
                } else {
                    Err(StopReason::PhoneAudioLost)
                }
            }
        }
    }

    /// Re-read the server and end the route (cleaning only ours) if the phone's voice endpoints
    /// or the local devices disappeared or one of our modules unloaded itself.
    pub fn reconcile(&mut self) -> Health {
        let Some(active) = self.active.clone() else {
            return Health::NoRoute;
        };
        let snap = match self.read() {
            Ok(s) => s,
            Err(e) => return Health::ServerUnreachable(e),
        };
        match self.health(&snap, &active) {
            Ok(()) => Health::Healthy,
            Err(reason) => Health::Ended(Box::new(self.stop(reason))),
        }
    }

    /// After [`Health::ServerUnreachable`] or a connection error: reconnect once and settle.
    ///
    /// * Same server instance proven (non-zero matching cookie): our modules are re-verified
    ///   and the route is *adopted*; nothing is reloaded, so no duplicates.
    /// * Otherwise indices are not trusted: only modules carrying our session tag are removed
    ///   and the route is reported ended. The caller starts again explicitly.
    pub fn recover(&mut self) -> Result<RecoverReport, ServerError> {
        let before = self
            .active
            .as_ref()
            .map(|a| a.server.clone())
            .or_else(|| self.last_identity.clone());
        self.server.reconnect()?;
        self.epoch += 1;
        let snap = self.read()?;
        let same = before
            .as_ref()
            .is_some_and(|b| b.proves_same_instance(&snap.identity));
        if same {
            let epoch = self.epoch;
            self.debt.iter_mut().for_each(|m| m.epoch = epoch);
            if let Some(a) = self.active.as_mut() {
                a.modules.iter_mut().for_each(|m| m.epoch = epoch);
            }
        }
        let route = match self.active.clone() {
            None => RecoverRoute::NoRoute,
            Some(active) if same => match self.health(&snap, &active) {
                Ok(()) => RecoverRoute::Adopted,
                Err(reason) => RecoverRoute::Ended(Box::new(self.stop(reason))),
            },
            Some(_) => RecoverRoute::Ended(Box::new(self.stop(StopReason::ServerRestarted))),
        };
        if !self.debt.is_empty() {
            self.retry_cleanup();
        }
        Ok(RecoverReport {
            route,
            debt_remaining: self.debt.len(),
        })
    }

    /// Unload GeekBridge-tagged modules left by sessions that no longer exist (a crashed
    /// process), as listed by [`Self::inventory`]. Each is re-read first: it must still be the
    /// same module, carry the same session tag, belong to no live session, and report its
    /// arguments. Anything else is left alone.
    pub fn reap_orphans(&mut self, orphans: &[OrphanModule]) -> TeardownReport {
        let mut report = TeardownReport::default();
        let snap = match self.read() {
            Ok(s) => s,
            Err(e) => {
                report.server_error = Some(e);
                return report;
            }
        };
        let live = self.live_sessions();
        let still: Vec<OrphanModule> = orphans_in(&snap, &live);
        for o in orphans.iter().rev() {
            let own = OwnedModule {
                index: o.index,
                module: if o.module == MODULE_ECHO_CANCEL {
                    MODULE_ECHO_CANCEL
                } else {
                    MODULE_LOOPBACK
                },
                role: if o.module == MODULE_ECHO_CANCEL {
                    ModuleRole::EchoCancel
                } else {
                    ModuleRole::LoopbackRx
                },
                session: o.session.clone(),
                epoch: self.epoch,
            };
            if !still.contains(o) {
                report.left_untouched.push(UntouchedModule {
                    module: own,
                    reason: "no longer an orphaned GeekBridge module",
                });
                continue;
            }
            match self.server.unload_module(o.index) {
                Ok(()) => report.unloaded.push(own),
                Err(e) => {
                    report.failed.push(FailedUnload {
                        module: own.clone(),
                        error: e.to_string(),
                    });
                    report.pending.push(own);
                    if e.is_connection_loss() {
                        report.server_error = Some(e);
                        break;
                    }
                }
            }
        }
        report
    }
}

impl<S: AudioServer> Drop for AudioRouteManager<S> {
    fn drop(&mut self) {
        if self.active.is_some() || !self.debt.is_empty() {
            let _ = self.stop(StopReason::Shutdown);
        }
    }
}
