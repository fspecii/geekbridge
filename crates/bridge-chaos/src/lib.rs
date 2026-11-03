// SPDX-License-Identifier: GPL-3.0-or-later
//! A small TCP fault proxy for failure-injection tests.
//!
//! `FaultProxy::start(target)` listens on a loopback port and forwards every connection to
//! `target`, applying the faults in [`Faults`]. The faults can be changed while connections are
//! open (`proxy.set(..)`), so a test can run a clean pairing and then degrade the link.
//!
//! TCP hides packet loss from the application (it retransmits), so "loss" here is modelled the way
//! an application sees it: a lost chunk is delivered one retransmission timeout late
//! (`loss_penalty`); `drop_connection_prob` kills the whole connection, like a NAT table flush.
//! Everything is deterministic for a given seed.
#![forbid(unsafe_code)]

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
    time::{Instant, sleep, sleep_until},
};

#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Added to every chunk, in each direction. Order is preserved.
    pub latency: Duration,
    /// Both directions, bytes per second. `None` is unlimited.
    pub bandwidth: Option<u64>,
    /// Probability (0..=1) that a chunk is "lost" and arrives `loss_penalty` late.
    pub loss: f64,
    pub loss_penalty: Duration,
    /// Probability (0..=1) per chunk that the connection is closed (FIN) on both sides.
    pub drop_connection_prob: f64,
    /// Reset (RST) both sides once this many bytes have crossed the connection, either way.
    pub reset_after_bytes: Option<u64>,
    /// Deliver data one byte per write, with TCP_NODELAY.
    pub fragment: bool,
    /// Hold the first bytes of every connection back this long (a slow TCP/TLS handshake).
    pub handshake_delay: Duration,
}

struct Shared {
    faults: Mutex<Faults>,
    /// Half-open: keep the sockets open and keep reading, but forward nothing and ACK everything.
    blackhole: AtomicBool,
    seed: AtomicU64,
    accepted: AtomicU64,
    live: AtomicU64,
    bytes: AtomicU64,
}

/// A proxied connection: its task, and its two sockets while it runs (so [`FaultProxy::reset_all`] can make the
/// close a reset). The connection empties `sockets` itself when it ends, so a finished one holds no descriptor.
type Conn = (JoinHandle<()>, Arc<Mutex<Vec<socket2::Socket>>>);

pub struct FaultProxy {
    addr: SocketAddr,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
    conns: Arc<Mutex<Vec<Conn>>>,
}

impl FaultProxy {
    pub async fn start(target: SocketAddr) -> Self {
        Self::start_with(target, Faults::default(), 1).await
    }

    pub async fn start_with(target: SocketAddr, faults: Faults, seed: u64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shared = Arc::new(Shared {
            faults: Mutex::new(faults),
            blackhole: AtomicBool::new(false),
            seed: AtomicU64::new(seed.max(1)),
            accepted: AtomicU64::new(0),
            live: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        });
        let conns: Arc<Mutex<Vec<Conn>>> = Arc::default();
        let (s, c) = (shared.clone(), conns.clone());
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let n = s.accepted.fetch_add(1, Ordering::Relaxed);
                let (s, mut c) = (s.clone(), c.lock().unwrap());
                c.retain(|(h, _)| !h.is_finished());
                let sockets: Arc<Mutex<Vec<socket2::Socket>>> = Arc::default();
                if let Ok(dup) = socket2::SockRef::from(&client).try_clone() {
                    sockets.lock().unwrap().push(dup);
                }
                let task = tokio::spawn(connection(client, target, s, n, sockets.clone()));
                c.push((task, sockets));
            }
        });
        Self {
            addr,
            shared,
            task,
            conns,
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn set(&self, faults: Faults) {
        *self.shared.faults.lock().unwrap() = faults;
    }

    pub fn update(&self, f: impl FnOnce(&mut Faults)) {
        f(&mut self.shared.faults.lock().unwrap());
    }

    /// Start or stop silently swallowing traffic (the link looks up but nothing flows).
    pub fn blackhole(&self, on: bool) {
        self.shared.blackhole.store(on, Ordering::SeqCst);
    }

    pub fn accepted(&self) -> u64 {
        self.shared.accepted.load(Ordering::Relaxed)
    }

    /// Connections currently being proxied.
    pub fn live(&self) -> u64 {
        self.shared.live.load(Ordering::Relaxed)
    }

    /// Bytes forwarded so far, both directions.
    pub fn forwarded(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    /// Abort every open connection at once (RST to both sides), keep listening.
    pub fn reset_all(&self) {
        for (h, sockets) in self.conns.lock().unwrap().drain(..) {
            for s in sockets.lock().unwrap().iter() {
                let _ = s.set_linger(Some(Duration::ZERO));
            }
            h.abort();
        }
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        self.task.abort();
        self.reset_all();
    }
}

struct Rng(u64);
impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct Live(Arc<Shared>);
impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::Relaxed);
    }
}

