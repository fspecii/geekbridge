// SPDX-License-Identifier: GPL-3.0-or-later
//! Re-pairing a phone while the machine is overloaded (soak 5: two re-pairs of the stand-in phone never
//! reached "connected" with the load average at 150 and the disk at 78% I/O pressure; docs/reports/soak-5.md,
//! docs/reports/repair-under-load.md). A busy disk was not available on demand, so the delays are injected:
//!
//! * the service's trust store is wrapped in [`SlowStore`], which serialises its calls like the file lock
//!   does and sleeps before each one lands (insert, remove, get) or fails an insert with the lock timeout;
//! * the consent / share / privacy files use `statefile::set_write_delay_ms` (process-wide: one test at a time);
//! * the TLS accept and the identity exchange are held back by `bridge_chaos::FaultProxy::handshake_delay`
//!   in front of the phone and in front of the service's inbound listener.
//!
//! Nothing here is compiled into a release binary. Each scenario runs `GB_REPAIR_ITERATIONS` times (default 25,
//! 100 for the report) with delays drawn from a seeded generator (`GB_REPAIR_SEED`); a failure names the
//! iteration, so it replays. Every iteration ends with the same checks (`converge`): the phone is paired and the
//! link connected within a bound, exactly one link on each side (no half-open leftovers), the trust record and
//! the consents are the ones expected once every in-flight write has landed, and a call still rings through.

mod support;

