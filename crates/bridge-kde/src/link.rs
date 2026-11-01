//! Protocol 8 connection establishment and the links it produces.
//!
//! Roles (the inverse of what intuition suggests):
//!
//! * the side that **initiates** the TCP connection is the **TLS server**;
//! * the side that **accepts** the TCP connection is the **TLS client**.
//!
//! Sequence: the initiator sends one plaintext identity line addressed to the
//! target device; TLS runs with both sides presenting certificates; then both
//! sides send their identity again inside TLS, and the secure copy must agree
//! with the plaintext claim and with the certificate.
//!
//! The result is either a [`TrustedLink`] (the peer's certificate is byte-equal
//! to the one pinned at pairing) or a [`PendingLink`], which can do nothing but
//! pair. Feature packets can only ever be sent or received on a `TrustedLink`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::cert::{CertInfo, DeviceCredentials, unix_now};
use crate::error::{KdeError, Result};
use crate::identity::{DeviceId, DeviceType, Identity, PROTOCOL_VERSION};
use crate::packet::{LineReader, Packet, read_line_unbuffered, write_packet};
use crate::pairing::{self, PAIR_TYPE, PairingConfig};
use crate::payload::{PayloadConfig, PayloadContext};
use crate::store::TrustStore;
use crate::tls::{
    PeerPolicy, PeerVerifier, TlsVersions, client_config, handshake_error, server_config,
};

pub(crate) type BoxRead = Box<dyn AsyncRead + Send + Unpin>;
pub(crate) type BoxWrite = Box<dyn AsyncWrite + Send + Unpin>;

#[derive(Clone, Debug)]
pub struct Timeouts {
    pub connect: Duration,
    pub plaintext_identity: Duration,
    pub tls_handshake: Duration,
    pub secure_identity: Duration,
    pub write: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            plaintext_identity: Duration::from_secs(5),
            tls_handshake: Duration::from_secs(10),
            secure_identity: Duration::from_secs(5),
            write: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    /// Longest identity line accepted (plaintext or in TLS). Desktop peers
    /// accept at most 8192, so that is what we accept.
    pub identity_line: usize,
    /// Longest identity line we will send; stays well under the 8192 other
    /// implementations accept.
    pub outgoing_identity_line: usize,
    /// Longest post-handshake packet line (stock peers use 32 MiB).
    pub packet_line: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            identity_line: 8192,
            outgoing_identity_line: 6144,
            packet_line: 32 * 1024 * 1024,
        }
    }
}

/// Which source addresses an inbound connection may come from.
#[derive(Clone, Debug)]
pub struct AddressPolicy {
    /// Allow private, link-local, CGNAT and unique-local addresses.
    pub local_networks: bool,
    pub loopback: bool,
}

impl Default for AddressPolicy {
    fn default() -> Self {
        Self {
            local_networks: true,
            loopback: false,
        }
    }
}

impl AddressPolicy {
    pub fn check(&self, ip: IpAddr) -> Result<()> {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
            v4 => v4,
        };
        let ok = if ip.is_loopback() {
            self.loopback
        } else {
            self.local_networks
                && match ip {
                    IpAddr::V4(a) => {
                        let o = a.octets();
                        a.is_private() || a.is_link_local() || (o[0] == 100 && (o[1] & 0xc0) == 64)
                    }
                    IpAddr::V6(a) => {
                        let s = a.segments();
                        (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
                    }
                }
        };
        if ok {
            Ok(())
        } else {
            Err(KdeError::AddressRejected(ip))
        }
    }
}

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub timeouts: Timeouts,
    pub limits: Limits,
    pub tls_versions: TlsVersions,
    pub address_policy: AddressPolicy,
    /// Concurrent unpaired (pending) connections, matching stock peers' cap.
    pub max_unpaired: usize,
    pub pairing: PairingConfig,
    /// Payload (file transfer) socket behaviour for trusted links.
    pub payload: PayloadConfig,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            timeouts: Timeouts::default(),
            limits: Limits::default(),
            tls_versions: TlsVersions::Tls12And13,
            address_policy: AddressPolicy::default(),
            max_unpaired: 42,
            pairing: PairingConfig::default(),
            payload: PayloadConfig::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// We opened the TCP connection and therefore acted as the TLS server.
    Initiator,
    /// We accepted the TCP connection and therefore acted as the TLS client.
    Acceptor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

