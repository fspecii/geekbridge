// SPDX-License-Identifier: GPL-3.0-or-later
//! Refusals on the authenticated control API that `control_api.rs` does not reach: endpoint sanity, the cap on
//! concurrent links, duplicate dials, and challenge/ping requests naming things that do not exist. The tests
//! assert that the refusal is typed and leaves no link, challenge or trust record behind.

use bridge_core::{
    Snapshot,
    ipc::{self, ErrorCode, IpcError, Request, Response, Status, Token},
};
use bridge_kde::{self as kde, TrustStore as _};
use bridge_service::{
    SharedState,
    calling::CallingProbe,
    control::{self, Control},
    daemon::{DaemonOptions, MAX_LINKS},
    prepare_control,
};
use std::{path::PathBuf, sync::Arc};
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};

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
    state_dir: PathBuf,
    token: Token,
    port: u16,
    task: JoinHandle<std::io::Result<()>>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(NoProbe));
        options.dev_loopback = true;
        let control = prepare_control(&state_dir, discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open in a fresh temp dir");
        };
        let token = token.clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(control::serve_control(listener, Arc::new(control)));
        Self {
            _tmp: tmp,
            state_dir,
            token,
            port,
            task,
        }
    }

    async fn call(&self, request: Request) -> Result<Response, IpcError> {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
    }

    async fn refused(&self, request: Request) -> ErrorCode {
        match self.call(request).await {
            Err(IpcError::Service(e)) => e.code,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    async fn status(&self) -> Status {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::status(port, &token))
            .await
            .unwrap()
            .unwrap()
    }
}

fn device(n: usize) -> String {
    format!("{n:032x}")
}

fn at(port: u16) -> Option<ipc::Endpoint> {
    Some(ipc::Endpoint {
        address: "127.0.0.1".parse().unwrap(),
        port,
    })
}

#[tokio::test]
async fn port_zero_and_non_local_endpoints_are_refused_without_creating_a_link() {
    let rig = Rig::start().await;
    for (address, port) in [("127.0.0.1", 0_u16), ("8.8.8.8", 1716), ("0.0.0.0", 1716)] {
        let code = rig
            .refused(Request::Connect {
                device_id: device(1),
                endpoint: Some(ipc::Endpoint {
                    address: address.parse().unwrap(),
                    port,
                }),
            })
            .await;
        assert_eq!(code, ErrorCode::InvalidEndpoint, "{address}:{port}");
    }
    assert!(rig.status().await.links.is_empty());
}

#[tokio::test]
async fn a_malformed_device_id_is_refused_and_names_no_file_or_record() {
    let rig = Rig::start().await;
    for bad in ["", "short", "../../etc/passwd", &"z".repeat(300)] {
        let code = rig
            .refused(Request::Connect {
                device_id: bad.into(),
                endpoint: at(9),
            })
            .await;
        assert!(
            matches!(code, ErrorCode::BadRequest | ErrorCode::NotFound),
            "{bad:?}: {code:?}"
        );
    }
    assert!(rig.status().await.links.is_empty());
    assert!(
        kde::FileTrustStore::open(&rig.state_dir)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn unknown_challenges_and_unconnected_devices_are_refused_not_acted_on() {
    let rig = Rig::start().await;
    for id in ["nope", &"0".repeat(64), ""] {
        let a = rig
            .refused(Request::Approve {
                challenge_id: id.into(),
            })
            .await;
        let d = rig
            .refused(Request::Deny {
                challenge_id: id.into(),
            })
            .await;
        assert!(
            matches!(a, ErrorCode::NotFound | ErrorCode::BadRequest),
            "approve {id:?}: {a:?}"
        );
        assert!(
            matches!(d, ErrorCode::NotFound | ErrorCode::BadRequest),
            "deny {id:?}: {d:?}"
        );
    }
    let code = rig
        .refused(Request::Ping {
            device_id: device(7),
        })
        .await;
    assert_eq!(code, ErrorCode::NotConnected);
    assert!(
        kde::FileTrustStore::open(&rig.state_dir)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_link_table_is_capped_and_a_second_dial_to_one_device_is_busy() {
    let rig = Rig::start().await;
    // Listeners that accept at the TCP level but never speak: every dial stays in the connecting phase.
    let mut silent = Vec::new();
    for n in 0..MAX_LINKS {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let reply = rig
            .call(Request::Connect {
                device_id: device(n + 1),
                endpoint: at(port),
            })
            .await;
        assert!(matches!(reply, Ok(Response::Ack)), "dial {n}: {reply:?}");
        silent.push(l);
    }
    assert_eq!(rig.status().await.links.len(), MAX_LINKS);

    // The same device again: busy, not a second link.
    let port = silent[0].local_addr().unwrap().port();
    let code = rig
        .refused(Request::Connect {
            device_id: device(1),
            endpoint: at(port),
        })
        .await;
    assert_eq!(code, ErrorCode::Busy);

    // One more device than the table holds.
    let code = rig
        .refused(Request::Connect {
            device_id: device(MAX_LINKS + 1),
            endpoint: at(port),
        })
        .await;
    assert_eq!(code, ErrorCode::TooMany);
    assert_eq!(rig.status().await.links.len(), MAX_LINKS);
}

async fn read_greeting(s: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut line = Vec::new();
    let mut b = [0_u8; 1];
    while s.read(&mut b).await.unwrap() == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    String::from_utf8(line).unwrap()
}

/// Everything the service sends after `request`, until it closes the connection (bounded by a timeout).
async fn drain(s: &mut tokio::net::TcpStream) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut out)).await;
    out
}

#[tokio::test]
async fn a_client_that_hangs_up_mid_request_or_floods_one_line_gets_nothing_and_frees_its_slot() {
    use tokio::io::AsyncWriteExt;
    let rig = Rig::start().await;

    // Hang up halfway through the request line, many times over (more than the connection cap).
    for _ in 0..(bridge_service::control::MAX_CONNECTIONS * 2) {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", rig.port))
            .await
            .unwrap();
        assert!(read_greeting(&mut s).await.starts_with("GB-IPC "));
        s.write_all(b"deadbeef-not-a-complete-frame").await.unwrap();
        drop(s);
    }

    // One line far over the request cap, terminated by a newline: closed without a reply, nothing parsed.
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", rig.port))
        .await
        .unwrap();
    read_greeting(&mut s).await;
    let mut huge = vec![b'a'; 64 * 1024];
    huge.push(b'\n');
    let _ = s.write_all(&huge).await;
    let reply = drain(&mut s).await;
    assert!(
        reply.is_empty(),
        "an oversize request must get no answer, got {} bytes",
        reply.len()
    );

    // The same, but the newline arrives inside the first chunk after the cap was already passed.
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", rig.port))
        .await
        .unwrap();
    read_greeting(&mut s).await;
    let _ = s.write_all(&vec![b'b'; ipc::MAX_REQUEST_BYTES + 200]).await;
    let _ = s.write_all(b"\n").await;
    assert!(drain(&mut s).await.is_empty());

    // A well-formed, authenticated call still works: no slot leaked.
    assert!(rig.status().await.links.is_empty());
}