use bridge_core::calls_link::{self as link, LinkCallReport, PhoneState};
use bridge_core::ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Status};
use bridge_kde::{self as kde, TrustStore as _};
use bridge_service::{
    SharedState,
    daemon::{Daemon, DaemonOptions},
    private::PrivateState,
    serve_api, serve_inbound,
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use support::chaos::{FaultProxy, Faults};
use support::{NoProbe, Rig, Rng, Yes, accept_kde_peer, link_of, listener};
use tokio::{
    sync::broadcast,
    task::JoinHandle,
    time::{sleep, timeout},
};

/// The process-wide write delay and the shared hooks: one scenario at a time.
static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How long the stand-in phone's driver waits for "connected" after approving (soak driver: 25 s).
const CONNECTED_WITHIN: Duration = Duration::from_secs(25);
/// The whole re-pair, over all attempts and back-offs, must end connected within this.
const REPAIR_WITHIN: Duration = Duration::from_secs(40);

fn iterations() -> u64 {
    std::env::var("GB_REPAIR_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(25)
}

fn base_seed() -> u64 {
    std::env::var("GB_REPAIR_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5eed_0005)
}

fn rng_for(scenario: u64, i: u64) -> Rng {
    let mut r = Rng(base_seed() ^ (scenario << 40) ^ (i.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1));
    for _ in 0..4 {
        r.next();
    }
    r
}

fn between(r: &mut Rng, lo: u64, hi: u64) -> u64 {
    lo + r.next() % (hi - lo).max(1)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ------------------------------------------------------------------------------------------ slow trust store

#[derive(Default)]
struct Slow {
    get_ms: AtomicU32,
    insert_ms: AtomicU32,
    remove_ms: AtomicU32,
    /// The next this-many inserts fail with the file-lock timeout.
    fail_inserts: AtomicU32,
    /// Calls inside the store now (waiting for the lock or sleeping).
    active: AtomicU32,
    inserts: AtomicU32,
    removes: AtomicU32,
    lock: Mutex<()>,
}

struct SlowStore(Arc<dyn kde::TrustStore>, Arc<Slow>);

fn nap(ms: &AtomicU32) {
    let ms = ms.load(Relaxed);
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}

struct Active<'a>(&'a AtomicU32);
impl<'a> Active<'a> {
    fn new(a: &'a AtomicU32) -> Self {
        a.fetch_add(1, Relaxed);
        Self(a)
    }
}
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

impl kde::TrustStore for SlowStore {
    fn get(&self, id: &kde::DeviceId) -> kde::Result<Option<kde::TrustedPeer>> {
        nap(&self.1.get_ms);
        self.0.get(id)
    }
    fn insert(&self, peer: kde::TrustedPeer) -> kde::Result<()> {
        let _a = Active::new(&self.1.active);
        let _g = self.1.lock.lock().unwrap_or_else(|e| e.into_inner());
        nap(&self.1.insert_ms);
        self.1.inserts.fetch_add(1, Relaxed);
        // Under the store lock: the load and the store cannot interleave with another insert.
        let failing = self.1.fail_inserts.load(Relaxed);
        if failing > 0 {
            self.1.fail_inserts.store(failing - 1, Relaxed);
            return Err(kde::KdeError::Store(
                "timed out waiting for the trust store lock".into(),
            ));
        }
        self.0.insert(peer)
    }
    fn remove(&self, id: &kde::DeviceId) -> kde::Result<bool> {
        let _a = Active::new(&self.1.active);
        let _g = self.1.lock.lock().unwrap_or_else(|e| e.into_inner());
        nap(&self.1.remove_ms);
        self.1.removes.fetch_add(1, Relaxed);
        self.0.remove(id)
    }
    fn list(&self) -> kde::Result<Vec<kde::TrustedPeer>> {
        self.0.list()
    }
}

// ------------------------------------------------------------------------------------------ the stand-in phone

#[derive(Clone)]
enum Cmd {
    Close,
    Send(kde::Packet),
    Unpair,
}

/// A phone with a fixed identity that listens (behind a fault proxy), holds every trusted link it ends up
/// with and counts them. Like the soak's stand-in it ignores a re-pair request from a service that forgot it.
struct Sim {
    creds: Arc<kde::DeviceCredentials>,
    endpoint: Arc<kde::Endpoint>,
    store: Arc<kde::MemoryTrustStore>,
    proxy: FaultProxy,
    cmds: broadcast::Sender<Cmd>,
    /// Answer a re-pair request like the app (the user accepts); the soak's old stand-in ignored it.
    reaccept: bool,
    live: Arc<AtomicUsize>,
    /// Trusted links this phone ever ended up holding (it accepted a pairing).
    held: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Sim {
    fn drop(&mut self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }
}

fn ringing() -> kde::Packet {
    let mut r = LinkCallReport::idle(true, true);
    r.state = PhoneState::Ringing;
    r.number = Some(bridge_core::calls::Sensitive::new("+15550100"));
    r.name = Some(bridge_core::calls::Sensitive::new("Ada"));
    kde::Packet::new(link::STATE, r.body())
}

impl Sim {
    async fn new() -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let store = Arc::new(kde::MemoryTrustStore::new());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        let incoming = vec![
            "kdeconnect.ping".to_string(),
            link::REQUEST.to_string(),
            bridge_core::heartbeat::MARKER.to_string(),
            bridge_core::goodbye::GOODBYE.to_string(),
        ];
        let outgoing = vec![
            "kdeconnect.ping".to_string(),
            link::STATE.to_string(),
            link::RESULT.to_string(),
            bridge_core::heartbeat::MARKER.to_string(),
        ];
        let endpoint = Arc::new(
            kde::Endpoint::new(
                creds.clone(),
                "Test Phone",
                kde::DeviceType::Phone,
                incoming,
                outgoing,
                store.clone(),
                config,
            )
            .unwrap(),
        );
        let l = listener().await;
        let proxy = FaultProxy::start_with(l.local_addr().unwrap(), Faults::default(), 7).await;
        let (cmds, _) = broadcast::channel(16);
        let sim = Arc::new(Self {
            creds,
            endpoint,
            store,
            proxy,
            cmds,
            reaccept: std::env::var_os("GB_REPAIR_IGNORE_REPAIR").is_none(),
            live: Arc::default(),
            held: Arc::default(),
            accepted: Arc::default(),
            tasks: Mutex::default(),
        });
        let s = sim.clone();
        let server = tokio::spawn(async move {
            loop {
                let stream = accept_kde_peer(&l).await;
                s.accepted.fetch_add(1, Relaxed);
                let s2 = s.clone();
                let t = tokio::spawn(async move {
                    let hs = timeout(Duration::from_secs(60), s2.endpoint.accept(stream)).await;
                    s2.adopt(hs.ok().and_then(Result::ok), PairMode::Await)
                        .await;
                });
                s.tasks.lock().unwrap().push(t);
            }
        });
        sim.tasks.lock().unwrap().push(server);
        sim
    }

    fn id(&self) -> String {
        self.creds.device_id().to_string()
    }

    fn endpoint_for_service(&self) -> ipc::Endpoint {
        ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port: self.proxy.port(),
        }
    }

    /// Take the result of a handshake: hold the link; an unpaired peer is paired (waiting for its request, or
    /// asking itself when it dialed).
    async fn adopt(self: &Arc<Self>, hs: Option<kde::Handshake>, mode: PairMode) {
        let trace = std::env::var_os("GB_REPAIR_TRACE").is_some();
        let link = match hs {
            Some(kde::Handshake::Trusted(link)) => {
                if trace {
                    eprintln!("phone: trusted link");
                }
                link
            }
            Some(kde::Handshake::Pending(p)) => {
                let m = match mode {
                    PairMode::Request => kde::PairMode::Request,
                    PairMode::Await => kde::PairMode::AwaitPeerRequest,
                };
                if trace {
                    eprintln!("phone: pending link, pairing {m:?}");
                }
                match p.pair(m, &mut Yes).await {
                    Ok(link) => link,
                    Err(e) => {
                        if trace {
                            eprintln!("phone: pairing ended: {e}");
                        }
                        return;
                    }
                }
            }
            None => {
                if trace {
                    eprintln!("phone: handshake failed");
                }
                return;
            }
        };
        self.hold(link).await;
        if trace {
            eprintln!("phone: link ended");
        }
    }

    async fn hold(&self, mut link: kde::TrustedLink) {
        struct Count(Arc<AtomicUsize>);
        impl Drop for Count {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Relaxed);
            }
        }
        self.live.fetch_add(1, Relaxed);
        self.held.fetch_add(1, Relaxed);
        let _count = Count(self.live.clone());
        let mut cmds = self.cmds.subscribe();
        loop {
            tokio::select! {
                ev = link.recv() => match ev {
                    Ok(kde::LinkEvent::PeerUnpaired) | Err(_) => return,
                    Ok(kde::LinkEvent::PeerRequestedRePair { .. }) if self.reaccept => {
                        // As the app does: the user accepts the service's request on this connection.
                        match link.into_re_pairing() {
                            Ok(pending) => match pending.pair(kde::PairMode::AwaitPeerRequest, &mut Yes).await {
                                Ok(again) => link = again,
                                Err(_) => return,
                            },
                            Err(back) => link = back,
                        }
                    }
                    Ok(_) => {}
                },
                c = cmds.recv() => match c {
                    Ok(Cmd::Close) | Err(broadcast::error::RecvError::Closed) => return,
                    Ok(Cmd::Send(p)) => { let _ = link.send(&p).await; }
                    Ok(Cmd::Unpair) => { let _ = link.unpair().await; return; }
                    Err(_) => {}
                },
            }
        }
    }

    /// The phone dials the service (its redial, or a reconnect to declare capabilities).
    fn dial_in(self: &Arc<Self>, to: SocketAddr, service: &str, mode: PairMode) {
        let (s, target) = (self.clone(), kde::DeviceId::parse(service).unwrap());
        let t = tokio::spawn(async move {
            let hs = timeout(Duration::from_secs(60), s.endpoint.connect(to, &target)).await;
            s.adopt(hs.ok().and_then(Result::ok), mode).await;
        });
        self.tasks.lock().unwrap().push(t);
    }

    /// Redial every 1.6-2.4 s while `when` says so.
    fn redial(
        self: &Arc<Self>,
        to: SocketAddr,
        service: String,
        seed: u64,
        always: bool,
    ) -> JoinHandle<()> {
        let s = self.clone();
        tokio::spawn(async move {
            let mut rng = Rng(seed | 1);
            loop {
                sleep(ms(between(&mut rng, 1600, 2400))).await;
                if always || s.live.load(Relaxed) == 0 {
                    s.dial_in(to, &service, PairMode::Request);
                }
            }
        })
    }

    fn send(&self, c: Cmd) {
        let _ = self.cmds.send(c);
    }
}

// ------------------------------------------------------------------------------------------ the service

