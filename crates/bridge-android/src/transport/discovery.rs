// SPDX-License-Identifier: GPL-3.0-or-later
//! Optional LAN discovery: KDE Connect identity datagrams over UDP.
//!
//! Discovery only ever *lists* what was heard; it never connects, pairs or trusts. Whoever heard
//! about a device still needs an explicit `transport_connect`, and trust still needs an explicit
//! pairing. A datagram is untrusted input: size-capped, parsed with the same identity rules as a
//! TCP identity, rate-limited, restricted by the address policy, and held in a small table that
//! forgets entries after [`TTL`].
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bridge_kde::{DeviceId, Identity, Packet};
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep};

use super::engine::Core;

/// Identity datagrams are one short JSON line; stock peers cap the same way.
pub(super) const MAX_DATAGRAM: usize = 8192;
const MAX_DISCOVERED: usize = 32;
const TTL: Duration = Duration::from_secs(120);
const ANNOUNCE_EVERY: Duration = Duration::from_secs(30);
/// Least gap between two announcements triggered by newcomers, so a chatty network cannot make us transmit
/// continuously.
const WAKE_GAP: Duration = Duration::from_secs(5);
const MAX_PER_SECOND: u32 = 64;
/// Ports below this belong to well-known services; a peer advertising one is ignored.
const MIN_TCP_PORT: u64 = 1024;

#[derive(Clone, Debug)]
pub(super) struct Discovered {
    pub device_id: DeviceId,
    pub name: String,
    pub device_type: &'static str,
    pub ip: IpAddr,
    pub tcp_port: u16,
    pub seen: Instant,
}

/// What `transport_start` achieved for discovery.
#[derive(Clone, Debug, Default)]
pub(super) struct Status {
    pub listening_port: Option<u16>,
    pub announcing: bool,
    pub error: Option<&'static str>,
}

impl Status {
    pub(super) fn to_json(&self) -> Value {
        json!({
            "listening": self.listening_port.is_some(),
            "port": self.listening_port,
            "announcing": self.announcing,
            "error": self.error,
        })
    }
}

/// The identity line other KDE Connect devices expect on UDP, plus the TCP port to dial.
pub(super) fn encode_announcement(identity: &Identity, tcp_port: u16) -> Vec<u8> {
    let mut packet = identity.for_broadcast().to_packet();
    packet.body.insert("tcpPort".into(), Value::from(tcp_port));
    packet.to_line()
}

/// Validate one received datagram. `None` for anything that is not a well-formed protocol 8
/// identity from some other device with a usable TCP port.
pub(super) fn parse_announcement(
    datagram: &[u8],
    from: IpAddr,
    local: &DeviceId,
    now: Instant,
) -> Option<Discovered> {
    if datagram.len() > MAX_DATAGRAM {
        return None;
    }
    let line = datagram.strip_suffix(b"\n").unwrap_or(datagram);
    let packet = Packet::from_line(line).ok()?;
    let identity = Identity::from_packet(&packet).ok()?;
    if identity.device_id == *local {
        return None;
    }
    let port = packet.body.get("tcpPort")?.as_u64()?;
    if !(MIN_TCP_PORT..=u64::from(u16::MAX)).contains(&port) {
        return None;
    }
    Some(Discovered {
        device_id: identity.device_id,
        name: identity.device_name,
        device_type: identity.device_type.as_str(),
        ip: from,
        tcp_port: u16::try_from(port).ok()?,
        seen: now,
    })
}

/// Insert or refresh, evicting the stalest entry when full.
pub(super) fn upsert(list: &mut Vec<Discovered>, d: Discovered) {
    if let Some(slot) = list.iter_mut().find(|e| e.device_id == d.device_id) {
        *slot = d;
        return;
    }
    if list.len() >= MAX_DISCOVERED
        && let Some(oldest) = list
            .iter()
            .enumerate()
            .min_by_key(|(_, e)| e.seen)
            .map(|(i, _)| i)
    {
        list.swap_remove(oldest);
    }
    list.push(d);
}