fn rst_on_drop(s: &TcpStream) {
    let _ = socket2::SockRef::from(s).set_linger(Some(Duration::ZERO));
}

/// Empties a connection's registered sockets when it ends, however it ends.
struct Forget(Arc<Mutex<Vec<socket2::Socket>>>);
impl Drop for Forget {
    fn drop(&mut self) {
        self.0.lock().unwrap().clear();
    }
}

async fn connection(
    client: TcpStream,
    target: SocketAddr,
    shared: Arc<Shared>,
    n: u64,
    sockets: Arc<Mutex<Vec<socket2::Socket>>>,
) {
    shared.live.fetch_add(1, Ordering::Relaxed);
    let _live = Live(shared.clone());
    let _forget = Forget(sockets.clone());
    let Ok(server) = TcpStream::connect(target).await else {
        rst_on_drop(&client);
        return;
    };
    if let Ok(dup) = socket2::SockRef::from(&server).try_clone() {
        sockets.lock().unwrap().push(dup);
    }
    let _ = client.set_nodelay(true);
    let _ = server.set_nodelay(true);
    let reset = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    // Dup'd descriptors keep the sockets alive until this function ends, so the linger option set
    // on them for a reset applies to the final close.
    let dups = [
        socket2::SockRef::from(&client).try_clone(),
        socket2::SockRef::from(&server).try_clone(),
    ];
    // `io::split`, not `into_split`: an owned write half shuts the socket down (FIN) when dropped, which would put a
    // clean close in front of an injected reset and the far end would read EOF instead of ECONNRESET.
    let (cr, cw) = tokio::io::split(client);
    let (sr, sw) = tokio::io::split(server);
    let seed = shared.seed.load(Ordering::Relaxed);
    let up = pump(
        cr,
        sw,
        shared.clone(),
        reset.clone(),
        seed ^ (n << 1),
        start,
    );
    let down = pump(
        sr,
        cw,
        shared.clone(),
        reset.clone(),
        seed ^ (n << 1 | 1) ^ 0x9e37,
        start,
    );
    // Either direction ending (EOF, error, injected drop) ends the whole connection.
    tokio::select! { _ = up => {}, _ = down => {} }
    if reset.load(Ordering::SeqCst) {
        for d in dups.iter().flatten() {
            let _ = d.set_linger(Some(Duration::ZERO));
        }
    }
}

struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Exit {
    Eof,
    Drop,
    Reset,
}

