// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end clipboard bridge tests: a real `Daemon` with an in-memory OS clipboard, and
//! stand-in phones that are real `bridge_kde` endpoints talking TLS over loopback.
//!
//! These show the service side of the protocol works against its own transport. They say nothing
//! about interoperability with the Android app or a real OS clipboard.

use bridge_core::{
    Snapshot,
    clipboard_wire::{self, CLIPBOARD, ITEM, ItemKind, NONE, NoneReason, PULL},
    ipc::{
        self, ClipboardStatus, ErrorCode, Feature, IpcError, LinkPhase, PairMode, Request,
        Response, Status, Token,
    },
};
use bridge_kde::{self as kde};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    clipboard::{ClipImage, ClipboardBackend, MemoryClipboard},
    control::{self, Control},
    daemon::DaemonOptions,
    hotkey_bind::HotkeyBinder,
    prepare_control,
};
use serde_json::Map;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::RwLock,
    task::JoinHandle,
    time::{sleep, timeout},
};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nimage-bytes-here";

#[derive(Default)]
struct FakeBinder {
    bound: Mutex<Vec<String>>,
}
impl HotkeyBinder for FakeBinder {
    fn name(&self) -> &'static str {
        "fake"
    }
    fn bind(&self, key: &bridge_core::hotkey::Hotkey, _command: &str) -> bool {
        self.bound
            .lock()
            .unwrap()
            .push(format!("+{}", key.normalized()));
        true
    }
    fn unbind(&self, key: &bridge_core::hotkey::Hotkey) {
        self.bound
            .lock()
            .unwrap()
            .push(format!("-{}", key.normalized()));
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    clip: Arc<MemoryClipboard>,
    binder: Arc<FakeBinder>,
    token: Token,
    control_port: u16,
    tasks: Vec<JoinHandle<std::io::Result<()>>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir: PathBuf = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let clip = Arc::new(MemoryClipboard::default());
        let binder = Arc::new(FakeBinder::default());
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(DisabledCallingProbe));
        options.dev_loopback = true;
        options.clipboard = Some(clip.clone() as Arc<dyn ClipboardBackend>);
        options.clipboard_settings = Some(state_dir.join("clipboard-settings.json"));
        options.hotkey_binder = Some(binder.clone());
        let control = prepare_control(&state_dir, discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open");
        };
        let token = token.clone();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let control_port = l.local_addr().unwrap().port();
        let tasks = vec![tokio::spawn(control::serve_control(l, Arc::new(control)))];
        Self {
            _tmp: tmp,
            clip,
            binder,
            token,
            control_port,
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
    async fn clipboard(&self) -> ClipboardStatus {
        match self.call(Request::ClipboardStatus).await {
            Ok(Response::ClipboardStatus(s)) => *s,
            other => panic!("expected clipboard status, got {other:?}"),
        }
    }
    async fn wait_clip(
        &self,
        what: &str,
        cond: impl Fn(&ClipboardStatus) -> bool,
    ) -> ClipboardStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let s = self.clipboard().await;
            if cond(&s) {
                return s;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {s:#?}"
            );
            sleep(Duration::from_millis(30)).await;
        }
    }
    async fn wait_until(&self, what: &str, cond: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            sleep(Duration::from_millis(20)).await;
        }
    }
}

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

struct Phone {
    creds: Arc<kde::DeviceCredentials>,
    endpoint: kde::Endpoint,
}
impl Phone {
    /// A phone that speaks the full GeekBridge clipboard protocol.
    fn geekbridge() -> Arc<Self> {
        Self::new(
            &[
                "kdeconnect.ping",
                CLIPBOARD,
                "kdeconnect.clipboard.connect",
                PULL,
                ITEM,
            ],
            &["kdeconnect.ping", ITEM, NONE],
        )
    }
    /// A stock KDE Connect phone: only the plain clipboard packets.
    fn stock() -> Arc<Self> {
        Self::new(
            &["kdeconnect.ping", CLIPBOARD, "kdeconnect.clipboard.connect"],
            &["kdeconnect.ping", CLIPBOARD, "kdeconnect.clipboard.connect"],
        )
    }
    /// A phone that does not do clipboard at all.
    fn silent() -> Arc<Self> {
        Self::new(&["kdeconnect.ping"], &["kdeconnect.ping"])
    }
    fn new(incoming: &[&str], outgoing: &[&str]) -> Arc<Self> {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.payload.any_port = true;
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
    fn id(&self) -> String {
        self.creds.device_id().to_string()
    }
}

async fn pair_phone(rig: &Rig, phone: &Arc<Phone>) -> kde::TrustedLink {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = ipc::Endpoint {
        address: "127.0.0.1".parse().unwrap(),
        port: l.local_addr().unwrap().port(),
    };
    let p = phone.clone();
    let link = tokio::spawn(async move {
        let (stream, _) = l.accept().await.unwrap();
        match p.endpoint.accept(stream).await.unwrap() {
            kde::Handshake::Trusted(link) => link,
            kde::Handshake::Pending(pending) => pending
                .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                .await
                .unwrap(),
        }
    });
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(endpoint),
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let s = rig.status().await;
        if s.links
            .iter()
            .any(|l| l.device_id == phone.id() && l.phase == LinkPhase::Unpaired)
        {
            break;
        }
        assert!(Instant::now() < deadline, "no unpaired link");
        sleep(Duration::from_millis(30)).await;
    }
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let challenge = loop {
        let s = rig.status().await;
        if let Some(c) = s.challenges.first() {
            break c.challenge_id.clone();
        }
        assert!(Instant::now() < deadline, "no challenge");
        sleep(Duration::from_millis(30)).await;
    };
    rig.ok(Request::Approve {
        challenge_id: challenge,
    })
    .await;
    let link = timeout(Duration::from_secs(10), link)
        .await
        .unwrap()
        .unwrap();
    loop {
        let s = rig.status().await;
        if s.links
            .iter()
            .any(|l| l.device_id == phone.id() && l.phase == LinkPhase::Connected)
        {
            return link;
        }
        assert!(Instant::now() < deadline, "not connected");
        sleep(Duration::from_millis(30)).await;
    }
}

