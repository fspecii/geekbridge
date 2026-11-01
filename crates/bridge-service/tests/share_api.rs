//! Black-box tests of file / link / text sharing through the control API.
//!
//! The "phone" is a real `bridge_kde` endpoint with a generated certificate; payload sockets are
//! real pinned-TLS connections over loopback. Nothing here talks to a real KDE Connect device.

use bridge_core::{
    Snapshot,
    ipc::{self, ErrorCode, EventKind, IpcError, LinkPhase, PairMode, Request, Response, Token},
    share::{
        ShareStatus, ShareTextKind, TransferDirection, TransferInfo, TransferKind, TransferState,
    },
};
use bridge_kde::{self as kde, share as kshare};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    control::{self, Control},
    daemon::DaemonOptions,
    prepare_control,
};
use std::{
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

const SHARE: &str = "kdeconnect.share.request";

struct Rig {
    _tmp: tempfile::TempDir,
    save_dir: PathBuf,
    token: Token,
    port: u16,
    tasks: Vec<JoinHandle<std::io::Result<()>>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let save_dir = tmp.path().join("downloads").join("GeekBridge");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(DisabledCallingProbe));
        options.dev_loopback = true;
        options.share.save_dir = save_dir.clone();
        options.share.settings_path = Some(state_dir.join(bridge_service::share::SETTINGS_FILE));
        options.share.offer_ttl = Duration::from_secs(3);
        let control = prepare_control(&state_dir, discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open");
        };
        let token = token.clone();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let tasks = vec![tokio::spawn(control::serve_control(l, Arc::new(control)))];
        Self {
            _tmp: tmp,
            save_dir,
            token,
            port,
            tasks,
        }
    }

    async fn call(&self, request: Request) -> Result<Response, IpcError> {
        let (port, token) = (self.port, self.token.clone());
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

    async fn share(&self) -> ShareStatus {
        match self.call(Request::ShareStatus).await {
            Ok(Response::Share(s)) => *s,
            other => panic!("expected share status, got {other:?}"),
        }
    }

    async fn wait_transfer(&self, what: &str, f: impl Fn(&TransferInfo) -> bool) -> TransferInfo {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let s = self.share().await;
            if let Some(t) = s.transfers.iter().find(|t| f(t)) {
                return t.clone();
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s:#?}");
            sleep(Duration::from_millis(30)).await;
        }
    }

    async fn status(&self) -> ipc::Status {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::status(port, &token))
            .await
            .unwrap()
            .unwrap()
    }

    /// Dial the phone, pair explicitly, and return the phone's side of the trusted link.
    async fn pair(&self, phone: &Arc<Phone>) -> kde::TrustedLink {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port: l.local_addr().unwrap().port(),
        };
        let phone2 = phone.clone();
        let join = tokio::spawn(async move {
            let (stream, _) = l.accept().await.unwrap();
            match phone2.endpoint.accept(stream).await.unwrap() {
                kde::Handshake::Pending(p) => p
                    .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                    .await
                    .unwrap(),
                kde::Handshake::Trusted(t) => t,
            }
        });
        self.ok(Request::Connect {
            device_id: phone.id(),
            endpoint: Some(endpoint),
        })
        .await;
        self.wait_link("unpaired", phone, LinkPhase::Unpaired).await;
        self.ok(Request::Pair {
            device_id: phone.id(),
            mode: PairMode::Request,
        })
        .await;
        let end = Instant::now() + Duration::from_secs(10);
        let challenge = loop {
            if let Some(c) = self.status().await.challenges.first() {
                break c.challenge_id.clone();
            }
            assert!(Instant::now() < end, "no challenge");
            sleep(Duration::from_millis(30)).await;
        };
        self.ok(Request::Approve {
            challenge_id: challenge,
        })
        .await;
        let link = timeout(Duration::from_secs(10), join)
            .await
            .unwrap()
            .unwrap();
        self.wait_link("connected", phone, LinkPhase::Connected)
            .await;
        link
    }

    async fn wait_link(&self, what: &str, phone: &Arc<Phone>, phase: LinkPhase) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let s = self.status().await;
            if s.links
                .iter()
                .any(|l| l.device_id == phone.id() && l.phase == phase)
            {
                return;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {s:#?}");
            sleep(Duration::from_millis(30)).await;
        }
    }
}