#[derive(Clone, Debug)]
pub struct LinkInfo {
    pub connection_id: ConnectionId,
    pub role: Role,
    /// The peer's identity as received inside TLS and cross-checked.
    pub peer: Identity,
    pub peer_cert: CertInfo,
    pub tls_version: String,
    pub cipher_suite: String,
    pub peer_ip: IpAddr,
}

pub(crate) struct Conn {
    pub info: LinkInfo,
    pub reader: LineReader<BoxRead>,
    pub writer: BoxWrite,
    pub limits: Limits,
    pub write_timeout: Duration,
    pub payload: PayloadContext,
}

impl Conn {
    pub(crate) async fn send(&mut self, packet: &Packet) -> Result<()> {
        let deadline = Instant::now() + self.write_timeout;
        write_packet(&mut self.writer, packet, self.limits.packet_line, deadline).await
    }

    pub(crate) async fn recv(&mut self) -> Result<Packet> {
        let line = self.reader.read_line(self.limits.packet_line).await?;
        Packet::from_line(&line)
    }
}

pub enum Handshake {
    Trusted(TrustedLink),
    Pending(PendingLink),
}

impl Handshake {
    /// Record the local IP of the TCP connection for later payload dials.
    fn with_local_ip(mut self, ip: Option<IpAddr>) -> Self {
        match &mut self {
            Self::Trusted(l) => l.conn.payload.local_ip = ip,
            Self::Pending(l) => l.conn.payload.local_ip = ip,
        }
        self
    }

    pub fn info(&self) -> &LinkInfo {
        match self {
            Self::Trusted(l) => l.info(),
            Self::Pending(l) => l.info(),
        }
    }
}

/// An authenticated link to a paired device. Constructible only by a handshake
/// that matched the pinned certificate or by a completed pairing.
pub struct TrustedLink {
    conn: Conn,
    store: Arc<dyn TrustStore>,
}

#[derive(Debug)]
pub enum LinkEvent {
    Packet(Packet),
    /// The peer sent `{pair:false}`; the trust record has already been deleted.
    PeerUnpaired,
    /// The peer asks to pair again (it has forgotten us). Our record and pin
    /// are untouched; unpair locally and pair again to accept.
    PeerRequestedRePair {
        timestamp: i64,
    },
}

impl TrustedLink {
    pub(crate) fn new(conn: Conn, store: Arc<dyn TrustStore>) -> Self {
        Self { conn, store }
    }

    pub fn info(&self) -> &LinkInfo {
        &self.conn.info
    }

    /// Send a feature packet. Protocol packets are reserved to this crate.
    pub async fn send(&mut self, packet: &Packet) -> Result<()> {
        if packet.kind == PAIR_TYPE || packet.kind == crate::identity::IDENTITY_TYPE {
            return Err(KdeError::MalformedPacket(
                "protocol packet types are reserved".into(),
            ));
        }
        if packet.payload_size.is_some() || packet.payload_transfer_info.is_some() {
            return Err(KdeError::MalformedPacket(
                "use send_with_payload for packets that announce a payload".into(),
            ));
        }
        self.conn.send(packet).await
    }

    /// Send a packet that announces a payload (`payloadSize` + `payloadTransferInfo.port`). The
    /// caller must already be listening on that port (see [`PayloadContext::listen`]) and serve the
    /// payload after this returns.
    pub async fn send_with_payload(&mut self, packet: &Packet) -> Result<()> {
        if packet.kind == PAIR_TYPE || packet.kind == crate::identity::IDENTITY_TYPE {
            return Err(KdeError::MalformedPacket(
                "protocol packet types are reserved".into(),
            ));
        }
        if packet.payload_size.is_none() || packet.payload_transfer_info.is_none() {
            return Err(KdeError::MalformedPacket(
                "packet does not announce a payload".into(),
            ));
        }
        self.conn.send(packet).await
    }

    /// Credentials for opening payload sockets to this (pinned) peer.
    pub fn payload_context(&self) -> PayloadContext {
        self.conn.payload.clone()
    }