struct Lab {
    rig: Rig,
    slow: Arc<Slow>,
    service_id: String,
    inbound_proxy: FaultProxy,
    approving: Arc<AtomicBool>,
    _approver: JoinHandle<()>,
}

impl Lab {
    async fn start() -> Self {
        Self::start_with(Duration::from_secs(45)).await
    }

    /// `persist_timeout`: the service's deadline for the trust write.
    async fn start_with(persist_timeout: Duration) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let discovery: SharedState =
            Arc::new(tokio::sync::RwLock::new(bridge_core::Snapshot::empty(1716)));
        let state = PrivateState::open(&state_dir).map_err(|e| e.1).unwrap();
        let slow = Arc::new(Slow::default());
        let store = Arc::new(SlowStore(state.store.clone(), slow.clone()));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(NoProbe));
        options.dev_loopback = true;
        options.transport.pairing.persist_timeout = persist_timeout;
        options.privacy_dir = Some(state_dir.clone());
        options.share.settings_path = Some(state_dir.join("share-settings.json"));
        options.share.save_dir = tmp.path().join("dl");
        let service_id = state.credentials.device_id().to_string();
        let daemon: Arc<Daemon> = Daemon::new(
            state.credentials.clone(),
            store,
            state.consents,
            discovery.clone(),
            options,
        )
        .unwrap();
        let control = bridge_service::control::Control::Ready {
            daemon: daemon.clone(),
            token: state.token.clone(),
        };
        let control_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_port = control_listener.local_addr().unwrap().port();
        let api_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let inbound = inbound_listener.local_addr().unwrap();
        daemon.set_inbound_listener(inbound);
        let tasks = vec![
            tokio::spawn(bridge_service::control::serve_control(
                control_listener,
                Arc::new(control),
            )),
            tokio::spawn(serve_api(api_listener, discovery.clone())),
            tokio::spawn(serve_inbound(inbound_listener, daemon.clone())),
        ];
        let rig = Rig {
            _tmp: tmp,
            state_dir,
            daemon,
            discovery,
            token: state.token,
            control_port,
            tasks,
        };
        let inbound_proxy = FaultProxy::start_with(inbound, Faults::default(), 11).await;
        // The user: approves every link request that appears (unless `approving` is switched off).
        let approving = Arc::new(AtomicBool::new(true));
        let approver = {
            let (port, token) = (rig.control_port, rig.token.clone());
            let on = approving.clone();
            tokio::spawn(async move {
                loop {
                    let (t, tk) = (token.clone(), token.clone());
                    let st = tokio::task::spawn_blocking(move || ipc::status(port, &t))
                        .await
                        .ok()
                        .and_then(Result::ok);
                    let challenges = st.map(|s| s.challenges).unwrap_or_default();
                    for c in challenges.into_iter().filter(|_| on.load(Relaxed)) {
                        let tk = tk.clone();
                        let _ = tokio::task::spawn_blocking(move || {
                            ipc::call(
                                port,
                                &tk,
                                &Request::Approve {
                                    challenge_id: c.challenge_id,
                                },
                            )
                        })
                        .await;
                    }
                    sleep(ms(15)).await;
                }
            })
        };
        Self {
            rig,
            slow,
            service_id,
            inbound_proxy,
            approving,
            _approver: approver,
        }
    }

    /// Connect to the phone and ask it to pair, without waiting for the outcome.
    async fn begin_pairing(&self, sim: &Sim) {
        let id = sim.id();
        self.rig
            .call(Request::Connect {
                device_id: id.clone(),
                endpoint: Some(sim.endpoint_for_service()),
            })
            .await
            .expect("connect");
        assert!(
            self.until(Duration::from_secs(10), |s| link_of(s, &id)
                .is_some_and(|l| l.phase == LinkPhase::Unpaired))
                .await,
            "never reached the pairing step"
        );
        self.rig
            .call(Request::Pair {
                device_id: id,
                mode: PairMode::Request,
            })
            .await
            .expect("pair");
    }

    /// Wait until the phone accepted and the service's trust write is running, then disconnect.
    async fn cancel_during_the_write(&self, sim: &Sim) {
        let end = Instant::now() + Duration::from_secs(15);
        while sim.held.load(Relaxed) == 0 || self.slow.active.load(Relaxed) == 0 {
            assert!(Instant::now() < end, "the write never started");
            sleep(ms(5)).await;
        }
        self.rig
            .call(Request::Disconnect {
                device_id: sim.id(),
            })
            .await
            .ok();
    }

    /// Wait for `cond`. `within` is how long the service may go without making progress (a new event, or
    /// a trust-store call in flight), not a wall-clock budget: the injected delays are exact, but a real
    /// fsync on a busy disk (or a CPU-starved runner) adds time of its own, and a test that fails on that
    /// says nothing about the pairing. A stuck service still fails after `within`; one that keeps working
    /// gets up to three times that.
    async fn until(&self, within: Duration, cond: impl Fn(&Status) -> bool) -> bool {
        let start = Instant::now();
        let (hard, mut idle_end, mut seen) = (start + within * 3, start + within, 0);
        loop {
            let st = self.rig.status().await;
            if cond(&st) {
                return true;
            }
            let now = Instant::now();
            if st.event_seq != seen || !self.quiet() {
                seen = st.event_seq;
                idle_end = now + within;
            }
            if now >= hard || now >= idle_end {
                return false;
            }
            sleep(ms(10)).await;
        }
    }

    /// The soak driver's `repair_p0` after its unpair: connect to the phone, ask to pair, approve, wait for
    /// "connected"; up to three attempts. A link that is already connected is a success (the pairing exists).
    /// `Busy` (an unpair still running) is retried without costing an attempt. Returns the attempts used.
    async fn repair(&self, sim: &Sim, connected_within: Duration) -> Result<(u32, u32), String> {
        let id = sim.id();
        let (mut busy, mut why) = (0u32, "connect");
        let connected =
            |s: &Status| link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Connected);
        for attempt in 1..=3u32 {
            let _ = self
                .rig
                .call(Request::Disconnect {
                    device_id: id.clone(),
                })
                .await;
            // Connect (Busy: the phone is still being unpaired).
            let end = Instant::now() + Duration::from_secs(20);
            loop {
                match self
                    .rig
                    .call(Request::Connect {
                        device_id: id.clone(),
                        endpoint: Some(sim.endpoint_for_service()),
                    })
                    .await
                {
                    Ok(_) => break,
                    Err(ipc::IpcError::Service(e)) if e.code == ipc::ErrorCode::Busy => {
                        busy += 1;
                        if connected(&self.rig.status().await) {
                            return Ok((attempt, busy));
                        }
                        if Instant::now() > end {
                            return Err("connect stayed Busy".into());
                        }
                        sleep(ms(40)).await;
                    }
                    Err(_) => break,
                }
            }
            why = "unpaired-phase";
            if !self
                .until(Duration::from_secs(10), |s| {
                    connected(s) || link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Unpaired)
                })
                .await
            {
                sleep(ms(300)).await;
                continue;
            }
            if connected(&self.rig.status().await) {
                return Ok((attempt, busy));
            }
            let _ = self
                .rig
                .call(Request::Pair {
                    device_id: id.clone(),
                    mode: PairMode::Request,
                })
                .await;
            why = "connected";
            if self.until(connected_within, connected).await {
                return Ok((attempt, busy));
            }
            sleep(ms(300)).await;
        }
        let st = self.rig.status().await;
        Err(format!(
            "never connected; last stopped at '{why}'; links {:?}; trust records {}; phone live {}, phone trusts service {}; events {:?}",
            st.links
                .iter()
                .map(|l| (&l.phase, l.role))
                .collect::<Vec<_>>(),
            self.rig.trust_len(),
            sim.live.load(Relaxed),
            sim.store
                .get(&kde::DeviceId::parse(&self.service_id).unwrap())
                .unwrap()
                .is_some(),
            self.rig
                .events()
                .await
                .events
                .iter()
                .rev()
                .take(14)
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        ))
    }

    async fn set_telephony(&self, id: &str, on: bool) {
        // The control API drops a connection when its eight slots are taken; a refused one is tried again,
        // and a slot that stays taken for long is reported.
        let started = Instant::now();
        loop {
            let r = self
                .rig
                .call(Request::SetConsent {
                    device_id: id.to_owned(),
                    feature: Feature::Telephony,
                    enabled: on,
                })
                .await;
            if matches!(r, Ok(Response::Ack)) {
                if started.elapsed() > ms(1000) {
                    eprintln!("set consent needed {:?}", started.elapsed());
                }
                return;
            }
            if started.elapsed() >= Duration::from_secs(20)
                && std::env::var_os("GB_REPAIR_HANG").is_some()
            {
                eprintln!("HANG pid {}", std::process::id());
                sleep(Duration::from_secs(120)).await;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "set consent: {r:?}; trust records {}, status {:#?}",
                self.rig.trust_len(),
                self.rig.status().await
            );
            sleep(ms(100)).await;
        }
    }

    /// First pairing from nothing, with the Telephony consent on.
    async fn pair_fresh(&self, sim: &Sim) {
        self.repair(sim, CONNECTED_WITHIN)
            .await
            .expect("first pairing");
        self.set_telephony(&sim.id(), true).await;
    }

    fn quiet(&self) -> bool {
        self.slow.active.load(Relaxed) == 0
    }

    async fn quiesce(&self) {
        let end = Instant::now() + Duration::from_secs(30);
        while !self.quiet() {
            assert!(Instant::now() < end, "a trust-store call never finished");
            sleep(ms(10)).await;
        }
    }

    /// Quiet: no trust-store call running and no new event for a while (the consent, share and privacy
    /// files are written after the event that triggers them, so they are covered by the wait).
    async fn settle(&self) {
        let end = Instant::now() + Duration::from_secs(60);
        let (mut seq, mut since) = (0, Instant::now());
        loop {
            let now = self.rig.status().await.event_seq;
            if now != seq || !self.quiet() {
                (seq, since) = (now, Instant::now());
            } else if since.elapsed() >= ms(500) {
                return;
            }
            assert!(Instant::now() < end, "the service never went quiet");
            sleep(ms(20)).await;
        }
    }

    fn telephony(status: &Status, id: &str) -> Option<bool> {
        status
            .paired
            .iter()
            .find(|d| d.device_id == id)
            .and_then(|d| d.consents.iter().find(|c| c.feature == Feature::Telephony))
            .map(|c| c.enabled)
    }
}

