// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers shared by the chaos tests: a stand-in phone that sits behind a fault proxy and survives
//! failed handshakes, and the short / soak switch.
#![allow(dead_code)]

use super::*;
pub use bridge_chaos::{FaultProxy, Faults};
use tokio::sync::mpsc;

/// `GEEKBRIDGE_CHAOS=soak` (scripts/soak.sh) runs the long variants; anything else is the short,
/// CI-friendly mode.
pub fn soak() -> bool {
    std::env::var("GEEKBRIDGE_CHAOS").is_ok_and(|v| v == "soak")
        || std::env::var("GEEKBRIDGE_SOAK").is_ok_and(|v| v == "1")
}

pub fn scaled(short: usize, long: usize) -> usize {
    std::env::var("GEEKBRIDGE_CHAOS_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if soak() { long } else { short })
}

pub fn thread_count() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, |d| d.count())
}

/// A phone that accepts any number of dials on one port (reachable through `proxy`), pairs when
/// asked and hands every trusted link to `links`. A handshake that fails (injected faults) just
/// waits for the next dial.
pub struct PhoneServer {
    pub proxy: FaultProxy,
    pub links: mpsc::UnboundedReceiver<kde::TrustedLink>,
    pub failed: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for PhoneServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PhoneServer {
    pub async fn start(phone: &Arc<Phone>, faults: Faults, seed: u64) -> Self {
        let l = listener().await;
        let proxy = FaultProxy::start_with(l.local_addr().unwrap(), faults, seed).await;
        let (tx, links) = mpsc::unbounded_channel();
        let failed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (p, f) = (phone.clone(), failed.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = l.accept().await else {
                    return;
                };
                // Handshake in a task of its own: a stalled one must not block the next dial.
                let (p, tx, f) = (p.clone(), tx.clone(), f.clone());
                tokio::spawn(async move {
                    let hs = match tokio::time::timeout(
                        Duration::from_secs(30),
                        p.endpoint.accept(stream),
                    )
                    .await
                    {
                        Ok(Ok(hs)) => hs,
                        _ => {
                            f.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            return;
                        }
                    };
                    let link = match hs {
                        kde::Handshake::Trusted(l) => l,
                        kde::Handshake::Pending(pending) => {
                            match pending
                                .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                                .await
                            {
                                Ok(l) => l,
                                Err(_) => {
                                    f.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                    return;
                                }
                            }
                        }
                    };
                    let _ = tx.send(link);
                });
            }
        });
        Self {
            proxy,
            links,
            failed,
            task,
        }
    }

    pub fn endpoint(&self) -> ipc::Endpoint {
        ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port: self.proxy.port(),
        }
    }

    pub async fn next_link(&mut self, within: Duration) -> kde::TrustedLink {
        timeout(within, self.links.recv())
            .await
            .expect("the phone never got a trusted link")
            .expect("phone server stopped")
    }
}

/// Ask the service to dial `server` (through its proxy).
pub async fn dial(rig: &Rig, phone: &Phone, server: &PhoneServer) {
    rig.ok(Request::Connect {
        device_id: phone.id(),
        endpoint: Some(server.endpoint()),
    })
    .await;
}

/// The whole pairing dance against a (possibly faulty) phone server; returns the phone's link.
pub async fn pair_through(
    rig: &Rig,
    phone: &Arc<Phone>,
    server: &mut PhoneServer,
) -> kde::TrustedLink {
    dial(rig, phone, server).await;
    rig.wait("unpaired link", |s| {
        link_of(s, &phone.id()).is_some_and(|l| l.phase == LinkPhase::Unpaired)
    })
    .await;
    rig.ok(Request::Pair {
        device_id: phone.id(),
        mode: PairMode::Request,
    })
    .await;
    let status = rig.wait("challenge", |s| !s.challenges.is_empty()).await;
    rig.ok(Request::Approve {
        challenge_id: status.challenges[0].challenge_id.clone(),
    })
    .await;
    let link = server.next_link(Duration::from_secs(30)).await;
    wait_connected(rig, phone).await;
    link
}
