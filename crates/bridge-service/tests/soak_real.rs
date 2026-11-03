// SPDX-License-Identifier: GPL-3.0-or-later
//! Long soak of the REAL `geekbridge-service` binary (not the in-process core) in a sandbox.
//!
//! Ignored by default; `scripts/soak-real.sh` runs it. The service gets a private state dir, a scratch
//! HOME, in-memory clipboard, no tray, no D-Bus and ports of its own, so the live service, the real
//! config and the default ports are never touched. A stand-in phone (a genuine `bridge_kde` endpoint
//! behind the `bridge-chaos` fault proxy) pairs, unpairs, reconnects through random network faults,
//! syncs the clipboard, moves files both ways and sends battery / notification / SMS / call packets,
//! while the control API is polled. Every `GEEKBRIDGE_SOAK_SAMPLE_SECS` the service's RSS, descriptors,
//! threads, context switches (wake-ups), CPU ticks and the state / log / save directory sizes are
//! appended to `<out>/samples.csv`; `scripts/soak-fit.py` fits slopes and decides pass or fail.
//!
//! Environment: `GEEKBRIDGE_SOAK_SECS` (default 3900), `GEEKBRIDGE_SOAK_OUT` (required, an empty dir
//! under the scratch area), `GEEKBRIDGE_SOAK_SEED`, `GEEKBRIDGE_SOAK_SAMPLE_SECS` (30).

mod support;

use bridge_core::{
    battery_link, calls_link, clipboard_wire,
    ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token},
    notification_link,
};
use bridge_kde::{self as kde, Packet, share as kshare};
use bridge_service::gate;
use serde_json::{Map, Value, json};
use std::{
    fmt::Write as _,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use support::chaos::*;
use support::*;
use tokio::{
    io::AsyncReadExt,
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};

const SHARE: &str = "kdeconnect.share.request";

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn obj(v: Value) -> Map<String, Value> {
    let Value::Object(m) = v else { unreachable!() };
    m
}

// ------------------------------------------------------------------ control API against the process

#[derive(Clone)]
struct Ctl {
    port: u16,
    token: Token,
}

impl Ctl {
    async fn call(&self, request: Request) -> Result<Response, ipc::IpcError> {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
    }
    async fn ok(&self, request: Request) -> bool {
        matches!(self.call(request).await, Ok(Response::Ack))
    }
    async fn status(&self) -> Option<ipc::Status> {
        match self.call(Request::Status).await {
            Ok(Response::Status(s)) => Some(*s),
            _ => None,
        }
    }
    async fn wait(&self, within: Duration, cond: impl Fn(&ipc::Status) -> bool) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if let Some(s) = self.status().await
                && cond(&s)
            {
                return true;
            }
            sleep(Duration::from_millis(20)).await;
        }
        false
    }
}

// ------------------------------------------------------------------ the stand-in phone

