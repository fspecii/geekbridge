// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted phone for the desktop apps: pairs with a running `geekbridge-service` (it approves
//! the code itself), allows Contacts, Calendar, Calendar editing and Calendar alerts for it, and then
//! answers like the Android app (`bridge_phone_sim::data::PhoneScript`) until stopped. It is how the
//! Contacts and Calendar windows are shown and screenshotted without an emulator.
//!
//!   cargo run -p bridge-phone-sim --example data_sim -- STATE_DIR CONTROL_PORT
//!
//! Made-up data only. Nothing here reads a real phone.
use bridge_core::ipc::{self, Feature, LinkPhase, PairMode, Request, Response, Token};
use bridge_kde as kde;
use bridge_phone_sim::data::{self, PhoneScript};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

fn call(port: u16, token: &Token, r: Request) -> Response {
    ipc::call(port, token, &r).expect("control call")
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let state_dir = PathBuf::from(args.next().expect("STATE_DIR"));
    let port: u16 = args.next().expect("CONTROL_PORT").parse().expect("port");
    let bytes = kde::fsutil::read_private_file(&ipc::token_path(&state_dir))
        .expect("token file")
        .expect("token exists");
    let token = Token::parse(&bytes).expect("token");

    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let id = creds.device_id().to_string();
    let (incoming, outgoing) = data::capabilities_with_photos();
    let mut config = kde::TransportConfig::default();
    config.address_policy.loopback = true;
    config.limits.packet_line = 2 * 1024 * 1024;
    let endpoint = kde::Endpoint::new(
        creds,
        "Pixel (simulated)",
        kde::DeviceType::Phone,
        incoming,
        outgoing,
        Arc::new(kde::MemoryTrustStore::new()),
        config,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let phone_port = listener.local_addr().unwrap().port();
    let accept = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        match endpoint.accept(stream).await.unwrap() {
            kde::Handshake::Pending(p) => p
                .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                .await
                .unwrap(),
            kde::Handshake::Trusted(t) => t,
        }
    });
    let t2 = token.clone();
    let id2 = id.clone();
    let drive = tokio::task::spawn_blocking(move || {
        call(
            port,
            &t2,
            Request::Connect {
                device_id: id2.clone(),
                endpoint: Some(ipc::Endpoint {
                    address: "127.0.0.1".parse().unwrap(),
                    port: phone_port,
                }),
            },
        );
        let wait = |f: &dyn Fn(&ipc::Status) -> bool| {
            for _ in 0..250 {
                if let Ok(s) = ipc::status(port, &t2)
                    && f(&s)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            panic!("timed out");
        };
        wait(&|s| {
            s.links
                .iter()
                .any(|l| l.device_id == id2 && l.phase == LinkPhase::Unpaired)
        });
        call(
            port,
            &t2,
            Request::Pair {
                device_id: id2.clone(),
                mode: PairMode::Request,
            },
        );
        wait(&|s| !s.challenges.is_empty());
        let challenge = ipc::status(port, &t2).unwrap().challenges[0]
            .challenge_id
            .clone();
        call(
            port,
            &t2,
            Request::Approve {
                challenge_id: challenge,
            },
        );
        wait(&|s| {
            s.links
                .iter()
                .any(|l| l.device_id == id2 && l.phase == LinkPhase::Connected)
        });
        for f in [
            Feature::Contacts,
            Feature::Calendar,
            Feature::CalendarEdit,
            Feature::CalendarAlerts,
            Feature::Sms,
            Feature::Photos,
        ] {
            call(
                port,
                &t2,
                Request::SetConsent {
                    device_id: id2.clone(),
                    feature: f,
                    enabled: true,
                },
            );
        }
    });
    let mut link = accept.await.unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut script = PhoneScript::demo(now);
    script.photos_on = true;
    drive.await.unwrap();
    eprintln!("data_sim: paired as {id}; answering until killed");
    while let Ok(ev) = link.recv().await {
        if let kde::LinkEvent::Packet(p) = ev {
            for (kind, body) in script.handle(&p.kind, &p.body) {
                if link.send(&kde::Packet::new(kind, body)).await.is_err() {
                    return;
                }
            }
        }
    }
}
