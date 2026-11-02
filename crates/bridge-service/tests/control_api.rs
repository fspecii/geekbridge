// SPDX-License-Identifier: GPL-3.0-or-later
//! Black-box tests of the authenticated control API and the link lifecycle behind it.
//!
//! The "phones" are real `bridge_kde` endpoints with freshly generated certificates talking TLS to
//! the service over loopback sockets. Nothing here needs a LAN, a phone, Bluetooth or the system
//! bus, and nothing asserts interoperability with real KDE Connect devices.

use bridge_core::{
    Snapshot, fetch_snapshot,
    ipc::{
        self, ChallengeDirection, ErrorCode, EventKind, Feature, IpcError, LinkPhase, PairMode,
        Request, Response, Status, Token,
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
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::RwLock,
    task::JoinHandle,
    time::{sleep, timeout},
};

const SECRET_BODY: &str = "SECRET-PHONE-CONTENT-7f3a";

// ------------------------------------------------------------------ harness

#[derive(Default)]
struct CountingProbe(AtomicUsize);
impl CallingProbe for CountingProbe {
    fn probe(&self) -> ipc::CallingStatus {
        self.0.fetch_add(1, Ordering::SeqCst);
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

struct Rig {
    _tmp: tempfile::TempDir,
    state_dir: PathBuf,
    daemon: Arc<Daemon>,
    token: Token,
    control_port: u16,
    api_port: u16,
    discovery: SharedState,
    probe: Arc<CountingProbe>,
    tasks: Vec<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Rig {
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    async fn start_with(configure: impl FnOnce(&mut DaemonOptions)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let probe = Arc::new(CountingProbe::default());
        let mut options = DaemonOptions::new("Test Desktop", probe.clone());
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
        let api_port = api_listener.local_addr().unwrap().port();
        let tasks = vec![
            tokio::spawn(control::serve_control(control_listener, Arc::new(control))),
            tokio::spawn(serve_api(api_listener, discovery.clone())),
        ];
        Self {
            _tmp: tmp,
            state_dir,
            daemon,
            token,
            control_port,
            api_port,
            discovery,
            probe,
            tasks,
        }
    }

    async fn call(&self, request: Request) -> Result<Response, IpcError> {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
    }

    async fn ok(&self, request: Request) {
        match self.call(request).await {
            Ok(Response::Ack) => {}
            other => panic!("expected ack, got {other:?}"),
        }
    }

    async fn refused(&self, request: Request) -> ErrorCode {
        match self.call(request).await {
            Err(IpcError::Service(e)) => e.code,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    async fn status(&self) -> Status {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::status(port, &token))
            .await
            .unwrap()
            .unwrap()
    }

    async fn events(&self, after: u64) -> ipc::EventPage {
        let (port, token) = (self.control_port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::events(port, &token, after))
            .await
            .unwrap()
            .unwrap()
    }

    async fn wait(&self, what: &str, cond: impl Fn(&Status) -> bool) -> Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.status().await;
            if cond(&status) {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {status:#?}"
            );
            sleep(Duration::from_millis(30)).await;
        }
    }

    async fn has_event(&self, device: &str, matches: impl Fn(&EventKind) -> bool) -> bool {
        self.events(0)
            .await
            .events
            .iter()
            .any(|e| e.device_id.as_deref() == Some(device) && matches(&e.kind))
    }

    fn reopen_trust(&self) -> kde::FileTrustStore {
        kde::FileTrustStore::open(&self.state_dir).unwrap()
    }

    fn endpoint_of(listener: &TcpListener) -> ipc::Endpoint {
        ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port: listener.local_addr().unwrap().port(),
        }
    }
}

fn link_of<'a>(status: &'a Status, id: &str) -> Option<&'a ipc::LinkStatus> {
    status.links.iter().find(|l| l.device_id == id)
}

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

/// A stand-in phone: a real `bridge_kde` endpoint with its own generated identity.
struct Phone {
    creds: Arc<kde::DeviceCredentials>,
    endpoint: kde::Endpoint,
}

impl Phone {
    fn new(incoming: &[&str], outgoing: &[&str]) -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        Self::with_creds(creds, incoming, outgoing)
    }

    fn with_creds(
        creds: Arc<kde::DeviceCredentials>,
        incoming: &[&str],
        outgoing: &[&str],
    ) -> Arc<Self> {
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            incoming.iter().map(|s| s.to_string()).collect(),
            outgoing.iter().map(|s| s.to_string()).collect(),
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Arc::new(Self { creds, endpoint })
    }

    fn ping_phone() -> Arc<Self> {
        Self::new(&["kdeconnect.ping"], &["kdeconnect.ping"])
    }

    fn id(&self) -> String {
        self.creds.device_id().to_string()
    }

    fn fingerprint(&self) -> String {
        self.creds.cert().fingerprint_hex()
    }

    /// Accept one dial from the service and complete (or keep) the trusted link. The phone never
    /// asks the user anything: it approves its side automatically, like a tester tapping "accept".
    fn answer(self: &Arc<Self>, listener: TcpListener) -> JoinHandle<kde::TrustedLink> {
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

    /// Accept one dial and hold the (unpaired) link open without pairing.
    fn answer_and_hold(self: &Arc<Self>, listener: TcpListener) -> JoinHandle<kde::PendingLink> {
        let phone = self.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            match phone.endpoint.accept(stream).await.unwrap() {
                kde::Handshake::Pending(p) => p,
                kde::Handshake::Trusted(_) => panic!("phone should not trust the service yet"),
            }
        })
    }
}