fn soak_phone() -> Arc<Phone> {
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    config.payload.any_port = true;
    config.payload.accept_timeout = Duration::from_secs(20);
    config.payload.idle_timeout = Duration::from_secs(20);
    // Mirror image of the service: the phone sends what the service accepts and vice versa, so the
    // link negotiates every packet family (and looks like a GeekBridge phone).
    let incoming: Vec<String> = gate::LOCAL_OUTGOING.iter().map(|s| (*s).into()).collect();
    let mut outgoing: Vec<String> = gate::LOCAL_INCOMING.iter().map(|s| (*s).into()).collect();
    outgoing.dedup();
    let endpoint = kde::Endpoint::new(
        creds.clone(),
        "Soak Phone",
        kde::DeviceType::Phone,
        incoming,
        outgoing,
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    Arc::new(Phone { creds, endpoint })
}

#[derive(Default)]
struct Stats {
    reconnect_ok: AtomicU64,
    reconnect_fail: AtomicU64,
    pairs: AtomicU64,
    pair_fail: AtomicU64,
    unpairs: AtomicU64,
    files_up: AtomicU64,
    files_down: AtomicU64,
    file_fail: AtomicU64,
    packets_sent: AtomicU64,
    packets_recv: AtomicU64,
    polls: AtomicU64,
    poll_fail: AtomicU64,
    bytes_moved: AtomicU64,
}

enum Cmd {
    Send(Packet),
    Offer { name: String, size: u64 },
    Unpair,
}

struct Pump {
    tx: mpsc::UnboundedSender<Cmd>,
    task: JoinHandle<()>,
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Owns the phone's side of one link: answers what the service asks, receives offered files into
/// the void, and executes commands from the driver.
fn spawn_pump(mut link: kde::TrustedLink, stats: Arc<Stats>) -> Pump {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let task = tokio::spawn(async move {
        let ctx = link.payload_context();
        loop {
            tokio::select! {
                ev = link.recv() => match ev {
                    Ok(kde::LinkEvent::Packet(p)) => {
                        stats.packets_recv.fetch_add(1, Relaxed);
                        if p.kind == battery_link::REQUEST {
                            let _ = link.send(&Packet::new(battery_link::BATTERY, battery_link::body(61, false, false))).await;
                        } else if p.kind == SHARE
                            && let Ok(kshare::ShareContent::File(o)) = kshare::parse_share(&p, 1 << 40)
                        {
                            let (ctx, stats) = (ctx.clone(), stats.clone());
                            tokio::spawn(async move {
                                let mut sink = tokio::io::sink();
                                let r = ctx.receive(o.port, o.size, 1 << 40, &mut sink, &mut |_| {}, &kde::CancelToken::new()).await;
                                match r {
                                    Ok(n) => { stats.files_down.fetch_add(1, Relaxed); stats.bytes_moved.fetch_add(n, Relaxed); }
                                    Err(_) => { stats.file_fail.fetch_add(1, Relaxed); }
                                }
                            });
                        }
                    }
                    Ok(kde::LinkEvent::PeerUnpaired) | Err(_) => break,
                    Ok(kde::LinkEvent::PeerRequestedRePair { .. }) => {}
                },
                cmd = rx.recv() => match cmd {
                    None => break,
                    Some(Cmd::Send(p)) => {
                        if link.send(&p).await.is_err() { break; }
                        stats.packets_sent.fetch_add(1, Relaxed);
                    }
                    Some(Cmd::Offer { name, size }) => {
                        let Ok(l) = ctx.listen().await else { stats.file_fail.fetch_add(1, Relaxed); continue };
                        let packet = kshare::file_packet(&name, size, None, l.port());
                        if link.send_with_payload(&packet).await.is_err() { break; }
                        let stats = stats.clone();
                        tokio::spawn(async move {
                            let src = tokio::io::repeat(0x5a).take(size);
                            match l.send(src, size, &mut |_| {}, &kde::CancelToken::new()).await {
                                Ok(n) => { stats.files_up.fetch_add(1, Relaxed); stats.bytes_moved.fetch_add(n, Relaxed); }
                                Err(_) => { stats.file_fail.fetch_add(1, Relaxed); }
                            }
                        });
                    }
                    Some(Cmd::Unpair) => { let _ = link.unpair().await; return; }
                }
            }
        }
    });
    Pump { tx, task }
}

/// Packets the phone sends in one burst: battery, notification (posted, sometimes cancelled), SMS,
/// call state ring -> idle, clipboard text.
fn burst(rng: &mut Rng, n: u64) -> Vec<Packet> {
    let mut out = Vec::new();
    let pct = (10 + rng.below(90)) as u8;
    out.push(Packet::new(
        battery_link::BATTERY,
        battery_link::body(pct, rng.below(2) == 0, pct < 15),
    ));
    let id = format!("0|soak|{}", n % 40);
    out.push(Packet::new(
        notification_link::NOTIFICATION,
        obj(json!({
            "id": id, "appName": "Chat", "title": format!("Alice {n}"), "text": "lunch?".repeat(1 + rng.below(8)),
            "time": "1700000000000", "isClearable": true, "silent": false, "onlyOnce": false,
            "actions": ["Reply", "Mute"],
        })),
    ));
    if rng.below(2) == 0 {
        out.push(Packet::new(
            notification_link::NOTIFICATION,
            obj(json!({"id": id, "isCancel": true})),
        ));
    }
    out.push(Packet::new(
        kde::sms::MESSAGES,
        obj(json!({"version": 2, "messages": [{
            "addresses": [{"address": "+15551230000"}], "body": format!("hi {n} 😀"),
            "date": 1700000000000u64 + n, "type": 1, "read": 0, "thread_id": 1 + (n % 5), "_id": n, "event": 1,
        }]})),
    ));
    out.push(Packet::new(
        calls_link::STATE,
        obj(
            json!({"state": "ringing", "number": "+15551230000", "name": "Alice",
                   "canAnswer": true, "canEnd": true, "canDial": false}),
        ),
    ));
    out.push(Packet::new(
        calls_link::STATE,
        obj(json!({"state": "idle", "canAnswer": false, "canEnd": false, "canDial": false})),
    ));
    let text = format!("clip {n} {}", "x".repeat(rng.below(4000)));
    out.push(Packet::new(
        clipboard_wire::CLIPBOARD,
        obj(json!({"content": text})),
    ));
    out
}

fn random_faults(rng: &mut Rng) -> (Faults, &'static str) {
    let pick = rng.below(8);
    let mut ms = |lo: usize, hi: usize| Duration::from_millis((lo + rng.below(hi - lo + 1)) as u64);
    match pick {
        0 => (Faults::default(), "clean"),
        1 => (
            Faults {
                latency: ms(20, 120),
                fragment: true,
                ..Faults::default()
            },
            "latency+fragment",
        ),
        2 => (
            Faults {
                bandwidth: Some(200_000 + rng.below(1_800_000) as u64),
                ..Faults::default()
            },
            "bandwidth",
        ),
        3 => (
            Faults {
                loss: 0.1,
                loss_penalty: ms(100, 400),
                latency: ms(5, 30),
                ..Faults::default()
            },
            "loss",
        ),
        4 => (
            Faults {
                drop_connection_prob: 0.02,
                ..Faults::default()
            },
            "drop-prob",
        ),
        5 => (
            Faults {
                reset_after_bytes: Some(20_000 + rng.below(250_000) as u64),
                ..Faults::default()
            },
            "reset-after-bytes",
        ),
        6 => (
            Faults {
                handshake_delay: ms(200, 900),
                ..Faults::default()
            },
            "slow-handshake",
        ),
        _ => (Faults::default(), "blackhole"),
    }
}

// ------------------------------------------------------------------ process sampling

fn proc_field(pid: u32, file: &str, key: &str) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/{file}"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// `Pss`, `Private_Dirty` and `Anonymous` (KiB) from `/proc/PID/smaps_rollup`: the heap-and-stack series that
/// code-page refaults (memtrim) do not move.
fn smaps_rollup(pid: u32) -> (u64, u64, u64) {
    (
        proc_field(pid, "smaps_rollup", "Pss:"),
        proc_field(pid, "smaps_rollup", "Private_Dirty:"),
        proc_field(pid, "smaps_rollup", "Anonymous:"),
    )
}

fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path).map_or_else(|e| format!("<{e}>"), |s| s.trim().to_owned())
}

