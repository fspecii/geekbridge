// SPDX-License-Identifier: GPL-3.0-or-later
//! A scriptable stand-in phone for `scripts/chaos-crash.sh`: pairs with a REAL `geekbridge-service` over loopback
//! (control port and token file only), turns on consents, then does what it is told on stdin, one command per line:
//!
//!   file MIB          offer a pattern file of that many MiB (name chaos.bin) and serve it
//!   messages N        push N text messages in batches of 100
//!   ring              a call starts ringing on the phone
//!   talk              the call is answered
//!   hangup            the call ends
//!   quit              leave
//!
//! Prints `PAIRED <device id>` once the link is up, `OK <command>` after each command was handed to the link and
//! `LINK-DOWN` when the computer's side went away (the service was killed); it then exits. Every run pairs afresh
//! with a new identity; the chaos script pairs once and copies the computer's state directory.
//!
//!   chaos-phone --state-dir DIR --control-port PORT [--features sms,clipboard,telephony,...]
#[cfg(target_os = "linux")]
mod linux {
    use bridge_core::ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token};
    use bridge_kde::{self as kde, share as kshare, sms};
    use serde_json::{Value, json};
    use std::{
        io::BufRead,
        path::PathBuf,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
        time::{Duration, Instant},
    };
    use tokio::{
        io::{AsyncRead, ReadBuf},
        net::TcpListener,
        time::sleep,
    };

    const SHARE: &str = "kdeconnect.share.request";
    const TELEPHONY: &str = "geekbridge.calls.state";

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
    }

    async fn wait_status(ctl: &Ctl, cond: impl Fn(&ipc::Status) -> bool) -> ipc::Status {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let s = ctl.status().await;
            if cond(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out: {s:#?}");
            sleep(Duration::from_millis(20)).await;
        }
    }

    fn feature(name: &str) -> Feature {
        match name {
            "sms" => Feature::Sms,
            "contacts" => Feature::Contacts,
            "calendar" => Feature::Calendar,
            "clipboard" => Feature::Clipboard,
            "notifications" => Feature::Notifications,
            "telephony" => Feature::Telephony,
            "battery" => Feature::Battery,
            other => panic!("unknown feature {other}"),
        }
    }

    /// Deterministic bytes without holding them in memory.
    struct Pattern {
        pos: u64,
        len: u64,
    }
    impl AsyncRead for Pattern {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let n = (buf.remaining() as u64).min(self.len - self.pos) as usize;
            let start = self.pos;
            let out = buf.initialize_unfilled_to(n);
            for (k, b) in out.iter_mut().enumerate() {
                *b = ((start + k as u64).wrapping_mul(13) % 251) as u8;
            }
            buf.advance(n);
            self.pos += n as u64;
            Poll::Ready(Ok(()))
        }
    }

    fn sms_msg(thread: i64, id: i64, ty: i64, date: u64) -> Value {
        json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + thread)}],
               "body": format!("chaos test message {id}"),
               "date": date, "type": ty, "read": 1, "thread_id": thread, "_id": id, "sub_id": 1})
    }

    pub async fn main() {
        let mut args = std::env::args().skip(1);
        let (mut dir, mut port) = (None, None);
        let mut features = "sms,telephony".to_owned();
        while let Some(a) = args.next() {
            match a.as_str() {
                "--state-dir" => dir = args.next().map(PathBuf::from),
                "--control-port" => port = args.next().and_then(|p| p.parse::<u16>().ok()),
                "--features" => features = args.next().unwrap_or_default(),
                other => panic!("unknown argument {other}"),
            }
        }
        let (dir, port) = (dir.expect("--state-dir"), port.expect("--control-port"));
        let token =
            Token::parse(&std::fs::read(ipc::token_path(&dir)).expect("token file")).unwrap();
        let ctl = Ctl { port, token };

        let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.payload.any_port = true;
        config.payload.accept_timeout = Duration::from_secs(30);
        let caps: Vec<String> = [
            "kdeconnect.ping",
            SHARE,
            TELEPHONY,
            "geekbridge.calls.request",
            "geekbridge.calls.result",
            sms::REQUEST_CONVERSATIONS,
            sms::REQUEST_CONVERSATION,
            sms::MESSAGES,
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Chaos Test Phone",
            kde::DeviceType::Phone,
            caps.clone(),
            caps,
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
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
        for name in features.split(',').filter(|f| !f.is_empty()) {
            ctl.ok(Request::SetConsent {
                device_id: id.clone(),
                feature: feature(name),
                enabled: true,
            })
            .await;
        }
        println!("PAIRED {id}");

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let now = 1_793_523_600_000_u64;
        let mut next_msg = 1_i64;
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { break };
                    let mut w = cmd.split_whitespace();
                    let verb = w.next().unwrap_or("");
                    let n: u64 = w.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let sent: Result<(), String> = match verb {
                        "file" => {
                            let size = n.max(1) << 20;
                            let ctx = link_.payload_context();
                            match ctx.listen().await {
                                Ok(l) => {
                                    let packet = kshare::file_packet("chaos.bin", size, None, l.port());
                                    let r = link_.send_with_payload(&packet).await.map_err(|e| e.to_string());
                                    tokio::spawn(async move {
                                        let _ = l
                                            .send(Pattern { pos: 0, len: size }, size, &mut |_| {}, &kde::CancelToken::new())
                                            .await;
                                    });
                                    r
                                }
                                Err(e) => Err(e.to_string()),
                            }
                        }
                        "messages" => {
                            let mut r = Ok(());
                            let mut left = n as i64;
                            while left > 0 && r.is_ok() {
                                let batch = left.min(100);
                                let msgs: Vec<Value> = (0..batch)
                                    .map(|i| {
                                        let id = next_msg + i;
                                        sms_msg(1 + id % 40, id, 1 + id % 2, now - id as u64 * 60_000)
                                    })
                                    .collect();
                                next_msg += batch;
                                left -= batch;
                                let mut body = json!({"version": 2, "messages": msgs, "threadsTotal": 40});
                                let packet = kde::Packet::new(sms::MESSAGES, body.as_object_mut().unwrap().clone());
                                r = link_.send(&packet).await.map_err(|e| e.to_string());
                            }
                            r
                        }
                        "ring" | "talk" | "hangup" => {
                            let mut body = serde_json::Map::new();
                            let state = match verb {
                                "ring" => "ringing",
                                "talk" => "offhook",
                                _ => "idle",
                            };
                            body.insert("state".into(), state.into());
                            if verb != "hangup" {
                                body.insert("number".into(), "+46701234567".into());
                                body.insert("name".into(), "Chaos Caller".into());
                            }
                            body.insert("canAnswer".into(), true.into());
                            body.insert("canEnd".into(), true.into());
                            link_.send(&kde::Packet::new(TELEPHONY, body)).await.map_err(|e| e.to_string())
                        }
                        "quit" => break,
                        _ => Err(format!("unknown command {verb}")),
                    };
                    match sent {
                        Ok(()) => println!("OK {cmd}"),
                        Err(e) => {
                            println!("ERR {cmd}: {e}");
                            println!("LINK-DOWN");
                            return;
                        }
                    }
                }
                ev = link_.recv() => {
                    if ev.is_err() {
                        println!("LINK-DOWN");
                        return;
                    }
                }
            }
        }
        println!("DONE");
    }
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    linux::main().await;
}

#[cfg(not(target_os = "linux"))]
fn main() {}
