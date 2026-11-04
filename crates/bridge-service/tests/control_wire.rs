// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw-socket tests of the control listener's authentication: wrong or replayed MACs, a valid MAC over a garbage body,
//! a client that never speaks, a flood of idle connections, and the "unavailable" greeting. Nothing here uses the
//! well-behaved client in `bridge_core::ipc`, so the server is exercised exactly as a hostile local process would.

use bridge_core::{
    Snapshot,
    ipc::{self, Hello, Request, Response, Token},
};
use bridge_service::{
    SharedState,
    calling::CallingProbe,
    control::{self, Control},
    daemon::DaemonOptions,
    prepare_control,
};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::RwLock,
    task::JoinHandle,
};

struct NoProbe;
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

struct Rig {
    _tmp: tempfile::TempDir,
    token: Token,
    port: u16,
    task: JoinHandle<std::io::Result<()>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(control: Control) -> (u16, JoinHandle<std::io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    (
        port,
        tokio::spawn(control::serve_control(listener, Arc::new(control))),
    )
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(NoProbe));
        options.dev_loopback = true;
        let control = prepare_control(&tmp.path().join("state"), discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open");
        };
        let token = token.clone();
        let (port, task) = serve(control).await;
        Self {
            _tmp: tmp,
            token,
            port,
            task,
        }
    }

    async fn connect(&self) -> (TcpStream, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).await.unwrap();
        let nonce = match ipc::parse_hello(&read_line(&mut s).await).unwrap() {
            Hello::Ready { nonce } => nonce,
            other => panic!("expected ready, got {other:?}"),
        };
        (s, nonce)
    }

    fn frame(&self, nonce: &str, body: &[u8]) -> Vec<u8> {
        let mac = self.token.request_mac(nonce, body);
        let mut f = ipc::encode_hex(&mac).into_bytes();
        f.push(b' ');
        f.extend_from_slice(body);
        f.push(b'\n');
        f
    }
}

async fn read_line(s: &mut TcpStream) -> String {
    let mut line = Vec::new();
    let mut b = [0_u8; 1];
    while s.read(&mut b).await.unwrap() == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    String::from_utf8(line).unwrap()
}

async fn drain(s: &mut TcpStream) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(
        control::EXCHANGE_DEADLINE + Duration::from_secs(15),
        s.read_to_end(&mut out),
    )
    .await;
    out
}

fn status_json() -> Vec<u8> {
    serde_json::to_vec(&Request::Status).unwrap()
}

#[tokio::test]
async fn a_correct_frame_is_answered_and_the_response_mac_binds_the_request() {
    let rig = Rig::start().await;
    let (mut s, nonce) = rig.connect().await;
    let body = status_json();
    s.write_all(&rig.frame(&nonce, &body)).await.unwrap();
    let reply = drain(&mut s).await;
    let line = &reply[..reply.len() - 1];
    let (mac, json) = ipc::split_frame(line).unwrap();
    let request_mac = rig.token.request_mac(&nonce, &body);
    assert_eq!(mac, rig.token.response_mac(&nonce, &request_mac, json));
    assert!(matches!(
        serde_json::from_slice::<Response>(json).unwrap(),
        Response::Status(_)
    ));
}