/// What the service (and this driver) were doing when a control-API call timed out: host load average, the
/// service's process state and `wchan`, every thread's state / wchan / CPU ticks, the busiest threads, and the
/// same for the driver process. One multi-line block per timeout, appended to `poll-diag.log`.
fn stall_diagnostics(pid: u32) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "loadavg: {}", read_trim("/proc/loadavg"));
    let _ = writeln!(
        out,
        "pressure: cpu[{}] io[{}]",
        read_trim("/proc/pressure/cpu").replace('\n', " | "),
        read_trim("/proc/pressure/io").replace('\n', " | ")
    );
    for (label, p) in [("service", pid), ("driver", std::process::id())] {
        let proc_state = std::fs::read_to_string(format!("/proc/{p}/stat"))
            .ok()
            .and_then(|s| {
                s.rsplit(") ")
                    .next()
                    .and_then(|t| t.split_whitespace().next().map(str::to_owned))
            })
            .unwrap_or_else(|| "?".into());
        let _ = writeln!(
            out,
            "{label} pid {p}: state {proc_state} wchan {}",
            read_trim(&format!("/proc/{p}/wchan"))
        );
        let mut threads: Vec<(u64, String)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(format!("/proc/{p}/task")) {
            for t in rd.flatten() {
                let tid = t.file_name().to_string_lossy().into_owned();
                let base = format!("/proc/{p}/task/{tid}");
                let stat = std::fs::read_to_string(format!("{base}/stat")).unwrap_or_default();
                let name = stat
                    .find('(')
                    .zip(stat.rfind(')'))
                    .map_or("?", |(a, b)| &stat[a + 1..b])
                    .to_owned();
                let f: Vec<&str> = stat
                    .rsplit(") ")
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                let state = f.first().copied().unwrap_or("?");
                let ticks = f.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
                    + f.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                let wchan = read_trim(&format!("{base}/wchan"));
                threads.push((
                    ticks,
                    format!("  tid {tid} {name} state {state} wchan {wchan} cpu_ticks {ticks}"),
                ));
            }
        }
        threads.sort_by_key(|a| std::cmp::Reverse(a.0));
        let _ = writeln!(out, "{label} threads ({}), busiest first:", threads.len());
        for (_, l) in threads.iter().take(12) {
            let _ = writeln!(out, "{l}");
        }
    }
    out
}