/// What the consent should be after the iteration.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Consent {
    /// The Telephony consent given before is still on.
    Kept,
    /// It was forgotten by an unpair.
    Forgotten,
    /// Either is acceptable (an unpair raced a pairing); reported.
    Either,
}

/// The end state of one iteration. Panics, naming `what`, when anything is off.
async fn converge(lab: &Lab, sim: &Arc<Sim>, what: &str, consent: Consent) -> Consent {
    let id = sim.id();
    let ctx = |why: &str| format!("[{what}] {why}");
    // Paired and connected in time; a single link in the table.
    let ok = lab
        .until(REPAIR_WITHIN, |s| {
            link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    if !ok {
        panic!(
            "{}",
            ctx(&format!(
                "never connected: links {:?}; trust records {}; phone live {}, phone trusts service {}; events {:?}",
                lab.rig
                    .status()
                    .await
                    .links
                    .iter()
                    .map(|l| (&l.phase, l.role))
                    .collect::<Vec<_>>(),
                lab.rig.trust_len(),
                sim.live.load(Relaxed),
                sim.store
                    .get(&kde::DeviceId::parse(&lab.service_id).unwrap())
                    .unwrap()
                    .is_some(),
                lab.rig
                    .events()
                    .await
                    .events
                    .iter()
                    .rev()
                    .take(16)
                    .map(|e| format!("{:?}", e.kind))
                    .collect::<Vec<_>>()
            ))
        );
    }
    // Every write has landed; then nothing may change any more (a late delete would show up here).
    lab.settle().await;
    let st = lab.rig.status().await;
    assert_eq!(
        st.links.len(),
        1,
        "{}",
        ctx(&format!(
            "half-open leftovers or no link: {:#?}; phone holds {}; events {:?}",
            st.links,
            sim.live.load(Relaxed),
            lab.rig
                .events()
                .await
                .events
                .iter()
                .rev()
                .take(16)
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        ))
    );
    let l = link_of(&st, &id).unwrap();
    assert_eq!(
        l.phase,
        LinkPhase::Connected,
        "{}",
        ctx("link not connected after the writes settled")
    );
    assert!(
        st.challenges.is_empty(),
        "{}",
        ctx("a challenge is left over")
    );
    assert!(
        st.paired.iter().any(|d| d.device_id == id),
        "{}",
        ctx(&format!(
            "the service no longer lists the phone as paired while the link is connected (trust records {}, link {l:?}, events {:?})",
            lab.rig.trust_len(),
            lab.rig
                .events()
                .await
                .events
                .iter()
                .rev()
                .take(14)
                .map(|e| format!("{:?}", e.kind))
                .collect::<Vec<_>>()
        ))
    );
    assert_eq!(lab.rig.trust_len(), 1, "{}", ctx("trust records"));
    let record = kde::FileTrustStore::open(&lab.rig.state_dir)
        .unwrap()
        .get(&kde::DeviceId::parse(&id).unwrap())
        .unwrap()
        .expect("the trust record is missing");
    assert_eq!(
        record.cert.fingerprint(),
        sim.creds.cert().fingerprint(),
        "{}",
        ctx("the record pins another certificate")
    );
    // The phone agrees: it trusts the service and holds exactly one link.
    assert!(
        sim.store
            .get(&kde::DeviceId::parse(&lab.service_id).unwrap())
            .unwrap()
            .is_some(),
        "{}",
        ctx("the phone forgot the service while the service holds a connected link")
    );
    let end = Instant::now() + Duration::from_secs(30);
    while sim.live.load(Relaxed) != 1 {
        assert!(
            Instant::now() < end,
            "{}",
            ctx(&format!("the phone holds {} links", sim.live.load(Relaxed)))
        );
        sleep(ms(20)).await;
    }
    // Consents.
    let got = Lab::telephony(&st, &id);
    let seen = match got {
        Some(true) => Consent::Kept,
        _ => Consent::Forgotten,
    };
    if consent != Consent::Either {
        assert_eq!(seen, consent, "{}", ctx("consent"));
    }
    // The link carries traffic: a ring from the phone shows on the call list (only when the consent is on).
    if seen == Consent::Kept {
        sim.send(Cmd::Send(ringing()));
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let calls = match lab.rig.call(Request::Calls).await {
                Ok(Response::Calls(c)) => *c,
                other => panic!("{}", ctx(&format!("calls: {other:?}"))),
            };
            if calls.view.ringing().is_some() {
                break;
            }
            assert!(
                Instant::now() < end,
                "{}",
                ctx("the ring never showed on the connected link")
            );
            sleep(ms(25)).await;
        }
    }
    seen
}

/// Run `body` once per iteration on a freshly paired lab (kept across iterations: that is the soak's shape).
async fn run(
    scenario: u64,
    name: &str,
    body: impl AsyncFn(&Lab, &Arc<Sim>, &mut Rng, u64) -> Consent,
) {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.pair_fresh(&sim).await;
    let (mut kept, mut forgotten) = (0, 0);
    let started = Instant::now();
    for i in 0..iterations() {
        let mut rng = rng_for(scenario, i);
        let seen = body(&lab, &sim, &mut rng, i).await;
        bridge_core::statefile::set_write_delay_ms(0);
        lab.slow.get_ms.store(0, Relaxed);
        lab.slow.insert_ms.store(0, Relaxed);
        lab.slow.remove_ms.store(0, Relaxed);
        lab.slow.fail_inserts.store(0, Relaxed);
        lab.inbound_proxy.set(Faults::default());
        sim.proxy.set(Faults::default());
        match seen {
            Consent::Kept => kept += 1,
            _ => forgotten += 1,
        }
        // The next iteration starts from the shape the previous one proved: connected, consent on.
        lab.set_telephony(&sim.id(), true).await;
    }
    eprintln!(
        "repair-under-load {name}: {} iterations in {:.1}s, consent kept {kept} / forgotten {forgotten}, inserts {} removes {}",
        iterations(),
        started.elapsed().as_secs_f64(),
        lab.slow.inserts.load(Relaxed),
        lab.slow.removes.load(Relaxed)
    );
}

fn label(what: &str, i: u64) -> String {
    format!("{what} iteration {i} seed {:#x}", base_seed())
}

/// Unpair through the control API, as the soak driver does, and wait for the answer.
async fn unpair(lab: &Lab, id: &str) -> bool {
    let r = lab
        .rig
        .call(Request::Unpair {
            device_id: id.to_owned(),
        })
        .await;
    let st = lab.rig.status().await;
    assert!(
        matches!(r, Ok(Response::Ack)),
        "unpair: {r:?}; trust records {}, links {:?}, paired {}",
        lab.rig.trust_len(),
        st.links
            .iter()
            .map(|l| (&l.phase, l.role))
            .collect::<Vec<_>>(),
        st.paired.len()
    );
    true
}

// ------------------------------------------------------------------------------------------ scenarios

/// 1. Re-pair while the previous pairing's unpair task still runs: the connect is refused as Busy until it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_while_the_unpair_task_still_runs() {
    run(1, "unpair-busy", async |lab, sim, rng, i| {
        lab.slow
            .remove_ms
            .store(between(rng, 50, 700) as u32, Relaxed);
        let id = sim.id();
        let unpairing = {
            let (port, token, id) = (lab.rig.control_port, lab.rig.token.clone(), id.clone());
            tokio::task::spawn_blocking(move || {
                ipc::call(port, &token, &Request::Unpair { device_id: id })
            })
        };
        sleep(ms(between(rng, 0, 400))).await;
        let (attempts, busy) = lab
            .repair(sim, CONNECTED_WITHIN)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", label("unpair-busy", i)));
        assert!(
            matches!(unpairing.await.unwrap(), Ok(Response::Ack)),
            "the unpair must have succeeded"
        );
        let _ = (attempts, busy);
        converge(lab, sim, &label("unpair-busy", i), Consent::Forgotten).await
    })
    .await;
}

