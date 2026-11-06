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

use bridge_callwifi::{
    codec::Encoder,
    selftest::Verdict,
    tone::Tone,
    wire::{self as cw, Dir, Frame, Msg, Offer, Ready, Uplink},
};
use bridge_core::{
    battery_link, calls_link, clipboard_wire,
    ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token},
    notification_link, photos_link,
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

/// `GEEKBRIDGE_SOAK_SKIP=a,b`: leave out parts of the load to see which one a trend belongs to (bursts, clip, files,
/// faults, pairs, sms, data, rings, wifi). `GEEKBRIDGE_SOAK_FAST=1` shortens the pause between rounds.
fn skip(name: &str) -> bool {
    std::env::var("GEEKBRIDGE_SOAK_SKIP").is_ok_and(|v| v.split(',').any(|w| w == name))
}

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
    // Feature rounds (GEEKBRIDGE_SOAK_FEATURES=1).
    rings: AtomicU64,
    answered: AtomicU64,
    declined: AtomicU64,
    missed: AtomicU64,
    ring_not_seen: AtomicU64,
    ring_retry: AtomicU64,
    ring_stuck: AtomicU64,
    card_missing: AtomicU64,
    card_orphan: AtomicU64,
    wifi_live: AtomicU64,
    wifi_fail: AtomicU64,
    pwcat_orphan: AtomicU64,
    feature_req: AtomicU64,
    polls_slow: AtomicU64,
    poll_max_ms: AtomicU64,
}

/// The stand-in phone's message history: 100 conversations of 200 messages (served 20 a page).
const HIST_THREADS: u64 = 100;
const HIST_DEPTH: u64 = 200;
const HIST_FIRST: i64 = 100;
static HIST_GEN: AtomicU64 = AtomicU64::new(0);

fn hist_date(epoch: u64, thread: u64, k: u64) -> u64 {
    1_700_000_000_000 + epoch * 50_000_000 + thread * 1_000 + (HIST_DEPTH - k) * 60_000
}

fn hist_msg(epoch: u64, thread: u64, k: u64) -> Value {
    json!({
        "addresses": [{"address": format!("+1555{:07}", thread)}], "body": format!("history {epoch}/{thread}/{k} hello there"),
        "date": hist_date(epoch, thread, k), "type": 1 + (k % 2), "read": 1,
        "thread_id": HIST_FIRST as u64 + thread, "_id": epoch * 1_000_000 + thread * 1_000 + k, "event": 1,
    })
}

