// SPDX-License-Identifier: GPL-3.0-or-later
//! Failure injection against the update checker: a server that returns garbage, drips bytes
//! forever (slow loris), never answers, loops redirects or resets the connection. Every case must
//! end in an error within the configured deadlines, leave no download behind, and leave the
//! updater usable for the next, well-behaved server.
//!
//! `GEEKBRIDGE_CHAOS=soak` repeats each case many times and watches the thread count.

use bridge_update::{Config, Platform, Updater};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const LATEST: &str = "/repos/fspecii/geekbridge/releases/latest";
const ASSET: &str = "geekbridge-linux-x86_64.tar.gz";

#[derive(Clone)]
enum B {
    /// A correct reply.
    Ok(Vec<u8>),
    /// Header bytes dripped one at a time, never finishing the head.
    LorisHead,
    /// Valid head promising 4 MiB, then one byte per tick.
    LorisBody,
    /// Accept and say nothing.
    Silent,
    /// 302 to itself.
    RedirectSelf,
    /// 302 to a new path every time.
    RedirectChain,
    /// Random bytes of the given length, then close.
    Garbage(usize),
    /// A header line that never ends.
    EndlessHeader,
    /// Close with RST right away.
    Reset,
    /// Chunked body with an absurd chunk size.
    BadChunk,
}

struct Server {
    port: u16,
    routes: Arc<Mutex<HashMap<String, B>>>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicUsize>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Server {
    fn start() -> Server {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, B>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let (r, s, a) = (routes.clone(), stop.clone(), active.clone());
        std::thread::spawn(move || {
            for conn in l.incoming() {
                if s.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let (r, s, a) = (r.clone(), s.clone(), a.clone());
                a.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    serve(conn, &r, &s);
                    a.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        Server {
            port,
            routes,
            stop,
            active,
        }
    }
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn set(&self, path: &str, b: B) {
        self.routes.lock().unwrap().insert(path.to_owned(), b);
    }
}

fn serve(mut c: TcpStream, routes: &Mutex<HashMap<String, B>>, stop: &AtomicBool) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut one = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match c.read(&mut one) {
            Ok(1) => buf.push(one[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let path = text.split(' ').nth(1).unwrap_or("/").to_owned();
    let b = routes
        .lock()
        .unwrap()
        .get(&path)
        .cloned()
        .or_else(|| path.starts_with("/hop").then_some(B::RedirectChain));
    let alive = || !stop.load(Ordering::SeqCst);
    match b {
        None => {
            let _ = c.write_all(b"HTTP/1.1 404 X\r\nContent-Length: 0\r\n\r\n");
        }
        Some(B::Ok(body)) => {
            let _ = c.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
            );
            let _ = c.write_all(&body);
        }
        Some(B::LorisHead) => {
            for b in
                b"HTTP/1.1 200 OK\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .iter()
                    .cycle()
            {
                if !alive() || c.write_all(&[*b]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Some(B::LorisBody) => {
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4194304\r\n\r\n");
            while alive() {
                if c.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        Some(B::Silent) => {
            while alive() {
                std::thread::sleep(Duration::from_millis(100));
                match c.read(&mut one) {
                    // Closed, or reset (Windows reports a peer's close as an error).
                    Ok(0) => return,
                    Err(e)
                        if !matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        return;
                    }
                    _ => {}
                }
            }
        }
        Some(B::RedirectSelf) => {
            let _ = c.write_all(
                format!("HTTP/1.1 302 X\r\nLocation: {path}\r\nContent-Length: 0\r\n\r\n")
                    .as_bytes(),
            );
        }
        Some(B::RedirectChain) => {
            let n: u64 = path.trim_start_matches("/hop").parse().unwrap_or(0);
            let _ = c.write_all(
                format!(
                    "HTTP/1.1 302 X\r\nLocation: /hop{}\r\nContent-Length: 0\r\n\r\n",
                    n + 1
                )
                .as_bytes(),
            );
        }
        Some(B::Garbage(n)) => {
            let mut x = 0x9e3779b97f4a7c15u64;
            let bytes: Vec<u8> = (0..n)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x as u8
                })
                .collect();
            let _ = c.write_all(&bytes);
        }
        Some(B::EndlessHeader) => {
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\nX-Long: ");
            let block = vec![b'a'; 8192];
            while alive() {
                if c.write_all(&block).is_err() {
                    return;
                }
            }
        }
        Some(B::Reset) => {
            let _ = socket2_reset(&c);
        }
        Some(B::BadChunk) => {
            let _ = c.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nFFFFFFFFFFFFFFFF\r\nabc",
            );
        }
    }
}

fn socket2_reset(c: &TcpStream) -> std::io::Result<()> {
    // SO_LINGER 0 through std: not available, so close after writing nothing; the client sees EOF.
    c.shutdown(std::net::Shutdown::Both)
}

fn sha_hex(data: &[u8]) -> String {
    bridge_core::ipc::encode_hex(&Sha256::digest(data))
}

fn release_json(base: &str, size: usize) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "tag_name": "v0.2.0",
        "html_url": format!("{base}/release/v0.2.0"),
        "body": "notes",
        "assets": [
            {"name": ASSET, "size": size, "browser_download_url": format!("{base}/dl/{ASSET}")},
            {"name": "SHA256SUMS", "size": 100, "browser_download_url": format!("{base}/dl/SHA256SUMS")},
        ],
    }))
    .unwrap()
}

struct Rig {
    server: Server,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            server: Server::start(),
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn updater(&self) -> Arc<Updater> {
        let mut cfg = Config::github("0.1.0");
        cfg.base_url = self.server.url();
        cfg.platform = Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        cfg.state_file = Some(self.dir.path().join("update.json"));
        let root = self.dir.path().join("dl");
        std::fs::create_dir_all(&root).unwrap();
        cfg.download_root = Some(root);
        cfg.io_timeout = Duration::from_millis(800);
        cfg.check_timeout = Duration::from_secs(3);
        cfg.download_timeout = Duration::from_secs(3);
        cfg.manual_cooldown = Duration::ZERO;
        Updater::new(cfg).unwrap()
    }
    fn good_release(&self, payload: &[u8]) {
        self.server.set(
            LATEST,
            B::Ok(release_json(&self.server.url(), payload.len())),
        );
        self.server
            .set(&format!("/dl/{ASSET}"), B::Ok(payload.to_vec()));
        self.server.set(
            "/dl/SHA256SUMS",
            B::Ok(format!("{}  {ASSET}\n", sha_hex(payload)).into_bytes()),
        );
    }
    fn downloads(&self) -> usize {
        std::fs::read_dir(self.dir.path().join("dl"))
            .unwrap()
            .count()
    }
}

/// Runs `body` on its own thread and fails the test, instead of wedging the CI job, if it has
/// not finished in `limit`. A panic inside `body` is re-raised here.
fn bounded(limit: Duration, body: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        body();
        let _ = tx.send(());
    });
    match rx.recv_timeout(limit) {
        Ok(()) => worker.join().unwrap(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            std::panic::resume_unwind(worker.join().unwrap_err())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            panic!("test body did not finish within {limit:?}")
        }
    }
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, |d| d.count())
}