async fn listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").await.unwrap()
}

/// Dial, request pairing, approve explicitly; return the phone's side of the trusted link.
async fn pair_phone(rig: &Rig, phone: &Arc<Phone>) -> kde::TrustedLink {
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
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
    let link = timeout(Duration::from_secs(10), phone_link)
        .await
        .unwrap()
        .unwrap();
    rig.wait("connected link", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
    })
    .await;
    link
}

// ------------------------------------------------------------------ pairing

#[tokio::test]
async fn pairing_waits_for_explicit_approval_and_pins_the_presented_certificate() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let phone_link = phone.answer(l);

    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    let status = rig
        .wait("unpaired link", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
        })
        .await;
    // Connecting alone neither pairs nor asks the phone to pair.
    let link = link_of(&status, &phone.id()).unwrap();
    assert_eq!(
        link.peer_cert_sha256.as_deref(),
        Some(phone.fingerprint().as_str())
    );
    assert!(
        link.negotiated.is_empty(),
        "nothing is negotiated before pairing"
    );
    assert!(status.paired.is_empty() && status.challenges.is_empty());

    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let status = rig.wait("challenge", |s| !s.challenges.is_empty()).await;
    let challenge = status.challenges[0].clone();
    assert_eq!(challenge.device_id, phone.id());
    assert_eq!(challenge.direction, ChallengeDirection::Outgoing);
    assert_eq!(challenge.peer_cert_sha256, phone.fingerprint());
    assert_eq!(challenge.own_cert_sha256, status.own_cert_sha256);
    assert_eq!(challenge.verification_key.len(), 8);
    assert!(challenge.expires_in_ms > 0);

    // The phone has already agreed; without the local decision nothing is paired.
    sleep(Duration::from_millis(500)).await;
    assert!(rig.reopen_trust().list().unwrap().is_empty());
    let status = rig.status().await;
    assert!(status.paired.is_empty());
    assert_eq!(
        link_of(&status, &phone.id()).unwrap().phase,
        LinkPhase::Pairing
    );

    // A decision for an unknown challenge changes nothing.
    assert_eq!(
        rig.refused(Request::Approve {
            challenge_id: "00".repeat(16)
        })
        .await,
        ErrorCode::NotFound
    );
    assert!(rig.reopen_trust().list().unwrap().is_empty());

    rig.ok(Request::Approve {
        challenge_id: challenge.challenge_id.clone(),
    })
    .await;
    let phone_link = timeout(Duration::from_secs(10), phone_link)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        phone_link.info().peer.device_id.to_string(),
        status.device_id
    );
    let status = rig
        .wait("paired and connected", |s| {
            s.paired.len() == 1
                && link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    assert!(status.challenges.is_empty());
    assert_eq!(status.paired[0].cert_sha256, phone.fingerprint());
    assert_eq!(
        link_of(&status, &phone.id()).unwrap().negotiated,
        vec!["kdeconnect.ping"]
    );

    // Persisted: a fresh store over the same directory pins exactly that certificate.
    let stored = rig.reopen_trust().list().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].cert.fingerprint_hex(), phone.fingerprint());

    // The challenge cannot be replayed.
    let again = rig
        .refused(Request::Approve {
            challenge_id: challenge.challenge_id,
        })
        .await;
    assert!(matches!(
        again,
        ErrorCode::NotFound | ErrorCode::ChallengeStale
    ));
    for kind in ["needs_pairing", "pair_challenge", "paired", "connected"] {
        assert!(
            rig.events(0)
                .await
                .events
                .iter()
                .any(|e| serde_json::to_value(e).unwrap()["kind"] == kind),
            "missing {kind} event"
        );
    }
}

#[tokio::test]
async fn denying_leaves_no_trust_and_ends_the_link() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let held = phone.answer_and_hold(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    rig.wait("unpaired", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
    })
    .await;
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let status = rig.wait("challenge", |s| !s.challenges.is_empty()).await;
    let id = status.challenges[0].challenge_id.clone();
    rig.ok(Request::Deny {
        challenge_id: id.clone(),
    })
    .await;
    rig.wait("link gone", |s| {
        s.links.is_empty() && s.challenges.is_empty()
    })
    .await;
    assert!(rig.reopen_trust().list().unwrap().is_empty());
    assert!(rig.status().await.paired.is_empty());
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::PairFailed { .. }))
            .await
    );
    assert_eq!(
        rig.refused(Request::Approve { challenge_id: id }).await,
        ErrorCode::NotFound
    );
    drop(held);
}

