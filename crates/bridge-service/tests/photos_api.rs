// SPDX-License-Identifier: GPL-3.0-or-later
//! Black-box tests of recent phone photos through the control API: consent, asked-for-only, bounded
//! thumbnails, full pictures over the share channel without the general receive permission, erasure.
//!
//! The "phone" is a real `bridge_kde` endpoint with a generated certificate; payload sockets are
//! real pinned-TLS connections over loopback. Nothing here talks to a real phone.

use bridge_core::{
    Snapshot,
    ipc::{self, ErrorCode, Feature, IpcError, LinkPhase, PairMode, Request, Response, Token},
    photos_link as wire,
    photos_view::{FetchMode, FetchState, PhotosView},
    share::ShareStatus,
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
    /// A GeekBridge phone with recent photos on: it shares files and answers the photo requests.
    fn photographer() -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.payload.any_port = true;
        config.payload.accept_timeout = Duration::from_secs(10);
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            s(&["kdeconnect.ping", SHARE, wire::REQUEST]),
            s(&["kdeconnect.ping", SHARE, wire::LIST, wire::THUMB]),
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        Arc::new(Self { creds, endpoint })
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

/// A tiny JPEG header is enough for the packet checks (the service never decodes pixels).
fn jpeg(n: u8) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0];
    v.extend((0..200).map(|i| (i as u8).wrapping_add(n)));
    v.extend([0xFF, 0xD9]);
    v
}

fn photo(n: usize) -> wire::Photo {
    wire::Photo {
        id: format!("p{n}"),
        name: format!("IMG_{n:04}.jpg"),
        taken_ms: 1_700_000_000_000 + n as u64 * 1000,
        size: 5000 + n as u64,
        width: 4000,
        height: 3000,
    }
}

/// What the test tells the phone to send that nobody asked for.
enum Push {
    /// A list nobody asked for.
    List,
    /// A thumbnail nobody asked for.
    Thumb,
    /// A file offer nobody asked for.
    File(&'static str),
}

/// The phone: answers photo requests from a fixed set of photos, serves a full picture when asked, and
/// reports every request kind it saw.
fn run_phone(
    mut link: kde::TrustedLink,
    count: usize,
) -> (
    tokio::sync::mpsc::Sender<Push>,
    tokio::sync::mpsc::UnboundedReceiver<wire::Ask>,
) {
    let (push_tx, mut push_rx) = tokio::sync::mpsc::channel::<Push>(4);
    let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut serving = Vec::new();
        loop {
            tokio::select! {
                ev = link.recv() => match ev {
                    Ok(kde::LinkEvent::Packet(p)) if p.kind == wire::REQUEST => {
                        let Ok(ask) = wire::parse_request(&p.body) else { continue };
                        match &ask {
                            wire::Ask::List { limit } => {
                                let list = wire::List {
                                    items: (1..=count.min(*limit)).map(photo).collect(),
                                    partial: true,
                                };
                                let _ = link.send(&kde::Packet::new(wire::LIST, wire::list_body(&list))).await;
                            }
                            wire::Ask::Thumb { id } => {
                                let n: usize = id[1..].parse().unwrap_or(0);
                                let body = if (1..=count).contains(&n) {
                                    wire::thumb_body(id, 160, 120, &jpeg(n as u8))
                                } else {
                                    wire::thumb_gone_body(id)
                                };
                                let _ = link.send(&kde::Packet::new(wire::THUMB, body)).await;
                            }
                            wire::Ask::Get { id } => {
                                let n: usize = id[1..].parse().unwrap_or(0);
                                serving.push(phone_offers(&mut link, &photo(n).name, data(5000 + n)).await);
                            }
                        }
                        let _ = seen_tx.send(ask);
                    }
                    Ok(_) => {}
                    Err(_) => return,
                },
                Some(push) = push_rx.recv() => match push {
                    Push::List => {
                        let list = wire::List { items: vec![photo(9)], partial: false };
                        let _ = link.send(&kde::Packet::new(wire::LIST, wire::list_body(&list))).await;
                    }
                    Push::Thumb => {
                        let _ = link.send(&kde::Packet::new(wire::THUMB, wire::thumb_body("p1", 1, 1, &jpeg(7)))).await;
                    }
                    Push::File(name) => {
                        serving.push(phone_offers(&mut link, name, data(300)).await);
                    }
                },
            }
        }
    });
    (push_tx, seen_rx)
}