pub(super) fn to_json(list: &[Discovered], now: Instant, paired: &[DeviceId]) -> Vec<Value> {
    let mut fresh: Vec<&Discovered> = list
        .iter()
        .filter(|d| now.saturating_duration_since(d.seen) < TTL)
        .collect();
    fresh.sort_by_key(|d| std::cmp::Reverse(d.seen));
    fresh
        .into_iter()
        .map(|d| {
            json!({
                "device_id": d.device_id.as_str(),
                "name": d.name,
                "device_type": d.device_type,
                "ip": d.ip.to_string(),
                "tcp_port": d.tcp_port,
                "age_ms": u64::try_from(now.saturating_duration_since(d.seen).as_millis()).unwrap_or(u64::MAX),
                "paired": paired.contains(&d.device_id),
            })
        })
        .collect()
}

pub(super) async fn listen_loop(core: Arc<Core>, socket: UdpSocket) {
    let mut buf = vec![0u8; MAX_DATAGRAM + 1];
    let mut window = Instant::now();
    let mut seen_in_window = 0u32;
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(x) => x,
            Err(_) => {
                sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let now = Instant::now();
        if now.saturating_duration_since(window) >= Duration::from_secs(1) {
            window = now;
            seen_in_window = 0;
        }
        seen_in_window += 1;
        if seen_in_window > MAX_PER_SECOND {
            continue;
        }
        let Some(datagram) = buf.get(..n) else {
            continue;
        };
        if core.policy.check(from.ip()).is_err() {
            continue;
        }
        if let Some(d) = parse_announcement(datagram, from.ip(), core.local_id(), now) {
            core.record_discovery(d);
        }
    }
}

