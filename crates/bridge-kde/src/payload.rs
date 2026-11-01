//! KDE Connect payload sockets (file bodies that follow a packet with `payloadSize`).
//!
//! Roles are inverted relative to the link, exactly like the main connection:
//!
//! * the **sender** opens a listener on 1739-1764 and is the **TLS server**;
//! * the **receiver** connects to the sender's address at `payloadTransferInfo.port` as **TLS client**.
//!
//! Both sides use the pinned-certificate configuration of the paired device (never the unpaired
//! "inspect" path), so a payload socket can only ever be served to, or read from, the device whose
//! exact certificate was pinned at pairing. There is no identity exchange: the stream is raw bytes
//! until the sender closes, or until the announced size has been transferred.
//!
//! Everything is bounded: connect/accept/handshake timeouts, an idle timeout between chunks, a hard
//! byte limit on what the receiver will accept, and a [`CancelToken`] that stops either direction.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::cert::{CertInfo, DeviceCredentials};
use crate::error::KdeError;
use crate::identity::DeviceId;
use crate::tls::{PeerPolicy, PeerVerifier, TlsVersions, client_config, server_config};

/// Payload port range of stock KDE Connect.
pub const PAYLOAD_PORTS: RangeInclusive<u16> = 1739..=1764;
const CHUNK: usize = 64 * 1024;
const MAX_PENDING_HANDSHAKES: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error("transfer cancelled")]
    Cancelled,
    #[error("payload exceeds the {limit} byte limit")]
    TooLarge { limit: u64 },
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("the peer closed the payload connection after {received} of {expected} bytes")]
    Truncated { received: u64, expected: u64 },
    #[error("payload port {0} is not permitted")]
    PortRejected(u16),
    #[error("no free payload port")]
    NoPort,
    #[error("payload I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("payload TLS error: {0}")]
    Link(#[from] KdeError),
}

impl PayloadError {
    /// Short fixed code, safe to show or log.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::TooLarge { .. } => "too_large",
            Self::Timeout(_) => "timeout",
            Self::Truncated { .. } => "truncated",
            Self::PortRejected(_) => "port_rejected",
            Self::NoPort => "no_port",
            Self::Io(_) => "io_error",
            Self::Link(KdeError::PinMismatch { .. }) => "pin_mismatch",
            Self::Link(_) => "tls_failed",
        }
    }
}

pub type PayloadResult<T> = std::result::Result<T, PayloadError>;