fn count_dir(pid: u32, sub: &str) -> u64 {
    std::fs::read_dir(format!("/proc/{pid}/{sub}")).map_or(0, |d| d.count() as u64)
}

fn cpu_ticks(pid: u32) -> u64 {
    let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    let tail = s.rsplit(") ").next().unwrap_or("");
    let f: Vec<&str> = tail.split_whitespace().collect();
    // After the comm field: state is f[0]; utime f[11], stime f[12].
    f.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
        + f.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)
}

fn tree_size(dir: &Path) -> (u64, u64) {
    let (mut bytes, mut files) = (0, 0);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                bytes += m.len();
                files += 1;
            }
        }
    }
    (bytes, files)
}

struct Sampler {
    pid: u32,
    t0: Instant,
    state_dir: PathBuf,
    log: PathBuf,
    saves: PathBuf,
    csv: std::fs::File,
}

const HEADER: &str = "t_s,rss_kib,hwm_kib,threads,fds,ctx_vol,ctx_invol,cpu_ticks,state_bytes,state_files,log_bytes,saves_bytes,saves_files,transfers,events,notifications,links,paired,challenges,reconnect_ok,reconnect_fail,pairs,pair_fail,unpairs,files_up,files_down,file_fail,pkts_sent,pkts_recv,polls,poll_fail,bytes_moved,driver_fds,driver_threads,pss_kib,priv_dirty_kib,anon_kib,tag";

impl Sampler {
    async fn sample(&mut self, ctl: &Ctl, stats: &Stats, tag: &str) {
        let pid = self.pid;
        let (sb, sf) = tree_size(&self.state_dir);
        let (vb, vf) = tree_size(&self.saves);
        let log = std::fs::metadata(&self.log).map_or(0, |m| m.len());
        let transfers = match ctl.call(Request::ShareStatus).await {
            Ok(Response::Share(s)) => s.transfers.len(),
            _ => 0,
        };
        let events = match ctl.call(Request::Events { after: 0 }).await {
            Ok(Response::Events(p)) => p.events.len(),
            _ => 0,
        };
        let notifs = match ctl.call(Request::Notifications { device_id: None }).await {
            Ok(Response::Notifications(n)) => n.items.len(),
            _ => 0,
        };
        let (links, paired, challenges) = ctl.status().await.map_or((0, 0, 0), |s| {
            (s.links.len(), s.paired.len(), s.challenges.len())
        });
        let mut row = String::new();
        let _ = write!(
            row,
            "{:.1},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},",
            self.t0.elapsed().as_secs_f64(),
            proc_field(pid, "status", "VmRSS:"),
            proc_field(pid, "status", "VmHWM:"),
            proc_field(pid, "status", "Threads:"),
            count_dir(pid, "fd"),
            proc_field(pid, "status", "voluntary_ctxt_switches:"),
            proc_field(pid, "status", "nonvoluntary_ctxt_switches:"),
            cpu_ticks(pid),
            sb,
            sf,
            log,
            vb,
            vf,
            transfers,
            events,
            notifs,
            links,
            paired,
            challenges,
        );
        let smaps = smaps_rollup(pid);
        let g = |a: &AtomicU64| a.load(Relaxed);
        let _ = write!(
            row,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            g(&stats.reconnect_ok),
            g(&stats.reconnect_fail),
            g(&stats.pairs),
            g(&stats.pair_fail),
            g(&stats.unpairs),
            g(&stats.files_up),
            g(&stats.files_down),
            g(&stats.file_fail),
            g(&stats.packets_sent),
            g(&stats.packets_recv),
            g(&stats.polls),
            g(&stats.poll_fail),
            g(&stats.bytes_moved),
            fd_count(),
            thread_count(),
            smaps.0,
            smaps.1,
            smaps.2,
            tag,
        );
        let _ = writeln!(self.csv, "{row}");
        let _ = self.csv.flush();
    }
}

fn free_port() -> u16 {
    loop {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        if ![1716, 17381, 8000].contains(&p) && p > 20000 {
            return p;
        }
    }
}