struct Phone {
    creds: Arc<kde::DeviceCredentials>,
    endpoint: kde::Endpoint,
}

impl Phone {
    fn new(caps: &[&str]) -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        Self::with_creds(creds, caps)
    }

    /// Same identity, empty trust store: the phone after a reinstall.
    fn reinstalled(&self) -> Arc<Self> {
        Self::with_creds(self.creds.clone(), &["kdeconnect.ping", SHARE])
    }

    fn with_creds(creds: Arc<kde::DeviceCredentials>, caps: &[&str]) -> Arc<Self> {
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.payload.any_port = true;
        config.payload.accept_timeout = Duration::from_secs(10);
        let caps: Vec<String> = caps.iter().map(|s| s.to_string()).collect();
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            caps.clone(),
            caps,
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Arc::new(Self { creds, endpoint })
    }

    fn sharing() -> Arc<Self> {
        Self::new(&["kdeconnect.ping", SHARE])
    }

    fn id(&self) -> String {
        self.creds.device_id().to_string()
    }
}

/// The phone announces a file and serves `bytes` on a payload socket.
async fn phone_offers(
    link: &mut kde::TrustedLink,
    name: &str,
    bytes: Vec<u8>,
) -> JoinHandle<Result<u64, kde::PayloadError>> {
    let ctx = link.payload_context();
    let listener = ctx.listen().await.unwrap();
    let packet = kshare::file_packet(
        name,
        bytes.len() as u64,
        Some(1_700_000_000_000),
        listener.port(),
    );
    link.send_with_payload(&packet).await.unwrap();
    let size = bytes.len() as u64;
    tokio::spawn(async move {
        listener
            .send(
                std::io::Cursor::new(bytes),
                size,
                &mut |_| {},
                &kde::CancelToken::new(),
            )
            .await
    })
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 13 % 251) as u8).collect()
}

#[tokio::test]
async fn receiving_is_off_until_the_user_allows_it() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    let serve = phone_offers(&mut link, "a.txt", data(100)).await;
    sleep(Duration::from_millis(300)).await;
    let s = rig.share().await;
    assert!(
        s.transfers.is_empty(),
        "an unpermitted offer must leave no trace"
    );
    assert!(
        !serve.is_finished(),
        "the payload socket must stay untouched"
    );
    assert_eq!(s.devices.len(), 1);
    assert!(!s.devices[0].receive && !s.devices[0].auto_accept);
    assert!(
        rig.save_dir
            .read_dir()
            .map_or(true, |mut d| d.next().is_none())
    );
    let status = rig.status().await;
    assert!(status.links[0].rejected_packets >= 1);
}