/// 2. Re-pair during a slow trust-record write, longer than the phone's wait for "connected" (and, in some
/// iterations, the file lock timing out first).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repair_during_a_trust_write_slower_than_the_phones_wait() {
    run(2, "slow-trust-write", async |lab, sim, rng, i| {
        assert!(unpair(lab, &sim.id()).await);
        lab.slow
            .insert_ms
            .store(between(rng, 100, 1800) as u32, Relaxed);
        lab.slow
            .fail_inserts
            .store(between(rng, 0, 3) as u32, Relaxed);
        // The phone's wait is shorter than the write in about half of the iterations.
        let wait = ms(between(rng, 300, 1500));
        // The driver may give up (its wait is shorter than the write); the pairing still has to finish by itself.
        let _ = lab.repair(sim, wait).await;
        converge(lab, sim, &label("slow-trust-write", i), Consent::Forgotten).await
    })
    .await;
}

/// 3. A second connection from the same device inside the tie-break window (the phone dials while the
/// service's own link is brand new). Pairing and consents stay; one link survives on each side.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_connect_inside_the_tie_break_window() {
    run(3, "duplicate-connect", async |lab, sim, rng, i| {
        let id = sim.id();
        let _ = lab
            .rig
            .call(Request::Disconnect {
                device_id: id.clone(),
            })
            .await;
        assert!(
            lab.until(Duration::from_secs(10), |s| s.links.is_empty())
                .await
        );
        // Slow TLS accept / identity on the way in, a slow disk behind it.
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 300)));
        lab.slow.get_ms.store(between(rng, 0, 30) as u32, Relaxed);
        bridge_core::statefile::set_write_delay_ms(between(rng, 0, 120) as u32);
        let _ = lab
            .rig
            .call(Request::Connect {
                device_id: id.clone(),
                endpoint: Some(sim.endpoint_for_service()),
            })
            .await;
        sleep(ms(between(rng, 0, 400))).await;
        sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        // Sometimes a third one, from the phone's redial.
        if rng.below(2) == 0 {
            sleep(ms(between(rng, 0, 300))).await;
            sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        }
        // The loser must go away on both ends; a stray phone link dies when the service turns it away.
        // (The kept connection is the smaller id's; with a half-open leftover the next redial repairs it.)
        let redial = sim.redial(
            lab.inbound_proxy.addr(),
            lab.service_id.clone(),
            rng.next(),
            false,
        );
        let seen = converge(lab, sim, &label("duplicate-connect", i), Consent::Kept).await;
        redial.abort();
        seen
    })
    .await;
}