enum Cmd {
    Send(Packet),
    Offer {
        name: String,
        size: u64,
    },
    Unpair,
    /// The handset's call state: "ringing", "offhook" or "idle".
    Call(&'static str),
}

fn call_packet(state: &str) -> Packet {
    Packet::new(
        calls_link::STATE,
        obj(
            json!({"state": state, "number": "+15551230000", "name": "Alice",
                   "canAnswer": true, "canEnd": true, "canDial": false}),
        ),
    )
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
        let features = std::env::var("GEEKBRIDGE_SOAK_FEATURES").is_ok_and(|v| v == "1");
        // A new history each link, so reconnects bring new messages instead of the ones already held.
        let epoch = HIST_GEN.fetch_add(1, Relaxed);
        let mut call = "idle";
        // (session, next seq) of a live Wi-Fi listen session.
        let mut live: Option<(u64, u32)> = None;
        let mut enc = Encoder::new().unwrap();
        let mut tone = Tone::new(440.0, 8000.0);
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        if features {
            let ready = Msg::Ready(Ready {
                ready: true,
                listen: true,
                speak: false,
                test: Verdict::Works,
            });
            let _ = link
                .send(&Packet::new(ready.kind(), cw::body(&ready)))
                .await;
            let _ = link.send(&call_packet("idle")).await;
        }
        loop {
            tokio::select! {
                _ = tick.tick(), if live.is_some() => {
                    if let Some((session, seq)) = live.as_mut() {
                        let opus = enc.encode(&tone.frame()).unwrap();
                        let f = Msg::Frame(Frame { session: *session, dir: Dir::Down, seq: *seq, ts: *seq * 320, cap: 0, opus });
                        *seq += 1;
                        if link.send(&Packet::new(f.kind(), cw::body(&f))).await.is_err() { break; }
                    }
                }
                ev = link.recv() => match ev {
                    Ok(kde::LinkEvent::Packet(p)) => {
                        stats.packets_recv.fetch_add(1, Relaxed);
                        if features && p.kind == calls_link::REQUEST {
                            if let Ok((id, action)) = calls_link::parse_request(&p.body) {
                                eprintln!("pump: {action:?} while {call}");
                                use calls_link::{LinkAction as A, LinkFailure};
                                let next = match (action, call) {
                                    (A::Answer, "ringing") => Ok(Some("offhook")),
                                    (A::Reject, "ringing") | (A::Hangup, "offhook") => Ok(Some("idle")),
                                    (A::Report, _) => Ok(None),
                                    _ => Err(LinkFailure::NoSuchCall),
                                };
                                if action == A::Report {
                                    let _ = link.send(&call_packet(call)).await;
                                } else {
                                    let ok = next.map(|_| ());
                                    let _ = link.send(&Packet::new(calls_link::RESULT, calls_link::result_body(&id, ok))).await;
                                    if let Ok(Some(n)) = next { call = n; let _ = link.send(&call_packet(n)).await; }
                                }
                            }
                        } else if features && p.kind == photos_link::REQUEST {
                            match photos_link::parse_request(&p.body) {
                                Ok(photos_link::Ask::List { .. }) => {
                                    let items = (0..6).map(|n| photos_link::Photo {
                                        id: format!("p{n}"), name: format!("IMG_{n:04}.jpg"),
                                        taken_ms: 1_700_000_000_000 + n * 1000, size: 5000 + n, width: 4000, height: 3000,
                                    }).collect();
                                    let list = photos_link::List { items, partial: false };
                                    let _ = link.send(&Packet::new(photos_link::LIST, photos_link::list_body(&list))).await;
                                }
                                Ok(photos_link::Ask::Thumb { id }) => {
                                    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
                                    jpeg.extend((0..200).map(|i| i as u8));
                                    jpeg.extend([0xFF, 0xD9]);
                                    let _ = link.send(&Packet::new(photos_link::THUMB, photos_link::thumb_body(&id, 160, 120, &jpeg))).await;
                                }
                                _ => {}
                            }
                        } else if features && cw::is_callaudio_kind(&p.kind) {
                            match cw::parse(&p.kind, &p.body) {
                                Ok(Msg::Request { session }) if call == "offhook" => {
                                    let m = Msg::Offer(Offer { session, down: true, up: false, uplink: Uplink::None });
                                    let _ = link.send(&Packet::new(m.kind(), cw::body(&m))).await;
                                }
                                Ok(Msg::Answer(a)) if a.accept => {
                                    live = Some((a.session, 0));
                                }
                                Ok(Msg::Stop { session, .. }) if live.is_some_and(|l| l.0 == session) => {
                                    live = None;
                                }
                                _ => {}
                            }
                        }
                        if features && p.kind == kde::sms::REQUEST_CONVERSATIONS {
                            // The thread list: the newest message of each of 100 conversations.
                            let msgs: Vec<Value> = (0..HIST_THREADS).map(|t| hist_msg(epoch, t, 0)).collect();
                            let _ = link.send(&Packet::new(kde::sms::MESSAGES, obj(json!({"version": 2, "messages": msgs})))).await;
                        } else if features && p.kind == kde::sms::REQUEST_CONVERSATION {
                            // One history page: up to 20 messages at or before the given date, newest first.
                            let tid = p.body.get("threadID").and_then(Value::as_i64).unwrap_or(0);
                            let start = p.body.get("rangeStartTimestamp").and_then(Value::as_u64);
                            let t = (tid - HIST_FIRST) as u64;
                            let msgs: Vec<Value> = (0..HIST_DEPTH)
                                .filter(|k| start.is_none_or(|s| hist_date(epoch, t, *k) <= s))
                                .take(20)
                                .map(|k| hist_msg(epoch, t, k))
                                .collect();
                            if tid >= HIST_FIRST && t < HIST_THREADS {
                                let _ = link.send(&Packet::new(kde::sms::MESSAGES, obj(json!({"version": 2, "messages": msgs})))).await;
                            }
                        }
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
                    Some(Cmd::Call(state)) => {
                        call = state;
                        if state != "offhook" { live = None; }
                        if link.send(&call_packet(state)).await.is_err() { break; }
                    }
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
    // With ring rounds running the call storms come from `ring_round` instead (see there).
    let ring_rounds =
        std::env::var("GEEKBRIDGE_SOAK_FEATURES").is_ok_and(|v| v == "1") && !skip("rings");
    if !ring_rounds {
        out.push(call_packet("ringing"));
        out.push(Packet::new(
            calls_link::STATE,
            obj(json!({"state": "idle", "canAnswer": false, "canEnd": false, "canDial": false})),
        ));
    }
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
                // What a blocked thread is doing (syscall number, then its arguments; x86-64: 1 write, 2 open,
                // 257 openat, 74 fsync, 75 fdatasync, 59 execve, 56 clone, 435 clone3, 82 rename).
                let sys = if state == "D" {
                    format!(" syscall [{}]", read_trim(&format!("{base}/syscall")))
                } else {
                    String::new()
                };
                threads.push((
                    ticks,
                    format!(
                        "  tid {tid} {name} state {state} wchan {wchan}{sys} cpu_ticks {ticks}"
                    ),
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

/// Direct children of the service: (pid, command line with NULs as spaces).
fn children(pid: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return out;
    };
    for t in tasks.flatten() {
        let Ok(c) = std::fs::read_to_string(t.path().join("children")) else {
            continue;
        };
        for c in c.split_whitespace().filter_map(|c| c.parse::<u32>().ok()) {
            let cmd = std::fs::read(format!("/proc/{c}/cmdline"))
                .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
                .unwrap_or_default();
            out.push((c, cmd));
        }
    }
    out
}
fn count_children(pid: u32, needle: &str) -> usize {
    children(pid)
        .iter()
        .filter(|(_, c)| c.contains(needle))
        .count()
}

struct Sampler {
    pid: u32,
    t0: Instant,
    state_dir: PathBuf,
    log: PathBuf,
    saves: PathBuf,
    csv: std::fs::File,
}

const HEADER: &str = "t_s,rss_kib,hwm_kib,threads,fds,ctx_vol,ctx_invol,cpu_ticks,state_bytes,state_files,log_bytes,saves_bytes,saves_files,transfers,events,notifications,links,paired,challenges,reconnect_ok,reconnect_fail,pairs,pair_fail,unpairs,files_up,files_down,file_fail,pkts_sent,pkts_recv,polls,poll_fail,bytes_moved,driver_fds,driver_threads,pss_kib,priv_dirty_kib,anon_kib,children,banners,cards,pwcat,err_lines,rings,answered,declined,missed,ring_not_seen,ring_retry,ring_stuck,card_missing,card_orphan,wifi_live,wifi_fail,pwcat_orphan,feature_req,polls_slow,poll_max_ms,tag";

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
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},",
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
        );
        row.truncate(row.rfind(',').map_or(row.len(), |i| i + 1));
        let kids = children(pid);
        let errs = std::fs::read_to_string(&self.log).map_or(0, |l| {
            l.lines()
                .filter(|l| {
                    let l = l.to_ascii_lowercase();
                    l.contains("error") || l.contains("panic") || l.contains("failed")
                })
                .count()
        });
        let _ = write!(
            row,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            kids.iter()
                .filter(|(_, c)| !c.starts_with("notify-send"))
                .count(),
            kids.iter()
                .filter(|(_, c)| c.starts_with("notify-send"))
                .count(),
            kids.iter()
                .filter(|(_, c)| c.contains("geekbridge-calls"))
                .count(),
            kids.iter().filter(|(_, c)| c.contains("pw-cat")).count(),
            errs,
            g(&stats.rings),
            g(&stats.answered),
            g(&stats.declined),
            g(&stats.missed),
            g(&stats.ring_not_seen),
            g(&stats.ring_retry),
            g(&stats.ring_stuck),
            g(&stats.card_missing),
            g(&stats.card_orphan),
            g(&stats.wifi_live),
            g(&stats.wifi_fail),
            g(&stats.pwcat_orphan),
            g(&stats.feature_req),
            g(&stats.polls_slow),
            stats.poll_max_ms.swap(0, Relaxed),
            tag,
        );
        let _ = writeln!(self.csv, "{row}");
        let _ = self.csv.flush();
    }
}

static PORTS_TAKEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());

fn free_port() -> u16 {
    // `GEEKBRIDGE_SOAK_PORT_BASE=27000`: pick from base..base+999 instead of the kernel's ephemeral range.
    if let Some(base) = std::env::var("GEEKBRIDGE_SOAK_PORT_BASE")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
    {
        for p in base..base + 1000 {
            if let Ok(l) = std::net::TcpListener::bind(("127.0.0.1", p))
                && std::net::UdpSocket::bind(("127.0.0.1", p)).is_ok()
            {
                drop(l);
                if !PORTS_TAKEN.lock().unwrap().contains(&p) {
                    PORTS_TAKEN.lock().unwrap().push(p);
                    return p;
                }
            }
        }
        panic!("no free port in {base}..+1000");
    }
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
    let features = std::env::var("GEEKBRIDGE_SOAK_FEATURES").is_ok_and(|v| v == "1");
    // Feature mode: the service runs from a copy of the same binary with a stand-in `geekbridge-calls` beside
    // it (the real card needs a display; the stub only has to live and die like one), a fake DISPLAY so the
    // overlay launcher proceeds, and, when the script started a private PipeWire, that runtime dir for pw-cat.
    let mut exe = PathBuf::from(env!("CARGO_BIN_EXE_geekbridge-service"));
    let mut runtime = home.join("run");
    if features {
        let bin = out.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let copy = bin.join("geekbridge-service");
        std::fs::copy(&exe, &copy).unwrap();
        let stub = bin.join("geekbridge-calls");
        std::fs::write(
            &stub,
            "#!/bin/sh\nexec -a geekbridge-calls-stub sleep 600\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Do Not Disturb probe: a `makoctl` that reads a flag file the driver toggles.
        let mako = bin.join("makoctl");
        std::fs::write(
            &mako,
            format!(
                "#!/bin/sh\nif [ -e '{}' ]; then echo do-not-disturb; else echo default; fi\n",
                bin.join("dnd.flag").display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&mako, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        exe = copy;
        if let Ok(r) = std::env::var("GEEKBRIDGE_SOAK_AUDIO_RUN") {
            runtime = PathBuf::from(r);
        }
    }
    let mut cmd = Command::new(&exe);
    cmd.env_clear();
    if features {
        cmd.env("GEEKBRIDGE_CALL_OVERLAY", "on")
            .env("DISPLAY", ":99");
        // No notification daemon in the sandbox: a bus address that refuses at once keeps notify-send from
        // autolaunching a bus and waiting on it.
        cmd.env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/no-bus", runtime.display()),
        );
        if std::env::var("GEEKBRIDGE_SOAK_AUDIO_RUN").is_ok() {
            cmd.env(
                "PULSE_SERVER",
                format!("unix:{}/pulse/native", runtime.display()),
            )
            .env("PIPEWIRE_RUNTIME_DIR", &runtime);
        }
    }
    cmd.envs(preload.as_deref().map(|lib| ("LD_PRELOAD", lib)))
        .env(
            "PATH",
            if features {
                format!("{}:/usr/bin:/bin", out.join("bin").display())
            } else {
                "/usr/bin:/bin".to_string()
            },
        )
        .envs(
            std::env::var("FSYNC_SHIM_LOG")
                .ok()
                .map(|l| ("FSYNC_SHIM_LOG", l)),
        )
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("TMPDIR", home.join("tmp"))
        .env("GEEKBRIDGE_CLIPBOARD", "memory")
        .envs(match std::env::var("GEEKBRIDGE_SOAK_UPDATE_URL") {
            Ok(u) => vec![("GEEKBRIDGE_UPDATE_BASE_URL", u)],
            Err(_) => vec![("GEEKBRIDGE_NO_UPDATE_CHECK", "1".to_string())],
        })
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

// ------------------------------------------------------------------ feature rounds

async fn set_consents(ctl: &Ctl, id: &str, features: bool) -> bool {
    let mut all = vec![
        Feature::Clipboard,
        Feature::Notifications,
        Feature::Battery,
        Feature::Sms,
        Feature::Telephony,
    ];
    if features {
        all.extend([Feature::Contacts, Feature::Photos, Feature::CallAudioWifi]);
    }
    for f in all {
        if !ctl
            .ok(Request::SetConsent {
                device_id: id.into(),
                feature: f,
                enabled: true,
            })
            .await
        {
            return false;
        }
    }
    ctl.ok(Request::ShareSettings {
        device_id: id.into(),
        receive: Some(true),
        auto_accept: Some(true),
    })
    .await
}

struct Feat<'a> {
    ctl: &'a Ctl,
    stats: &'a Arc<Stats>,
    current: &'a Arc<tokio::sync::Mutex<Option<Pump>>>,
    pid: u32,
    scratch: &'a Path,
    t0: Instant,
}

impl Feat<'_> {
    async fn send(&self, cmd: Cmd) {
        if let Some(p) = self.current.lock().await.as_ref() {
            let _ = p.tx.send(cmd);
        }
    }
    /// Press the card's button: the target comes from the latest `Calls` view, as in the apps.
    async fn act(&self, action: ipc::CallAction) {
        let Some(c) = self.calls().await else { return };
        let Some(row) = c.view.rows.first() else {
            return;
        };
        let target = match action {
            ipc::CallAction::Answer => row.answer.clone(),
            ipc::CallAction::Reject => row.reject.clone(),
            ipc::CallAction::Hangup => row.hangup.clone(),
        };
        if let Some(target) = target {
            let r = self.ctl.call(Request::CallAction { target, action }).await;
            eprintln!(
                "act {action:?}: {}",
                if matches!(r, Ok(Response::Ack)) {
                    "ack".to_string()
                } else {
                    format!("{r:?}")
                }
            );
        } else {
            eprintln!("act {action:?}: no target on the row");
        }
    }
    /// Whether the stub `makoctl` currently says Do Not Disturb.
    fn dnd_on(&self) -> bool {
        self.scratch
            .parent()
            .is_some_and(|d| d.join("bin").join("dnd.flag").exists())
    }
    async fn calls(&self) -> Option<ipc::CallsStatus> {
        match self.ctl.call(Request::Calls).await {
            Ok(Response::Calls(c)) => Some(*c),
            _ => None,
        }
    }
    async fn wait_calls(&self, within: Duration, cond: impl Fn(&ipc::CallsStatus) -> bool) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if self.calls().await.is_some_and(|c| cond(&c)) {
                return true;
            }
            sleep(Duration::from_millis(100)).await;
        }
        false
    }
    async fn wait_children(
        &self,
        needle: &str,
        within: Duration,
        cond: impl Fn(usize) -> bool,
    ) -> bool {
        let end = Instant::now() + within;
        loop {
            if cond(count_children(self.pid, needle)) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            sleep(Duration::from_millis(100)).await;
        }
    }
    async fn wifi(
        &self,
        action: bridge_core::call_wifi_view::CallWifiAction,
    ) -> Option<bridge_core::call_wifi_view::CallWifiView> {
        match self.ctl.call(Request::CallWifi { action }).await {
            Ok(Response::CallWifi(v)) => Some(*v),
            _ => None,
        }
    }
}

/// One ring, played out one of five ways, and the checks a user would care about afterwards.
async fn ring_round(
    f: &Feat<'_>,
    rng: &mut Rng,
    server: &mut PhoneServer,
    p0: &Arc<Phone>,
    ev: &mut std::fs::File,
    during_transfer: bool,
) {
    use bridge_core::call_wifi_view::CallWifiAction as W;
    let kind = rng.below(6);
    let what = [
        "answer-pc",
        "decline-pc",
        "answer-phone",
        "miss",
        "drop-link",
        "decline-msg",
    ][kind];
    let st = f.stats;
    st.rings.fetch_add(1, Relaxed);
    if during_transfer {
        let file = f.scratch.join("call-xfer.bin");
        let _ = std::fs::write(&file, vec![0x3cu8; 24_000_000]);
        let _ = f
            .ctl
            .call(Request::ShareSendFile {
                device_id: p0.id(),
                path: file.display().to_string(),
            })
            .await;
    }
    // A call event storm first, ordered with the ring itself: the phone flips ringing / idle quickly, then settles.
    // (Raw call packets from the burst generator are left out while rings run: they cannot be told apart from the
    // ring under test and made the driver act on a ring that was not its own.)
    for _ in 0..8 {
        f.send(Cmd::Send(call_packet("ringing"))).await;
        f.send(Cmd::Send(call_packet("idle"))).await;
    }
    f.send(Cmd::Call("idle")).await;
    sleep(Duration::from_millis(300)).await;
    f.send(Cmd::Call("ringing")).await;
    let mut seen = f
        .wait_calls(Duration::from_secs(6), |c| c.view.ringing().is_some())
        .await;
    if !seen {
        // Evidence first (what the service says about its call sources), then one more ring: a ring that is
        // missed once and seen the second time is logged as a retry, one missed twice is a failure.
        let why = f
            .calls()
            .await
            .map(|c| c.sources.join(" | "))
            .unwrap_or_default();
        let _ = writeln!(
            ev,
            "{:.1}\tring-missed\t{why}\t\t",
            f.t0.elapsed().as_secs_f64()
        );
        st.ring_retry.fetch_add(1, Relaxed);
        f.send(Cmd::Call("idle")).await;
        sleep(Duration::from_secs(1)).await;
        f.send(Cmd::Call("ringing")).await;
        seen = f
            .wait_calls(Duration::from_secs(6), |c| c.view.ringing().is_some())
            .await;
    }
    if !seen {
        st.ring_not_seen.fetch_add(1, Relaxed);
    } else if !f.dnd_on()
        && !f
            .wait_children("geekbridge-calls", Duration::from_secs(5), |n| n >= 1)
            .await
    {
        // (With Do Not Disturb on the service keeps the ring quiet: no card, by design.)
        st.card_missing.fetch_add(1, Relaxed);
    }
    let mut wifi_used = false;
    match kind {
        5 if seen => {
            // Decline with a saved text; if the service refuses (no Messages path) fall back to a plain decline.
            let target = f
                .calls()
                .await
                .and_then(|c| c.view.rows.first().and_then(|r| r.reject.clone()));
            let mut sent = false;
            if let Some(target) = target {
                sent = matches!(
                    f.ctl
                        .call(Request::CallDeclineReply { target, reply: 0 })
                        .await,
                    Ok(Response::Ack)
                );
            }
            if !sent {
                f.act(ipc::CallAction::Reject).await;
            }
            st.declined.fetch_add(1, Relaxed);
        }
        5 => {
            st.missed.fetch_add(1, Relaxed);
        }
        0 | 1 if seen => {
            let a = if kind == 0 {
                ipc::CallAction::Answer
            } else {
                ipc::CallAction::Reject
            };
            f.act(a).await;
            if kind == 1 {
                st.declined.fetch_add(1, Relaxed);
            } else {
                st.answered.fetch_add(1, Relaxed);
            }
        }
        2 => {
            f.send(Cmd::Call("offhook")).await;
            st.answered.fetch_add(1, Relaxed);
        }
        3 => {
            sleep(Duration::from_millis(1500 + rng.below(2500) as u64)).await;
            f.send(Cmd::Call("idle")).await;
            st.missed.fetch_add(1, Relaxed);
        }
        4 => {
            sleep(Duration::from_millis(300 + rng.below(1500) as u64)).await;
            *f.current.lock().await = None;
            server.proxy.reset_all();
            sleep(Duration::from_millis(200 + rng.below(1500) as u64)).await;
            let _ = f.ctl.call(Request::Disconnect { device_id: p0.id() }).await;
            let _ = f
                .ctl
                .ok(Request::Connect {
                    device_id: p0.id(),
                    endpoint: Some(server.endpoint()),
                })
                .await;
            f.ctl
                .wait(Duration::from_secs(25), |s| {
                    link_of(s, &p0.id()).is_some_and(|l| l.phase == LinkPhase::Connected)
                })
                .await;
            st.missed.fetch_add(1, Relaxed);
        }
        _ => {}
    }
    // In a call: sometimes listen over Wi-Fi for a while.
    if matches!(kind, 0 | 2) && !skip("wifi") && rng.below(10) < 7 {
        if f.wait_calls(Duration::from_secs(6), |c| c.view.live().is_some())
            .await
        {
            f.send(Cmd::Call("offhook")).await; // the phone's own state after an answer from here
            let _ = f.wifi(W::Listen { device_id: p0.id() }).await;
            wifi_used = true;
            let ok = {
                let end = Instant::now() + Duration::from_secs(8);
                let mut live = false;
                while Instant::now() < end && !live {
                    live = f
                        .wifi(W::Status)
                        .await
                        .and_then(|v| v.session)
                        .is_some_and(|s| s.state == "live");
                    sleep(Duration::from_millis(100)).await;
                }
                live
            };
            if ok {
                st.wifi_live.fetch_add(1, Relaxed);
            } else {
                st.wifi_fail.fetch_add(1, Relaxed);
            }
            sleep(Duration::from_millis(800 + rng.below(2500) as u64)).await;
            if rng.below(3) > 0 {
                let _ = f.wifi(W::Stop).await;
            } // else the call ending ends it
        } else {
            sleep(Duration::from_millis(500)).await;
        }
    } else if matches!(kind, 0 | 2) {
        sleep(Duration::from_millis(500 + rng.below(2000) as u64)).await;
    }
    if matches!(kind, 0 | 2) {
        if kind == 0 && rng.below(2) == 0 {
            f.act(ipc::CallAction::Hangup).await;
        } else {
            f.send(Cmd::Call("idle")).await;
        }
    }
    let t = Instant::now();
    let cleared = f
        .wait_calls(Duration::from_secs(15), |c| c.view.rows.is_empty())
        .await;
    if !cleared {
        st.ring_stuck.fetch_add(1, Relaxed);
        // What the call list still shows: rows, sources, and the trace tail of the service.
        if let Ok(Response::CallTrace { events }) =
            f.ctl.call(Request::CallTrace { clear: false }).await
        {
            let tail = events.len().saturating_sub(60);
            let text: String = events[tail..]
                .iter()
                .map(|e| format!("{}\t{}\t{}\n", e.ms, e.kind, e.detail))
                .collect();
            let _ = std::fs::write(
                f.scratch
                    .join(format!("stuck-{:.0}.trace", f.t0.elapsed().as_secs_f64())),
                text,
            );
        }
        if let Some(c) = f.calls().await {
            let _ = std::fs::write(
                f.scratch
                    .join(format!("stuck-{:.0}.json", f.t0.elapsed().as_secs_f64())),
                format!("{c:#?}"),
            );
        }
    }
    let cards_gone = f
        .wait_children("geekbridge-calls", Duration::from_secs(10), |n| n == 0)
        .await;
    if !cards_gone {
        st.card_orphan.fetch_add(1, Relaxed);
    }
    let mut pw_ok = true;
    if wifi_used {
        pw_ok = f
            .wait_children("pw-cat", Duration::from_secs(8), |n| n == 0)
            .await;
        if !pw_ok {
            st.pwcat_orphan.fetch_add(1, Relaxed);
        }
        if f.wifi(W::Status).await.is_some_and(|v| v.session.is_some()) {
            let _ = f.wifi(W::Stop).await;
            st.pwcat_orphan.fetch_add(1, Relaxed);
        }
    }
    let _ = writeln!(
        ev,
        "{:.1}\tring\t{what}{}{}\t{}\t{:.1}",
        f.t0.elapsed().as_secs_f64(),
        if during_transfer { "+transfer" } else { "" },
        if wifi_used { "+wifi" } else { "" },
        if seen && cleared && cards_gone && pw_ok {
            "ok"
        } else {
            "FAIL"
        },
        t.elapsed().as_secs_f64()
    );
    let _ = std::fs::remove_file(f.scratch.join("call-xfer.bin"));
}

async fn data_requests(f: &Feat<'_>, rng: &mut Rng, p0: &Arc<Phone>) {
    use bridge_core::privacy::PrivacyKey;
    let id = p0.id();
    let reqs = [
        Request::PhotosRefresh {
            device_id: id.clone(),
        },
        Request::Photos { device_id: None },
        Request::PhotoThumb {
            device_id: id.clone(),
            id: "p1".into(),
        },
        Request::AgendaRefresh { device_id: None },
        Request::AgendaView {
            query: String::new(),
            day: "2026-11-04".into(),
            utc_offset_minutes: 0,
            days: 7,
            people_limit: 50,
        },
        Request::MessagesRefresh {
            device_id: id.clone(),
            thread_id: None,
        },
        Request::MessagesView {
            device_id: None,
            thread_id: Some(1),
            query: None,
            message_limit: 20,
        },
        Request::PrivacyStatus,
    ];
    // Message history burst: the thread list, then every conversation asked for older pages (100 x 20 and more).
    let _ = f
        .ctl
        .call(Request::MessagesRefresh {
            device_id: id.clone(),
            thread_id: None,
        })
        .await;
    sleep(Duration::from_millis(300)).await;
    for t in 0..HIST_THREADS as i64 {
        let _ = f
            .ctl
            .call(Request::MessagesRefresh {
                device_id: id.clone(),
                thread_id: Some(HIST_FIRST + t),
            })
            .await;
    }
    f.stats.feature_req.fetch_add(HIST_THREADS, Relaxed);
    // Update checks against the local mock release server.
    if std::env::var("GEEKBRIDGE_SOAK_UPDATE_URL").is_ok() {
        for _ in 0..2 {
            let t = Instant::now();
            let _ = f.ctl.call(Request::UpdateCheck).await;
            let _ = f.ctl.call(Request::UpdateStatus).await;
            let ms = t.elapsed().as_millis() as u64;
            f.stats.poll_max_ms.fetch_max(ms, Relaxed);
            if ms > 1000 {
                f.stats.polls_slow.fetch_add(1, Relaxed);
            }
        }
    }
    for r in reqs {
        let t = Instant::now();
        let _ = f.ctl.call(r).await;
        f.stats.feature_req.fetch_add(1, Relaxed);
        let ms = t.elapsed().as_millis() as u64;
        f.stats.poll_max_ms.fetch_max(ms, Relaxed);
        if ms > 1000 {
            f.stats.polls_slow.fetch_add(1, Relaxed);
        }
    }
    // Do Not Disturb on / off (the stub `makoctl` reads this flag).
    if let Some(dir) = f.scratch.parent() {
        let flag = dir.join("bin").join("dnd.flag");
        if rng.below(2) == 0 {
            let _ = std::fs::write(&flag, b"1");
        } else {
            let _ = std::fs::remove_file(&flag);
        }
    }
    for k in PrivacyKey::ALL {
        let _ = f
            .ctl
            .call(Request::PrivacySet {
                key: k,
                on: rng.below(2) == 0,
            })
            .await;
        f.stats.feature_req.fetch_add(1, Relaxed);
    }
}

async fn repair_p0(
    f: &Feat<'_>,
    server: &mut PhoneServer,
    p0: &Arc<Phone>,
    ev: &mut std::fs::File,
) {
    let t = Instant::now();
    let id = p0.id();
    let _ = f
        .ctl
        .call(Request::Unpair {
            device_id: id.clone(),
        })
        .await;
    f.ctl
        .wait(Duration::from_secs(15), |s| {
            !s.paired.iter().any(|d| d.device_id == id)
        })
        .await;
    *f.current.lock().await = None;
    server.proxy.reset_all();
    let mut ok = false;
    // The step that stopped the last attempt, written to the event line.
    let mut why = "";
    for _ in 0..3 {
        let _ = f
            .ctl
            .call(Request::Disconnect {
                device_id: id.clone(),
            })
            .await;
        why = "connect";
        if !f
            .ctl
            .ok(Request::Connect {
                device_id: id.clone(),
                endpoint: Some(server.endpoint()),
            })
            .await
        {
            sleep(Duration::from_secs(2)).await;
            continue;
        }
        why = "unpaired-phase";
        if !f
            .ctl
            .wait(Duration::from_secs(20), |s| {
                link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Unpaired)
            })
            .await
        {
            sleep(Duration::from_secs(2)).await;
            continue;
        }
        why = "pair";
        if !f
            .ctl
            .ok(Request::Pair {
                device_id: id.clone(),
                mode: PairMode::Request,
            })
            .await
        {
            sleep(Duration::from_secs(2)).await;
            continue;
        }
        why = "challenge";
        if !f
            .ctl
            .wait(Duration::from_secs(20), |s| !s.challenges.is_empty())
            .await
        {
            sleep(Duration::from_secs(2)).await;
            continue;
        }
        if let Some(c) = f
            .ctl
            .status()
            .await
            .and_then(|s| s.challenges.first().map(|c| c.challenge_id.clone()))
        {
            let _ = f.ctl.ok(Request::Approve { challenge_id: c }).await;
        }
        why = "connected";
        if f.ctl
            .wait(Duration::from_secs(25), |s| {
                link_of(s, &id).is_some_and(|l| l.phase == LinkPhase::Connected)
            })
            .await
        {
            ok = set_consents(f.ctl, &id, true).await;
            why = "consents";
            break;
        }
        if let Some(st) = f.ctl.status().await {
            // What the service thought of the link when the pairing did not reach "connected".
            let _ = std::fs::write(
                f.scratch.join(format!(
                    "repair-fail-{:.0}.json",
                    f.t0.elapsed().as_secs_f64()
                )),
                serde_json::to_string_pretty(&st).unwrap_or_default(),
            );
        }
        sleep(Duration::from_secs(2)).await;
    }
    if ok {
        f.stats.pairs.fetch_add(1, Relaxed);
    } else {
        f.stats.pair_fail.fetch_add(1, Relaxed);
    }
    let _ = writeln!(
        ev,
        "{:.1}\trepair-p0\t{}\t{}\t{:.1}",
        f.t0.elapsed().as_secs_f64(),
        if ok { "" } else { why },
        if ok { "ok" } else { "FAIL" },
        t.elapsed().as_secs_f64()
    );
}