#[tokio::test]
async fn wrong_replayed_and_malformed_frames_get_only_unauthorized() {
    let rig = Rig::start().await;
    let body = status_json();

    // MAC made with another secret.
    let (mut s, nonce) = rig.connect().await;
    let other = Token::from_bytes([7; ipc::TOKEN_BYTES]);
    let mut f = ipc::encode_hex(&other.request_mac(&nonce, &body)).into_bytes();
    f.push(b' ');
    f.extend_from_slice(&body);
    f.push(b'\n');
    s.write_all(&f).await.unwrap();
    assert_eq!(drain(&mut s).await, b"ERR unauthorized\n");

    // A frame recorded from an earlier connection (valid there) replayed on a fresh nonce.
    let (mut first, first_nonce) = rig.connect().await;
    let recorded = rig.frame(&first_nonce, &body);
    first.write_all(&recorded).await.unwrap();
    assert!(drain(&mut first).await.len() > 100);
    let (mut second, second_nonce) = rig.connect().await;
    assert_ne!(first_nonce, second_nonce, "nonces are fresh per connection");
    second.write_all(&recorded).await.unwrap();
    assert_eq!(drain(&mut second).await, b"ERR unauthorized\n");

    // A valid MAC over a different body than the one sent (body swapped after signing).
    let (mut s, nonce) = rig.connect().await;
    let mut f = ipc::encode_hex(&rig.token.request_mac(&nonce, &body)).into_bytes();
    f.push(b' ');
    f.extend_from_slice(br#"{"type":"unpair","device_id":"x"}"#);
    f.push(b'\n');
    s.write_all(&f).await.unwrap();
    assert_eq!(drain(&mut s).await, b"ERR unauthorized\n");

    // Not a frame at all: short, no space, non-hex, non-UTF-8, empty line.
    for junk in [
        &b"\n"[..],
        b"hello\n",
        &[b'a'; 64][..],
        &[b'z'; 80][..],
        b"\xff\xfe\xfd\xfc\n",
    ] {
        let (mut s, _) = rig.connect().await;
        let mut line = junk.to_vec();
        if !line.ends_with(b"\n") {
            line.push(b'\n');
        }
        s.write_all(&line).await.unwrap();
        assert_eq!(drain(&mut s).await, b"ERR unauthorized\n", "{junk:?}");
    }
}

#[tokio::test]
async fn a_signed_but_unparseable_body_is_a_bad_request_not_a_crash() {
    let rig = Rig::start().await;
    for body in [
        &b"not json"[..],
        b"{}",
        br#"{"type":"no_such_request"}"#,
        b"\xff\xfe",
        b"null",
    ] {
        let (mut s, nonce) = rig.connect().await;
        s.write_all(&rig.frame(&nonce, body)).await.unwrap();
        let reply = drain(&mut s).await;
        let line = &reply[..reply.len() - 1];
        let (_, json) = ipc::split_frame(line).unwrap();
        match serde_json::from_slice::<Response>(json).unwrap() {
            Response::Error(e) => assert_eq!(e.code, ipc::ErrorCode::BadRequest),
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn bytes_after_the_newline_are_ignored_and_a_slow_client_is_cut_off() {
    let rig = Rig::start().await;
    let (mut s, nonce) = rig.connect().await;
    let mut f = rig.frame(&nonce, &status_json());
    f.extend_from_slice(b"trailing garbage that must be ignored\n");
    s.write_all(&f).await.unwrap();
    assert!(drain(&mut s).await.len() > 100);

    // Sends half a frame and then nothing: the exchange deadline closes it without a reply.
    let (mut slow, _) = rig.connect().await;
    slow.write_all(b"0123456789").await.unwrap();
    let started = std::time::Instant::now();
    assert!(drain(&mut slow).await.is_empty());
    let waited = started.elapsed();
    assert!(
        waited >= control::EXCHANGE_DEADLINE - Duration::from_secs(1)
            && waited < control::EXCHANGE_DEADLINE + Duration::from_secs(5),
        "closed by the deadline, took {waited:?}"
    );
}

#[tokio::test]
async fn idle_connections_beyond_the_cap_are_dropped_and_do_not_block_a_real_client() {
    let rig = Rig::start().await;
    let mut idle = Vec::new();
    for _ in 0..(control::MAX_CONNECTIONS * 3) {
        idle.push(TcpStream::connect(("127.0.0.1", rig.port)).await.unwrap());
    }
    // The first MAX_CONNECTIONS hold slots until the deadline; the rest are closed with no greeting.
    let mut closed_without_greeting = 0;
    for s in idle.iter_mut().skip(control::MAX_CONNECTIONS) {
        let mut buf = [0_u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(2), s.read(&mut buf))
            .await
            .map(|r| r.unwrap_or(0));
        if n == Ok(0) {
            closed_without_greeting += 1;
        }
    }
    assert!(
        closed_without_greeting >= control::MAX_CONNECTIONS,
        "{closed_without_greeting} extra connections were closed at once"
    );
    drop(idle);
    // Slots return once the clients are gone.
    let (port, token) = (rig.port, rig.token.clone());
    let status = tokio::time::timeout(
        control::EXCHANGE_DEADLINE + Duration::from_secs(15),
        tokio::task::spawn_blocking(move || {
            loop {
                if let Ok(s) = ipc::status(port, &token) {
                    return s;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }),
    )
    .await
    .expect("slots are released")
    .unwrap();
    assert!(status.links.is_empty());
}

#[tokio::test]
async fn an_unavailable_service_says_so_in_the_greeting_and_nothing_else() {
    let (port, task) = serve(Control::Unavailable(bridge_service::private::Unavailable {
        code: "state_locked",
        message: "another copy is running",
    }))
    .await;
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let hello = ipc::parse_hello(&read_line(&mut s).await).unwrap();
    assert_eq!(
        hello,
        Hello::Unavailable {
            code: "state_locked".into(),
            message: "another copy is running".into()
        }
    );
    // Even a signed-looking request gets nothing back.
    let _ = s.write_all(&[b'a'; 70]).await;
    assert!(drain(&mut s).await.is_empty());
    let err = tokio::task::spawn_blocking(move || {
        ipc::call(
            port,
            &Token::from_bytes([1; ipc::TOKEN_BYTES]),
            &Request::Status,
        )
    })
    .await
    .unwrap();
    assert!(matches!(err, Err(ipc::IpcError::Unavailable { .. })));
    task.abort();
}