#[tokio::test]
async fn accept_saves_the_file_safely_and_never_overwrites() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: Some(true),
        auto_accept: None,
    })
    .await;
    std::fs::create_dir_all(&rig.save_dir).unwrap();
    std::fs::write(rig.save_dir.join("report.pdf"), b"precious").unwrap();

    let body = data(250_000);
    let serve = phone_offers(&mut link, "../../report.pdf", body.clone()).await;
    let t = rig
        .wait_transfer("an offer", |t| t.state == TransferState::Offered)
        .await;
    assert_eq!(
        (t.name.as_str(), t.direction, t.kind),
        (
            "report.pdf",
            TransferDirection::Incoming,
            TransferKind::File
        )
    );
    assert_eq!(t.size, Some(250_000));
    assert!(t.expires_in_ms.is_some());
    sleep(Duration::from_millis(200)).await;
    assert!(!serve.is_finished(), "nothing is fetched before Accept");
    assert_eq!(
        std::fs::read(rig.save_dir.join("report.pdf")).unwrap(),
        b"precious"
    );

    rig.ok(Request::ShareDecide {
        transfer_id: t.transfer_id,
        accept: true,
    })
    .await;
    let done = rig
        .wait_transfer("completion", |t| t.state == TransferState::Done)
        .await;
    assert_eq!(done.bytes, 250_000);
    let path = PathBuf::from(done.path.unwrap());
    assert_eq!(path, rig.save_dir.join("report (1).pdf"));
    assert_eq!(std::fs::read(&path).unwrap(), body);
    assert_eq!(
        std::fs::read(rig.save_dir.join("report.pdf")).unwrap(),
        b"precious"
    );
    serve.await.unwrap().unwrap();
    // Deciding twice is refused, and a finished entry can be dismissed.
    assert_eq!(
        rig.refused(Request::ShareDecide {
            transfer_id: done.transfer_id,
            accept: true
        })
        .await,
        ErrorCode::Busy
    );
    rig.ok(Request::ShareDismiss {
        transfer_id: done.transfer_id,
    })
    .await;
    assert!(rig.share().await.transfers.is_empty());
    // No stray temporary files.
    let names: Vec<_> = std::fs::read_dir(&rig.save_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[tokio::test]
async fn one_phone_cannot_fill_every_offer_slot() {
    let rig = Rig::start().await;
    let (a, b) = (Phone::sharing(), Phone::sharing());
    let mut link_a = rig.pair(&a).await;
    let mut link_b = rig.pair(&b).await;
    for p in [&a, &b] {
        rig.ok(Request::ShareSettings {
            device_id: p.id(),
            receive: Some(true),
            auto_accept: None,
        })
        .await;
    }
    let cap = bridge_service::share::MAX_OFFERED_PER_DEVICE;
    let mut serving = Vec::new();
    for i in 0..bridge_service::share::MAX_OFFERED + 2 {
        serving.push(phone_offers(&mut link_a, &format!("a{i}.bin"), data(10)).await);
    }
    serving.push(phone_offers(&mut link_b, "b.bin", data(10)).await);
    rig.wait_transfer("the other phone's offer", |t| t.name == "b.bin")
        .await;
    let from_a = rig
        .share()
        .await
        .transfers
        .iter()
        .filter(|t| t.device_id == a.id() && t.state == TransferState::Offered)
        .count();
    assert_eq!(from_a, cap);
}

#[tokio::test]
async fn decline_and_expiry_leave_nothing_on_disk() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: Some(true),
        auto_accept: None,
    })
    .await;
    let serve = phone_offers(&mut link, "no.bin", data(10)).await;
    let t = rig
        .wait_transfer("offer", |t| t.state == TransferState::Offered)
        .await;
    rig.ok(Request::ShareDecide {
        transfer_id: t.transfer_id,
        accept: false,
    })
    .await;
    rig.wait_transfer("declined", |t| t.state == TransferState::Declined)
        .await;
    assert!(!serve.is_finished());

    // An offer nobody answers lapses (the test rig uses a 3 s lifetime).
    let _serve2 = phone_offers(&mut link, "late.bin", data(10)).await;
    let late = rig
        .wait_transfer("second offer", |t| {
            t.name == "late.bin" && t.state == TransferState::Offered
        })
        .await;
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let s = rig.share().await;
        let t = s
            .transfers
            .iter()
            .find(|t| t.transfer_id == late.transfer_id)
            .unwrap();
        if t.state == TransferState::Failed {
            assert_eq!(t.code.as_deref(), Some("expired"));
            break;
        }
        assert!(Instant::now() < end, "offer never expired");
        sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        rig.refused(Request::ShareDecide {
            transfer_id: late.transfer_id,
            accept: true
        })
        .await,
        ErrorCode::Busy
    );
    assert!(!rig.save_dir.exists() || rig.save_dir.read_dir().unwrap().next().is_none());
}