#[tokio::test]
async fn an_unanswered_challenge_expires_and_cannot_be_approved_afterwards() {
    let rig = Rig::start_with(|o| {
        o.transport.pairing.request_timeout = Duration::from_millis(800);
    })
    .await;
    let phone = Phone::ping_phone();
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let held = phone.answer_and_hold(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    rig.wait("unpaired", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
    })
    .await;
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let status = rig.wait("challenge", |s| !s.challenges.is_empty()).await;
    let id = status.challenges[0].challenge_id.clone();
    rig.wait("expiry", |s| s.challenges.is_empty() && s.links.is_empty())
        .await;
    assert!(matches!(
        rig.refused(Request::Approve { challenge_id: id }).await,
        ErrorCode::NotFound | ErrorCode::ChallengeExpired
    ));
    assert!(rig.reopen_trust().list().unwrap().is_empty());
    drop(held);
}

#[tokio::test]
async fn an_inbound_pairing_request_also_needs_explicit_approval() {
    let rig = Rig::start().await;
    let inbound = listener().await;
    let inbound_addr = inbound.local_addr().unwrap();
    rig.daemon.set_inbound_listener(inbound_addr);
    let accept = tokio::spawn(serve_inbound(inbound, rig.daemon.clone()));
    let status = rig.status().await;
    assert_eq!(status.inbound_listener, Some(inbound_addr));

    let phone = Phone::ping_phone();
    let service_id = kde::DeviceId::parse(&status.device_id).unwrap();
    let dial = {
        let phone = phone.clone();
        tokio::spawn(async move {
            match phone
                .endpoint
                .connect(inbound_addr, &service_id)
                .await
                .unwrap()
            {
                kde::Handshake::Pending(p) => {
                    p.pair(kde::PairMode::Request, &mut Yes).await.unwrap()
                }
                kde::Handshake::Trusted(l) => l,
            }
        })
    };
    let status = rig
        .wait("incoming challenge", |s| !s.challenges.is_empty())
        .await;
    assert_eq!(status.challenges[0].direction, ChallengeDirection::Incoming);
    assert_eq!(status.challenges[0].peer_cert_sha256, phone.fingerprint());
    sleep(Duration::from_millis(400)).await;
    assert!(
        rig.reopen_trust().list().unwrap().is_empty(),
        "nothing pairs by itself"
    );
    rig.ok(Request::Approve {
        challenge_id: status.challenges[0].challenge_id.clone(),
    })
    .await;
    let mut phone_link = timeout(Duration::from_secs(10), dial)
        .await
        .unwrap()
        .unwrap();
    let status = rig
        .wait("connected", |s| {
            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
        })
        .await;
    assert_eq!(
        link_of(&status, &phone.id()).unwrap().role,
        Some(ipc::LinkRole::Acceptor)
    );

    // The same authenticated phone dialling again replaces the old link: still one link.
    let service_id = kde::DeviceId::parse(&status.device_id).unwrap();
    let second = phone.endpoint.connect(inbound_addr, &service_id).await;
    // The phone's in-memory store does not know the service's record from this side, so it may
    // see either a trusted or a pending link; only the service's view matters here.
    let _second = second.unwrap();
    rig.wait("replacement", |s| {
        s.links.len() == 1 && s.links[0].phase == LinkPhase::Connected
    })
    .await;
    assert!(
        rig.has_event(
            &phone.id(),
            |k| matches!(k, EventKind::LinkLost { reason } if reason.contains("replaced"))
        )
        .await
    );
    // The displaced connection is closed.
    let end = timeout(Duration::from_secs(5), phone_link.recv())
        .await
        .unwrap();
    assert!(end.is_err());
    accept.abort();
}

// ------------------------------------------------------------------ authentication

async fn raw_exchange(port: u16, frame: impl FnOnce(&str) -> Vec<u8>) -> Vec<u8> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let mut hello = Vec::new();
    while !hello.ends_with(b"\n") {
        let mut b = [0_u8; 1];
        stream.read_exact(&mut b).await.unwrap();
        hello.push(b[0]);
    }
    let hello = String::from_utf8(hello).unwrap();
    let nonce = hello.trim_end().rsplit(' ').next().unwrap().to_owned();
    stream.write_all(&frame(&nonce)).await.unwrap();
    let mut reply = Vec::new();
    let _ = timeout(Duration::from_secs(4), stream.read_to_end(&mut reply)).await;
    reply
}

