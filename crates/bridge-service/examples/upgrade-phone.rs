// SPDX-License-Identifier: GPL-3.0-or-later
//! A stand-in phone for `scripts/e2e/upgrade-path.sh`: pairs with a REAL `geekbridge-service` process over loopback
//! (its control port and token file, nothing else), turns on a few consents for it, pushes a handful of text
//! messages the way the phone would, stays connected for a while and leaves. The pairing stays in the computer's
//! state directory; that is the point. Only the stable IPC requests (Connect, Pair, Approve, SetConsent) are used, so
//! the same build of this example pairs with an old service too.
//!
//!   upgrade-phone --state-dir DIR --control-port PORT [--messages N] [--hold SECONDS] [--features sms,clipboard,...]
//!
//! Prints `PAIRED <device id>` once the link is up and `DONE` when it leaves. Nothing here touches a real phone,
//! Bluetooth or a sound server.
#[cfg(target_os = "linux")]
mod linux {
    use bridge_core::ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token};
    use bridge_kde::{self as kde, sms};
    use serde_json::{Value, json};
    use std::{
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::{net::TcpListener, time::sleep};

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

    fn sms_msg(thread: i64, id: i64, ty: i64, date: u64) -> Value {
        json!({"addresses": [{"address": format!("+4670{:07}", 1_000_000 + thread)}],
               "body": format!("upgrade test message {id}"),
               "date": date, "type": ty, "read": 1, "thread_id": thread, "_id": id, "sub_id": 1})
    }

    pub async fn main() {
        let mut args = std::env::args().skip(1);
        let (mut dir, mut port) = (None, None);
        let (mut messages, mut hold) = (6_i64, 8_u64);
        let mut features = "sms,clipboard,contacts".to_owned();
        while let Some(a) = args.next() {
            match a.as_str() {
                "--state-dir" => dir = args.next().map(PathBuf::from),
                "--control-port" => port = args.next().and_then(|p| p.parse::<u16>().ok()),
                "--messages" => messages = args.next().and_then(|p| p.parse().ok()).unwrap_or(6),
                "--hold" => hold = args.next().and_then(|p| p.parse().ok()).unwrap_or(8),
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
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Upgrade Test Phone",
            kde::DeviceType::Phone,
            [
                "kdeconnect.ping",
                sms::REQUEST_CONVERSATIONS,
                sms::REQUEST_CONVERSATION,
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            ["kdeconnect.ping", sms::MESSAGES]
                .into_iter()
                .map(String::from)
                .collect(),
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

        sleep(Duration::from_secs(1)).await;
        let now = 1_793_523_600_000_u64;
        let msgs: Vec<Value> = (1..=messages)
            .map(|i| sms_msg(1 + i % 3, i, 1 + i % 2, now - i as u64 * 60_000))
            .collect();
        let mut body = json!({"version": 2, "messages": msgs, "threadsTotal": 3});
        let packet = kde::Packet::new(sms::MESSAGES, body.as_object_mut().unwrap().clone());
        link_.send(&packet).await.unwrap();
        // Stay while the computer takes the messages in; answer nothing, ignore what it asks.
        let end = Instant::now() + Duration::from_secs(hold);
        while Instant::now() < end {
            if tokio::time::timeout(Duration::from_millis(200), link_.recv())
                .await
                .is_ok_and(|r| r.is_err())
            {
                break;
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