    /// Next event. Cancel-safe. Any framing or parse error is fatal.
    pub async fn recv(&mut self) -> Result<LinkEvent> {
        loop {
            let packet = self.conn.recv().await?;
            if packet.kind == crate::identity::IDENTITY_TYPE {
                return Err(KdeError::MalformedPacket(
                    "identity packet after the handshake".into(),
                ));
            }
            if packet.kind != PAIR_TYPE {
                return Ok(LinkEvent::Packet(packet));
            }
            let body = pairing::PairBody::parse(&packet)?;
            match (body.pair, body.timestamp) {
                (false, _) => {
                    self.store.remove(&self.conn.info.peer.device_id)?;
                    return Ok(LinkEvent::PeerUnpaired);
                }
                // Duplicate accept on an already paired link: ignore (stock Android would
                // drop trust here, which is why we never send one).
                (true, None) => continue,
                (true, Some(ts)) => return Ok(LinkEvent::PeerRequestedRePair { timestamp: ts }),
            }
        }
    }

    /// Delete the local trust record first, then tell the peer (best effort).
    pub async fn unpair(mut self) -> Result<()> {
        self.store.remove(&self.conn.info.peer.device_id)?;
        let _ = self.conn.send(&pairing::pair_packet(false, None)).await;
        Ok(())
    }
}

/// A TLS-secured link whose peer is not (yet) trusted. The certificate can be
/// inspected and a pairing can be run; nothing else is exposed.
pub struct PendingLink {
    pub(crate) conn: Conn,
    pub(crate) store: Arc<dyn TrustStore>,
    pub(crate) own_cert: CertInfo,
    pub(crate) pairing: PairingConfig,
    pub(crate) _permit: Option<OwnedSemaphorePermit>,
}

impl PendingLink {
    pub fn info(&self) -> &LinkInfo {
        &self.conn.info
    }

    /// Put a one-time QR token in our pair request. Call only after checking that the peer's
    /// certificate on this link is the one the QR code pinned ([`crate::qrpair`]).
    pub fn with_pair_token(mut self, token: String) -> Self {
        self.pairing.token = Some(token);
        self
    }

    /// Close without pairing. Nothing is persisted.
    pub fn close(self) {}
}

/// Our side of the transport: identity, credentials, trust store, limits.
pub struct Endpoint {
    local: Identity,
    creds: Arc<DeviceCredentials>,
    store: Arc<dyn TrustStore>,
    config: TransportConfig,
    next_connection: AtomicU64,
    unpaired_slots: Arc<Semaphore>,
}

struct TlsFacts {
    peer_cert: Vec<u8>,
    version: String,
    cipher: String,
}

fn facts(cs: &rustls::CommonState) -> Result<TlsFacts> {
    let peer_cert = cs
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| c.as_ref().to_vec())
        .ok_or_else(|| KdeError::Tls("peer presented no certificate".into()))?;
    Ok(TlsFacts {
        peer_cert,
        version: cs
            .protocol_version()
            .map(|v| format!("{v:?}"))
            .unwrap_or_default(),
        cipher: cs
            .negotiated_cipher_suite()
            .map(|s| format!("{:?}", s.suite()))
            .unwrap_or_default(),
    })
}

impl Endpoint {
    pub fn new(
        creds: Arc<DeviceCredentials>,
        device_name: &str,
        device_type: DeviceType,
        incoming: Vec<String>,
        outgoing: Vec<String>,
        store: Arc<dyn TrustStore>,
        config: TransportConfig,
    ) -> Result<Self> {
        let local = Identity::local(
            creds.device_id().clone(),
            device_name,
            device_type,
            incoming,
            outgoing,
        )?;
        let slots = config.max_unpaired;
        Ok(Self {
            local,
            creds,
            store,
            config,
            next_connection: AtomicU64::new(1),
            unpaired_slots: Arc::new(Semaphore::new(slots)),
        })
    }

    pub fn local_identity(&self) -> &Identity {
        &self.local
    }

    pub fn credentials(&self) -> &DeviceCredentials {
        &self.creds
    }

    pub fn config(&self) -> &TransportConfig {
        &self.config
    }