/// Reads from `r`, schedules each chunk for delivery, and a writer task delivers them in order.
async fn pump(
    mut r: tokio::io::ReadHalf<TcpStream>,
    mut w: tokio::io::WriteHalf<TcpStream>,
    shared: Arc<Shared>,
    reset: Arc<AtomicBool>,
    seed: u64,
    start: Instant,
) {
    let (tx, mut rx) = mpsc::channel::<(Instant, Vec<u8>)>(1024);
    let writer_shared = shared.clone();
    let mut writer = AbortOnDrop(tokio::spawn(async move {
        while let Some((due, chunk)) = rx.recv().await {
            sleep_until(due).await;
            // Swallowing starts now, also for chunks queued before it (a shaped link can hold
            // many seconds of backlog, longer still where timers are coarse, as on Windows).
            if writer_shared.blackhole.load(Ordering::SeqCst) {
                continue;
            }
            let f = writer_shared.faults.lock().unwrap().clone();
            if f.fragment {
                for b in &chunk {
                    if w.write_all(std::slice::from_ref(b)).await.is_err() {
                        return;
                    }
                }
            } else if w.write_all(&chunk).await.is_err() {
                return;
            }
            if let Some(bps) = f.bandwidth {
                sleep(Duration::from_secs_f64(
                    chunk.len() as f64 / bps.max(1) as f64,
                ))
                .await;
            }
        }
        let _ = w.shutdown().await;
    }));
    let mut rng = Rng(seed | 1);
    let mut buf = vec![0u8; 16 * 1024];
    let exit = loop {
        let n = match r.read(&mut buf).await {
            Ok(0) | Err(_) => {
                if shared.blackhole.load(Ordering::SeqCst) {
                    // Half-open: the far end's close is swallowed too.
                    std::future::pending::<()>().await;
                }
                break Exit::Eof;
            }
            Ok(n) => n,
        };
        if shared.blackhole.load(Ordering::SeqCst) {
            continue; // read (the kernel ACKs), forward nothing
        }
        let f = shared.faults.lock().unwrap().clone();
        if rng.unit() < f.drop_connection_prob {
            break Exit::Drop;
        }
        let total = shared.bytes.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        if f.reset_after_bytes.is_some_and(|limit| total >= limit) {
            break Exit::Reset;
        }
        let mut delay = f.latency;
        if start.elapsed() < f.handshake_delay {
            delay += f.handshake_delay - start.elapsed();
        }
        if rng.unit() < f.loss {
            delay += f.loss_penalty;
        }
        if tx
            .send((Instant::now() + delay, buf[..n].to_vec()))
            .await
            .is_err()
        {
            break Exit::Drop;
        }
    };
    match exit {
        Exit::Reset => reset.store(true, Ordering::SeqCst),
        Exit::Drop => {}
        Exit::Eof => {
            // Let queued chunks drain (generous bound; a slow consumer is not a fault) so a clean EOF is not truncated.
            drop(tx);
            let _ = tokio::time::timeout(Duration::from_secs(300), &mut writer.0).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An echo-less sink behind the proxy; returns the client side of one proxied connection and the server side.
    async fn pair(faults: Faults) -> (FaultProxy, TcpStream, TcpStream) {
        let sink = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = FaultProxy::start_with(sink.local_addr().unwrap(), faults, 7).await;
        let client = TcpStream::connect(proxy.addr()).await.unwrap();
        let (server, _) = sink.accept().await.unwrap();
        (proxy, client, server)
    }

    async fn read_outcome(s: &mut TcpStream) -> std::io::Result<usize> {
        let mut buf = [0u8; 64];
        tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf))
            .await
            .expect("the close must arrive")
    }

    /// A reset, not EOF. Windows may report a reset that races a local operation as "aborted".
    fn is_reset(r: &std::io::Result<usize>) -> bool {
        matches!(r, Err(e) if matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        ))
    }

    #[tokio::test]
    async fn reset_all_is_a_reset_on_both_sides_not_a_clean_close() {
        let (proxy, mut client, mut server) = pair(Faults::default()).await;
        client.write_all(b"x").await.unwrap();
        let mut one = [0u8; 1];
        server.read_exact(&mut one).await.unwrap();
        proxy.reset_all();
        let (c, s) = (
            read_outcome(&mut client).await,
            read_outcome(&mut server).await,
        );
        assert!(is_reset(&c), "client saw {c:?}");
        assert!(is_reset(&s), "server saw {s:?}");
    }

    #[tokio::test]
    async fn reset_after_bytes_is_a_reset_on_both_sides() {
        let faults = Faults {
            reset_after_bytes: Some(1),
            ..Faults::default()
        };
        let (_proxy, mut client, mut server) = pair(faults).await;
        client.write_all(b"xy").await.unwrap();
        let (c, s) = (
            read_outcome(&mut client).await,
            read_outcome(&mut server).await,
        );
        assert!(is_reset(&c), "client saw {c:?}");
        assert!(is_reset(&s), "server saw {s:?}");
    }

    #[tokio::test]
    async fn a_clean_close_still_arrives_as_eof_after_the_data() {
        let (_proxy, mut client, mut server) = pair(Faults::default()).await;
        client.write_all(b"bye").await.unwrap();
        client.shutdown().await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), server.read_to_end(&mut got))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, b"bye");
    }

    #[tokio::test]
    async fn blackhole_keeps_both_sides_open_and_silent() {
        let (proxy, mut client, mut server) = pair(Faults::default()).await;
        proxy.blackhole(true);
        client.write_all(b"lost").await.unwrap();
        drop(client); // the close is swallowed as well
        let mut buf = [0u8; 8];
        let r = tokio::time::timeout(Duration::from_millis(500), server.read(&mut buf)).await;
        assert!(r.is_err(), "the server must hear nothing, got {r:?}");
    }
}