fn bad_behaviours() -> Vec<(&'static str, B)> {
    vec![
        ("slow-loris head", B::LorisHead),
        ("slow-loris body", B::LorisBody),
        ("silent", B::Silent),
        ("redirect to self", B::RedirectSelf),
        ("endless redirect chain", B::RedirectChain),
        ("garbage", B::Garbage(100_000)),
        ("endless header", B::EndlessHeader),
        ("closed immediately", B::Reset),
        ("bad chunk size", B::BadChunk),
    ]
}

#[test]
fn a_misbehaving_api_server_fails_within_the_deadline_and_the_next_check_works() {
    bounded(Duration::from_secs(120), api_server_chaos);
}

fn api_server_chaos() {
    let rig = Rig::new();
    let up = rig.updater();
    let rounds = if std::env::var("GEEKBRIDGE_CHAOS").is_ok_and(|v| v == "soak") {
        20
    } else {
        1
    };
    let start_threads = threads();
    for round in 0..rounds {
        for (name, b) in bad_behaviours() {
            eprintln!("api chaos: {name} (round {round})");
            rig.server.set(LATEST, b);
            let t = Instant::now();
            let r = up.check(true);
            let took = t.elapsed();
            assert!(r.is_err(), "{name}: expected an error, got {r:?}");
            assert!(
                took < Duration::from_secs(6),
                "{name} round {round}: took {took:?} (check_timeout is 3 s)"
            );
            assert!(
                up.status().error.is_some(),
                "{name}: the failure must be visible in the status"
            );
        }
    }
    // A well-behaved server is accepted afterwards.
    rig.good_release(&[7u8; 1000]);
    up.check(true).unwrap();
    assert!(up.status().newer);
    // Let the misbehaving server threads notice their closed sockets.
    let end = Instant::now() + Duration::from_secs(10);
    while rig.server.active.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        threads() <= start_threads + 12,
        "threads {start_threads} -> {}",
        threads()
    );
}

#[test]
fn a_misbehaving_download_server_fails_within_the_deadline_and_leaves_nothing() {
    bounded(Duration::from_secs(120), download_server_chaos);
}

fn download_server_chaos() {
    let rig = Rig::new();
    let up = rig.updater();
    rig.good_release(&[9u8; 4096]);
    up.check(true).unwrap();
    for (name, b) in bad_behaviours() {
        eprintln!("download chaos: {name}");
        rig.server.set(&format!("/dl/{ASSET}"), b);
        let t = Instant::now();
        let r = up.download();
        let took = t.elapsed();
        assert!(r.is_err(), "{name}: expected an error, got {r:?}");
        assert!(
            took < Duration::from_secs(6),
            "{name}: took {took:?} (download_timeout is 3 s)"
        );
        assert_eq!(
            rig.downloads(),
            0,
            "{name}: a failed download must leave nothing"
        );
    }
    rig.good_release(&[9u8; 4096]);
    let path = up.download().unwrap();
    assert!(path.exists());
}