/// 4. The old connection's teardown arrives after the new link's accept: the phone closes its end and
/// redials at once. The teardown must not delete the new link's state, trust or consent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn old_teardown_after_the_new_accept() {
    run(4, "old-teardown", async |lab, sim, rng, i| {
        bridge_core::statefile::set_write_delay_ms(between(rng, 0, 200) as u32);
        lab.slow.get_ms.store(between(rng, 0, 30) as u32, Relaxed);
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 150)));
        // Dial first, then close the old link, in either order within a few ms.
        let first_dial = rng.below(2) == 0;
        if first_dial {
            sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
            sleep(ms(between(rng, 0, 60))).await;
            sim.send(Cmd::Close);
        } else {
            sim.send(Cmd::Close);
            sleep(ms(between(rng, 0, 60))).await;
            sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        }
        // `Close` goes to every link the phone holds, a fresh one included when the dial came first: the
        // phone's redial (1.6-2.4 s) is what brings the link back, as on a real phone.
        let redial = sim.redial(
            lab.inbound_proxy.addr(),
            lab.service_id.clone(),
            rng.next(),
            false,
        );
        let seen = converge(lab, sim, &label("old-teardown", i), Consent::Kept).await;
        redial.abort();
        seen
    })
    .await;
}

/// 5. Dial-back and inbound crossing: no link exists, the service connects to the phone while the phone
/// connects to the service, at offsets of -100..+100 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dial_back_crossing_an_inbound_dial() {
    run(5, "crossing", async |lab, sim, rng, i| {
        let id = sim.id();
        let _ = lab
            .rig
            .call(Request::Disconnect {
                device_id: id.clone(),
            })
            .await;
        assert!(
            lab.until(Duration::from_secs(10), |s| s.links.is_empty())
                .await
        );
        sim.send(Cmd::Close);
        let end = Instant::now() + Duration::from_secs(5);
        while sim.live.load(Relaxed) != 0 {
            assert!(
                Instant::now() < end,
                "the phone kept a link after the service disconnected"
            );
            sleep(ms(10)).await;
        }
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 200)));
        sim.proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 200)));
        lab.slow.get_ms.store(between(rng, 0, 30) as u32, Relaxed);
        let offset = between(rng, 0, 200) as i64 - 100;
        let dial_out = async {
            let _ = lab
                .rig
                .call(Request::Connect {
                    device_id: id.clone(),
                    endpoint: Some(sim.endpoint_for_service()),
                })
                .await;
        };
        let dial_in = async {
            sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        };
        if offset < 0 {
            dial_in.await;
            sleep(ms(offset.unsigned_abs())).await;
            dial_out.await;
        } else {
            dial_out.await;
            sleep(ms(offset as u64)).await;
            dial_in.await;
        }
        let redial = sim.redial(
            lab.inbound_proxy.addr(),
            lab.service_id.clone(),
            rng.next(),
            false,
        );
        let seen = converge(lab, sim, &label("crossing", i), Consent::Kept).await;
        redial.abort();
        seen
    })
    .await;
}