/// Cooperative cancellation shared between the task moving bytes and whoever owns the UI.
#[derive(Clone, Debug)]
pub struct CancelToken(Arc<watch::Sender<bool>>);

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(watch::channel(false).0))
    }

    pub fn cancel(&self) {
        self.0.send_replace(true);
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once cancelled; stays pending forever otherwise.
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        loop {
            if *rx.borrow_and_update() {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct PayloadConfig {
    /// Ports we may listen on and the only ports we will dial.
    pub ports: RangeInclusive<u16>,
    /// Development/test switch: bind an ephemeral port and dial any non-zero port.
    pub any_port: bool,
    /// How long a sender waits for the receiver to connect (the receiver may be waiting for the
    /// user to press Accept).
    pub accept_timeout: Duration,
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    /// Longest silence tolerated between chunks in either direction.
    pub idle_timeout: Duration,
}

impl Default for PayloadConfig {
    fn default() -> Self {
        Self {
            ports: PAYLOAD_PORTS,
            any_port: false,
            accept_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
        }
    }
}

/// Everything needed to open payload sockets to one paired device. Obtained from
/// [`TrustedLink::payload_context`](crate::TrustedLink::payload_context), so it can only exist for a
/// peer whose exact certificate was pinned.
#[derive(Clone)]
pub struct PayloadContext {
    pub(crate) creds: Arc<DeviceCredentials>,
    pub(crate) peer_id: DeviceId,
    pub(crate) peer_cert: CertInfo,
    pub(crate) peer_ip: IpAddr,
    pub(crate) versions: TlsVersions,
    pub(crate) config: PayloadConfig,
}

impl std::fmt::Debug for PayloadContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PayloadContext")
            .field("peer", &self.peer_id)
            .finish()
    }
}

/// A bound payload listener; its port goes into `payloadTransferInfo.port`.
pub struct PayloadListener {
    listener: TcpListener,
    port: u16,
    ctx: PayloadContext,
}

impl PayloadContext {
    pub fn peer_id(&self) -> &DeviceId {
        &self.peer_id
    }

    pub fn peer_ip(&self) -> IpAddr {
        self.peer_ip
    }

    pub fn config(&self) -> &PayloadConfig {
        &self.config
    }

    /// Bind the first free port of the payload range (sender side).
    pub async fn listen(&self) -> PayloadResult<PayloadListener> {
        let any: IpAddr = if self.peer_ip.is_ipv6() && !is_mapped(self.peer_ip) {
            Ipv6Addr::UNSPECIFIED.into()
        } else {
            Ipv4Addr::UNSPECIFIED.into()
        };
        let candidates: Vec<u16> = if self.config.any_port {
            vec![0]
        } else {
            self.config.ports.clone().collect()
        };
        for p in candidates {
            if let Ok(listener) = TcpListener::bind(SocketAddr::new(any, p)).await {
                let port = listener.local_addr()?.port();
                return Ok(PayloadListener {
                    listener,
                    port,
                    ctx: self.clone(),
                });
            }
        }
        Err(PayloadError::NoPort)
    }

    /// Connect to the sender's payload port and write what it sends into `out` (receiver side).
    ///
    /// `expected` is the announced size: `Some(n)` reads exactly `n` bytes (fewer is
    /// [`PayloadError::Truncated`]); `None` (size `-1`) reads until the sender closes. At most
    /// `max` bytes are ever accepted. Returns the number of bytes written.
    pub async fn receive<W>(
        &self,
        port: u16,
        expected: Option<u64>,
        max: u64,
        out: &mut W,
        progress: &mut (dyn FnMut(u64) + Send),
        cancel: &CancelToken,
    ) -> PayloadResult<u64>
    where
        W: AsyncWrite + Unpin,
    {
        if let Some(n) = expected
            && n > max
        {
            return Err(PayloadError::TooLarge { limit: max });
        }
        if port == 0 || (!self.config.any_port && !self.config.ports.contains(&port)) {
            return Err(PayloadError::PortRejected(port));
        }
        let c = &self.config;
        let work = async {
            let addr = SocketAddr::new(self.peer_ip, port);
            let tcp = timeout(c.connect_timeout, TcpStream::connect(addr))
                .await
                .map_err(|_| PayloadError::Timeout("payload connect"))??;
            tcp.set_nodelay(true)?;
            let (verifier, _failure) = PeerVerifier::new(
                PeerPolicy::Pinned(self.peer_cert.clone()),
                self.peer_id.clone(),
            );
            let cfg = client_config(&self.creds, verifier, self.versions)?;
            let name = rustls::pki_types::ServerName::IpAddress(self.peer_ip.into());
            let mut tls = timeout(
                c.handshake_timeout,
                TlsConnector::from(cfg).connect(name, tcp),
            )
            .await
            .map_err(|_| PayloadError::Timeout("payload TLS handshake"))?
            .map_err(|e| PayloadError::Link(KdeError::Tls(e.to_string())))?;
            copy_in(&mut tls, out, expected, max, c.idle_timeout, progress).await
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(PayloadError::Cancelled),
            r = work => r,
        }
    }
}

impl PayloadListener {
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Wait for the paired device to connect, then stream exactly `size` bytes from `src`.
    /// Connections that fail the pinned-certificate handshake are dropped and the wait continues,
    /// so a stranger on the LAN cannot consume the one-shot payload slot.
    pub async fn send<R>(
        self,
        mut src: R,
        size: u64,
        progress: &mut (dyn FnMut(u64) + Send),
        cancel: &CancelToken,
    ) -> PayloadResult<u64>
    where
        R: AsyncRead + Unpin,
    {
        let c = self.ctx.config.clone();
        let work = async {
            let mut tls = self.accept_peer().await?;
            copy_out(&mut src, &mut tls, size, c.idle_timeout, progress).await?;
            let _ = timeout(Duration::from_secs(5), tls.shutdown()).await;
            Ok(size)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(PayloadError::Cancelled),
            r = work => r,
        }
    }

    async fn accept_peer(&self) -> PayloadResult<tokio_rustls::server::TlsStream<TcpStream>> {
        let c = &self.ctx.config;
        let deadline = Instant::now() + c.accept_timeout;
        let (tx, mut rx) = mpsc::channel(MAX_PENDING_HANDSHAKES);
        let mut pending = 0usize;
        loop {
            tokio::select! {
                accepted = timeout_at(deadline, self.listener.accept()) => {
                    let (tcp, _) = accepted
                        .map_err(|_| PayloadError::Timeout("waiting for the receiver"))??;
                    if pending >= MAX_PENDING_HANDSHAKES {
                        continue; // dropped unread
                    }
                    pending += 1;
                    let ctx = self.ctx.clone();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx.send(handshake_server(&ctx, tcp).await).await;
                    });
                }
                done = rx.recv() => {
                    pending = pending.saturating_sub(1);
                    if let Some(Ok(tls)) = done {
                        return Ok(tls);
                    }
                }
            }
        }
    }
}