async fn enable(rig: &Rig, phone: &Arc<Phone>) {
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Clipboard,
        enabled: true,
    })
    .await;
}

async fn next_packet(link: &mut kde::TrustedLink) -> kde::Packet {
    loop {
        match timeout(Duration::from_secs(10), link.recv())
            .await
            .unwrap()
            .unwrap()
        {
            kde::LinkEvent::Packet(p) if p.kind != "kdeconnect.ping" => return p,
            _ => {}
        }
    }
}

fn packet(kind: &str, body: Map<String, serde_json::Value>) -> kde::Packet {
    kde::Packet::new(kind, body)
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn clipboard_is_off_per_device_until_the_user_opts_in() {
    let rig = Rig::start().await;
    let phone = Phone::stock();
    let mut link = pair_phone(&rig, &phone).await;
    let s = rig.clipboard().await;
    assert!(s.available);
    assert!(!s.watching, "nothing is watched before opt-in");
    let d = s
        .devices
        .iter()
        .find(|d| d.device_id == phone.id())
        .unwrap();
    assert!(!d.enabled && d.connected && d.negotiated);

    // A phone that sends anyway is ignored: not written, not counted as misbehaviour.
    link.send(&packet(CLIPBOARD, clipboard_wire::update_body("sneaky")))
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.clip.text(), None);
    assert_eq!(rig.clip.writes(), 0);
    assert_eq!(rig.status().await.links[0].rejected_packets, 0);

    enable(&rig, &phone).await;
    link.send(&packet(CLIPBOARD, clipboard_wire::update_body("hello pc")))
        .await
        .unwrap();
    rig.wait_until("phone text applied", || {
        rig.clip.text().as_deref() == Some("hello pc")
    })
    .await;
    let d = rig.clipboard().await.devices.remove(0);
    assert_eq!((d.received, d.sent), (1, 0));
}

#[tokio::test]
async fn computer_copy_reaches_the_phone_and_the_phone_echo_does_not_loop() {
    let rig = Rig::start().await;
    let phone = Phone::stock();
    let mut link = pair_phone(&rig, &phone).await;
    rig.clip.user_copy("copied before opt-in", false);
    enable(&rig, &phone).await;
    rig.wait_clip("watcher", |s| s.watching).await;
    sleep(Duration::from_millis(700)).await;
    rig.clip.user_copy("fresh on the pc", false);
    let p = next_packet(&mut link).await;
    assert_eq!(p.kind, CLIPBOARD);
    assert_eq!(p.body["content"], "fresh on the pc");
    // The phone writes the same text back (as a stock phone's listener would): no second send.
    link.send(&packet(
        CLIPBOARD,
        clipboard_wire::update_body("fresh on the pc"),
    ))
    .await
    .unwrap();
    sleep(Duration::from_millis(900)).await;
    let d = rig.clipboard().await.devices.remove(0);
    assert_eq!(
        d.sent, 1,
        "the pre-opt-in text and the echo were never sent"
    );
}