/// 6. The phone unpairs (its end deletes the record, tells the service) and re-pairs at once while the
/// service's delete of the old record is still slow: a late delete must not remove the fresh record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn late_delete_of_the_old_record_after_a_fresh_pairing() {
    run(6, "late-delete", async |lab, sim, rng, i| {
        lab.slow
            .remove_ms
            .store(between(rng, 100, 900) as u32, Relaxed);
        lab.slow
            .insert_ms
            .store(between(rng, 0, 300) as u32, Relaxed);
        bridge_core::statefile::set_write_delay_ms(between(rng, 0, 150) as u32);
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 100)));
        let old_connection =
            link_of(&lab.rig.status().await, &sim.id()).and_then(|l| l.connection_id);
        sim.send(Cmd::Unpair);
        // Gone from the phone's side (its record deleted, the unpair sent) ...
        while sim.live.load(Relaxed) != 0 {
            sleep(ms(2)).await;
        }
        // ... and back at a random moment while the service is still deleting its record.
        sleep(ms(between(rng, 0, 500))).await;
        // The phone asks to pair again, from its side (it has no record now).
        sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        let redial = sim.redial(
            lab.inbound_proxy.addr(),
            lab.service_id.clone(),
            rng.next(),
            false,
        );
        // From the phone's side unpairing and pairing again is one user action: both ends end up paired. Whether the
        // old consent survives depends on whether the service's session saw the unpair, either is accepted.
        let seen = repair_either(lab, sim, &label("late-delete", i), old_connection).await;
        redial.abort();
        seen
    })
    .await;
}

/// The phone-initiated paths end in a pairing made by the phone's own request (the approver task is the user);
/// if only a record without a link is left, the soak driver's service-side connect finishes it.
async fn repair_either(
    lab: &Lab,
    sim: &Arc<Sim>,
    what: &str,
    old_connection: Option<u64>,
) -> Consent {
    let id = sim.id();
    // The service has noticed the unpair: the old link is gone or replaced. (Its consent, share and privacy files
    // are still being written after the delete, with nothing in the event log or the store to show it, so
    // "quiet for a while" alone is not enough on a slow runner.)
    assert!(
        lab.until(Duration::from_secs(40), |s| {
            link_of(s, &id).and_then(|l| l.connection_id) != old_connection
        })
        .await,
        "[{what}] the service never noticed the phone's unpair"
    );
    lab.settle().await;
    let linked = lab
        .until(Duration::from_secs(8), |s| {
            link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    if !linked {
        let _ = lab
            .repair(sim, CONNECTED_WITHIN)
            .await
            .unwrap_or_else(|e| panic!("[{what}] {e}"));
    }
    converge(lab, sim, what, Consent::Either).await
}

/// 7. The service-side unpair (control API) with the phone's redial arriving while it runs: an inbound link
/// must not slip in between the unpair's delete and the phone being told.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_redial_overlapping_an_unpair() {
    run(7, "redial-during-unpair", async |lab, sim, rng, i| {
        let id = sim.id();
        lab.slow
            .remove_ms
            .store(between(rng, 100, 900) as u32, Relaxed);
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 100)));
        let unpairing = {
            let (port, token, id) = (lab.rig.control_port, lab.rig.token.clone(), id.clone());
            tokio::task::spawn_blocking(move || {
                ipc::call(port, &token, &Request::Unpair { device_id: id })
            })
        };
        // The phone's redial lands somewhere inside the unpair.
        sleep(ms(between(rng, 0, 600))).await;
        sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
        let _ = unpairing.await;
        lab.quiesce().await;
        // Unpair is done on the service. The driver re-pairs it; the phone must not be left trusting a service
        // that forgot it.
        let _ = lab.repair(sim, CONNECTED_WITHIN).await;
        converge(lab, sim, &label("redial-during-unpair", i), Consent::Either).await
    })
    .await;
}

/// 8. The phone's redial (1.6-2.4 s) overlapping a pairing that is slow (trust write, TLS, disk).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn phone_redial_overlapping_a_slow_pairing() {
    run(8, "redial-during-pairing", async |lab, sim, rng, i| {
        assert!(unpair(lab, &sim.id()).await);
        lab.slow
            .insert_ms
            .store(between(rng, 500, 2600) as u32, Relaxed);
        lab.slow.get_ms.store(between(rng, 0, 30) as u32, Relaxed);
        bridge_core::statefile::set_write_delay_ms(between(rng, 0, 300) as u32);
        lab.inbound_proxy
            .update(|f| f.handshake_delay = ms(between(rng, 0, 300)));
        let redial = sim.redial(
            lab.inbound_proxy.addr(),
            lab.service_id.clone(),
            rng.next(),
            true,
        );
        let r = lab.repair(sim, CONNECTED_WITHIN).await;
        redial.abort();
        r.unwrap_or_else(|e| panic!("{}: {e}", label("redial-during-pairing", i)));
        converge(
            lab,
            sim,
            &label("redial-during-pairing", i),
            Consent::Forgotten,
        )
        .await
    })
    .await;
}

/// Regression (soak 5): a phone that dials in while its unpair is deleting the trust record must be turned
/// away, not accepted on the record that is about to go. Before, the link stayed "connected" with no record
/// and the phone kept trusting a service that had forgotten it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_inbound_dial_during_an_unpair_is_turned_away() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.pair_fresh(&sim).await;
    let id = sim.id();
    lab.slow.remove_ms.store(800, Relaxed);
    // The service forgets the phone without a live trusted session: drop the link first so the unpair goes
    // the take-down way (the one that never tells the phone).
    sim.send(Cmd::Close);
    assert!(
        lab.until(Duration::from_secs(10), |s| s.links.is_empty())
            .await
    );
    let unpairing = {
        let (port, token, id) = (lab.rig.control_port, lab.rig.token.clone(), id.clone());
        tokio::task::spawn_blocking(move || {
            ipc::call(port, &token, &Request::Unpair { device_id: id })
        })
    };
    sleep(ms(200)).await;
    sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
    let _ = unpairing.await;
    lab.slow.remove_ms.store(0, Relaxed);
    lab.settle().await;
    let st = lab.rig.status().await;
    assert_eq!(lab.rig.trust_len(), 0, "unpaired");
    assert!(
        st.links.is_empty() || st.links.iter().all(|l| l.phase != LinkPhase::Connected),
        "a link is connected with no trust record: {:?}",
        st.links
    );
    let _ = sim;
}

// ------------------------------------------------------------------------ abandoning a pairing

impl Sim {
    fn knows_service(&self, lab: &Lab) -> bool {
        self.store
            .get(&kde::DeviceId::parse(&lab.service_id).unwrap())
            .unwrap()
            .is_some()
    }