#[tokio::test]
async fn missing_or_wrong_token_is_rejected_before_any_side_effect() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let json = serde_json::to_vec(&Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .unwrap();
    let wrong = Token::from_bytes([9; 32]);

    // Wrong token through the real client.
    let port = rig.control_port;
    let request = serde_json::from_slice::<Request>(&json).unwrap();
    let err = tokio::task::spawn_blocking(move || ipc::call(port, &wrong, &request))
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(err, IpcError::Unauthorized), "{err}");

    // No mac at all, garbage mac, truncated mac, and a valid mac computed for another nonce.
    let other_nonce_mac = rig.token.request_mac(&"ab".repeat(32), &json);
    type Frame = Box<dyn FnOnce(&str) -> Vec<u8>>;
    let frames: Vec<Frame> = vec![
        Box::new({
            let json = json.clone();
            move |_| [json.as_slice(), b"\n"].concat()
        }),
        Box::new({
            let json = json.clone();
            move |_| [b"00".repeat(32).as_slice(), b" ", json.as_slice(), b"\n"].concat()
        }),
        Box::new({
            let json = json.clone();
            move |_| [b"0".repeat(10).as_slice(), b" ", json.as_slice(), b"\n"].concat()
        }),
        Box::new({
            let json = json.clone();
            move |_| {
                [
                    ipc::encode_hex(&other_nonce_mac).as_bytes(),
                    b" ",
                    json.as_slice(),
                    b"\n",
                ]
                .concat()
            }
        }),
    ];
    for frame in frames {
        let reply = raw_exchange(rig.control_port, frame).await;
        assert_eq!(reply, b"ERR unauthorized\n");
    }

    // The mutating requests that were refused did nothing: nobody was dialled, no events.
    // Unrelated local processes may poke an ephemeral port; what matters is that the service
    // did not send its KDE Connect identity there.
    let deadline = Instant::now() + Duration::from_millis(400);
    while let Ok(Ok((mut stream, _))) = timeout(
        deadline.saturating_duration_since(Instant::now()),
        l.accept(),
    )
    .await
    {
        let mut first = vec![0_u8; 256];
        let n = timeout(Duration::from_millis(150), stream.read(&mut first))
            .await
            .unwrap_or(Ok(0))
            .unwrap_or(0);
        assert!(
            !String::from_utf8_lossy(&first[..n]).contains("kdeconnect.identity"),
            "service dialled the phone"
        );
    }
    let status = rig.status().await;
    assert!(status.links.is_empty() && status.paired.is_empty() && status.challenges.is_empty());
    assert_eq!(status.event_seq, 0);
    assert!(rig.events(0).await.events.is_empty());
}

#[tokio::test]
async fn a_valid_mac_over_garbage_is_a_bad_request_not_a_command() {
    let rig = Rig::start().await;
    for body in [
        &b"{\"op\":\"format_disk\"}"[..],
        b"not json",
        b"{\"op\":\"ping\",\"device_id\":\"x\",\"extra\":1}",
    ] {
        let token = rig.token.clone();
        let body = body.to_vec();
        let reply = raw_exchange(rig.control_port, move |nonce| {
            let mac = token.request_mac(nonce, &body);
            [ipc::encode_hex(&mac).as_bytes(), b" ", &body, b"\n"].concat()
        })
        .await;
        let line = std::str::from_utf8(&reply).unwrap().trim_end();
        let (_, payload) = line.split_once(' ').unwrap();
        let response: Response = serde_json::from_str(payload).unwrap();
        assert!(matches!(response, Response::Error(e) if e.code == ErrorCode::BadRequest));
    }
    assert_eq!(rig.status().await.event_seq, 0);
}

#[tokio::test]
async fn unavailable_private_state_is_reported_and_nothing_is_served() {
    let tmp = tempfile::tempdir().unwrap();
    let control = Arc::new(Control::Unavailable(bridge_service::private::Unavailable {
        code: "state_insecure",
        message: "the state directory or one of its files is not private to this user",
    }));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(control::serve_control(listener, control));
    let token = Token::from_bytes([3; 32]);
    let err = tokio::task::spawn_blocking(move || ipc::call(port, &token, &Request::Status))
        .await
        .unwrap()
        .unwrap_err();
    assert!(
        matches!(&err, IpcError::Unavailable { code, .. } if code == "state_insecure"),
        "{err}"
    );
    assert!(!err.to_string().contains(tmp.path().to_str().unwrap()));
    let hello = tokio::task::spawn_blocking(move || ipc::probe_protected(port))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(hello, ipc::Hello::Unavailable { .. }));
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn an_insecure_state_directory_makes_prepare_control_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
    let options = DaemonOptions::new("x", Arc::new(CountingProbe::default()));
    let control = prepare_control(&dir, discovery, options);
    assert!(matches!(control, Control::Unavailable(ref u) if u.code == "state_insecure"));
    // Nothing was created inside the unsafe directory.
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn connection_cap_deadline_and_size_limits_hold() {
    let rig = Rig::start().await;
    let mut idle = Vec::new();
    for _ in 0..control::MAX_CONNECTIONS {
        let mut s = TcpStream::connect(("127.0.0.1", rig.control_port))
            .await
            .unwrap();
        let mut hello = [0_u8; 8];
        s.read_exact(&mut hello).await.unwrap();
        idle.push(s);
    }
    // The next connection is refused outright instead of queueing behind the idle ones.
    let mut extra = TcpStream::connect(("127.0.0.1", rig.control_port))
        .await
        .unwrap();
    let mut buf = Vec::new();
    let read = timeout(Duration::from_secs(1), extra.read_to_end(&mut buf)).await;
    assert!(
        read.is_ok() && buf.is_empty(),
        "extra connection should be closed without service"
    );

    // Idle clients are cut off by the exchange deadline, after which service resumes.
    let start = Instant::now();
    let mut rest = Vec::new();
    let _ = timeout(Duration::from_secs(5), idle[0].read_to_end(&mut rest))
        .await
        .unwrap();
    assert!(start.elapsed() < control::EXCHANGE_DEADLINE + Duration::from_secs(1));
    drop(idle);
    rig.wait("service available again", |_| true).await;

    // An oversized request line is dropped without an answer.
    let reply = raw_exchange(rig.control_port, |_| vec![b'a'; ipc::MAX_REQUEST_BYTES * 2]).await;
    assert!(reply.is_empty());
    assert_eq!(rig.status().await.event_seq, 0);
}