#[tokio::test]
async fn paste_from_phone_pulls_a_text_item_and_reports_the_outcome() {
    let rig = Rig::start().await;
    let phone = Phone::geekbridge();
    let mut link = pair_phone(&rig, &phone).await;
    // Not opted in yet.
    assert_eq!(
        rig.refused(Request::ClipboardPull { device_id: None })
            .await,
        ErrorCode::NotConnected
    );
    enable(&rig, &phone).await;
    rig.ok(Request::ClipboardPull { device_id: None }).await;
    let pull = next_packet(&mut link).await;
    assert_eq!(pull.kind, PULL);
    let id = pull.body["request_id"].as_str().unwrap().to_owned();
    assert_eq!(pull.body["max_age_ms"], 30_000);
    assert_eq!(rig.clipboard().await.last_pull.unwrap().state, "waiting");

    link.send(&packet(
        ITEM,
        clipboard_wire::text_item_body(
            ItemKind::Url,
            "https://example.org/p",
            None,
            Some(&id),
            2_000,
        ),
    ))
    .await
    .unwrap();
    let s = rig
        .wait_clip("delivered", |s| {
            s.last_pull.as_ref().is_some_and(|p| p.state == "delivered")
        })
        .await;
    assert_eq!(s.last_pull.unwrap().kind.as_deref(), Some("url"));
    assert_eq!(rig.clip.text().as_deref(), Some("https://example.org/p"));

    // A refusal from the phone is reported with its reason.
    rig.ok(Request::ClipboardPull {
        device_id: Some(phone.id()),
    })
    .await;
    let pull = next_packet(&mut link).await;
    let id = pull.body["request_id"].as_str().unwrap().to_owned();
    link.send(&packet(
        NONE,
        clipboard_wire::none_body(&id, NoneReason::Locked),
    ))
    .await
    .unwrap();
    let s = rig
        .wait_clip("none", |s| {
            s.last_pull.as_ref().is_some_and(|p| p.state == "none")
        })
        .await;
    assert_eq!(s.last_pull.unwrap().reason.as_deref(), Some("locked"));
}

#[tokio::test]
async fn items_older_than_the_window_are_dropped_even_if_the_phone_sends_them() {
    let rig = Rig::start().await;
    let phone = Phone::geekbridge();
    let mut link = pair_phone(&rig, &phone).await;
    enable(&rig, &phone).await;
    rig.ok(Request::ClipboardSettings {
        window_ms: Some(10_000),
        hotkey: None,
        max_image_bytes: None,
    })
    .await;
    rig.ok(Request::ClipboardPull { device_id: None }).await;
    let pull = next_packet(&mut link).await;
    assert_eq!(
        pull.body["max_age_ms"], 10_000,
        "the setting is what the phone is asked to honour"
    );
    let id = pull.body["request_id"].as_str().unwrap().to_owned();
    link.send(&packet(
        ITEM,
        clipboard_wire::text_item_body(ItemKind::Text, "stale", None, Some(&id), 11_000),
    ))
    .await
    .unwrap();
    let s = rig
        .wait_clip("expired", |s| {
            s.last_pull.as_ref().is_some_and(|p| p.state == "none")
        })
        .await;
    assert_eq!(s.last_pull.unwrap().reason.as_deref(), Some("expired"));
    assert_eq!(rig.clip.writes(), 0);
}

#[tokio::test]
async fn images_arrive_as_a_payload_and_are_validated_before_use() {
    let rig = Rig::start().await;
    let phone = Phone::geekbridge();
    let mut link = pair_phone(&rig, &phone).await;
    enable(&rig, &phone).await;
    for (bytes, expect_ok) in [
        (PNG.to_vec(), true),
        (b"GIF89a-not-allowed-here".to_vec(), false),
    ] {
        rig.ok(Request::ClipboardPull { device_id: None }).await;
        let pull = next_packet(&mut link).await;
        let id = pull.body["request_id"].as_str().unwrap().to_owned();
        let ctx = link.payload_context();
        let listener = ctx.listen().await.unwrap();
        let mut p = packet(
            ITEM,
            clipboard_wire::image_item_body("image/png", bytes.len() as u64, Some(&id), 500),
        );
        p.payload_size = Some(bytes.len() as i64);
        let mut info = Map::new();
        info.insert("port".into(), listener.port().into());
        p.payload_transfer_info = Some(info);
        link.send_with_payload(&p).await.unwrap();
        let size = bytes.len() as u64;
        let sender = tokio::spawn(async move {
            let cancel = kde::CancelToken::new();
            let mut progress = |_: u64| {};
            let _ = listener
                .send(std::io::Cursor::new(bytes), size, &mut progress, &cancel)
                .await;
        });
        let s = rig
            .wait_clip("pull settled", |s| {
                s.last_pull.as_ref().is_some_and(|p| p.state != "waiting")
            })
            .await;
        let _ = sender.await;
        let lp = s.last_pull.unwrap();
        if expect_ok {
            assert_eq!(
                (lp.state.as_str(), lp.kind.as_deref()),
                ("delivered", Some("image"))
            );
            assert_eq!(rig.clip.image().unwrap().bytes, PNG);
        } else {
            assert_eq!(lp.state, "none");
            assert_eq!(
                rig.clip.image().unwrap().bytes,
                PNG,
                "the bad image did not replace it"
            );
        }
    }
}