async fn handshake_server(
    ctx: &PayloadContext,
    tcp: TcpStream,
) -> PayloadResult<tokio_rustls::server::TlsStream<TcpStream>> {
    tcp.set_nodelay(true)?;
    let (verifier, _failure) = PeerVerifier::new(
        PeerPolicy::Pinned(ctx.peer_cert.clone()),
        ctx.peer_id.clone(),
    );
    let cfg = server_config(&ctx.creds, verifier, ctx.versions)?;
    timeout(
        ctx.config.handshake_timeout,
        TlsAcceptor::from(cfg).accept(tcp),
    )
    .await
    .map_err(|_| PayloadError::Timeout("payload TLS handshake"))?
    .map_err(|e| PayloadError::Link(KdeError::Tls(e.to_string())))
}

fn is_mapped(ip: IpAddr) -> bool {
    matches!(ip, IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some())
}

async fn copy_in<R, W>(
    src: &mut R,
    out: &mut W,
    expected: Option<u64>,
    max: u64,
    idle: Duration,
    progress: &mut (dyn FnMut(u64) + Send),
) -> PayloadResult<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let want = match expected {
            Some(n) if total >= n => break,
            Some(n) => usize::try_from((n - total).min(CHUNK as u64)).unwrap_or(CHUNK),
            None => CHUNK,
        };
        let n = match timeout(idle, src.read(&mut buf[..want])).await {
            Err(_) => return Err(PayloadError::Timeout("payload idle")),
            // A sender that vanishes without close_notify is just a short read.
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
            Ok(r) => r?,
        };
        if n == 0 {
            match expected {
                Some(e) => {
                    return Err(PayloadError::Truncated {
                        received: total,
                        expected: e,
                    });
                }
                None => break,
            }
        }
        total += n as u64;
        if total > max {
            return Err(PayloadError::TooLarge { limit: max });
        }
        timeout(idle, out.write_all(&buf[..n]))
            .await
            .map_err(|_| PayloadError::Timeout("local write"))??;
        progress(total);
    }
    out.flush().await?;
    Ok(total)
}

async fn copy_out<R, W>(
    src: &mut R,
    out: &mut W,
    size: u64,
    idle: Duration,
    progress: &mut (dyn FnMut(u64) + Send),
) -> PayloadResult<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    while sent < size {
        let want = usize::try_from((size - sent).min(CHUNK as u64)).unwrap_or(CHUNK);
        let n = src.read(&mut buf[..want]).await?;
        if n == 0 {
            // The file shrank while we were sending it.
            return Err(PayloadError::Truncated {
                received: sent,
                expected: size,
            });
        }
        timeout(idle, out.write_all(&buf[..n]))
            .await
            .map_err(|_| PayloadError::Timeout("payload idle"))??;
        sent += n as u64;
        progress(sent);
    }
    timeout(idle, out.flush())
        .await
        .map_err(|_| PayloadError::Timeout("payload idle"))??;
    Ok(())
}

#[cfg(test)]
mod tests;