// ------------------------------------------------------------------ lifecycle

#[tokio::test]
async fn only_one_live_link_per_device() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let held = phone.answer_and_hold(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint.clone()),
    })
    .await;
    // Second connect while the first is still alive, whatever its phase.
    assert_eq!(
        rig.refused(Request::Connect {
            device_id: phone.id(),
            endpoint: Some(endpoint),
        })
        .await,
        ErrorCode::Busy
    );
    rig.wait("unpaired", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
    })
    .await;
    assert_eq!(
        rig.refused(Request::Connect {
            device_id: phone.id(),
            endpoint: Some(ipc::Endpoint {
                address: "127.0.0.1".parse().unwrap(),
                port: 1,
            }),
        })
        .await,
        ErrorCode::Busy
    );
    assert_eq!(rig.status().await.links.len(), 1);
    // Pairing can only start on an unpaired link, and only once.
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    assert_eq!(
        rig.refused(Request::Pair {
            device_id: phone.id(),
            mode: PairMode::Request
        })
        .await,
        ErrorCode::NotUnpaired
    );
    // Disconnect frees the slot.
    rig.ok(Request::Disconnect {
        device_id: phone.id(),
    })
    .await;
    assert!(rig.status().await.links.is_empty() && rig.status().await.challenges.is_empty());
    assert_eq!(
        rig.refused(Request::Disconnect {
            device_id: phone.id()
        })
        .await,
        ErrorCode::NotFound
    );
    drop(held);
}

#[tokio::test]
async fn failed_dials_are_reported_and_free_the_slot() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let dead = listener().await;
    let endpoint = Rig::endpoint_of(&dead);
    drop(dead);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    rig.wait("failure", |s| s.links.is_empty()).await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(
            k,
            EventKind::ConnectFailed { .. }
        ))
        .await
    );
    // Unknown device without an endpoint and without discovery data.
    assert_eq!(
        rig.refused(Request::Connect {
            device_id: phone.id(),
            endpoint: None
        })
        .await,
        ErrorCode::NotFound
    );
    assert_eq!(
        rig.refused(Request::Connect {
            device_id: "../etc".into(),
            endpoint: None
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rig.refused(Request::Connect {
            device_id: rig.status().await.device_id,
            endpoint: Some(ipc::Endpoint {
                address: "127.0.0.1".parse().unwrap(),
                port: 1716
            }),
        })
        .await,
        ErrorCode::BadRequest
    );
}

#[tokio::test]
async fn endpoints_outside_the_local_network_policy_are_refused() {
    let rig = Rig::start_with(|o| o.dev_loopback = false).await;
    let phone = Phone::ping_phone();
    for (address, port) in [
        ("127.0.0.1", 1716),
        ("8.8.8.8", 1716),
        ("0.0.0.0", 1716),
        ("169.254.169.254", 80),
        ("192.168.1.5", 22),
        ("192.168.1.5", 0),
        ("224.0.0.1", 1716),
    ] {
        assert_eq!(
            rig.refused(Request::Connect {
                device_id: phone.id(),
                endpoint: Some(ipc::Endpoint {
                    address: address.parse().unwrap(),
                    port
                }),
            })
            .await,
            ErrorCode::InvalidEndpoint,
            "{address}:{port}"
        );
    }
    assert!(rig.status().await.links.is_empty());
}

#[tokio::test]
async fn link_loss_is_reported_and_keeps_the_pairing() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let phone_link = pair_phone(&rig, &phone).await;
    drop(phone_link);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::LinkLost { .. }))
            .await
    );
    let status = rig.status().await;
    assert_eq!(status.paired.len(), 1);
    assert_eq!(rig.reopen_trust().list().unwrap().len(), 1);

    // Reconnecting a paired device authenticates against the pinned certificate: no pairing step.
    let l = listener().await;
    let endpoint = Rig::endpoint_of(&l);
    let again = phone.answer(l);
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    let _link = timeout(Duration::from_secs(10), again)
        .await
        .unwrap()
        .unwrap();
    rig.wait("trusted again", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
    })
    .await;
}