fn spawn_service(out: &Path, state: &Path, home: &Path, ports: [u16; 4]) -> Child {
    let log = std::fs::File::create(out.join("service.log")).unwrap();
    let err = log.try_clone().unwrap();
    let [api, control, disc, inbound] = ports;
    // Fault injection for the service only (for example a shim that makes fsync slow).
    let preload = std::env::var("GEEKBRIDGE_SOAK_PRELOAD").ok();
    Command::new(env!("CARGO_BIN_EXE_geekbridge-service"))
        .env_clear()
        .envs(preload.as_deref().map(|lib| ("LD_PRELOAD", lib)))
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("TMPDIR", home.join("tmp"))
        .env("GEEKBRIDGE_CLIPBOARD", "memory")
        .env("GEEKBRIDGE_NO_UPDATE_CHECK", "1")
        .env("GEEKBRIDGE_NO_TRAY", "1")
        .arg("--state-dir")
        .arg(state)
        .args([
            "--api-port",
            &api.to_string(),
            "--control-port",
            &control.to_string(),
        ])
        .args(["--discovery-port", &disc.to_string()])
        .args(["--listen-inbound", &format!("127.0.0.1:{inbound}")])
        .args([
            "--no-calling-probe",
            "--no-announce",
            "--no-tray",
            "--dev-loopback",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .expect("start geekbridge-service")
}

// ------------------------------------------------------------------ the soak

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long; run through scripts/soak-real.sh"]
async fn soak_real_service() {
    let secs = env_u64("GEEKBRIDGE_SOAK_SECS", 3900);
    let sample_every = Duration::from_secs(env_u64("GEEKBRIDGE_SOAK_SAMPLE_SECS", 30));
    let out = PathBuf::from(std::env::var("GEEKBRIDGE_SOAK_OUT").expect("GEEKBRIDGE_SOAK_OUT"));
    let seed = env_u64("GEEKBRIDGE_SOAK_SEED", 0x50a4_0001);
    let mut rng = Rng(seed | 1);
    let home = out.join("home");
    let state = out.join("state");
    for d in ["run", "state", "config", "data", "cache", "tmp"] {
        std::fs::create_dir_all(home.join(d)).unwrap();
    }
    // The share plugin's save directory comes from XDG user dirs / ~/Downloads of the scratch HOME.
    let dl = home.join("Downloads").join("GeekBridge");
    std::fs::create_dir_all(&dl).unwrap();
    let ports = [free_port(), free_port(), free_port(), free_port()];
    let mut child = spawn_service(&out, &state, &home, ports);
    let pid = child.id();
    let token_path = ipc::token_path(&state);
    let end_wait = Instant::now() + Duration::from_secs(30);
    let token = loop {
        if let Ok(b) = std::fs::read(&token_path)
            && let Ok(t) = Token::parse(&b)
        {
            break t;
        }
        assert!(Instant::now() < end_wait, "service did not write its token");
        sleep(Duration::from_millis(50)).await;
    };
    let ctl = Ctl {
        port: ports[1],
        token,
    };
    assert!(
        ctl.wait(Duration::from_secs(30), |_| true).await,
        "control API never answered"
    );

    let stats = Arc::new(Stats::default());
    let mut csv = std::fs::File::create(out.join("samples.csv")).unwrap();
    writeln!(csv, "{HEADER}").unwrap();
    let t0 = Instant::now();
    let sampler = Sampler {
        pid,
        t0,
        state_dir: state.clone(),
        log: out.join("service.log"),
        saves: dl.clone(),
        csv,
    };
    let sampler = Arc::new(tokio::sync::Mutex::new(sampler));

    // Persistent phone behind the fault proxy.
    let p0 = soak_phone();
    let mut server = PhoneServer::start(&p0, Faults::default(), seed).await;
    let (link_tx, mut link_rx) = mpsc::unbounded_channel();
    let mut links = std::mem::replace(&mut server.links, mpsc::unbounded_channel().1);
    let acceptor = tokio::spawn(async move {
        while let Some(l) = links.recv().await {
            let _ = link_tx.send(l);
        }
    });
    let current: Arc<tokio::sync::Mutex<Option<Pump>>> = Arc::new(tokio::sync::Mutex::new(None));
    let adopt = {
        let (current, stats) = (current.clone(), stats.clone());
        tokio::spawn(async move {
            while let Some(l) = link_rx.recv().await {
                *current.lock().await = Some(spawn_pump(l, stats.clone()));
            }
        })
    };

    // Pair P0 once, through the proxy.
    assert!(
        ctl.ok(Request::Connect {
            device_id: p0.id(),
            endpoint: Some(server.endpoint())
        })
        .await
    );
    assert!(
        ctl.wait(Duration::from_secs(30), |s| link_of(s, &p0.id())
            .is_some_and(|l| l.phase == LinkPhase::Unpaired))
            .await,
        "no unpaired link"
    );
    assert!(
        ctl.ok(Request::Pair {
            device_id: p0.id(),
            mode: PairMode::Request
        })
        .await
    );
    assert!(
        ctl.wait(Duration::from_secs(30), |s| !s.challenges.is_empty())
            .await,
        "no challenge"
    );
    let cid = ctl.status().await.unwrap().challenges[0]
        .challenge_id
        .clone();
    assert!(ctl.ok(Request::Approve { challenge_id: cid }).await);
    assert!(
        ctl.wait(Duration::from_secs(30), |s| link_of(s, &p0.id())
            .is_some_and(|l| l.phase == LinkPhase::Connected))
            .await,
        "not connected"
    );
    for f in [
        Feature::Clipboard,
        Feature::Notifications,
        Feature::Battery,
        Feature::Sms,
        Feature::Telephony,
    ] {
        assert!(
            ctl.ok(Request::SetConsent {
                device_id: p0.id(),
                feature: f,
                enabled: true
            })
            .await
        );
    }
    assert!(
        ctl.ok(Request::ShareSettings {
            device_id: p0.id(),
            receive: Some(true),
            auto_accept: Some(true)
        })
        .await
    );
    stats.pairs.fetch_add(1, Relaxed);

    // Status polling task (the tray / Home / ctl all poll like this).
    let poll = {
        let (ctl, stats, poll_log) = (ctl.clone(), stats.clone(), out.join("poll-errors.log"));
        let diag_log = out.join("poll-diag.log");
        tokio::spawn(async move {
            let mut i = 0u64;
            loop {
                let request = match i % 10 {
                    0 => Request::Events { after: 0 },
                    3 => Request::ShareStatus,
                    5 => Request::ClipboardStatus,
                    7 => Request::Notifications { device_id: None },
                    _ => Request::Status,
                };
                let started = Instant::now();
                // A call still pending after 2 s gets a snapshot while it is stuck (the evidence), another one
                // if it ends in an error; a slow call that still succeeds is logged too.
                let call = ctl.call(request);
                tokio::pin!(call);
                let mut mid = None;
                let result = tokio::select! {
                    r = &mut call => r,
                    () = sleep(Duration::from_secs(2)) => {
                        mid = Some((t0.elapsed().as_secs_f64(), stall_diagnostics(pid)));
                        (&mut call).await
                    }
                };
                let ok = result.is_ok();
                if let Some((at, diag)) = &mid {
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&diag_log)
                        .and_then(|mut d| {
                            writeln!(
                                d,
                                "=== {at:.1}s after start: call pending after 2 s (ends {}, took {:?})\n{diag}",
                                if ok { "ok" } else { "in error" },
                                started.elapsed()
                            )
                        });
                }
                if let Err(e) = &result {
                    let diag = stall_diagnostics(pid);
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&diag_log)
                        .and_then(|mut d| {
                            writeln!(
                                d,
                                "=== {:.1}s after start: {e:?} (took {:?})\n{diag}",
                                t0.elapsed().as_secs_f64(),
                                started.elapsed()
                            )
                        });
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&poll_log)
                        .unwrap();
                    let _ = writeln!(
                        f,
                        "{:.1}s after start: {e:?} (took {:?})",
                        t0.elapsed().as_secs_f64(),
                        started.elapsed()
                    );
                }
                stats.polls.fetch_add(1, Relaxed);
                if !ok {
                    stats.poll_fail.fetch_add(1, Relaxed);
                }
                i += 1;
                sleep(Duration::from_millis(500)).await;
            }
        })
    };

    // Sampler task.
    let samp = {
        let (sampler, ctl, stats) = (sampler.clone(), ctl.clone(), stats.clone());
        tokio::spawn(async move {
            loop {
                sampler.lock().await.sample(&ctl, &stats, "run").await;
                sleep(sample_every).await;
            }
        })
    };

    // Scratch cleanup of received files (user data, not service state; sizes are sampled first).
    let scratch = out.join("send");
    std::fs::create_dir_all(&scratch).unwrap();

    let mut events_log = std::fs::File::create(out.join("events.tsv")).unwrap();
    let mut round = 0u64;
    let end = t0 + Duration::from_secs(secs);
    let mut died = false;
    while Instant::now() < end {
        round += 1;
        if child.try_wait().unwrap().is_some() {
            died = true;
            break;
        }
        // 1. packets from the phone
        if let Some(p) = current.lock().await.as_ref() {
            for pkt in burst(&mut rng, round) {
                let _ = p.tx.send(Cmd::Send(pkt));
            }
        }
        // 2. desktop-side clipboard traffic
        let _ = ctl.call(Request::ClipboardPush { device_id: None }).await;
        if round.is_multiple_of(3) {
            let _ = ctl.call(Request::ClipboardPull { device_id: None }).await;
        }
        // 3. files both ways
        if round.is_multiple_of(4) {
            let size = 50_000 + rng.below(3_000_000) as u64;
            if let Some(p) = current.lock().await.as_ref() {
                let _ = p.tx.send(Cmd::Offer {
                    name: format!("up-{round}.bin"),
                    size,
                });
            }
        }
        if round.is_multiple_of(5) {
            let f = scratch.join(format!("down-{round}.bin"));
            std::fs::write(&f, vec![0xa5u8; 20_000 + rng.below(2_000_000)]).unwrap();
            let r = ctl
                .call(Request::ShareSendFile {
                    device_id: p0.id(),
                    path: f.display().to_string(),
                })
                .await;
            if r.is_err() {
                stats.file_fail.fetch_add(1, Relaxed);
            }
            sleep(Duration::from_secs(2)).await;
            let _ = std::fs::remove_file(&f);
        }
        // 4. fault + reconnect
        if round % 4 == 2 {
            let (faults, what) = random_faults(&mut rng);
            let t = Instant::now();
            server.proxy.set(faults);
            *current.lock().await = None; // drops the pump task and the phone side of the link
            server.proxy.reset_all();
            sleep(Duration::from_millis(100 + rng.below(1500) as u64)).await;
            if what == "blackhole" {
                server.proxy.blackhole(true);
            }
            let mut ok = false;
            for attempt in 0..3 {
                if attempt == 1 {
                    server.proxy.blackhole(false);
                }
                if attempt == 2 {
                    server.proxy.set(Faults::default());
                }
                let _ = ctl.call(Request::Disconnect { device_id: p0.id() }).await;
                if !ctl
                    .ok(Request::Connect {
                        device_id: p0.id(),
                        endpoint: Some(server.endpoint()),
                    })
                    .await
                {
                    continue;
                }
                let within = Duration::from_secs(if attempt == 0 { 15 } else { 25 });
                if ctl
                    .wait(within, |s| {
                        link_of(s, &p0.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
                    })
                    .await
                {
                    ok = true;
                    break;
                }
            }
            server.proxy.blackhole(false);
            server.proxy.set(Faults::default());
            if ok {
                stats.reconnect_ok.fetch_add(1, Relaxed);
            } else {
                stats.reconnect_fail.fetch_add(1, Relaxed);
            }
            let _ = writeln!(
                events_log,
                "{:.1}\treconnect\t{what}\t{}\t{:.1}",
                t0.elapsed().as_secs_f64(),
                if ok { "ok" } else { "FAIL" },
                t.elapsed().as_secs_f64()
            );
        }
        // 5. pair / unpair a fresh identity
        if round.is_multiple_of(7) {
            let t = Instant::now();
            let phone = soak_phone();
            let faulty = rng.below(3) == 0;
            let (faults, _) = if faulty {
                random_faults(&mut rng)
            } else {
                (Faults::default(), "")
            };
            let mut srv = PhoneServer::start(&phone, faults, seed ^ round).await;
            let mut ok = false;
            if ctl
                .ok(Request::Connect {
                    device_id: phone.id(),
                    endpoint: Some(srv.endpoint()),
                })
                .await
                && ctl
                    .wait(Duration::from_secs(20), |s| {
                        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
                    })
                    .await
                && ctl
                    .ok(Request::Pair {
                        device_id: phone.id(),
                        mode: PairMode::Request,
                    })
                    .await
                && ctl
                    .wait(Duration::from_secs(20), |s| {
                        s.challenges.iter().any(|c| !c.challenge_id.is_empty())
                    })
                    .await
            {
                if let Some(c) = ctl
                    .status()
                    .await
                    .and_then(|s| s.challenges.first().map(|c| c.challenge_id.clone()))
                {
                    let _ = ctl.ok(Request::Approve { challenge_id: c }).await;
                }
                if let Ok(Some(link)) = timeout(Duration::from_secs(25), srv.links.recv()).await {
                    let pump = spawn_pump(link, stats.clone());
                    if ctl
                        .wait(Duration::from_secs(20), |s| {
                            link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
                        })
                        .await
                    {
                        ok = true;
                        for pkt in burst(&mut rng, round) {
                            let _ = pump.tx.send(Cmd::Send(pkt));
                        }
                        sleep(Duration::from_millis(600)).await;
                        if round.is_multiple_of(2) {
                            ctl.ok(Request::Unpair {
                                device_id: phone.id(),
                            })
                            .await;
                        } else {
                            let _ = pump.tx.send(Cmd::Unpair);
                        }
                        let id = phone.id();
                        ctl.wait(Duration::from_secs(20), |s| {
                            !s.paired.iter().any(|d| d.device_id == id)
                        })
                        .await;
                        stats.unpairs.fetch_add(1, Relaxed);
                    }
                    drop(pump);
                }
            }
            // Whatever happened, leave nothing paired or half-open for this identity.
            // Retried: a control call can fail while the service is stalled (see the report).
            let id = phone.id();
            for _ in 0..6 {
                let _ = ctl
                    .call(Request::Disconnect {
                        device_id: id.clone(),
                    })
                    .await;
                let _ = ctl
                    .call(Request::Unpair {
                        device_id: id.clone(),
                    })
                    .await;
                if ctl
                    .status()
                    .await
                    .is_some_and(|s| !s.paired.iter().any(|d| d.device_id == id))
                {
                    break;
                }
                sleep(Duration::from_secs(2)).await;
            }
            if let Some(s) = ctl.status().await {
                for c in s.challenges {
                    let _ = ctl
                        .call(Request::Deny {
                            challenge_id: c.challenge_id,
                        })
                        .await;
                }
            }
            if ok {
                stats.pairs.fetch_add(1, Relaxed);
            } else {
                stats.pair_fail.fetch_add(1, Relaxed);
            }
            let _ = writeln!(
                events_log,
                "{:.1}\tpair\t{}\t{}\t{:.1}",
                t0.elapsed().as_secs_f64(),
                if faulty { "faulty" } else { "clean" },
                if ok { "ok" } else { "FAIL" },
                t.elapsed().as_secs_f64()
            );
        }
        // 6. dismiss / clear what accumulated like a user would, and drop received files
        if round.is_multiple_of(6)
            && let Ok(Response::Share(s)) = ctl.call(Request::ShareStatus).await
        {
            {
                for t in s.transfers.iter().take(8) {
                    let _ = ctl
                        .call(Request::ShareDismiss {
                            transfer_id: t.transfer_id,
                        })
                        .await;
                }
            }
        }
        if round.is_multiple_of(10)
            && let Ok(rd) = std::fs::read_dir(&dl)
        {
            {
                for e in rd.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        sleep(Duration::from_millis(1500 + rng.below(2500) as u64)).await;
    }

    // Quiesce: stop the driver, keep P0 connected and idle, sample the settled state.
    poll.abort();
    let _ = writeln!(
        events_log,
        "{:.1}\tquiesce\t\t\t",
        t0.elapsed().as_secs_f64()
    );
    sleep(Duration::from_secs(20)).await;
    let _ = std::fs::remove_dir_all(&scratch);
    for _ in 0..4 {
        sampler.lock().await.sample(&ctl, &stats, "idle").await;
        sleep(sample_every).await;
    }
    samp.abort();
    let alive = child.try_wait().unwrap().is_none() && !died;
    acceptor.abort();
    adopt.abort();
    // Clean shutdown, then make sure nothing is left.
    let _ = unsafe_term(pid);
    for _ in 0..100 {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    std::fs::write(out.join("done"), if alive { "alive\n" } else { "DIED\n" }).unwrap();
    assert!(alive, "the service died during the soak; see service.log");
}

fn unsafe_term(pid: u32) -> std::io::Result<()> {
    Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map(|_| ())
}

#[test]
fn stall_diagnostics_and_smaps_are_readable() {
    let pid = std::process::id();
    let d = stall_diagnostics(pid);
    assert!(d.contains("loadavg: "), "{d}");
    assert!(d.contains(&format!("service pid {pid}: state ")), "{d}");
    assert!(d.contains("driver threads ("), "{d}");
    assert!(d.contains("wchan"), "{d}");
    let (pss, dirty, anon) = smaps_rollup(pid);
    assert!(pss > 0 && dirty > 0 && anon > 0, "{pss} {dirty} {anon}");
}
