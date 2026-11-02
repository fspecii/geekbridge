//! Shared harness for the lifecycle and leak tests: a real service core (`prepare_control`) with
//! the control API on loopback, and stand-in phones that are real `bridge_kde` endpoints with
//! freshly generated certificates. No LAN, phone, Bluetooth or system bus is involved.
#![allow(dead_code)]

use bridge_core::{
    Snapshot,
    ipc::{
        self, ErrorCode, EventKind, IpcError, LinkPhase, LinkStatus, PairMode, Request, Response,
        Status, Token,
    },
};
use bridge_kde::{self as kde, TrustStore as _};
use bridge_service::{
    SharedState,
    calling::CallingProbe,
    control::{self, Control},
    daemon::{Daemon, DaemonOptions},
    prepare_control, serve_api, serve_inbound,
};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::RwLock,
    task::JoinHandle,
    time::{sleep, timeout},
};

pub struct NoProbe;
impl CallingProbe for NoProbe {
    fn probe(&self) -> ipc::CallingStatus {
        ipc::CallingStatus {
            platform: "test".into(),
            backend: "fake".into(),
            verdict: "no_phone".into(),
            telephony_service_present: true,
            bluetooth_controllers: Some(1),
            phones: 0,
            calls: 0,
            read_only: true,
        }
    }
}

pub struct Rig {
    pub _tmp: tempfile::TempDir,
    pub state_dir: PathBuf,
    pub daemon: Arc<Daemon>,
    pub token: Token,
    pub control_port: u16,
    pub tasks: Vec<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Rig {
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    pub async fn start_with(configure: impl FnOnce(&mut DaemonOptions)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(NoProbe));
        options.dev_loopback = true;
        configure(&mut options);
        let control = prepare_control(&state_dir, discovery.clone(), options);
        let Control::Ready { daemon, token } = &control else {
            panic!("private state should open in a fresh temp dir");
        };
        let (daemon, token) = (daemon.clone(), token.clone());
        let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_port = control_listener.local_addr().unwrap().port();
        let api_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tasks = vec![
            tokio::spawn(control::serve_control(control_listener, Arc::new(control))),
            tokio::spawn(serve_api(api_listener, discovery)),
        ];
        Self {
            _tmp: tmp,
            state_dir,
            daemon,
            token,
            control_port,
            tasks,
        }
    }

    /// Accept phones that dial the service, like `--listen-inbound`.
    pub async fn listen_inbound(&mut self) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        self.daemon.set_inbound_listener(addr);
        self.tasks
            .push(tokio::spawn(serve_inbound(listener, self.daemon.clone())));
        addr
    }

    pub async fn call(&self, request: Request) -> Result<Response, IpcError> {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
    }

    pub async fn ok(&self, request: Request) {
        match self.call(request).await {
            Ok(Response::Ack) => {}
            other => panic!("expected ack, got {other:?}"),
        }
    }

    pub async fn refused(&self, request: Request) -> ErrorCode {
        match self.call(request).await {
            Err(IpcError::Service(e)) => e.code,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    pub async fn status(&self) -> Status {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::status(port, &token))
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn events(&self) -> ipc::EventPage {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::events(port, &token, 0))
            .await
            .unwrap()
            .unwrap()
    }

    pub async fn wait(&self, what: &str, cond: impl Fn(&Status) -> bool) -> Status {
        // Generous: shared CI runners and busy dev machines stretch fault-injection timings.
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let status = self.status().await;
            if cond(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {status:#?}"
            );
            sleep(Duration::from_millis(5)).await;
        }
    }

    pub async fn has_event(&self, device: &str, matches: impl Fn(&EventKind) -> bool) -> bool {
        self.events()
            .await
            .events
            .iter()
            .any(|e| e.device_id.as_deref() == Some(device) && matches(&e.kind))
    }

    pub fn trust_len(&self) -> usize {
        kde::FileTrustStore::open(&self.state_dir)
            .unwrap()
            .list()
            .unwrap()
            .len()
    }

    pub fn state_files(&self) -> usize {
        std::fs::read_dir(&self.state_dir).unwrap().count()
    }
}

pub fn endpoint_of(listener: &TcpListener) -> ipc::Endpoint {
    ipc::Endpoint {
        address: "127.0.0.1".parse().unwrap(),
        port: listener.local_addr().unwrap().port(),
    }
}

pub fn link_of<'a>(status: &'a Status, id: &str) -> Option<&'a LinkStatus> {
    status.links.iter().find(|l| l.device_id == id)
}

pub struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

pub struct Phone {
    pub creds: Arc<kde::DeviceCredentials>,
    pub endpoint: kde::Endpoint,
}

impl Phone {
    pub fn new() -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        let ping = vec!["kdeconnect.ping".to_string()];
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            ping.clone(),
            ping,
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Arc::new(Self { creds, endpoint })
    }

    pub fn id(&self) -> String {
        self.creds.device_id().to_string()
    }

    /// Accept one dial from the service; pair if asked to. Returns the phone's side of the link.
    pub fn answer(self: &Arc<Self>, listener: TcpListener) -> JoinHandle<kde::TrustedLink> {
        let phone = self.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            match phone.endpoint.accept(stream).await.unwrap() {
                kde::Handshake::Trusted(link) => link,
                kde::Handshake::Pending(pending) => pending
                    .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                    .await
                    .unwrap(),
            }
        })
    }
}

pub async fn listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

/// Dial, request pairing, approve explicitly; return the phone's side of the trusted link.
pub async fn pair_phone(rig: &Rig, phone: &Arc<Phone>) -> kde::TrustedLink {
    let l = listener().await;
    let endpoint = endpoint_of(&l);
    let phone_link = phone.answer(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    rig.wait("unpaired link", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
    })
    .await;
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let status = rig.wait("challenge", |s| !s.challenges.is_empty()).await;
    rig.ok(Request::Approve {
        challenge_id: status.challenges[0].challenge_id.clone(),
    })
    .await;
    let link = timeout(Duration::from_secs(15), phone_link)
        .await
        .unwrap()
        .unwrap();
    wait_connected(rig, phone).await;
    link
}

pub async fn wait_connected(rig: &Rig, phone: &Phone) {
    rig.wait("connected link", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
    })
    .await;
}

/// Reconnect an already paired phone (no pairing step); returns the phone's side.
pub async fn reconnect(rig: &Rig, phone: &Arc<Phone>) -> kde::TrustedLink {
    let l = listener().await;
    let endpoint = endpoint_of(&l);
    let again = phone.answer(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    let link = timeout(Duration::from_secs(15), again)
        .await
        .unwrap()
        .unwrap();
    wait_connected(rig, phone).await;
    link
}

// ---------------------------------------------------------------- process probes (Linux)

pub fn fd_count() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, |d| d.count())
}

pub fn rss_kib() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(0, |pages| pages * 4)
}

/// Deterministic xorshift for reproducible junk.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

pub mod chaos;