#[tokio::test]
async fn the_phone_unpairing_removes_trust_and_the_link() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Sms,
        enabled: true,
    })
    .await;
    phone_link.unpair().await.unwrap();
    rig.wait("peer unpair", |s| s.links.is_empty() && s.paired.is_empty())
        .await;
    assert!(
        rig.has_event(&phone.id(), |k| matches!(k, EventKind::PeerUnpaired))
            .await
    );
    assert!(rig.reopen_trust().list().unwrap().is_empty());
    // Consent does not outlive the pairing.
    let consents = bridge_service::private::ConsentStore::open(&rig.state_dir).unwrap();
    assert!(!consents.is_enabled(&phone.id(), Feature::Sms));
}

#[tokio::test]
async fn local_unpair_tells_the_phone_and_forgets_everything() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Clipboard,
        enabled: true,
    })
    .await;
    rig.ok(Request::Unpair {
        device_id: phone.id(),
    })
    .await;
    let event = timeout(Duration::from_secs(5), phone_link.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(event, kde::LinkEvent::PeerUnpaired));
    let status = rig.status().await;
    assert!(status.paired.is_empty() && status.links.is_empty());
    assert!(rig.reopen_trust().list().unwrap().is_empty());
    assert!(
        !bridge_service::private::ConsentStore::open(&rig.state_dir)
            .unwrap()
            .is_enabled(&phone.id(), Feature::Clipboard)
    );
    // Unpairing an unknown device is an error, not a silent success.
    assert_eq!(
        rig.refused(Request::Unpair {
            device_id: phone.id()
        })
        .await,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn unpairing_without_a_live_link_forgets_locally() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    drop(pair_phone(&rig, &phone).await);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    rig.ok(Request::Unpair {
        device_id: phone.id(),
    })
    .await;
    assert!(rig.status().await.paired.is_empty());
    assert!(rig.reopen_trust().list().unwrap().is_empty());
}

#[tokio::test]
async fn discovery_cannot_redirect_a_paired_device_or_replace_its_pin() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    drop(pair_phone(&rig, &phone).await);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    let pinned = rig.reopen_trust().list().unwrap()[0].cert.fingerprint_hex();
    assert_eq!(pinned, phone.fingerprint());

    // An impostor with the same device id (a fresh key) and a forged discovery record.
    let evil_creds = Arc::new(
        kde::DeviceCredentials::generate_with_id(&kde::DeviceId::parse(&phone.id()).unwrap())
            .unwrap(),
    );
    let evil = Phone::with_creds(evil_creds, &["kdeconnect.ping"], &["kdeconnect.ping"]);
    let l = listener().await;
    let port = l.local_addr().unwrap().port();
    let evil_dial = tokio::spawn(async move {
        let (stream, _) = l.accept().await.unwrap();
        evil.endpoint.accept(stream).await.map(|_| ())
    });
    rig.discovery
        .write()
        .await
        .devices
        .push(bridge_core::Device {
            id: phone.id(),
            name: "Totally the phone".into(),
            device_type: "phone".into(),
            address: "127.0.0.1".parse().unwrap(),
            tcp_port: port,
            protocol_version: 8,
            incoming_capabilities: vec![],
            outgoing_capabilities: vec![],
            state: bridge_core::ConnectionState::Discovered,
            last_seen_unix_ms: bridge_service::now_ms(),
        });
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: None,
    })
    .await;
    rig.wait("rejected", |s| s.links.is_empty()).await;
    let _ = evil_dial.await;
    assert!(
        rig.has_event(
            &phone.id(),
            |k| matches!(k, EventKind::ConnectFailed { reason } if reason.contains("pinned"))
        )
        .await,
        "{:?}",
        rig.events(0).await
    );
    // The pin is untouched and the impostor never became a trusted link.
    assert_eq!(
        rig.reopen_trust().list().unwrap()[0].cert.fingerprint_hex(),
        pinned
    );
    let status = rig.status().await;
    assert_eq!(status.paired[0].cert_sha256, pinned);
    assert!(status.links.is_empty());
}

// ------------------------------------------------------------------ packets and consent