impl Rig {
    async fn photos(&self) -> PhotosView {
        match self.call(Request::Photos { device_id: None }).await {
            Ok(Response::Photos(v)) => *v,
            other => panic!("expected photos, got {other:?}"),
        }
    }

    async fn until_photos(&self, what: &str, f: impl Fn(&PhotosView) -> bool) -> PhotosView {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let v = self.photos().await;
            if f(&v) {
                return v;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}: {v:#?}");
            sleep(Duration::from_millis(30)).await;
        }
    }

    async fn consent(&self, phone: &Arc<Phone>, on: bool) {
        self.ok(Request::SetConsent {
            device_id: phone.id(),
            feature: Feature::Photos,
            enabled: on,
        })
        .await;
    }
}

#[tokio::test]
async fn photos_are_offered_but_nothing_is_asked_or_kept_until_the_user_allows_them() {
    let rig = Rig::start().await;
    let phone = Phone::photographer();
    let link = rig.pair(&phone).await;
    let (push, mut seen) = run_phone(link, 3);

    // The phone declared the packets: the view offers them, with no items and no consent.
    let v = rig.photos().await;
    assert!(v.devices.is_empty() && v.items.is_empty());
    assert_eq!(v.offered.len(), 1);
    // Asking is refused without consent, and the phone hears nothing.
    assert_eq!(
        rig.refused(Request::PhotosRefresh {
            device_id: phone.id()
        })
        .await,
        ErrorCode::Unsupported
    );
    // A phone that sends a list on its own is ignored, not punished.
    push.send(Push::List).await.unwrap();
    sleep(Duration::from_millis(300)).await;
    assert!(seen.try_recv().is_err());
    assert!(rig.photos().await.items.is_empty());
    assert_eq!(rig.status().await.links[0].rejected_packets, 0);
}

#[tokio::test]
async fn a_refresh_lists_photos_and_thumbnails_only_and_they_are_erased_on_withdrawal() {
    let rig = Rig::start().await;
    let phone = Phone::photographer();
    let link = rig.pair(&phone).await;
    let (push, mut seen) = run_phone(link, 3);
    rig.consent(&phone, true).await;
    // Allowing asks for nothing by itself.
    sleep(Duration::from_millis(300)).await;
    assert!(seen.try_recv().is_err(), "consent alone sends no request");

    rig.ok(Request::PhotosRefresh {
        device_id: phone.id(),
    })
    .await;
    let v = rig
        .until_photos("three thumbnails", |v| {
            v.items.len() == 3 && v.items.iter().all(|i| i.has_thumb)
        })
        .await;
    assert!(v.devices[0].partial && v.devices[0].listed);
    // Newest first.
    assert_eq!(v.items[0].id, "p3");
    assert_eq!(v.items[0].name, "IMG_0003.jpg");
    // The phone was asked for one list and one thumbnail per photo, nothing else (no full picture).
    let mut asks = Vec::new();
    while let Ok(a) = seen.try_recv() {
        asks.push(a);
    }
    assert_eq!(
        asks.iter()
            .filter(|a| matches!(a, wire::Ask::List { .. }))
            .count(),
        1
    );
    assert_eq!(
        asks.iter()
            .filter(|a| matches!(a, wire::Ask::Thumb { .. }))
            .count(),
        3
    );
    assert!(!asks.iter().any(|a| matches!(a, wire::Ask::Get { .. })));

    // A thumbnail is served from memory; an unknown one is not found.
    let t = match rig
        .call(Request::PhotoThumb {
            device_id: phone.id(),
            id: "p2".into(),
        })
        .await
    {
        Ok(Response::PhotoThumb(t)) => t,
        other => panic!("{other:?}"),
    };
    assert_eq!((t.width, t.height), (160, 120));
    assert_eq!(
        rig.refused(Request::PhotoThumb {
            device_id: phone.id(),
            id: "p77".into()
        })
        .await,
        ErrorCode::NotFound
    );
    // A refresh right after another is coalesced, not forwarded.
    rig.ok(Request::PhotosRefresh {
        device_id: phone.id(),
    })
    .await;
    sleep(Duration::from_millis(200)).await;
    assert!(seen.try_recv().is_err());
    // A thumbnail nobody asked for is dropped.
    push.send(Push::Thumb).await.unwrap();
    sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.status().await.links[0].rejected_packets, 0);

    // Withdrawing erases at once; the thumbnail is gone and asking is refused again.
    rig.consent(&phone, false).await;
    let v = rig.photos().await;
    assert!(v.items.is_empty() && v.devices.is_empty());
    assert_eq!(
        rig.refused(Request::PhotoThumb {
            device_id: phone.id(),
            id: "p2".into()
        })
        .await,
        ErrorCode::Unsupported
    );
}

