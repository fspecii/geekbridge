// SPDX-License-Identifier: GPL-3.0-or-later
//! The effortless-pairing parts of the daemon: QR tokens, the "Test connection" check, and the LAN
//! announcement that lets phones list this computer by name. A child module of `daemon`, so it can
//! use the daemon's private state without widening it.

use super::*;
use bridge_core::ipc::PairQr;
use bridge_core::pairing_help::{ConnectionTest, Measured, Probe, build_report};
use bridge_kde::qrpair::{QrPayload, TOKEN_TTL, now_ms as unix_ms};
use std::net::Ipv4Addr;
use tokio::net::UdpSocket;

/// How often the identity goes out on the LAN.
const ANNOUNCE_EVERY: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// Addresses written into one QR code.
const MAX_QR_ADDRESSES: usize = 4;

/// The addresses this computer is probably reachable on: what the OS would use to leave for a
/// private network. Nothing is sent; connecting a UDP socket only selects a route.
pub(super) fn lan_addresses() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for probe in [
        "192.0.2.1:9",
        "10.255.255.254:9",
        "172.31.255.254:9",
        "192.168.255.254:9",
    ] {
        let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if socket.connect(probe).is_err() {
            continue;
        }
        if let Ok(local) = socket.local_addr() {
            let ip = local.ip();
            if !ip.is_loopback() && !ip.is_unspecified() && !out.contains(&ip) {
                out.push(ip);
            }
        }
    }
    out
}