#[tokio::test]
async fn only_negotiated_packets_are_accepted_and_no_content_is_retained() {
    let rig = Rig::start().await;
    // The phone offers far more than the service ever declared.
    let phone = Phone::new(
        &["kdeconnect.ping", "kdeconnect.notification.request"],
        &["kdeconnect.ping", "kdeconnect.notification"],
    );
    let mut phone_link = pair_phone(&rig, &phone).await;
    let status = rig.status().await;
    assert_eq!(
        link_of(&status, &phone.id()).unwrap().negotiated,
        vec!["kdeconnect.ping"],
        "capabilities not declared by the service must not be negotiated"
    );
    let body = |text: &str| {
        let mut m = serde_json::Map::new();
        m.insert("text".into(), text.into());
        m
    };
    for kind in [
        "kdeconnect.sms.messages",
        "kdeconnect.clipboard",
        "kdeconnect.contacts.response_vcards",
    ] {
        phone_link
            .send(&kde::Packet::new(kind, body(SECRET_BODY)))
            .await
            .unwrap();
    }
    phone_link
        .send(&kde::Packet::new("kdeconnect.ping", body(SECRET_BODY)))
        .await
        .unwrap();
    let status = rig
        .wait("packets counted", |s| {
            link_of(s, &phone.id())
                .is_some_and(|l| l.pings_received == 1 && l.rejected_packets == 3)
        })
        .await;
    assert_eq!(
        link_of(&status, &phone.id()).unwrap().phase,
        LinkPhase::Connected
    );
    let everything = format!(
        "{}{}",
        serde_json::to_string(&status).unwrap(),
        serde_json::to_string(&rig.events(0).await).unwrap()
    );
    assert!(
        !everything.contains(SECRET_BODY),
        "phone content leaked into status or events"
    );
    assert!(everything.contains("packet_rejected") && everything.contains("ping_received"));
    // Nothing was persisted either.
    for entry in std::fs::read_dir(&rig.state_dir).unwrap() {
        let path = entry.unwrap().path();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains(SECRET_BODY)
        );
    }
}

#[tokio::test]
async fn ping_is_not_accepted_unless_the_phone_advertised_it() {
    let rig = Rig::start().await;
    let phone = Phone::new(&[], &[]);
    let mut phone_link = pair_phone(&rig, &phone).await;
    phone_link
        .send(&kde::Packet::new("kdeconnect.ping", serde_json::Map::new()))
        .await
        .unwrap();
    rig.wait("rejected", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.rejected_packets == 1)
    })
    .await;
    assert_eq!(
        link_of(&rig.status().await, &phone.id())
            .unwrap()
            .pings_received,
        0
    );
    assert_eq!(
        rig.refused(Request::Ping {
            device_id: phone.id()
        })
        .await,
        ErrorCode::Unsupported
    );
}

#[tokio::test]
async fn a_phone_that_floods_unnegotiated_packets_is_dropped() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let mut phone_link = pair_phone(&rig, &phone).await;
    for _ in 0..100 {
        if phone_link
            .send(&kde::Packet::new(
                "kdeconnect.sms.messages",
                serde_json::Map::new(),
            ))
            .await
            .is_err()
        {
            break;
        }
    }
    rig.wait("dropped", |s| s.links.is_empty()).await;
    assert!(
        rig.has_event(
            &phone.id(),
            |k| matches!(k, EventKind::LinkLost { reason } if reason.contains("negotiated"))
        )
        .await
    );
    assert_eq!(
        rig.status().await.paired.len(),
        1,
        "misbehaviour does not unpair"
    );
}

#[tokio::test]
async fn consent_defaults_off_needs_a_pairing_and_ping_needs_no_consent() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    assert_eq!(
        rig.refused(Request::SetConsent {
            device_id: phone.id(),
            feature: Feature::Sms,
            enabled: true
        })
        .await,
        ErrorCode::NotFound,
        "consent for an unpaired device"
    );
    assert_eq!(
        rig.refused(Request::Ping {
            device_id: phone.id()
        })
        .await,
        ErrorCode::NotConnected
    );
    let mut phone_link = pair_phone(&rig, &phone).await;
    let status = rig.status().await;
    assert_eq!(status.paired[0].consents.len(), Feature::ALL.len());
    assert!(status.paired[0].consents.iter().all(|c| !c.enabled
        && c.adapter_integrated
            == matches!(
                c.feature,
                Feature::Clipboard
                    | Feature::Telephony
                    | Feature::Sms
                    | Feature::Contacts
                    | Feature::Calendar
                    | Feature::CalendarEdit
                    | Feature::CalendarAlerts
                    | Feature::Debug
                    | Feature::Battery
            )));

    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Contacts,
        enabled: true,
    })
    .await;
    let status = rig.status().await;
    for c in &status.paired[0].consents {
        assert_eq!(c.enabled, c.feature == Feature::Contacts, "{:?}", c.feature);
    }
    let reopened = bridge_service::private::ConsentStore::open(&rig.state_dir).unwrap();
    assert!(reopened.is_enabled(&phone.id(), Feature::Contacts));
    assert!(!reopened.is_enabled(&phone.id(), Feature::Sms));

    // Enabling consent still does not make the service send anything: no adapter exists. The
    // only thing the phone ever receives is the explicit ping.
    rig.ok(Request::Ping {
        device_id: phone.id(),
    })
    .await;
    match timeout(Duration::from_secs(5), phone_link.recv())
        .await
        .unwrap()
        .unwrap()
    {
        kde::LinkEvent::Packet(p) => {
            assert_eq!(p.kind, "kdeconnect.ping");
            assert!(p.body.is_empty(), "ping carries no user text");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        timeout(Duration::from_millis(400), phone_link.recv())
            .await
            .is_err()
    );
}