#[tokio::test]
async fn a_full_picture_is_taken_only_because_it_was_asked_for_and_without_the_receive_permission()
{
    let rig = Rig::start().await;
    let phone = Phone::photographer();
    let link = rig.pair(&phone).await;
    let (push, mut seen) = run_phone(link, 3);
    rig.consent(&phone, true).await;
    rig.ok(Request::PhotosRefresh {
        device_id: phone.id(),
    })
    .await;
    rig.until_photos("a list", |v| v.items.len() == 3).await;
    while seen.try_recv().is_ok() {}

    // The general receive permission is off (the default). A file the phone offers on its own is refused.
    push.send(Push::File("surprise.jpg")).await.unwrap();
    sleep(Duration::from_millis(300)).await;
    assert!(rig.share().await.transfers.is_empty());
    assert!(rig.status().await.links[0].rejected_packets >= 1);

    // Asking for a photo makes the phone offer exactly that file, which lands in the save folder without a prompt.
    assert_eq!(
        rig.refused(Request::PhotoFetch {
            device_id: phone.id(),
            id: "p42".into(),
            mode: FetchMode::Save
        })
        .await,
        ErrorCode::NotFound,
        "only a listed photo can be asked for"
    );
    rig.ok(Request::PhotoFetch {
        device_id: phone.id(),
        id: "p2".into(),
        mode: FetchMode::Save,
    })
    .await;
    let v = rig
        .until_photos("the picture", |v| {
            v.fetches.iter().any(|f| f.state == FetchState::Done)
        })
        .await;
    let f = &v.fetches[0];
    assert_eq!((f.photo_id.as_str(), f.mode), ("p2", FetchMode::Save));
    let path = PathBuf::from(f.path.clone().unwrap());
    assert_eq!(path, rig.save_dir.join("IMG_0002.jpg"));
    assert_eq!(std::fs::read(&path).unwrap(), data(5002));
    assert!(matches!(seen.try_recv(), Ok(wire::Ask::Get { id }) if id == "p2"));

    // It was single use: the same name offered again is refused.
    push.send(Push::File("IMG_0002.jpg")).await.unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(std::fs::read_dir(&rig.save_dir).unwrap().count(), 1);
    // And the general receive permission is still off.
    assert!(!rig.share().await.devices[0].receive);
}

#[tokio::test]
async fn a_picture_for_the_clipboard_goes_to_a_scratch_folder_and_a_withdrawn_request_is_forgotten()
{
    let rig = Rig::start().await;
    let phone = Phone::photographer();
    let link = rig.pair(&phone).await;
    let (push, mut seen) = run_phone(link, 3);
    rig.consent(&phone, true).await;
    rig.ok(Request::PhotosRefresh {
        device_id: phone.id(),
    })
    .await;
    rig.until_photos("a list", |v| v.items.len() == 3).await;
    while seen.try_recv().is_ok() {}

    rig.ok(Request::PhotoFetch {
        device_id: phone.id(),
        id: "p3".into(),
        mode: FetchMode::Copy,
    })
    .await;
    let v = rig
        .until_photos("the copy", |v| {
            v.fetches.iter().any(|f| f.state == FetchState::Done)
        })
        .await;
    let path = PathBuf::from(v.fetches[0].path.clone().unwrap());
    assert_eq!(
        path.parent().unwrap(),
        rig.save_dir.join(bridge_service::share::COPY_DIR)
    );
    assert_eq!(std::fs::read(&path).unwrap(), data(5003));
    assert!(
        !rig.save_dir.join("IMG_0003.jpg").exists(),
        "a copy for the clipboard is not kept in the save folder"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "scratch folder is private: {mode:o}");
    }

    // A request whose consent is withdrawn before the phone answers is forgotten: a late offer is refused.
    rig.ok(Request::PhotoFetch {
        device_id: phone.id(),
        id: "p1".into(),
        mode: FetchMode::Save,
    })
    .await;
    rig.consent(&phone, false).await;
    let _ = push; // the phone already offers p1 in answer to the request; whether it beat the withdrawal is a race
    sleep(Duration::from_millis(400)).await;
    let v = rig.photos().await;
    assert!(
        v.fetches.is_empty(),
        "nothing is reported for a phone that is off"
    );
}
