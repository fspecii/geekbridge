// SPDX-License-Identifier: GPL-3.0-or-later
//! Speed probe for the size-versus-speed comparison of release profiles (docs/performance.md, "Size vs speed").
//! A stand-in phone pairs with a REAL `geekbridge-service` over loopback (its control port and token file, nothing
//! else) and times what a person waits for, printing one `R scenario key=value` line per figure. The service
//! process is sampled from /proc for CPU time. Made-up data only; the payload sockets of the phone stay inside the
//! port range given on the command line.
//!
//!   size_speed STATE_DIR CONTROL_PORT SERVICE_PID PAYLOAD_PORT_LOW PAYLOAD_PORT_HIGH [scenario ...]
//!
//! Scenarios (default: all, in this order): pair reconnect sync search bigview photos clipboard share
//!   pair       TLS handshake + pairing latency (Connect -> handshake done on the phone, Pair -> paired)
//!   reconnect  10 times disconnect + connect of a paired phone (trusted handshake)
//!   sync       20,000 messages in 300 conversations: list, then every thread's history pages; wall + CPU
//!   search     message search over the 20,000 held messages, 40 requests (service-side encode + loopback)
//!   bigview    a 2,000-message thread view, 20 requests
//!   photos     photo list + all thumbnails
//!   clipboard  pull a 7.5 MB image from the phone
//!   share      the phone sends a file (SHARE_MB, default 200) that is received to disk
#[cfg(target_os = "linux")]
mod linux {
    use bridge_core::calls::Sensitive;
    use bridge_core::calls_link::{self, LinkCallReport, PhoneState};
    use bridge_core::clipboard_wire as cw;
    use bridge_core::ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token};
    use bridge_kde::{self as kde, Packet, share as kshare, sms};
    use bridge_phone_sim::data::PhoneScript;
    use serde_json::{Value, json};
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use tokio::{io::AsyncReadExt, net::TcpListener, sync::mpsc};

    struct Yes;
    impl kde::Approver for Yes {
        async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
            true
        }
    }

    enum Cmd {
        Offer { name: String, size: u64 },
        Image { bytes: Vec<u8>, request_id: String },
        Messages(Vec<Value>),
        Ring(bool),
    }

    #[derive(Default)]
    struct Marks {
        hs_done: Option<Instant>,
        paired: Option<Instant>,
    }

    fn cpu_ns(pid: u32) -> u64 {
        let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
            return 0;
        };
        rd.flatten()
            .filter_map(|t| std::fs::read_to_string(t.path().join("schedstat")).ok())
            .filter_map(|s| s.split_whitespace().next()?.parse::<u64>().ok())
            .sum()
    }
    fn proc_ticks(pid: u32) -> u64 {
        let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let rest = s.rsplit_once(')').map(|x| x.1).unwrap_or("");
        let f: Vec<&str> = rest.split_whitespace().collect();
        let g = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        g(11) + g(12) // utime + stime (fields 14, 15)
    }
    fn rss_kib(pid: u32) -> u64 {
        std::fs::read_to_string(format!("/proc/{pid}/status"))
            .unwrap_or_default()
            .lines()
            .find(|l| l.starts_with("VmRSS"))
            .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
            .unwrap_or(0)
    }

    fn ms(d: Duration) -> f64 {
        d.as_secs_f64() * 1000.0
    }
    fn out(sc: &str, kv: &str) {
        println!("R {sc} {kv}");
    }

    const THREADS: usize = 300;

    /// 20,000 messages in 300 conversations: thread 0 holds 2,000, the rest 60 each, and the first 60 of them one more.
    fn history(now: u64) -> Vec<Value> {
        let mut v = Vec::new();
        let mut id = 1_i64;
        for t in 0..THREADS {
            let n = if t == 0 {
                2000
            } else if t <= 60 {
                61
            } else {
                60
            };
            for k in 0..n {
                let body = if id % 97 == 0 {
                    "see the Parcel at the door, thanks a lot".to_owned()
                } else {
                    format!("ordinary message number {id} about lunch plans tonight")
                };
                v.push(json!({
                    "addresses": [{"address": format!("+1555{:07}", t + 1)}], "body": body,
                    "date": now - (k as u64 * 90_000) - t as u64 * 1_000, "type": 1 + (k % 2), "read": 1,
                    "thread_id": 100 + t as i64, "_id": id, "sub_id": 1, "event": 1,
                }));
                id += 1;
            }
        }
        v
    }

    pub fn main() {
        let mut args = std::env::args().skip(1);
        let state_dir = PathBuf::from(args.next().expect("STATE_DIR"));
        let port: u16 = args.next().expect("CONTROL_PORT").parse().unwrap();
        let pid: u32 = args.next().expect("SERVICE_PID").parse().unwrap();
        let lo: u16 = args.next().expect("PAYLOAD_PORT_LOW").parse().unwrap();
        let hi: u16 = args.next().expect("PAYLOAD_PORT_HIGH").parse().unwrap();
        let mut scenarios: Vec<String> = args.collect();
        if scenarios.is_empty() {
            scenarios = "pair reconnect sync search bigview photos clipboard share"
                .split(' ')
                .map(String::from)
                .collect();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(run(state_dir, port, pid, lo..=hi, scenarios));
    }

    async fn run(
        state_dir: PathBuf,
        port: u16,
        pid: u32,
        payload_ports: std::ops::RangeInclusive<u16>,
        scenarios: Vec<String>,
    ) {
        let bytes = kde::fsutil::read_private_file(&ipc::token_path(&state_dir))
            .unwrap()
            .unwrap();
        let token = Token::parse(&bytes).unwrap();
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let id = creds.device_id().to_string();
        let incoming: Vec<String> = bridge_service::gate::LOCAL_OUTGOING
            .iter()
            .map(|s| (*s).into())
            .collect();
        let outgoing: Vec<String> = bridge_service::gate::LOCAL_INCOMING
            .iter()
            .map(|s| (*s).into())
            .collect();
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.limits.packet_line = 2 * 1024 * 1024;
        config.payload.ports = payload_ports;
        config.payload.any_port = false;
        config.payload.accept_timeout = Duration::from_secs(30);
        config.payload.idle_timeout = Duration::from_secs(30);
        let endpoint = Arc::new(
            kde::Endpoint::new(
                creds,
                "Pixel (simulated)",
                kde::DeviceType::Phone,
                incoming,
                outgoing,
                Arc::new(kde::MemoryTrustStore::new()),
                config,
            )
            .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let phone_port = listener.local_addr().unwrap().port();
        let marks = Arc::new(Mutex::new(Marks::default()));
        let (link_tx, mut link_rx) = mpsc::unbounded_channel::<kde::TrustedLink>();
        {
            let (endpoint, marks) = (endpoint.clone(), marks.clone());
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    let (endpoint, marks, tx) = (endpoint.clone(), marks.clone(), link_tx.clone());
                    tokio::spawn(async move {
                        let Ok(h) = endpoint.accept(stream).await else {
                            return;
                        };
                        marks.lock().unwrap().hs_done = Some(Instant::now());
                        let link = match h {
                            kde::Handshake::Pending(p) => {
                                match p.pair(kde::PairMode::AwaitPeerRequest, &mut Yes).await {
                                    Ok(l) => l,
                                    Err(_) => return,
                                }
                            }
                            kde::Handshake::Trusted(t) => t,
                        };
                        marks.lock().unwrap().paired = Some(Instant::now());
                        let _ = tx.send(link);
                    });
                }
            });
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<Cmd>();
        let pump = tokio::spawn(async move {
            let mut script = PhoneScript::demo(now as i64);
            script.photos_on = true;
            script.messages.clear();
            let mut by_thread: HashMap<i64, Vec<Value>> = HashMap::new();
            let mut link: Option<kde::TrustedLink> = None;
            loop {
                tokio::select! {
                    l = link_rx.recv() => { match l { Some(l) => link = Some(l), None => return } }
                    cmd = cmd_rx.recv() => {
                        let Some(cmd) = cmd else { return };
                        match cmd {
                            Cmd::Ring(on) => {
                                if let Some(l) = link.as_mut() {
                                    let st = if on { PhoneState::Ringing } else { PhoneState::Idle };
                                    let rep = LinkCallReport {
                                        state: st, number: None,
                                        name: on.then(|| Sensitive::new("Ada")),
                                        can_answer: true, can_end: true, can_dial: false, can_tones: false,
                                    };
                                    let _ = l.send(&Packet::new(calls_link::STATE, rep.body())).await;
                                }
                            }
                            Cmd::Messages(v) => {
                                for m in &v {
                                    by_thread.entry(m["thread_id"].as_i64().unwrap()).or_default().push(m.clone());
                                }
                                for l in by_thread.values_mut() {
                                    l.sort_by_key(|m| std::cmp::Reverse(m["date"].as_u64().unwrap()));
                                }
                                script.messages = v;
                            }
                            Cmd::Offer { name, size } => {
                                let Some(link) = link.as_mut() else { continue };
                                let ctx = link.payload_context();
                                let l = ctx.listen().await.unwrap();
                                let packet = kshare::file_packet(&name, size, None, l.port());
                                link.send_with_payload(&packet).await.unwrap();
                                tokio::spawn(async move {
                                    let src = tokio::io::repeat(0x5a).take(size);
                                    let _ = l.send(src, size, &mut |_| {}, &kde::CancelToken::new()).await;
                                });
                            }
                            Cmd::Image { bytes, request_id } => {
                                let Some(link) = link.as_mut() else { continue };
                                let ctx = link.payload_context();
                                let l = ctx.listen().await.unwrap();
                                let size = bytes.len() as u64;
                                let mut p = Packet::new(cw::ITEM, cw::image_item_body("image/png", size, Some(&request_id), 0));
                                p.payload_size = Some(size as i64);
                                let mut info = serde_json::Map::new();
                                info.insert("port".into(), l.port().into());
                                p.payload_transfer_info = Some(info);
                                link.send_with_payload(&p).await.unwrap();
                                tokio::spawn(async move {
                                    let _ = l.send(std::io::Cursor::new(bytes), size, &mut |_| {}, &kde::CancelToken::new()).await;
                                });
                            }
                        }
                    }
                    ev = async { match link.as_mut() { Some(l) => l.recv().await, None => std::future::pending().await } } => {
                        match ev {
                            Ok(kde::LinkEvent::Packet(p)) => {
                                let l = link.as_mut().unwrap();
                                if p.kind == sms::REQUEST_CONVERSATION {
                                    let t = p.body.get("threadID").and_then(Value::as_i64).unwrap_or(-1);
                                    let start = p.body.get("rangeStartTimestamp").and_then(Value::as_u64).unwrap_or(u64::MAX);
                                    let n = p.body.get("numberToRequest").and_then(Value::as_u64).unwrap_or(50) as usize;
                                    let msgs: Vec<Value> = by_thread.get(&t).map(|v| v.iter()
                                        .filter(|m| m["date"].as_u64().unwrap() <= start).take(n).cloned().collect()).unwrap_or_default();
                                    if let (Some(last), Some(all)) = (msgs.last(), by_thread.get(&t))
                                        && last["_id"] == all.last().unwrap()["_id"]
                                    {
                                        DONE_THREADS.lock().unwrap().insert(t);
                                    }
                                    let _ = l.send(&Packet::new(sms::MESSAGES, obj(json!({"version": 2, "messages": msgs})))).await;
                                } else if p.kind == cw::PULL {
                                    // answered by the driver through Cmd::Image
                                    PULLS.lock().unwrap().push(p.body.get("request_id").and_then(Value::as_str).unwrap_or("").to_owned());
                                    let _ = l;
                                } else if p.kind == "kdeconnect.share.request" {
                                    // nothing from the service to the phone here
                                } else {
                                    for (kind, body) in script.handle(&p.kind, &p.body) {
                                        if l.send(&Packet::new(kind, body)).await.is_err() { break; }
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(_) => { link = None; }
                        }
                    }
                }
            }
        });

        // The driver talks to the control API from a blocking thread.
        let drive = tokio::task::spawn_blocking({
            let cmd_tx = cmd_tx.clone();
            let marks = marks.clone();
            move || {
                drive(
                    port, pid, phone_port, &id, &token, &cmd_tx, &marks, &scenarios, now,
                )
            }
        });
        drive.await.unwrap();
        pump.abort();
    }

    static PULLS: Mutex<Vec<String>> = Mutex::new(Vec::new());
    /// Threads whose oldest message the phone has already served.
    static DONE_THREADS: std::sync::LazyLock<Mutex<std::collections::HashSet<i64>>> =
        std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

    fn obj(v: Value) -> serde_json::Map<String, Value> {
        match v {
            Value::Object(m) => m,
            _ => Default::default(),
        }
    }

    fn call(port: u16, t: &Token, r: Request) -> Response {
        ipc::call(port, t, &r).expect("control call")
    }

    fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) -> Instant {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(secs) {
            if f() {
                return Instant::now();
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("timed out waiting for {what}");
    }

    fn median(v: &mut [f64]) -> f64 {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    }

    #[allow(clippy::too_many_arguments)]
    fn drive(
        port: u16,
        pid: u32,
        phone_port: u16,
        id: &str,
        t: &Token,
        cmd_tx: &mpsc::UnboundedSender<Cmd>,
        marks: &Arc<Mutex<Marks>>,
        scenarios: &[String],
        now: u64,
    ) {
        let has = |s: &str| scenarios.iter().any(|x| x == s);
        let connected = |t: &Token| {
            ipc::status(port, t).is_ok_and(|s| {
                s.links
                    .iter()
                    .any(|l| l.device_id == id && l.phase == LinkPhase::Connected)
            })
        };
        let endpoint = || {
            Some(ipc::Endpoint {
                address: "127.0.0.1".parse().unwrap(),
                port: phone_port,
            })
        };
        // pair
        *marks.lock().unwrap() = Marks::default();
        let rss0 = rss_kib(pid);
        let (c0, k0) = (cpu_ns(pid), proc_ticks(pid));
        let t0 = Instant::now();
        call(
            port,
            t,
            Request::Connect {
                device_id: id.to_owned(),
                endpoint: endpoint(),
            },
        );
        let t_unpaired = wait_until("unpaired link", 20, || {
            ipc::status(port, t).is_ok_and(|s| {
                s.links
                    .iter()
                    .any(|l| l.device_id == id && l.phase == LinkPhase::Unpaired)
            })
        });
        let t1 = Instant::now();
        call(
            port,
            t,
            Request::Pair {
                device_id: id.to_owned(),
                mode: PairMode::Request,
            },
        );
        let mut ch = String::new();
        wait_until("challenge", 20, || {
            if let Ok(s) = ipc::status(port, t)
                && let Some(c) = s.challenges.first()
            {
                ch = c.challenge_id.clone();
                return true;
            }
            false
        });
        let t_chal = Instant::now();
        call(port, t, Request::Approve { challenge_id: ch });
        let t_conn = wait_until("connected", 20, || connected(t));
        let hs = marks.lock().unwrap().hs_done.map(|h| ms(h - t0));
        let paired = marks.lock().unwrap().paired.map(|h| ms(h - t1));
        if has("pair") {
            out(
                "pair",
                &format!(
                    "handshake_ms={:.2} connect_to_unpaired_ms={:.2} pair_request_to_challenge_ms={:.2} approve_to_connected_ms={:.2} pair_total_ms={:.2} phone_handshake_ms={:.2} phone_pair_ms={:.2} cpu_ms={:.1} cpu_ticks_ms={}",
                    ms(t_unpaired - t0),
                    ms(t_unpaired - t0),
                    ms(t_chal - t1),
                    ms(t_conn - t_chal),
                    ms(t_conn - t0),
                    hs.unwrap_or(0.0),
                    paired.unwrap_or(0.0),
                    (cpu_ns(pid) - c0) as f64 / 1e6,
                    (proc_ticks(pid) - k0) * 10,
                ),
            );
            out(
                "pair",
                &format!("rss_kib_before={rss0} rss_kib_after={}", rss_kib(pid)),
            );
        }
        for f in [
            Feature::Sms,
            Feature::Contacts,
            Feature::Calendar,
            Feature::Clipboard,
            Feature::Photos,
            Feature::Telephony,
        ] {
            call(
                port,
                t,
                Request::SetConsent {
                    device_id: id.to_owned(),
                    feature: f,
                    enabled: true,
                },
            );
        }
        call(
            port,
            t,
            Request::ShareSettings {
                device_id: id.to_owned(),
                receive: Some(true),
                auto_accept: Some(true),
            },
        );
        std::thread::sleep(Duration::from_millis(500));

        if has("reconnect") {
            let mut v = Vec::new();
            let mut hs_v = Vec::new();
            for _ in 0..10 {
                call(
                    port,
                    t,
                    Request::Disconnect {
                        device_id: id.to_owned(),
                    },
                );
                wait_until("disconnected", 10, || !connected(t));
                std::thread::sleep(Duration::from_millis(200));
                *marks.lock().unwrap() = Marks::default();
                let t0 = Instant::now();
                call(
                    port,
                    t,
                    Request::Connect {
                        device_id: id.to_owned(),
                        endpoint: endpoint(),
                    },
                );
                let done = wait_until("reconnected", 20, || connected(t));
                v.push(ms(done - t0));
                if let Some(h) = marks.lock().unwrap().hs_done {
                    hs_v.push(ms(h - t0));
                }
            }
            out(
                "reconnect",
                &format!(
                    "connect_to_connected_ms_median={:.2} handshake_ms_median={:.2}",
                    median(&mut v),
                    median(&mut hs_v)
                ),
            );
            std::thread::sleep(Duration::from_millis(500));
        }

        let sync_needed = has("sync") || has("search") || has("bigview") || has("ctl");
        if sync_needed {
            let msgs = history(now);
            let total = msgs.len();
            cmd_tx.send(Cmd::Messages(msgs)).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            let view = |thread: Option<i64>, q: Option<&str>, lim: usize| match call(
                port,
                t,
                Request::MessagesView {
                    device_id: Some(id.to_owned()),
                    thread_id: thread,
                    query: q.map(String::from),
                    message_limit: lim,
                },
            ) {
                Response::Messages(m) => *m,
                other => panic!("messages view: {other:?}"),
            };
            let (c0, k0, w0) = (cpu_ns(pid), proc_ticks(pid), Instant::now());
            call(
                port,
                t,
                Request::MessagesRefresh {
                    device_id: id.to_owned(),
                    thread_id: None,
                },
            );
            wait_until("conversation list", 60, || {
                view(None, None, 1).conversations_total >= THREADS
            });
            let w_list = Instant::now();
            let c_list = cpu_ns(pid);
            // History: keep asking for older pages of every thread that holds fewer than the phone has.
            let mut polls = 0;
            loop {
                let done = DONE_THREADS.lock().unwrap().clone();
                polls += 1;
                if done.len() >= THREADS {
                    break;
                }
                for k in 0..THREADS as i64 {
                    if !done.contains(&(100 + k)) {
                        let _ = ipc::call(
                            port,
                            t,
                            &Request::MessagesRefresh {
                                device_id: id.to_owned(),
                                thread_id: Some(100 + k),
                            },
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
                assert!(w0.elapsed() < Duration::from_secs(300), "sync stuck");
            }
            // The service has taken the last page once the oldest thread shows all it holds.
            wait_until("last page ingested", 30, || {
                view(Some(399), None, 1)
                    .selected
                    .is_some_and(|s| s.held >= 60)
            });
            let w_all = Instant::now();
            if has("sync") {
                out(
                    "sync",
                    &format!(
                        "list_ms={:.1} list_cpu_ms={:.1} full_wall_ms={:.1} full_cpu_ms={:.1} full_cpu_ticks_ms={} polls={polls} messages={total} rss_kib={}",
                        ms(w_list - w0),
                        (c_list - c0) as f64 / 1e6,
                        ms(w_all - w0),
                        (cpu_ns(pid) - c0) as f64 / 1e6,
                        (proc_ticks(pid) - k0) * 10,
                        rss_kib(pid)
                    ),
                );
            }
            std::thread::sleep(Duration::from_millis(500));
            if has("search") {
                let mut lat = Vec::new();
                let mut hits = 0;
                let c0 = cpu_ns(pid);
                for _ in 0..40 {
                    let t0 = Instant::now();
                    let v = view(None, Some("parcel"), 0);
                    lat.push(ms(t0.elapsed()));
                    hits = v.hits.iter().map(|g| g.hits.len()).sum::<usize>();
                }
                let mut miss = Vec::new();
                for _ in 0..40 {
                    let t0 = Instant::now();
                    let _ = view(None, Some("zzzqqq"), 0);
                    miss.push(ms(t0.elapsed()));
                }
                let mut one = Vec::new();
                for _ in 0..40 {
                    let t0 = Instant::now();
                    let _ = view(None, None, 0);
                    one.push(ms(t0.elapsed()));
                }
                out(
                    "search",
                    &format!(
                        "hit_roundtrip_ms_median={:.3} hit_roundtrip_ms_max={:.3} hits={hits} miss_roundtrip_ms_median={:.3} list_view_ms_median={:.3} service_cpu_ms_per_req={:.3}",
                        median(&mut lat.clone()),
                        lat.iter().cloned().fold(0.0, f64::max),
                        median(&mut miss),
                        median(&mut one),
                        (cpu_ns(pid) - c0) as f64 / 1e6 / 120.0,
                    ),
                );
            }
            if has("bigview") {
                let mut lat = Vec::new();
                let mut n = 0;
                let c0 = cpu_ns(pid);
                for _ in 0..20 {
                    let t0 = Instant::now();
                    let v = view(Some(100), None, 2000);
                    lat.push(ms(t0.elapsed()));
                    n = v.selected.map(|s| s.messages.len()).unwrap_or(0);
                }
                out(
                    "bigview",
                    &format!(
                        "roundtrip_ms_median={:.3} roundtrip_ms_max={:.3} messages_in_reply={n} service_cpu_ms_per_req={:.3}",
                        median(&mut lat.clone()),
                        lat.iter().cloned().fold(0.0, f64::max),
                        (cpu_ns(pid) - c0) as f64 / 1e6 / 20.0,
                    ),
                );
            }
        }

        if has("ctl") {
            // CTLS=path,path: `geekbridge-ctl messages` (the list) and `messages DEVICE 100` (a 889-message thread view)
            // as a process, wall time from spawn to exit, 20 each; the render cost of the CLI build.
            for ctl in std::env::var("CTLS")
                .unwrap_or_default()
                .split(',')
                .filter(|c| !c.is_empty())
            {
                let state_dir = std::env::var("STATE_DIR_FOR_CTL").unwrap_or_default();
                for (name, extra) in [
                    ("list", vec![]),
                    ("thread", vec![id.to_owned(), "100".to_owned()]),
                ] {
                    let mut v = Vec::new();
                    let mut bytes = 0;
                    for _ in 0..20 {
                        let t0 = Instant::now();
                        let o = std::process::Command::new(ctl)
                            .args([
                                "--state-dir",
                                &state_dir,
                                "--control-port",
                                &port.to_string(),
                                "messages",
                            ])
                            .args(&extra)
                            .output()
                            .expect("ctl");
                        v.push(ms(t0.elapsed()));
                        bytes = o.stdout.len();
                        assert!(o.status.success(), "ctl failed");
                    }
                    let tag = ctl.rsplit('/').nth(2).unwrap_or(ctl);
                    out(
                        "ctl",
                        &format!(
                            "{tag}_{name}_ms_median={:.2} {tag}_{name}_ms_min={:.2} {tag}_{name}_bytes={bytes}",
                            median(&mut v.clone()),
                            v.iter().cloned().fold(f64::MAX, f64::min)
                        ),
                    );
                }
            }
        }

        if has("ring") {
            let calls = || match call(port, t, Request::Calls) {
                Response::Calls(c) => *c,
                other => panic!("calls: {other:?}"),
            };
            // Warm up, then 40 rings: phone sends the ring, we poll the call list until it shows a ringing row.
            let mut v = Vec::new();
            let mut status = Vec::new();
            for round in 0..45 {
                std::thread::sleep(Duration::from_millis(250));
                let t0 = Instant::now();
                cmd_tx.send(Cmd::Ring(true)).unwrap();
                wait_until("ring visible", 10, || calls().view.ringing().is_some());
                if round >= 5 {
                    v.push(ms(t0.elapsed()));
                }
                let t1 = Instant::now();
                let _ = ipc::status(port, t);
                if round >= 5 {
                    status.push(ms(t1.elapsed()));
                }
                cmd_tx.send(Cmd::Ring(false)).unwrap();
                wait_until("idle", 10, || calls().view.ringing().is_none());
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            out(
                "ring",
                &format!(
                    "ring_to_visible_ms_median={:.3} p90={:.3} max={:.3} status_ms_median={:.3}",
                    v[v.len() / 2],
                    v[v.len() * 9 / 10],
                    v[v.len() - 1],
                    median(&mut status)
                ),
            );
        }

        if has("photos") {
            let t0 = Instant::now();
            call(
                port,
                t,
                Request::PhotosRefresh {
                    device_id: id.to_owned(),
                },
            );
            let mut items = Vec::new();
            wait_until("photo list", 20, || {
                if let Response::Photos(p) = call(port, t, Request::Photos { device_id: None }) {
                    items = p.items.iter().map(|i| i.id.clone()).collect();
                    return !items.is_empty();
                }
                false
            });
            let t_list = Instant::now();
            let mut thumbs = Vec::new();
            for pid_ in &items {
                let t1 = Instant::now();
                loop {
                    match ipc::call(
                        port,
                        t,
                        &Request::PhotoThumb {
                            device_id: id.to_owned(),
                            id: pid_.clone(),
                        },
                    ) {
                        Ok(Response::PhotoThumb(_)) => break,
                        _ => {
                            std::thread::sleep(Duration::from_millis(1));
                            assert!(t1.elapsed() < Duration::from_secs(10), "thumb timeout");
                        }
                    }
                }
                thumbs.push(ms(t1.elapsed()));
            }
            out(
                "photos",
                &format!(
                    "list_ms={:.2} items={} thumb_ms_median={:.2} thumb_ms_max={:.2}",
                    ms(t_list - t0),
                    items.len(),
                    median(&mut thumbs.clone()),
                    thumbs.iter().cloned().fold(0.0, f64::max)
                ),
            );
        }

        if has("clipboard") {
            // 7.5 MB "PNG": the signature then pseudo-random bytes (the service sniffs the signature only).
            let mut img = b"\x89PNG\r\n\x1a\n".to_vec();
            let mut x = 0x9E3779B97F4A7C15_u64;
            while img.len() < 7_500_000 {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                img.extend_from_slice(&x.to_le_bytes());
            }
            PULLS.lock().unwrap().clear();
            let (c0, k0) = (cpu_ns(pid), proc_ticks(pid));
            let t0 = Instant::now();
            call(
                port,
                t,
                Request::ClipboardPull {
                    device_id: Some(id.to_owned()),
                },
            );
            wait_until("pull request at the phone", 10, || {
                !PULLS.lock().unwrap().is_empty()
            });
            let t_req = Instant::now();
            let request_id = PULLS.lock().unwrap()[0].clone();
            cmd_tx
                .send(Cmd::Image {
                    bytes: img,
                    request_id,
                })
                .unwrap();
            let mut state = String::new();
            let done = wait_until("clipboard image", 60, || {
                if let Response::ClipboardStatus(s) = call(port, t, Request::ClipboardStatus) {
                    state = s
                        .last_pull
                        .map(|p| format!("{}/{}", p.state, p.reason.unwrap_or_default()))
                        .unwrap_or_default();
                    return state.starts_with("delivered") || state.starts_with("none");
                }
                false
            });
            out(
                "clipboard",
                &format!(
                    "pull_to_phone_ms={:.2} image_transfer_ms={:.1} state={state} cpu_ms={:.1} cpu_ticks_ms={}",
                    ms(t_req - t0),
                    ms(done - t_req),
                    (cpu_ns(pid) - c0) as f64 / 1e6,
                    (proc_ticks(pid) - k0) * 10,
                ),
            );
        }

        if has("share") {
            let mb: u64 = std::env::var("SHARE_MB")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(200);
            let size = mb * 1_000_000;
            let (c0, k0) = (cpu_ns(pid), proc_ticks(pid));
            let t0 = Instant::now();
            cmd_tx
                .send(Cmd::Offer {
                    name: "bench.bin".into(),
                    size,
                })
                .unwrap();
            let mut last = String::new();
            let done = wait_until("file transfer", 300, || {
                if let Response::Share(s) = call(port, t, Request::ShareStatus)
                    && let Some(tr) = s.transfers.last()
                {
                    last = format!("{:?}", tr.state);
                    return last == "Done" || last == "Failed";
                }
                false
            });
            let wall = ms(done - t0);
            let cpu = (cpu_ns(pid) - c0) as f64 / 1e6;
            out(
                "share",
                &format!(
                    "state={last} mb={mb} wall_ms={wall:.0} mb_per_s={:.1} cpu_ms={cpu:.0} cpu_ms_per_mb={:.2} cpu_ticks_ms={} rss_kib={}",
                    mb as f64 / (wall / 1000.0),
                    cpu / mb as f64,
                    (proc_ticks(pid) - k0) * 10,
                    rss_kib(pid)
                ),
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {}