    /// Dial an explicitly chosen endpoint as TCP initiator (TLS server).
    pub async fn connect(&self, addr: SocketAddr, target: &DeviceId) -> Result<Handshake> {
        let stream = timeout(self.config.timeouts.connect, TcpStream::connect(addr))
            .await
            .map_err(|_| KdeError::Timeout("TCP connect"))??;
        configure_tcp(&stream)?;
        let local = stream.local_addr().ok().map(|a| a.ip());
        self.initiate(stream, addr.ip(), target)
            .await
            .map(|h| h.with_local_ip(local))
    }

    /// Handle an accepted TCP connection as TLS client. The address policy is
    /// applied before a single byte is read.
    pub async fn accept(&self, stream: TcpStream) -> Result<Handshake> {
        let ip = stream.peer_addr()?.ip();
        self.config.address_policy.check(ip)?;
        configure_tcp(&stream)?;
        let local = stream.local_addr().ok().map(|a| a.ip());
        self.accept_io(stream, ip)
            .await
            .map(|h| h.with_local_ip(local))
    }

    /// TCP-initiator side over any byte stream.
    pub async fn initiate<IO>(
        &self,
        mut io: IO,
        peer_ip: IpAddr,
        target: &DeviceId,
    ) -> Result<Handshake>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let t = &self.config.timeouts;
        if *target == self.local.device_id {
            return Err(KdeError::TargetMismatch(
                "refusing to connect to ourselves".into(),
            ));
        }
        // Trust is decided from the device id we chose to dial, then enforced by the pin.
        let stored = self.store.get(target)?;
        let (policy, permit) = match &stored {
            Some(p) => (PeerPolicy::Pinned(p.cert.clone()), None),
            None => (PeerPolicy::Inspect, Some(self.take_unpaired_slot()?)),
        };

        let first = self.local.addressed_to(target).to_packet();
        write_packet(
            &mut io,
            &first,
            self.config.limits.outgoing_identity_line,
            Instant::now() + t.write,
        )
        .await?;