async fn feature_round(
    f: &Feat<'_>,
    round: u64,
    rng: &mut Rng,
    server: &mut PhoneServer,
    p0: &Arc<Phone>,
    ev: &mut std::fs::File,
) {
    if !skip("repair") && round % 30 == 29 {
        repair_p0(f, server, p0, ev).await;
        return;
    }
    if !skip("sms") && round.is_multiple_of(5) {
        // Notification and SMS bursts.
        for i in 0..6 {
            if let Some(p) = f.current.lock().await.as_ref() {
                for pkt in burst(rng, round * 10 + i) {
                    let _ = p.tx.send(Cmd::Send(pkt));
                }
            }
        }
    }
    if !skip("data") && round % 5 == 1 {
        data_requests(f, rng, p0).await;
    }
    if !skip("rings") && round.is_multiple_of(3) {
        ring_round(f, rng, server, p0, ev, round.is_multiple_of(9)).await;
    }
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
    let features = std::env::var("GEEKBRIDGE_SOAK_FEATURES").is_ok_and(|v| v == "1");
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
    assert!(set_consents(&ctl, &p0.id(), features).await);
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
                let ms = started.elapsed().as_millis() as u64;
                stats.poll_max_ms.fetch_max(ms, Relaxed);
                if ms > 1000 {
                    stats.polls_slow.fetch_add(1, Relaxed);
                }
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
        if !skip("bursts")
            && let Some(p) = current.lock().await.as_ref()
        {
            for pkt in burst(&mut rng, round) {
                let _ = p.tx.send(Cmd::Send(pkt));
            }
        }
        // 2. desktop-side clipboard traffic
        if !skip("clip") {
            let _ = ctl.call(Request::ClipboardPush { device_id: None }).await;
        }
        if !skip("clip") && round.is_multiple_of(3) {
            let _ = ctl.call(Request::ClipboardPull { device_id: None }).await;
        }
        // 3. files both ways
        if !skip("files") && round.is_multiple_of(4) {
            let size = 50_000 + rng.below(3_000_000) as u64;
            if let Some(p) = current.lock().await.as_ref() {
                let _ = p.tx.send(Cmd::Offer {
                    name: format!("up-{round}.bin"),
                    size,
                });
            }
        }
        if !skip("files") && round.is_multiple_of(5) {
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
        if !skip("faults") && round % 4 == 2 {
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
        if !skip("pairs") && round.is_multiple_of(7) {
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
        if features {
            let f = Feat {
                ctl: &ctl,
                stats: &stats,
                current: &current,
                pid,
                scratch: &scratch,
                t0,
            };
            feature_round(&f, round, &mut rng, &mut server, &p0, &mut events_log).await;
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
        if !skip("slow") && std::env::var("GEEKBRIDGE_SOAK_FAST").is_ok() {
            sleep(Duration::from_millis(100 + rng.below(150) as u64)).await;
        } else {
            sleep(Duration::from_millis(1500 + rng.below(2500) as u64)).await;
        }
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
    // Which threads are still there after the quiesce (names, state, what each waits in).
    {
        let mut dump = String::new();
        if let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/task")) {
            for t in rd.flatten() {
                let base = t.path();
                let comm = read_trim(&format!("{}/comm", base.display()));
                let wchan = read_trim(&format!("{}/wchan", base.display()));
                let stat = std::fs::read_to_string(base.join("stat")).unwrap_or_default();
                let state = stat
                    .rsplit(") ")
                    .next()
                    .and_then(|r| r.split_whitespace().next())
                    .unwrap_or("?");
                let _ = writeln!(
                    dump,
                    "{} {comm} state {state} wchan {wchan}",
                    t.file_name().to_string_lossy()
                );
            }
        }
        let _ = std::fs::write(out.join("threads-idle.txt"), dump);
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

// /proc and smaps_rollup exist only on Linux; the Windows and macOS runners have nothing to read.
#[cfg(target_os = "linux")]
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