/// A VPN-looking interface is up (Linux only; other systems do not report one here).
fn vpn_interface() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(dir) = std::fs::read_dir("/sys/class/net") else {
            return false;
        };
        dir.flatten().any(|e| {
            let name = e.file_name().to_string_lossy().to_ascii_lowercase();
            let vpn = [
                "tun",
                "tap",
                "wg",
                "tailscale",
                "ppp",
                "zt",
                "nordlynx",
                "proton",
            ]
            .iter()
            .any(|p| name.starts_with(p));
            vpn && std::fs::read_to_string(e.path().join("operstate"))
                .map(|s| s.trim() != "down")
                .unwrap_or(false)
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

impl Daemon {
    pub fn own_cert_sha256(&self) -> String {
        self.endpoint.credentials().cert().fingerprint_hex()
    }

    fn tokens(&self) -> MutexGuard<'_, kde::qrpair::TokenBook> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Issue a QR token and the text to put in the code.
    pub(super) fn pair_qr(&self) -> Result<PairQr, ErrorBody> {
        let Some(listener) = *self.inbound.lock().unwrap_or_else(|e| e.into_inner()) else {
            return Err(err(
                ErrorCode::Unsupported,
                "This computer is not accepting phone connections, so a QR code would not work. Start the service without --no-listen-inbound.",
            ));
        };
        let mut addresses: Vec<SocketAddr> = lan_addresses()
            .into_iter()
            .map(|ip| SocketAddr::new(ip, listener.port()))
            .collect();
        // Developer topologies (emulator NAT, loopback tests) name the address the phone must use.
        if self.options.dev_loopback
            && let Ok(list) = std::env::var("GEEKBRIDGE_QR_ADDRESSES")
        {
            addresses = list
                .split(',')
                .filter_map(|a| a.trim().parse().ok())
                .collect();
        }
        if addresses.is_empty() && self.options.dev_loopback {
            addresses.push(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), listener.port()));
        }
        addresses.truncate(MAX_QR_ADDRESSES);
        if addresses.is_empty() {
            return Err(err(
                ErrorCode::Failed,
                "This computer has no local network address. Connect to Wi-Fi or Ethernet first.",
            ));
        }
        let (token, _) = self
            .tokens()
            .issue(unix_ms(), TOKEN_TTL)
            .map_err(|_| err(ErrorCode::Failed, "could not create a pairing code"))?;
        let payload = QrPayload {
            device_id: self.endpoint.credentials().device_id().to_string(),
            name: self.options.device_name.clone(),
            cert_sha256: self.own_cert_sha256(),
            addresses: addresses.clone(),
            token,
        }
        .encode();
        Ok(PairQr {
            payload,
            expires_in_ms: TOKEN_TTL.as_millis() as u64,
            computer_name: self.options.device_name.clone(),
            addresses: addresses.iter().map(SocketAddr::to_string).collect(),
        })
    }

    pub(super) fn pair_qr_cancel(&self) {
        self.tokens().revoke_all();
    }

    /// Decide a pairing request that carried a QR token.
    pub(super) fn redeem_qr(&self, device_id: &str, token: &str) -> bool {
        // Wrong tokens are counted per source address, so a stranger cannot revoke the owner's code.
        let source = self
            .core()
            .links
            .get(device_id)
            .and_then(|l| l.status.peer_ip);
        let result = self.tokens().redeem_from(token, unix_ms(), source);
        let mut core = self.core();
        match result {
            Ok(()) => {
                core.emit(Some(device_id), EventKind::QrAccepted);
                true
            }
            Err(e) => {
                core.emit(
                    Some(device_id),
                    EventKind::QrRefused {
                        reason: e.message().to_owned(),
                    },
                );
                false
            }
        }
    }

    /// The "Test connection" diagnostic.
    pub(super) async fn test_connection(
        &self,
        device_id: Option<&str>,
        endpoint: Option<Endpoint>,
    ) -> Result<ConnectionTest, ErrorBody> {
        let target = match (endpoint, device_id) {
            (Some(e), _) => Some((e.address, e.port)),
            (None, Some(id)) => {
                parse_id(id)?;
                let discovery = self.discovery.read().await;
                discovery
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map(|d| (d.address, d.tcp_port))
            }
            // No phone named: the most recently heard one.
            (None, None) => {
                let discovery = self.discovery.read().await;
                discovery
                    .devices
                    .iter()
                    .filter(|d| d.device_type == "phone" || d.device_type == "tablet")
                    .max_by_key(|d| d.last_seen_unix_ms)
                    .map(|d| (d.address, d.tcp_port))
            }
        };
        let listening = self
            .inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        let probe = match target {
            None => Probe::NotTried,
            Some((ip, port)) => {
                if self.check_endpoint(ip, port).is_err() {
                    Probe::NoRoute
                } else {
                    match timeout(PROBE_TIMEOUT, TcpStream::connect(SocketAddr::new(ip, port)))
                        .await
                    {
                        Err(_) => Probe::TimedOut,
                        Ok(Ok(_)) => Probe::Connected,
                        Ok(Err(e)) => match e.kind() {
                            std::io::ErrorKind::ConnectionRefused => Probe::Refused,
                            std::io::ErrorKind::TimedOut => Probe::TimedOut,
                            std::io::ErrorKind::NetworkUnreachable
                            | std::io::ErrorKind::HostUnreachable => Probe::NoRoute,
                            _ => Probe::Other,
                        },
                    }
                }
            }
        };
        let mut own = lan_addresses();
        if own.is_empty() && self.options.dev_loopback {
            own.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        }
        Ok(build_report(&Measured {
            listening,
            own_addresses: own,
            peer_address: target.map(|t| t.0),
            probe,
            vpn_interface: vpn_interface(),
        }))
    }

    /// The identity datagram phones list this computer from.
    pub fn announcement(&self, tcp_port: u16) -> Vec<u8> {
        let mut packet = self.endpoint.current_identity().to_packet();
        packet.body.insert("tcpPort".into(), tcp_port.into());
        packet.to_line()
    }

    /// Announce this computer on the LAN until dropped. Hearing it grants nobody anything: a phone
    /// still has to dial, and pairing still needs approval.
    pub async fn announce_loop(self: Arc<Self>, discovery_port: u16) {
        let Some(listener) = *self.inbound.lock().unwrap_or_else(|e| e.into_inner()) else {
            return;
        };
        let Ok(socket) = UdpSocket::bind("0.0.0.0:0").await else {
            return;
        };
        if socket.set_broadcast(true).is_err() {
            return;
        }
        let port = listener.port();
        let socket = &socket;
        self.announce_every(port, ANNOUNCE_EVERY, |line| async move {
            let mut targets = vec![SocketAddr::from((Ipv4Addr::BROADCAST, discovery_port))];
            for ip in lan_addresses() {
                if let IpAddr::V4(v4) = ip {
                    let o = v4.octets();
                    targets.push(SocketAddr::from((
                        Ipv4Addr::new(o[0], o[1], o[2], 255),
                        discovery_port,
                    )));
                }
            }
            for target in targets {
                let _ = socket.send_to(&line, target).await;
            }
        })
        .await
    }

    /// Hand `send` a fresh announcement every `every`, for ever. Built each time, so a rename
    /// (`SetDeviceName`) shows up in the next announcement and not only after a restart.
    pub async fn announce_every<F, Fut>(&self, tcp_port: u16, every: Duration, mut send: F)
    where
        F: FnMut(Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        loop {
            send(self.announcement(tcp_port)).await;
            tokio::time::sleep(every).await;
        }
    }
}