#[tokio::test]
async fn push_to_phone_streams_an_image_to_a_geekbridge_phone() {
    let rig = Rig::start().await;
    let phone = Phone::geekbridge();
    let mut link = pair_phone(&rig, &phone).await;
    enable(&rig, &phone).await;
    rig.clip.user_copy_image(ClipImage {
        mime: "image/png",
        bytes: PNG.to_vec(),
    });
    rig.ok(Request::ClipboardPush { device_id: None }).await;
    let p = next_packet(&mut link).await;
    assert_eq!(p.kind, ITEM);
    assert_eq!(p.body["kind"], "image");
    assert_eq!(p.payload_size, Some(PNG.len() as i64));
    let port = p.payload_transfer_info.unwrap()["port"].as_u64().unwrap() as u16;
    let ctx = link.payload_context();
    let mut got: Vec<u8> = Vec::new();
    let cancel = kde::CancelToken::new();
    let mut progress = |_: u64| {};
    ctx.receive(
        port,
        Some(PNG.len() as u64),
        1 << 20,
        &mut got,
        &mut progress,
        &cancel,
    )
    .await
    .unwrap();
    assert_eq!(got, PNG);
}

#[tokio::test]
async fn a_phone_without_clipboard_support_cannot_be_asked_or_heard() {
    let rig = Rig::start().await;
    let phone = Phone::silent();
    let mut link = pair_phone(&rig, &phone).await;
    enable(&rig, &phone).await;
    assert_eq!(
        rig.refused(Request::ClipboardPull { device_id: None })
            .await,
        ErrorCode::Unsupported
    );
    // Un-negotiated clipboard packets are rejected like any other.
    link.send(&packet(CLIPBOARD, clipboard_wire::update_body("x")))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.status().await.links[0].rejected_packets == 0 {
        assert!(Instant::now() < deadline);
        sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(rig.clip.writes(), 0);
}

#[tokio::test]
async fn settings_are_validated_and_the_hotkey_follows_the_opt_in() {
    let rig = Rig::start().await;
    let phone = Phone::geekbridge();
    let _link = pair_phone(&rig, &phone).await;
    let s = rig.clipboard().await;
    assert_eq!((s.hotkey.as_str(), s.window_ms), ("Ctrl+Shift+V", 30_000));
    assert!(
        rig.binder.bound.lock().unwrap().is_empty(),
        "no binding before opt-in"
    );
    assert_eq!(
        rig.refused(Request::ClipboardSettings {
            window_ms: None,
            hotkey: Some("V".into()),
            max_image_bytes: None
        })
        .await,
        ErrorCode::BadRequest
    );
    enable(&rig, &phone).await;
    rig.wait_until("hotkey bound", || {
        rig.binder.bound.lock().unwrap().as_slice() == ["+Ctrl+Shift+V"]
    })
    .await;
    assert_eq!(rig.clipboard().await.hotkey_binding, "bound");
    rig.ok(Request::ClipboardSettings {
        window_ms: Some(60_000),
        hotkey: Some("ctrl+alt+p".into()),
        max_image_bytes: Some(2 * 1024 * 1024),
    })
    .await;
    let s = rig.clipboard().await;
    assert_eq!(
        (s.hotkey.as_str(), s.window_ms, s.max_image_bytes),
        ("Ctrl+Alt+P", 60_000, 2 * 1024 * 1024)
    );
    assert_eq!(
        rig.binder.bound.lock().unwrap().as_slice(),
        ["+Ctrl+Shift+V", "-Ctrl+Shift+V", "+Ctrl+Alt+P"]
    );
    // Opting out removes the binding again.
    rig.ok(Request::SetConsent {
        device_id: phone.id(),
        feature: Feature::Clipboard,
        enabled: false,
    })
    .await;
    rig.wait_until("hotkey unbound", || {
        rig.binder.bound.lock().unwrap().last().map(String::as_str) == Some("-Ctrl+Alt+P")
    })
    .await;
}

#[tokio::test]
async fn status_never_contains_clipboard_text() {
    let rig = Rig::start().await;
    let phone = Phone::stock();
    let mut link = pair_phone(&rig, &phone).await;
    enable(&rig, &phone).await;
    link.send(&packet(
        CLIPBOARD,
        clipboard_wire::update_body("TOP-SECRET-CLIP-9c1e"),
    ))
    .await
    .unwrap();
    rig.wait_until("applied", || rig.clip.text().is_some())
        .await;
    let json = serde_json::to_string(&rig.clipboard().await).unwrap();
    assert!(!json.contains("TOP-SECRET-CLIP-9c1e"));
    let st = serde_json::to_string(&rig.status().await).unwrap();
    assert!(!st.contains("TOP-SECRET-CLIP-9c1e"));
}