#[tokio::test]
async fn auto_accept_is_per_device_and_cancel_works() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: None,
        auto_accept: Some(true),
    })
    .await;
    let s = rig.share().await;
    assert!(
        s.devices[0].receive && s.devices[0].auto_accept,
        "auto-accept implies receive"
    );
    let body = data(50_000);
    let serve = phone_offers(&mut link, "auto.bin", body.clone()).await;
    let done = rig
        .wait_transfer("auto download", |t| t.state == TransferState::Done)
        .await;
    assert_eq!(std::fs::read(done.path.unwrap()).unwrap(), body);
    serve.await.unwrap().unwrap();
    assert!(
        rig.refused(Request::ShareCancel {
            transfer_id: done.transfer_id
        })
        .await
            == ErrorCode::Busy
    );

    // Switching receive off turns auto-accept off with it.
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: Some(false),
        auto_accept: None,
    })
    .await;
    let s = rig.share().await;
    assert!(!s.devices[0].receive && !s.devices[0].auto_accept);
    // Unknown device and unknown transfer.
    assert_eq!(
        rig.refused(Request::ShareSettings {
            device_id: "z".repeat(32),
            receive: Some(true),
            auto_accept: None
        })
        .await,
        ErrorCode::NotFound
    );
    assert_eq!(
        rig.refused(Request::ShareCancel { transfer_id: 9999 })
            .await,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn text_and_links_wait_for_the_user() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: Some(true),
        auto_accept: None,
    })
    .await;
    link.send(&kshare::url_packet("https://example.com/x"))
        .await
        .unwrap();
    link.send(&kshare::text_packet("hello \u{7} there"))
        .await
        .unwrap();
    link.send(&kshare::url_packet("javascript:alert(1)"))
        .await
        .unwrap(); // stored as inert text, never opened by the service
    let end = Instant::now() + Duration::from_secs(10);
    let s = loop {
        let s = rig.share().await;
        if s.transfers.len() == 3 {
            break s;
        }
        assert!(Instant::now() < end, "items missing: {s:#?}");
        sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(s.transfers[0].kind, TransferKind::Url);
    assert_eq!(
        s.transfers[0].text.as_deref(),
        Some("https://example.com/x")
    );
    assert_eq!(s.transfers[1].kind, TransferKind::Text);
    assert_eq!(s.transfers[1].text.as_deref(), Some("hello  there"));
    assert!(s.transfers.iter().all(|t| t.state == TransferState::Done));
    let Ok(Response::Events(page)) = rig.call(Request::Events { after: 0 }).await else {
        panic!("expected events");
    };
    assert!(
        page.events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ShareReceivedText { .. }))
    );
}