    /// The phone accepted (it held a trusted link) and was then told to forget the service, within `within`.
    async fn forgot_after_accepting(&self, lab: &Lab, within: Duration) {
        let end = Instant::now() + Duration::from_secs(15);
        while self.held.load(Relaxed) == 0 {
            assert!(Instant::now() < end, "the phone never accepted");
            sleep(ms(5)).await;
        }
        let t = Instant::now();
        while self.live.load(Relaxed) != 0 || self.knows_service(lab) {
            assert!(
                t.elapsed() < within,
                "the phone still trusts the service {:?} after accepting",
                t.elapsed()
            );
            sleep(ms(5)).await;
        }
    }
}

/// The service's trust write outlasts its deadline (the disk is stalled): it gives up and tells the phone,
/// which had accepted first, to forget it. Before, the phone kept trusting a service with no record.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_trust_write_past_the_deadline_makes_the_phone_forget_the_service() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start_with(ms(600)).await;
    let sim = Sim::new().await;
    lab.slow.insert_ms.store(2500, Relaxed);
    let t = Instant::now();
    lab.begin_pairing(&sim).await;
    // Told at the deadline, long before the slow write ends.
    sim.forgot_after_accepting(&lab, ms(1800)).await;
    assert!(t.elapsed() < ms(2400), "{:?}", t.elapsed());
    lab.quiesce().await;
    lab.settle().await;
    assert!(
        !sim.knows_service(&lab),
        "the phone trusts the service again"
    );
    let st = lab.rig.status().await;
    assert!(
        st.links.iter().all(|l| l.phase != LinkPhase::Connected),
        "{:?}",
        st.links
    );
}

/// The write fails on every attempt (lock timeouts): the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_trust_write_makes_the_phone_forget_the_service() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.slow.fail_inserts.store(3, Relaxed);
    lab.begin_pairing(&sim).await;
    sim.forgot_after_accepting(&lab, Duration::from_secs(5))
        .await;
    lab.settle().await;
    assert_eq!(lab.rig.trust_len(), 0, "no record on the service");
    assert!(!sim.knows_service(&lab));
}

/// The pairing is cancelled while the write is still running (a disconnect, or the service stopping) and the
/// write then fails: the phone is told on the way out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_pairing_during_a_trust_write_that_fails_tells_the_phone() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.slow.insert_ms.store(800, Relaxed);
    lab.slow.fail_inserts.store(3, Relaxed);
    lab.begin_pairing(&sim).await;
    lab.cancel_during_the_write(&sim).await;
    sim.forgot_after_accepting(&lab, Duration::from_secs(8))
        .await;
    lab.quiesce().await;
    assert_eq!(lab.rig.trust_len(), 0);
}

/// The service stops while a pairing's write is still running (it may not live to see the write end): the
/// phone is told at once, without waiting for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_service_during_a_trust_write_tells_the_phone() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.slow.insert_ms.store(4000, Relaxed);
    lab.begin_pairing(&sim).await;
    let end = Instant::now() + Duration::from_secs(15);
    while sim.held.load(Relaxed) == 0 || lab.slow.active.load(Relaxed) == 0 {
        assert!(Instant::now() < end, "the write never started");
        sleep(ms(5)).await;
    }
    lab.rig.daemon.goodbye_all().await;
    sim.forgot_after_accepting(&lab, ms(1500)).await;
    lab.quiesce().await;
}

/// Cancelled during a write that lands: nothing to undo, record and phone agree (the connection is gone).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_a_pairing_during_a_trust_write_that_lands_keeps_the_pairing() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.slow.insert_ms.store(800, Relaxed);
    lab.begin_pairing(&sim).await;
    lab.cancel_during_the_write(&sim).await;
    lab.quiesce().await;
    sleep(ms(500)).await;
    assert_eq!(lab.rig.trust_len(), 1);
    assert!(sim.knows_service(&lab));
}

/// A phone that still presents as paired to a service that has no record of it is never trusted on its
/// word: its request is a normal prompt, and nothing is stored until the user approves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_that_still_thinks_it_is_paired_gets_the_normal_prompt() {
    let _one = ONE.lock().await;
    bridge_core::statefile::set_write_delay_ms(0);
    let lab = Lab::start().await;
    let sim = Sim::new().await;
    lab.pair_fresh(&sim).await;
    let id = sim.id();
    // The service forgets the phone without telling it (the abandoned-pairing leftover, seen from the other side).
    sim.send(Cmd::Close);
    assert!(
        lab.until(Duration::from_secs(10), |s| s.links.is_empty())
            .await
    );
    lab.rig
        .call(Request::Unpair {
            device_id: id.clone(),
        })
        .await
        .ok();
    assert_eq!(lab.rig.trust_len(), 0);
    assert!(
        sim.knows_service(&lab),
        "the phone still trusts the service"
    );
    lab.approving.store(false, Relaxed);
    sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Await);
    // Trusted on the phone's side, unknown on ours.
    assert!(
        lab.until(Duration::from_secs(10), |s| link_of(s, &id)
            .is_some_and(|l| l.phase == LinkPhase::Pairing))
            .await
    );
    // It talks as if paired: the service answers with the standard "not paired" and the phone forgets.
    sim.send(Cmd::Send(ringing()));
    sim.forgot_after_accepting(&lab, Duration::from_secs(5))
        .await;
    assert_eq!(lab.rig.trust_len(), 0, "a stranger's packets made a record");
    // It then asks to pair afresh: an ordinary prompt.
    sim.dial_in(lab.inbound_proxy.addr(), &lab.service_id, PairMode::Request);
    assert!(
        lab.until(Duration::from_secs(10), |s| s
            .challenges
            .iter()
            .any(|c| c.device_id == id))
            .await,
        "no prompt"
    );
    sleep(ms(500)).await;
    assert_eq!(lab.rig.trust_len(), 0, "trusted before the user approved");
    let st = lab.rig.status().await;
    assert!(link_of(&st, &id).is_some_and(|l| l.phase != LinkPhase::Connected));
    // The user approves.
    lab.approving.store(true, Relaxed);
    assert!(
        lab.until(Duration::from_secs(15), |s| link_of(s, &id)
            .is_some_and(|l| l.phase == LinkPhase::Connected))
            .await
    );
    assert_eq!(lab.rig.trust_len(), 1);
}