// ------------------------------------------------------------------ public API and misc

#[tokio::test]
async fn public_api_stays_read_only_discovery_metadata() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    let _link = pair_phone(&rig, &phone).await;
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Sms,
        enabled: true,
    })
    .await;

    let port = rig.api_port;
    let snapshot = tokio::task::spawn_blocking(move || fetch_snapshot(port))
        .await
        .unwrap()
        .unwrap();
    assert!(
        snapshot.devices.is_empty(),
        "pairing must not mint discovery records"
    );

    // The wire document has exactly the legacy fields.
    let mut stream = TcpStream::connect(("127.0.0.1", rig.api_port))
        .await
        .unwrap();
    stream
        .write_all(b"GET /v1/discovery HTTP/1.0\r\n\r\n")
        .await
        .unwrap();
    let mut body = Vec::new();
    stream.read_to_end(&mut body).await.unwrap();
    let text = String::from_utf8(body).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "api_version",
            "capabilities_truncated",
            "devices",
            "discovery_error",
            "discovery_port"
        ]
    );
    assert!(
        !text.contains(&phone.id()) && !text.contains("consent") && !text.contains("challenge")
    );

    // Control requests sent to the public port get nothing; HTTP sent to the control port gets nothing.
    for request in [
        &b"GET /v1/status HTTP/1.0\r\n\r\n"[..],
        b"GET /v1/control HTTP/1.0\r\n\r\n",
        b"{\"op\":\"status\"}\n",
    ] {
        let mut s = TcpStream::connect(("127.0.0.1", rig.api_port))
            .await
            .unwrap();
        s.write_all(request).await.unwrap();
        let mut reply = Vec::new();
        let _ = timeout(Duration::from_secs(3), s.read_to_end(&mut reply)).await;
        let reply = String::from_utf8_lossy(&reply);
        assert!(!reply.contains(&phone.id()), "{reply}");
        assert!(!reply.contains("\"paired\""), "{reply}");
    }
    let reply = raw_exchange(rig.control_port, |_| {
        b"GET /v1/discovery HTTP/1.0\r\n\r\n".to_vec()
    })
    .await;
    assert_eq!(reply, b"ERR unauthorized\n");
}

#[tokio::test]
async fn calling_probe_only_runs_on_request_and_is_read_only_data() {
    let rig = Rig::start().await;
    rig.status().await;
    assert_eq!(
        rig.probe.0.load(Ordering::SeqCst),
        0,
        "no probing without a request"
    );
    match rig.call(Request::CallingStatus).await.unwrap() {
        Response::CallingStatus(c) => {
            assert!(c.read_only);
            assert_eq!(c.verdict, "no_phone");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(rig.probe.0.load(Ordering::SeqCst), 1);
    // No call-control request exists in the protocol at all.
    let reply = raw_exchange(rig.control_port, {
        let token = rig.token.clone();
        move |nonce| {
            let body = br#"{"op":"dial","number":"123"}"#;
            let mac = token.request_mac(nonce, body);
            [ipc::encode_hex(&mac).as_bytes(), b" ", body, b"\n"].concat()
        }
    })
    .await;
    assert!(String::from_utf8_lossy(&reply).contains("bad_request"));
    assert_eq!(rig.probe.0.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn event_cursor_pages_in_order_and_reports_gaps() {
    let rig = Rig::start().await;
    let phone = Phone::ping_phone();
    drop(pair_phone(&rig, &phone).await);
    rig.wait("link lost", |s| s.links.is_empty()).await;
    let all = rig.events(0).await;
    assert!(!all.gap && all.events.len() >= 5);
    assert!(all.events.windows(2).all(|w| w[0].seq + 1 == w[1].seq));
    let half = rig.events(all.events[1].seq).await;
    assert_eq!(half.events.first().unwrap().seq, all.events[2].seq);
    assert_eq!(half.next_after, all.next_after);
    let none = rig.events(all.next_after).await;
    assert!(none.events.is_empty() && none.next_after == all.next_after && !none.gap);
    // A cursor from a previous service run is flagged rather than trusted.
    let stale = rig.events(all.next_after + 1000).await;
    assert!(stale.gap && stale.events.is_empty() && stale.next_after == all.next_after);
}