#[tokio::test]
async fn sending_a_file_to_the_phone() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("holiday photo.jpg");
    let body = data(300_000);
    std::fs::write(&file, &body).unwrap();

    let Ok(Response::TransferStarted { transfer_id }) = rig
        .call(Request::ShareSendFile {
            device_id: phone.id(),
            path: file.display().to_string(),
        })
        .await
    else {
        panic!("expected a started transfer");
    };
    let ev = timeout(Duration::from_secs(10), link.recv())
        .await
        .unwrap()
        .unwrap();
    let kde::LinkEvent::Packet(p) = ev else {
        panic!("expected a packet")
    };
    assert_eq!(p.kind, SHARE);
    let Ok(kshare::ShareContent::File(offer)) = kshare::parse_share(&p, 1 << 30) else {
        panic!("{p:?}")
    };
    assert_eq!(
        (offer.name.as_str(), offer.size),
        ("holiday photo.jpg", Some(300_000))
    );
    let out = tempfile::tempdir().unwrap();
    let ctx = link.payload_context();
    let saved = kshare::receive_file(
        &ctx,
        &offer,
        out.path(),
        1 << 30,
        &mut |_| {},
        &kde::CancelToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), body);
    let t = rig
        .wait_transfer("sent", |t| {
            t.transfer_id == transfer_id && t.state == TransferState::Done
        })
        .await;
    assert_eq!(t.direction, TransferDirection::Outgoing);

    // Bad requests.
    let id = phone.id();
    assert_eq!(
        rig.refused(Request::ShareSendFile {
            device_id: id.clone(),
            path: "relative.txt".into()
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rig.refused(Request::ShareSendFile {
            device_id: id.clone(),
            path: dir.path().display().to_string()
        })
        .await,
        ErrorCode::BadRequest
    );
    assert_eq!(
        rig.refused(Request::ShareSendFile {
            device_id: id.clone(),
            path: dir.path().join("missing").display().to_string()
        })
        .await,
        ErrorCode::NotFound
    );
    assert_eq!(
        rig.refused(Request::ShareSendFile {
            device_id: "q".repeat(32),
            path: file.display().to_string()
        })
        .await,
        ErrorCode::NotConnected
    );
    assert_eq!(
        rig.refused(Request::ShareSendText {
            device_id: id.clone(),
            text_kind: ShareTextKind::Url,
            text: "ftp://x".into()
        })
        .await,
        ErrorCode::BadRequest
    );
    rig.ok(Request::ShareSendText {
        device_id: id,
        text_kind: ShareTextKind::Text,
        text: "hi".into(),
    })
    .await;
    let ev = timeout(Duration::from_secs(10), link.recv())
        .await
        .unwrap()
        .unwrap();
    let kde::LinkEvent::Packet(p) = ev else {
        panic!()
    };
    assert_eq!(
        kshare::parse_share(&p, 1),
        Ok(kshare::ShareContent::Text("hi".into()))
    );
}

#[tokio::test]
async fn a_phone_without_share_support_cannot_be_sent_to() {
    let rig = Rig::start().await;
    let phone = Phone::new(&["kdeconnect.ping"]);
    let _link = rig.pair(&phone).await;
    assert_eq!(
        rig.refused(Request::ShareSendText {
            device_id: phone.id(),
            text_kind: ShareTextKind::Text,
            text: "x".into()
        })
        .await,
        ErrorCode::Unsupported
    );
}

/// An outgoing offer holds a payload listener. Cancelling it, or unpairing the phone it was for, must close that
/// port at once instead of leaving it to the accept timeout.
#[tokio::test]
async fn cancel_and_unpair_close_the_payload_listener() {
    async fn offered_port(
        rig: &Rig,
        phone: &Arc<Phone>,
        link: &mut kde::TrustedLink,
        path: &str,
    ) -> (u64, u16) {
        let Ok(Response::TransferStarted { transfer_id }) = rig
            .call(Request::ShareSendFile {
                device_id: phone.id(),
                path: path.to_owned(),
            })
            .await
        else {
            panic!("expected a started transfer");
        };
        let ev = timeout(Duration::from_secs(10), link.recv())
            .await
            .unwrap()
            .unwrap();
        let kde::LinkEvent::Packet(p) = ev else {
            panic!("expected a packet")
        };
        let Ok(kshare::ShareContent::File(offer)) = kshare::parse_share(&p, 1 << 30) else {
            panic!("{p:?}")
        };
        (transfer_id, offer.port)
    }
    async fn closes(port: u16) -> bool {
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let mut link = rig.pair(&phone).await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.bin");
    std::fs::write(&file, data(1000)).unwrap();
    let path = file.display().to_string();

    let (id, port) = offered_port(&rig, &phone, &mut link, &path).await;
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    );
    rig.ok(Request::ShareCancel { transfer_id: id }).await;
    assert!(closes(port).await, "cancelled: port {port} still answers");

    let (_, port) = offered_port(&rig, &phone, &mut link, &path).await;
    rig.ok(Request::Unpair {
        device_id: phone.id(),
    })
    .await;
    assert!(closes(port).await, "unpaired: port {port} still answers");
}

#[tokio::test]
async fn unpairing_forgets_share_permissions() {
    let rig = Rig::start().await;
    let phone = Phone::sharing();
    let _link = rig.pair(&phone).await;
    rig.ok(Request::ShareSettings {
        device_id: phone.id(),
        receive: None,
        auto_accept: Some(true),
    })
    .await;
    rig.ok(Request::Unpair {
        device_id: phone.id(),
    })
    .await;
    let s = rig.share().await;
    assert!(s.devices.is_empty());
    // Pair again: permissions start from "off", not from the old grant.
    let _link = rig.pair(&phone.reinstalled()).await;
    let s = rig.share().await;
    assert!(!s.devices[0].receive && !s.devices[0].auto_accept);
}