/// Announce now and then every [`ANNOUNCE_EVERY`] for as long as the transport runs. Only started
/// after the TCP listener is bound, so the port in the datagram is already accepting connections.
pub(super) async fn announce_loop<N>(
    socket: UdpSocket,
    target: SocketAddr,
    payload: impl Fn() -> Vec<u8>,
    all_linked: impl Fn() -> bool,
    newcomer: impl Fn() -> N,
) where
    N: std::future::Future<Output = ()>,
{
    loop {
        // Every paired computer already has a trusted link: nobody needs the broadcast, and each one is a
        // radio wake-up. A lost link brings the announcements back within one round (the redials run at once).
        if !all_linked() {
            let _ = socket.send_to(&payload(), target).await;
        }
        // An unpaired computer that announces itself is answered even then (it can only find this phone to
        // pair through an announcement), at most once per `WAKE_GAP`.
        tokio::select! {
            () = sleep(ANNOUNCE_EVERY) => {}
            () = newcomer() => {
                let _ = socket.send_to(&payload(), target).await;
                sleep(WAKE_GAP).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_kde::DeviceType;

    fn ident(c: char) -> Identity {
        Identity::local(
            DeviceId::parse(&c.to_string().repeat(32)).unwrap(),
            "Desk",
            DeviceType::Desktop,
            vec!["kdeconnect.ping".into()],
            vec!["kdeconnect.ping".into()],
        )
        .unwrap()
    }

    fn local() -> DeviceId {
        DeviceId::parse(&"l".repeat(32)).unwrap()
    }

    const IP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 7));

    #[test]
    fn round_trips_and_marks_nothing_trusted() {
        let line = encode_announcement(&ident('a'), 1716);
        assert!(line.ends_with(b"\n"));
        let d = parse_announcement(&line, IP, &local(), Instant::now()).unwrap();
        assert_eq!(d.device_id.as_str(), "a".repeat(32));
        assert_eq!((d.tcp_port, d.ip, d.device_type), (1716, IP, "desktop"));
    }

    #[test]
    fn the_broadcast_names_no_geekbridge_feature() {
        let mut id = ident('a');
        id.incoming_capabilities.extend([
            "geekbridge.photos.list".into(),
            "geekbridge.heartbeat".into(),
        ]);
        id.outgoing_capabilities.push("geekbridge.debug.log".into());
        let line = encode_announcement(&id, 1716);
        let text = String::from_utf8(line.clone()).unwrap();
        assert!(!text.contains("geekbridge"), "{text}");
        assert!(text.contains("kdeconnect.ping"));
        assert!(parse_announcement(&line, IP, &local(), Instant::now()).is_some());
    }

    #[test]
    fn rejects_everything_else() {
        let now = Instant::now();
        let good = encode_announcement(&ident('a'), 1716);
        // Own announcement echoed back.
        let own = encode_announcement(&ident('l'), 1716);
        assert!(parse_announcement(&own, IP, &local(), now).is_none());
        // Missing, low, high and non-integer ports.
        for port in [
            json!(null),
            json!(80),
            json!(1023),
            json!(70000),
            json!("1716"),
            json!(1716.5),
        ] {
            let mut p = Packet::from_line(good.strip_suffix(b"\n").unwrap()).unwrap();
            p.body.insert("tcpPort".into(), port);
            assert!(parse_announcement(&p.to_line(), IP, &local(), now).is_none());
        }
        let mut p = Packet::from_line(good.strip_suffix(b"\n").unwrap()).unwrap();
        p.body.remove("tcpPort");
        assert!(parse_announcement(&p.to_line(), IP, &local(), now).is_none());
        // Wrong protocol version, wrong type, garbage, oversize.
        let mut p = Packet::from_line(good.strip_suffix(b"\n").unwrap()).unwrap();
        p.body.insert("protocolVersion".into(), json!(7));
        assert!(parse_announcement(&p.to_line(), IP, &local(), now).is_none());
        let mut p = Packet::from_line(good.strip_suffix(b"\n").unwrap()).unwrap();
        p.kind = "kdeconnect.ping".into();
        assert!(parse_announcement(&p.to_line(), IP, &local(), now).is_none());
        assert!(parse_announcement(b"not json", IP, &local(), now).is_none());
        assert!(parse_announcement(&[], IP, &local(), now).is_none());
        let mut big = good.clone();
        big.resize(MAX_DATAGRAM + 1, b' ');
        assert!(parse_announcement(&big, IP, &local(), now).is_none());
    }

    #[test]
    fn table_is_bounded_deduplicated_and_expires() {
        let now = Instant::now();
        let mut list = Vec::new();
        for i in 0..(MAX_DISCOVERED + 10) {
            let id = format!("{i:032}");
            let mut d =
                parse_announcement(&encode_announcement(&ident('a'), 1716), IP, &local(), now)
                    .unwrap();
            d.device_id = DeviceId::parse(&id).unwrap();
            upsert(&mut list, d);
        }
        assert_eq!(list.len(), MAX_DISCOVERED);
        let again = list[0].clone();
        upsert(&mut list, again);
        assert_eq!(list.len(), MAX_DISCOVERED);
        assert_eq!(to_json(&list, now, &[]).len(), MAX_DISCOVERED);
        assert!(to_json(&list, now + TTL, &[]).is_empty());
    }

    #[tokio::test]
    async fn announcements_pause_while_every_paired_computer_is_linked() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = rx.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let linked = Arc::new(AtomicBool::new(true));
        let flag = linked.clone();
        let task = tokio::spawn(announce_loop(
            tx,
            target,
            || b"hello".to_vec(),
            move || flag.load(Ordering::Relaxed),
            std::future::pending::<()>,
        ));
        let mut buf = [0u8; 16];
        let quiet = tokio::time::timeout(Duration::from_millis(300), rx.recv_from(&mut buf)).await;
        assert!(quiet.is_err(), "nothing is sent while linked");
        task.abort();
        linked.store(false, Ordering::Relaxed);
        let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let flag = linked.clone();
        let task = tokio::spawn(announce_loop(
            tx,
            target,
            || b"hello".to_vec(),
            move || flag.load(Ordering::Relaxed),
            std::future::pending::<()>,
        ));
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv_from(&mut buf)).await;
        assert!(got.is_ok(), "announces at once when not linked");
        task.abort();
    }
    /// A computer that is not paired yet finds this phone only through an announcement: while every paired
    /// computer is linked the loop is quiet, but a newcomer's announcement is answered at once.
    #[tokio::test]
    async fn a_newcomer_is_answered_even_while_every_paired_computer_is_linked() {
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = rx.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let wake = Arc::new(tokio::sync::Notify::new());
        let w = wake.clone();
        let task = tokio::spawn(announce_loop(
            tx,
            target,
            || b"hello".to_vec(),
            || true,
            move || {
                let w = w.clone();
                async move { w.notified().await }
            },
        ));
        let mut buf = [0u8; 16];
        let quiet = tokio::time::timeout(Duration::from_millis(300), rx.recv_from(&mut buf)).await;
        assert!(quiet.is_err(), "quiet while linked");
        wake.notify_one();
        let got = tokio::time::timeout(Duration::from_secs(2), rx.recv_from(&mut buf)).await;
        assert!(got.is_ok(), "answers the newcomer");
        task.abort();
    }
}
