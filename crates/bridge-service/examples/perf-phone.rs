// SPDX-License-Identifier: GPL-3.0-or-later
//! A stand-in phone for `scripts/perf-phone.sh`: pairs with a REAL `geekbridge-service` process over loopback (its control
//! port and token file, nothing else), turns on Telephony and the experimental Wi-Fi call audio for it, and then runs
//! measurement scenarios read from stdin, sampling the service process from `/proc` (RSS, threads, fds, child
//! processes, CPU ticks, voluntary context switches). Nothing here touches a real phone, Bluetooth or sound server.
//!
//!   perf-phone --state-dir DIR --control-port PORT --pid SERVICE_PID
//!
//! stdin commands (one per line; each ends with a `DONE <command>` line on stdout):
//!   sample LABEL         one SAMPLE line now
//!   idle SECONDS         SAMPLE every 5 s, then wakeups per second over the window
//!   cycles N             N times ring, answer on the computer, hang up on the computer; SAMPLE every N/10
//!   listen SECONDS       a Wi-Fi listen session with a 440 Hz tone at 50 frames/s; SAMPLE every 5 s; then stop
//!   quit
#[cfg(target_os = "linux")]
mod linux {
    use bridge_callwifi::{
        codec::Encoder,
        selftest::Verdict,
        tone::Tone,
        wire::{self, Dir, Frame, Msg, Offer, Ready, Uplink},
    };
    use bridge_core::phone_link as pl;
    use bridge_core::{
        call_wifi_view::{CallWifiAction, CallWifiView},
        calls::Sensitive,
        calls_link::{self as link, LinkCallReport, PhoneState},
        ipc::{self, CallAction, Feature, LinkPhase, PairMode, Request, Response, Token},
    };
    use bridge_kde::{self as kde, sms};
    use serde_json::{Map, Value, json};
    use std::{
        io::BufRead,
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{net::TcpListener, sync::mpsc, time::sleep};

    struct Yes;
    impl kde::Approver for Yes {
        async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
            true
        }
    }

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
        async fn ok(&self, request: Request) {
            match self.call(request).await {
                Ok(Response::Ack) => {}
                other => panic!("expected ack, got {other:?}"),
            }
        }
        async fn status(&self) -> ipc::Status {
            let (port, token) = (self.port, self.token.clone());
            tokio::task::spawn_blocking(move || ipc::status(port, &token))
                .await
                .unwrap()
                .unwrap()
        }
        async fn calls(&self) -> ipc::CallsStatus {
            match self.call(Request::Calls).await.unwrap() {
                Response::Calls(c) => *c,
                other => panic!("expected calls, got {other:?}"),
            }
        }
        async fn wifi(&self, action: CallWifiAction) -> CallWifiView {
            match self.call(Request::CallWifi { action }).await.unwrap() {
                Response::CallWifi(v) => *v,
                other => panic!("expected call_wifi, got {other:?}"),
            }
        }
    }

    // ------------------------------------------------------------------ /proc sampling

    struct Proc(u32);
    impl Proc {
        fn field(&self, key: &str) -> u64 {
            std::fs::read_to_string(format!("/proc/{}/status", self.0))
                .unwrap_or_default()
                .lines()
                .find_map(|l| {
                    l.strip_prefix(key)?
                        .trim()
                        .split_whitespace()
                        .next()?
                        .parse()
                        .ok()
                })
                .unwrap_or(0)
        }
        fn ticks(&self) -> u64 {
            let s = std::fs::read_to_string(format!("/proc/{}/stat", self.0)).unwrap_or_default();
            let rest = s.rsplit_once(')').map_or("", |x| x.1);
            let f: Vec<&str> = rest.split_whitespace().collect();
            let g = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            g(11) + g(12) // utime + stime (fields 14, 15 counting from 1; rest starts at field 3)
        }
        fn voluntary(&self) -> u64 {
            let Ok(rd) = std::fs::read_dir(format!("/proc/{}/task", self.0)) else {
                return 0;
            };
            rd.flatten()
                .filter_map(|t| std::fs::read_to_string(t.path().join("status")).ok())
                .flat_map(|s| {
                    s.lines()
                        .filter_map(|l| {
                            l.strip_prefix("voluntary_ctxt_switches:")?
                                .trim()
                                .parse::<u64>()
                                .ok()
                        })
                        .collect::<Vec<_>>()
                })
                .sum()
        }
        fn fds(&self) -> usize {
            std::fs::read_dir(format!("/proc/{}/fd", self.0)).map_or(0, |d| d.count())
        }
        /// Names of the direct child processes.
        fn children(&self) -> Vec<String> {
            let Ok(rd) = std::fs::read_dir("/proc") else {
                return vec![];
            };
            let mut out = vec![];
            for e in rd.flatten() {
                let Ok(s) = std::fs::read_to_string(e.path().join("stat")) else {
                    continue;
                };
                let (Some(open), Some((head, rest))) = (s.find('('), s.rsplit_once(')')) else {
                    continue;
                };
                let ppid: u32 = rest
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.parse().ok())
                    .unwrap_or(0);
                if ppid == self.0 {
                    out.push(head[open + 1..].to_owned());
                }
            }
            out.sort();
            out
        }
        fn line(&self, label: &str) -> String {
            let kids = self.children();
            format!(
                "SAMPLE {label} rss_kb={} hwm_kb={} anon_kb={} threads={} fds={} children={} [{}] ticks={}",
                self.field("VmRSS:"),
                self.field("VmHWM:"),
                self.field("RssAnon:"),
                self.field("Threads:"),
                self.fds(),
                kids.len(),
                kids.join(","),
                self.ticks()
            )
        }
    }

    // ------------------------------------------------------------------ the phone

    fn phone_endpoint() -> (Arc<kde::DeviceCredentials>, kde::Endpoint) {
        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Perf Phone",
            kde::DeviceType::Phone,
            [link::REQUEST]
                .into_iter()
                .chain(wire::PHONE_INCOMING)
                .chain([
                    "kdeconnect.ping",
                    pl::CONTACTS_REQUEST_UIDS,
                    pl::CONTACTS_REQUEST_VCARDS,
                    sms::REQUEST_CONVERSATIONS,
                    sms::REQUEST_CONVERSATION,
                ])
                .map(String::from)
                .collect(),
            [link::STATE, link::RESULT]
                .into_iter()
                .chain(wire::COMPUTER_INCOMING)
                .chain([
                    "kdeconnect.ping",
                    pl::CONTACTS_UIDS,
                    pl::CONTACTS_VCARDS,
                    sms::MESSAGES,
                ])
                .map(String::from)
                .collect(),
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        (creds, endpoint)
    }

    fn state_packet(state: PhoneState, name: bool) -> kde::Packet {
        kde::Packet::new(
            link::STATE,
            LinkCallReport {
                state,
                number: None,
                name: (name && state != PhoneState::Idle).then(|| Sensitive::new("Ada")),
                can_answer: true,
                can_end: true,
                can_dial: false,
            }
            .body(),
        )
    }

    fn sms_msg(thread: i64, id: i64, ty: i64, date: u64) -> Value {
        json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + thread)}],
               "body": format!("message {id} about lunch Cafe the quick brown fox jumps over the lazy dog"),
               "date": date, "type": ty, "read": 1, "thread_id": thread, "_id": id, "sub_id": 1})
    }

    fn vcard(i: usize) -> String {
        format!(
            "BEGIN:VCARD\nVERSION:3.0\nFN:Contact Number {i}\nTEL;TYPE=CELL:+4670{}\nTEL;TYPE=HOME:+4687{}\nEMAIL:p{i}@example.org\nEND:VCARD\n",
            1_000_000 + i,
            2_000_000 + i
        )
    }

    fn packet(m: &Msg) -> kde::Packet {
        kde::Packet::new(m.kind(), wire::body(m))
    }

    /// What the phone's link task is told to send.
    enum Out {
        Packet(kde::Packet),
        Offer, // answer the pending Request with an Offer
    }

    pub async fn main() {
        let mut args = std::env::args().skip(1);
        let (mut dir, mut port, mut pid) = (None, None, None);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--state-dir" => dir = args.next().map(PathBuf::from),
                "--control-port" => port = args.next().and_then(|p| p.parse::<u16>().ok()),
                "--pid" => pid = args.next().and_then(|p| p.parse::<u32>().ok()),
                other => panic!("unknown argument {other}"),
            }
        }
        let (dir, port, pid) = (
            dir.expect("--state-dir"),
            port.expect("--control-port"),
            pid.expect("--pid"),
        );
        let token =
            Token::parse(&std::fs::read(ipc::token_path(&dir)).expect("token file")).unwrap();
        let ctl = Ctl { port, token };
        let proc = Proc(pid);

        // Pair, like a user pressing Link on both sides.
        let (creds, endpoint) = phone_endpoint();
        let id = creds.device_id().to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            match endpoint.accept(stream).await.unwrap() {
                kde::Handshake::Trusted(l) => l,
                kde::Handshake::Pending(p) => p
                    .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                    .await
                    .unwrap(),
            }
        });
        ctl.ok(Request::Connect {
            device_id: id.clone(),
            endpoint: Some(ipc::Endpoint {
                address: addr.ip(),
                port: addr.port(),
            }),
        })
        .await;
        wait_status(&ctl, |s| {
            s.links
                .iter()
                .any(|l| l.device_id == id && l.phase == LinkPhase::Unpaired)
        })
        .await;
        ctl.ok(Request::Pair {
            device_id: id.clone(),
            mode: PairMode::Request,
        })
        .await;
        let st = wait_status(&ctl, |s| !s.challenges.is_empty()).await;
        ctl.ok(Request::Approve {
            challenge_id: st.challenges[0].challenge_id.clone(),
        })
        .await;
        let mut link_ = tokio::time::timeout(Duration::from_secs(15), accept)
            .await
            .unwrap()
            .unwrap();
        wait_status(&ctl, |s| {
            s.links
                .iter()
                .any(|l| l.device_id == id && l.phase == LinkPhase::Connected)
        })
        .await;
        for feature in [Feature::Telephony, Feature::CallAudioWifi] {
            ctl.ok(Request::SetConsent {
                device_id: id.clone(),
                feature,
                enabled: true,
            })
            .await;
        }
        link_
            .send(&packet(&Msg::Ready(Ready {
                ready: true,
                listen: true,
                speak: false,
                test: Verdict::Works,
            })))
            .await
            .unwrap();
        link_
            .send(&state_packet(PhoneState::Idle, true))
            .await
            .unwrap();
        println!("PAIRED {id}");

        // One task owns the link: it answers the computer's requests like Android would, and sends what it is told.
        let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
        let session = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let contacts_n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let contacts_n2 = contacts_n.clone();
        let sess2 = session.clone();
        let mut phone_state = PhoneState::Idle;
        tokio::spawn(async move {
            let mut pending_session = None;
            loop {
                tokio::select! {
                    ev = link_.recv() => match ev {
                        Ok(kde::LinkEvent::Packet(p)) if p.kind == link::REQUEST => {
                            let Ok((rid, action)) = link::parse_request(&p.body) else { continue };
                            use bridge_core::calls_link::LinkAction as A;
                            let next = match (action, phone_state) {
                                (A::Answer, PhoneState::Ringing) => Some(PhoneState::OffHook),
                                (A::Reject | A::Hangup, PhoneState::Ringing | PhoneState::OffHook) => Some(PhoneState::Idle),
                                _ => None,
                            };
                            if action == A::Report {
                                let _ = link_.send(&state_packet(phone_state, true)).await;
                                continue;
                            }
                            let result = if next.is_some() { Ok(()) } else { Err(link::LinkFailure::NoSuchCall) };
                            let _ = link_.send(&kde::Packet::new(link::RESULT, link::result_body(&rid, result))).await;
                            if let Some(n) = next {
                                phone_state = n;
                                let _ = link_.send(&state_packet(n, true)).await;
                            }
                        }
                        Ok(kde::LinkEvent::Packet(p)) if p.kind == pl::CONTACTS_REQUEST_UIDS => {
                            let n = contacts_n2.load(std::sync::atomic::Ordering::SeqCst);
                            let list: Vec<String> = (0..n).map(|i| format!("k{i}")).collect();
                            let mut b = Map::new();
                            b.insert("uids".into(), json!(list));
                            for k in &list { b.insert(k.clone(), json!("1")); }
                            let _ = link_.send(&kde::Packet::new(pl::CONTACTS_UIDS, b)).await;
                        }
                        Ok(kde::LinkEvent::Packet(p)) if p.kind == pl::CONTACTS_REQUEST_VCARDS => {
                            let want: Vec<String> = p.body.get("uids").and_then(|u| u.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                            let mut b = Map::new();
                            b.insert("uids".into(), json!(want));
                            for k in &want {
                                if let Ok(i) = k[1..].parse::<usize>() { b.insert(k.clone(), json!(vcard(i))); }
                            }
                            let _ = link_.send(&kde::Packet::new(pl::CONTACTS_VCARDS, b)).await;
                        }
                        Ok(kde::LinkEvent::Packet(p)) if wire::is_callaudio_kind(&p.kind) => {
                            match wire::parse(&p.kind, &p.body) {
                                Ok(Msg::Request { session }) => pending_session = Some(session),
                                Ok(Msg::Stop { .. }) => { sess2.store(0, std::sync::atomic::Ordering::SeqCst); }
                                _ => {}
                            }
                        }
                        Ok(_) => {}
                        Err(_) => { println!("PHONE LINK CLOSED"); return; }
                    },
                    Some(out) = rx.recv() => match out {
                        Out::Packet(p) => {
                            if let Some(n) = p.body.get("state").and_then(|s| s.as_str()) {
                                phone_state = match n { "ringing" => PhoneState::Ringing, "offhook" => PhoneState::OffHook, _ => PhoneState::Idle };
                            }
                            let _ = link_.send(&p).await;
                        }
                        Out::Offer => {
                            if let Some(s) = pending_session.take() {
                                sess2.store(s, std::sync::atomic::Ordering::SeqCst);
                                let _ = link_.send(&packet(&Msg::Offer(Offer { session: s, down: true, up: false, uplink: Uplink::None }))).await;
                            }
                        }
                    },
                }
            }
        });

        // Commands.
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<String>();
        std::thread::spawn(move || {
            for l in std::io::stdin().lock().lines().map_while(Result::ok) {
                if cmd_tx.send(l).is_err() {
                    break;
                }
            }
        });
        while let Some(line) = cmd_rx.recv().await {
            let mut w = line.split_whitespace();
            let cmd = w.next().unwrap_or("");
            let arg = w.next().unwrap_or("");
            match cmd {
                "sample" => println!("{}", proc.line(arg)),
                "idle" => {
                    let secs: u64 = arg.parse().unwrap_or(20);
                    let (c0, t0) = (proc.voluntary(), proc.ticks());
                    let mut min = u64::MAX;
                    for i in 0..secs / 5 {
                        sleep(Duration::from_secs(5)).await;
                        min = min.min(proc.field("VmRSS:"));
                        println!("{}", proc.line(&format!("idle+{}s", (i + 1) * 5)));
                    }
                    println!(
                        "IDLE window={secs}s rss_min_kb={min} wakeups_per_s={:.2} cpu_ticks={}",
                        proc.voluntary().saturating_sub(c0) as f64 / secs as f64,
                        proc.ticks() - t0
                    );
                }
                "cycles" => {
                    let n: usize = arg.parse().unwrap_or(10);
                    println!("{}", proc.line("cycles-start"));
                    let started = Instant::now();
                    let mut max_kids = 0;
                    for i in 1..=n {
                        cycle(&ctl, &tx).await;
                        max_kids = max_kids.max(proc.children().len());
                        if i % (n / 10).max(1) == 0 {
                            println!("{}", proc.line(&format!("cycle{i}")));
                        }
                    }
                    sleep(Duration::from_secs(8)).await; // the card lingers up to 6 s after the last ring
                    println!("{}", proc.line("cycles-after-linger"));
                    println!(
                        "CYCLES n={n} secs={:.1} max_children={max_kids}",
                        started.elapsed().as_secs_f64()
                    );
                }
                "listen" => {
                    let secs: u64 = arg.parse().unwrap_or(60);
                    println!("{}", proc.line("listen-before"));
                    let _ = tx.send(Out::Packet(state_packet(PhoneState::OffHook, true)));
                    sleep(Duration::from_millis(300)).await;
                    let v = ctl
                        .wifi(CallWifiAction::Listen {
                            device_id: id.clone(),
                        })
                        .await;
                    println!(
                        "LISTEN started: {:?}",
                        v.session.as_ref().map(|s| s.state.clone())
                    );
                    sleep(Duration::from_millis(500)).await;
                    let _ = tx.send(Out::Offer);
                    // Frames: the link task only ticks; the encoder lives here.
                    let mut enc = Encoder::new().unwrap();
                    let mut tone = Tone::new(440.0, 8000.0);
                    let (t0, k0) = (proc.ticks(), Instant::now());
                    let mut seq = 0u32;
                    let mut next_sample = Instant::now() + Duration::from_secs(5);
                    let mut peak = 0;
                    let mut interval = tokio::time::interval(Duration::from_millis(20));
                    while k0.elapsed() < Duration::from_secs(secs) {
                        interval.tick().await;
                        let s = session.load(std::sync::atomic::Ordering::SeqCst);
                        if s == 0 {
                            continue;
                        }
                        let opus = enc.encode(&tone.frame()).unwrap();
                        let _ = tx.send(Out::Packet(packet(&Msg::Frame(Frame {
                            session: s,
                            dir: Dir::Down,
                            seq,
                            ts: seq * 320,
                            cap: 0,
                            opus,
                        }))));
                        seq += 1;
                        if Instant::now() >= next_sample {
                            next_sample += Duration::from_secs(5);
                            peak = peak.max(proc.field("VmRSS:"));
                            println!(
                                "{}",
                                proc.line(&format!("listen+{}s", k0.elapsed().as_secs()))
                            );
                        }
                    }
                    let wall = k0.elapsed().as_secs_f64();
                    let ticks = proc.ticks() - t0;
                    println!(
                        "LISTEN frames={seq} cpu_ticks={ticks} cpu_pct={:.2} peak_rss_kb={peak}",
                        ticks as f64 / 100.0 / wall * 100.0
                    );
                    println!("{}", proc.line("listen-live-end"));
                    ctl.wifi(CallWifiAction::Stop).await;
                    let _ = tx.send(Out::Packet(state_packet(PhoneState::Idle, true)));
                    for s in [1u64, 3, 6, 12] {
                        sleep(Duration::from_secs(s)).await;
                        println!("{}", proc.line(&format!("listen-stopped+{s}s")));
                    }
                }
                "seed" => {
                    // seed MESSAGES CONTACTS
                    let msgs: usize = arg.parse().unwrap_or(5000);
                    let n: usize = w.next().and_then(|v| v.parse().ok()).unwrap_or(2000);
                    contacts_n.store(n, std::sync::atomic::Ordering::SeqCst);
                    for feature in [Feature::Contacts, Feature::Sms] {
                        ctl.ok(Request::SetConsent {
                            device_id: id.clone(),
                            feature,
                            enabled: true,
                        })
                        .await;
                    }
                    // The service asks for the conversation list on consent; push the data the way the phone would.
                    sleep(Duration::from_secs(1)).await;
                    let now = 1_793_523_600_000_u64;
                    let threads = 250_i64;
                    let mut mid = 0_i64;
                    let mut sent = 0usize;
                    let mut packets: Vec<Value> = vec![];
                    for chunk in (1..=threads).collect::<Vec<_>>().chunks(100) {
                        let m: Vec<Value> = chunk
                            .iter()
                            .map(|t| {
                                mid += 1;
                                sent += 1;
                                sms_msg(*t, mid, 1, now - *t as u64 * 60_000)
                            })
                            .collect();
                        let mut p = json!({"version": 2, "messages": m});
                        if chunk[0] == 1 {
                            p["threadsTotal"] = json!(threads);
                        }
                        packets.push(p);
                    }
                    let mut next_ts = now - 10_000_000;
                    'fill: loop {
                        for t in 1..=threads {
                            let take = 25.min(msgs.saturating_sub(sent));
                            if take == 0 {
                                break 'fill;
                            }
                            let m: Vec<Value> = (0..take)
                                .map(|_| {
                                    mid += 1;
                                    next_ts -= 1000;
                                    sms_msg(t, mid, 1 + mid % 2, next_ts)
                                })
                                .collect();
                            sent += take;
                            packets.push(json!({"version": 2, "messages": m}));
                        }
                    }
                    for p in packets {
                        let _ = tx.send(Out::Packet(kde::Packet::new(
                            sms::MESSAGES,
                            p.as_object().unwrap().clone(),
                        )));
                        sleep(Duration::from_millis(300)).await;
                    }
                    sleep(Duration::from_secs(8)).await;
                    let view = match ctl
                        .call(Request::MessagesView {
                            device_id: None,
                            thread_id: None,
                            query: None,
                            message_limit: 0,
                        })
                        .await
                    {
                        Ok(Response::Messages(v)) => v
                            .conversations
                            .iter()
                            .map(|c| c.message_count)
                            .sum::<usize>(),
                        other => {
                            println!("messages view: {other:?}");
                            0
                        }
                    };
                    println!("SEED messages_held={view} contacts_asked={n} api_port={port}");
                    println!("{}", proc.line("seeded"));
                }
                "quit" => break,
                _ => println!("unknown command {cmd}"),
            }
            println!("DONE {cmd}");
        }
    }

    async fn wait_status(ctl: &Ctl, cond: impl Fn(&ipc::Status) -> bool) -> ipc::Status {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let s = ctl.status().await;
            if cond(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out: {s:#?}");
            sleep(Duration::from_millis(5)).await;
        }
    }

    /// Ring, answer on the computer, hang up on the computer.
    async fn cycle(ctl: &Ctl, tx: &mpsc::UnboundedSender<Out>) {
        let _ = tx.send(Out::Packet(state_packet(PhoneState::Ringing, true)));
        let target = loop {
            let c = ctl.calls().await;
            if let Some(a) = c.view.ringing().and_then(|r| r.answer.clone()) {
                break a;
            }
            sleep(Duration::from_millis(10)).await;
        };
        sleep(Duration::from_millis(150)).await; // the card starts on the ring
        ctl.ok(Request::CallAction {
            target,
            action: CallAction::Answer,
        })
        .await;
        let hang = loop {
            let c = ctl.calls().await;
            if let Some(h) = c.view.live().and_then(|r| r.hangup.clone()) {
                break h;
            }
            sleep(Duration::from_millis(10)).await;
        };
        ctl.ok(Request::CallAction {
            target: hang,
            action: CallAction::Hangup,
        })
        .await;
        loop {
            let c = ctl.calls().await;
            if c.view.rows.is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    linux::main().await;
}

#[cfg(not(target_os = "linux"))]
fn main() {}