        let (verifier, failure) = PeerVerifier::new(policy, target.clone());
        let cfg = server_config(&self.creds, verifier, self.config.tls_versions)?;
        let tls = timeout(t.tls_handshake, TlsAcceptor::from(cfg).accept(io))
            .await
            .map_err(|_| KdeError::Timeout("TLS handshake"))?
            .map_err(|e| handshake_error(e, &failure, target))?;
        let tls_facts = facts(tls.get_ref().1)?;
        self.finish(
            tls,
            tls_facts,
            Role::Initiator,
            target.clone(),
            PROTOCOL_VERSION,
            stored.map(|p| p.cert),
            peer_ip,
            permit,
        )
        .await
    }

    /// TCP-acceptor side over any byte stream.
    pub async fn accept_io<IO>(&self, mut io: IO, peer_ip: IpAddr) -> Result<Handshake>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let t = &self.config.timeouts;
        let line = read_line_unbuffered(
            &mut io,
            self.config.limits.identity_line,
            Instant::now() + t.plaintext_identity,
        )
        .await?;
        let claimed = Identity::from_packet(&Packet::from_line(&line)?)?;

        match &claimed.target_device_id {
            Some(t) if *t == self.local.device_id => {}
            Some(_) => {
                return Err(KdeError::TargetMismatch(
                    "addressed to another device".into(),
                ));
            }
            None => return Err(KdeError::TargetMismatch("targetDeviceId missing".into())),
        }
        match claimed.target_protocol_version {
            Some(PROTOCOL_VERSION) => {}
            Some(v) => {
                return Err(KdeError::TargetMismatch(format!(
                    "addressed to protocol version {v}"
                )));
            }
            None => {
                return Err(KdeError::TargetMismatch(
                    "targetProtocolVersion missing".into(),
                ));
            }
        }
        if claimed.device_id == self.local.device_id {
            return Err(KdeError::TargetMismatch(
                "peer claims our own device id".into(),
            ));
        }

        let stored = self.store.get(&claimed.device_id)?;
        if let Some(p) = &stored
            && p.protocol_version > claimed.protocol_version
        {
            return Err(KdeError::ProtocolDowngrade {
                stored: p.protocol_version,
                claimed: claimed.protocol_version,
            });
        }
        let (policy, permit) = match &stored {
            Some(p) => (PeerPolicy::Pinned(p.cert.clone()), None),
            None => (PeerPolicy::Inspect, Some(self.take_unpaired_slot()?)),
        };

        let (verifier, failure) = PeerVerifier::new(policy, claimed.device_id.clone());
        let cfg = client_config(&self.creds, verifier, self.config.tls_versions)?;
        let name = rustls::pki_types::ServerName::IpAddress(peer_ip.into());
        let tls = timeout(t.tls_handshake, TlsConnector::from(cfg).connect(name, io))
            .await
            .map_err(|_| KdeError::Timeout("TLS handshake"))?
            .map_err(|e| handshake_error(e, &failure, &claimed.device_id))?;
        let tls_facts = facts(tls.get_ref().1)?;
        self.finish(
            tls,
            tls_facts,
            Role::Acceptor,
            claimed.device_id,
            claimed.protocol_version,
            stored.map(|p| p.cert),
            peer_ip,
            permit,
        )
        .await
    }

    fn take_unpaired_slot(&self) -> Result<OwnedSemaphorePermit> {
        self.unpaired_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| KdeError::TooManyConnections)
    }

    /// Everything after TLS: exchange secure identities (ours first, with no
    /// work in between), cross-check them, and classify the link.
    #[allow(clippy::too_many_arguments)]
    async fn finish<S>(
        &self,
        mut tls: S,
        tls_facts: TlsFacts,
        role: Role,
        claimed_id: DeviceId,
        claimed_version: u32,
        pinned: Option<CertInfo>,
        peer_ip: IpAddr,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<Handshake>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let t = &self.config.timeouts;
        let l = &self.config.limits;
        write_packet(
            &mut tls,
            &self.local.to_packet(),
            l.outgoing_identity_line,
            Instant::now() + t.write,
        )
        .await?;

        let (r, w) = tokio::io::split(tls);
        let mut reader = LineReader::new(Box::new(r) as BoxRead);
        let line = reader
            .read_line_until(l.identity_line, Instant::now() + t.secure_identity)
            .await?;
        let secure = Identity::from_packet(&Packet::from_line(&line)?)?;

        if secure.device_id != claimed_id {
            return Err(KdeError::IdentityMismatch(
                "deviceId changed between plaintext and secure identity".into(),
            ));
        }
        if secure.protocol_version != claimed_version {
            return Err(KdeError::IdentityMismatch(
                "protocolVersion changed between plaintext and secure identity".into(),
            ));
        }
        let peer_cert = CertInfo::parse(&tls_facts.peer_cert)?;
        if peer_cert.common_name() != secure.device_id.as_str() {
            return Err(KdeError::IdentityMismatch(
                "certificate common name differs from the device id".into(),
            ));
        }
        peer_cert.check_validity(unix_now())?;
        if let Some(pin) = &pinned
            && pin.der() != peer_cert.der()
        {
            return Err(KdeError::PinMismatch {
                device_id: claimed_id.to_string(),
            });
        }

        let info_peer_id = secure.device_id.clone();
        let info_peer_cert = peer_cert.clone();
        let info = LinkInfo {
            connection_id: ConnectionId(self.next_connection.fetch_add(1, Ordering::Relaxed)),
            role,
            peer: secure,
            peer_cert,
            tls_version: tls_facts.version,
            cipher_suite: tls_facts.cipher,
            peer_ip,
        };
        let conn = Conn {
            info,
            reader,
            writer: Box::new(w),
            limits: l.clone(),
            write_timeout: t.write,
            payload: PayloadContext {
                creds: self.creds.clone(),
                peer_id: info_peer_id,
                peer_cert: info_peer_cert,
                peer_ip,
                local_ip: None,
                versions: self.config.tls_versions,
                config: self.config.payload.clone(),
            },
        };
        Ok(if pinned.is_some() {
            Handshake::Trusted(TrustedLink::new(conn, self.store.clone()))
        } else {
            Handshake::Pending(PendingLink {
                conn,
                store: self.store.clone(),
                own_cert: self.creds.cert().clone(),
                pairing: self.config.pairing.clone(),
                _permit: permit,
            })
        })
    }
}

/// Keepalive so a half-dead phone link is noticed in about a minute.
fn configure_tcp(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    crate::keepalive::arm(&socket2::SockRef::from(stream))?;
    Ok(())
}
