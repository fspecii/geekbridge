// SPDX-License-Identifier: GPL-3.0-or-later
//! A deliberately small blocking HTTP/1.1 GET client (rustls + the bundled Mozilla roots).
//!
//! Only what the updater needs: GET, `Connection: close`, identity encoding, Content-Length or
//! chunked bodies, bounded redirects, a hard body cap and an overall deadline. Plain `http://` is
//! accepted only for loopback hosts, which is how the tests talk to a local mock server.
//!
//! Connecting: all resolved addresses are raced happy-eyeballs style (RFC 8305: families
//! interleaved, a new attempt every 250 ms or as soon as one fails, first success wins), so a
//! black-holed IPv6 address no longer delays IPv4 by the connect timeout. `HTTPS_PROXY` (or
//! `https_proxy`, then `ALL_PROXY`) of the form `http://host:port` is honoured through `CONNECT`
//! for the https hosts the policy allows, minus `NO_PROXY`; TLS still runs end to end against the
//! bundled roots, so the proxy only sees the host name and port. Credentials in the proxy URL,
//! `https://` and SOCKS proxies are not supported and mean "no proxy" (direct connection).
//! Loopback test origins are never proxied. `HTTP_PROXY` is not read: plain http is loopback only.

use crate::Error;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const MAX_HEAD_BYTES: usize = 32 * 1024;
const MAX_HEADERS: usize = 100;
const MAX_REDIRECTS: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(text: &str) -> Result<Url, Error> {
        let bad = |why: &str| Error::Url(why.to_owned());
        if text.len() > 2048 || text.bytes().any(|b| b <= b' ' || b == 0x7f) {
            return Err(bad("unusable URL"));
        }
        let (https, rest) = if let Some(rest) = text.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = text.strip_prefix("http://") {
            (false, rest)
        } else {
            return Err(bad("URL must be http(s)"));
        };
        let rest = rest.split('#').next().unwrap_or("");
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.is_empty() || authority.contains('@') {
            return Err(bad("URL has no host or carries credentials"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (addr, after) = v6.split_once(']').ok_or_else(|| bad("bad IPv6 host"))?;
            let port = match after {
                "" => None,
                p => Some(p.strip_prefix(':').ok_or_else(|| bad("bad port"))?),
            };
            (addr.to_owned(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_owned(), Some(p)),
                None => (authority.to_owned(), None),
            }
        };
        let port = match port {
            None => {
                if https {
                    443
                } else {
                    80
                }
            }
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| bad("bad port"))?,
        };
        if host.is_empty() {
            return Err(bad("empty host"));
        }
        let path = if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        };
        Ok(Url {
            https,
            host: host.to_ascii_lowercase(),
            port,
            path,
        })
    }

    pub fn is_loopback(&self) -> bool {
        self.host == "localhost" || self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    fn origin(&self) -> String {
        let default = if self.https { 443 } else { 80 };
        let scheme = if self.https { "https" } else { "http" };
        if self.port == default {
            format!("{scheme}://{}", self.host)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

/// Hosts a release is ever allowed to point us at, over https only.
pub fn is_github_host(host: &str) -> bool {
    matches!(
        host,
        "github.com"
            | "api.github.com"
            | "objects.githubusercontent.com"
            | "release-assets.githubusercontent.com"
            | "github-releases.githubusercontent.com"
    )
}

/// What the client may talk to: GitHub over https, plus (for tests) one explicitly configured
/// loopback origin.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    pub loopback_origin: Option<(String, u16)>,
}

impl Policy {
    /// Whether a bearer token may accompany a request to `url`: only the GitHub API host over
    /// TLS. Asset CDNs get pre-signed URLs and must never see the token, and a redirect to any
    /// other host is dropped before it is contacted. (The loopback test origin plays the API
    /// host for `/repos/...` paths only, so tests can tell API and CDN requests apart.)
    pub fn sends_auth(&self, url: &Url) -> bool {
        if url.https {
            return url.port == 443 && url.host == "api.github.com";
        }
        self.allows(url) && url.path.starts_with("/repos/")
    }

    pub fn allows(&self, url: &Url) -> bool {
        if url.https && url.port == 443 && is_github_host(&url.host) {
            return true;
        }
        url.is_loopback()
            && self
                .loopback_origin
                .as_ref()
                .is_some_and(|(host, port)| *host == url.host && *port == url.port)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fetched {
    pub status: u16,
    pub etag: Option<String>,
    pub content_length: Option<u64>,
    /// Seconds to wait, from `Retry-After` or the rate-limit reset header, when the server asked.
    pub retry_after: Option<u64>,
    pub rate_limited: bool,
}

pub struct Client {
    tls: Arc<rustls::ClientConfig>,
    user_agent: String,
    pub policy: Policy,
    pub io_timeout: Duration,
    token: Option<String>,
    proxy: Option<Proxy>,
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

impl Client {
    pub fn new(
        user_agent: &str,
        policy: Policy,
        io_timeout: Duration,
        token: Option<String>,
    ) -> Result<Client, Error> {
        if token
            .as_deref()
            .is_some_and(|t| t.is_empty() || t.bytes().any(|b| b <= b' ' || b >= 0x7f))
        {
            return Err(Error::Url("the update token is not usable".into()));
        }
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| Error::Tls(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Client {
            tls: Arc::new(tls),
            user_agent: user_agent.to_owned(),
            policy,
            io_timeout,
            token,
            proxy: Proxy::from_env(),
        })
    }

    /// GET `url`, following redirects that stay inside the policy. The body of a 200 response is
    /// handed to `sink` in pieces; non-200 bodies are discarded. More than `max_body` bytes is an
    /// error, whether declared or actual. `total` bounds the whole operation.
    pub fn get(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_body: u64,
        total: Duration,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Fetched, Error> {
        let deadline = Instant::now() + total;
        // A watchdog closes the socket at the deadline, so the error that follows (a read
        // cut short) is reported for what it is: the deadline passing.
        self.get_until(url, headers, max_body, deadline, sink)
            .map_err(|e| match e {
                Error::Timeout | Error::TooLarge => e,
                _ if Instant::now() >= deadline => Error::Timeout,
                other => other,
            })
    }

    fn get_until(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        max_body: u64,
        deadline: Instant,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Fetched, Error> {
        let mut url = Url::parse(url)?;
        for _ in 0..=MAX_REDIRECTS {
            if !self.policy.allows(&url) {
                return Err(Error::Url(format!("refusing to contact {}", url.origin())));
            }
            let (mut reader, ctl) = self.connect(&url, deadline)?;
            let guard = Guard::new(ctl, deadline, self.io_timeout)?;
            let mut request = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
                url.path,
                host_header(&url),
                self.user_agent
            );
            for (name, value) in headers {
                if name.bytes().chain(value.bytes()).any(|b| b < b' ') {
                    return Err(Error::Url("bad header".into()));
                }
                request.push_str(&format!("{name}: {value}\r\n"));
            }
            if let Some(token) = self
                .token
                .as_deref()
                .filter(|_| self.policy.sends_auth(&url))
            {
                request.push_str(&format!("Authorization: Bearer {token}\r\n"));
            }
            request.push_str("\r\n");
            guard.arm()?;
            reader
                .get_mut()
                .write_all(request.as_bytes())
                .and_then(|()| reader.get_mut().flush())
                .map_err(io_err)?;

            let head = read_head(&mut reader, &guard)?;
            if matches!(head.status, 301 | 302 | 303 | 307 | 308) {
                let location = head
                    .header("location")
                    .ok_or_else(|| Error::Malformed("redirect without Location".into()))?;
                url = resolve(&url, location)?;
                continue;
            }
            let mut fetched = head.summary();
            if head.status != 200 {
                return Ok(fetched);
            }
            if let Some(encoding) = head.header("content-encoding")
                && !encoding.eq_ignore_ascii_case("identity")
            {
                return Err(Error::Malformed("unexpected content encoding".into()));
            }
            if fetched.content_length.is_some_and(|n| n > max_body) {
                return Err(Error::TooLarge);
            }
            let mut counted = 0_u64;
            let mut limited = |chunk: &[u8]| -> io::Result<()> {
                counted += chunk.len() as u64;
                if counted > max_body {
                    return Err(io::Error::other(TooLarge));
                }
                sink(chunk)
            };
            let chunked = head
                .header("transfer-encoding")
                .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
            let result = if chunked {
                read_chunked(&mut reader, &guard, &mut limited)
            } else {
                read_plain(&mut reader, &guard, fetched.content_length, &mut limited)
            };
            if let Err(e) = result {
                return Err(match e {
                    Error::Io(ref m) if m == TOO_LARGE_MARK => Error::TooLarge,
                    other => other,
                });
            }
            fetched.content_length = Some(counted);
            return Ok(fetched);
        }
        Err(Error::Url("too many redirects".into()))
    }

    fn connect(
        &self,
        url: &Url,
        deadline: Instant,
    ) -> Result<(BufReader<Box<dyn Stream>>, TcpStream), Error> {
        let proxy = self
            .proxy
            .as_ref()
            .filter(|p| url.https && !url.is_loopback() && !p.bypasses(&url.host, url.port));
        let (host, port) =
            proxy.map_or((url.host.as_str(), url.port), |p| (p.host.as_str(), p.port));
        let addrs: Vec<_> = (host, port)
            .to_socket_addrs()
            .map_err(|e| unreachable_err(host, e))?
            .collect();
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout)?;
        let per_attempt = remaining.min(self.io_timeout);
        let connected = race_connect(
            &addrs,
            CONNECT_STAGGER,
            deadline,
            Arc::new(move |a| TcpStream::connect_timeout(a, per_attempt)),
        )
        .and_then(|sock| {
            let t = per_attempt.max(Duration::from_millis(1));
            let _ = sock.set_read_timeout(Some(t));
            let _ = sock.set_write_timeout(Some(t));
            match proxy {
                Some(_) => proxy_tunnel(sock, &url.host, url.port, deadline),
                None => Ok(sock),
            }
        });
        let sock = connected.map_err(|e| unreachable_err(&url.host, e))?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::Timeout)?;
        // Set on the original handle too: a clone's options are not relied upon.
        let t = remaining.min(self.io_timeout).max(Duration::from_millis(1));
        let _ = sock.set_read_timeout(Some(t));
        let _ = sock.set_write_timeout(Some(t));
        let ctl = sock.try_clone().map_err(io_err)?;
        let _ = sock.set_nodelay(true);
        let stream: Box<dyn Stream> = if url.https {
            let name = rustls::pki_types::ServerName::try_from(url.host.clone())
                .map_err(|_| Error::Url("bad host name".into()))?;
            let conn = rustls::ClientConnection::new(self.tls.clone(), name)
                .map_err(|e| Error::Tls(e.to_string()))?;
            Box::new(rustls::StreamOwned::new(conn, sock))
        } else {
            Box::new(sock)
        };
        Ok((BufReader::with_capacity(16 * 1024, stream), ctl))
    }
}

/// Delay before the next address is tried while earlier attempts are still pending (RFC 8305: 250 ms).
const CONNECT_STAGGER: Duration = Duration::from_millis(250);
/// More addresses than this are ignored; each attempt is a short-lived thread.
const MAX_ATTEMPTS: usize = 6;

/// Alternate address families, starting with the family of the first (resolver-preferred) address.
fn interleave(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let first_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (mut a, mut b): (Vec<_>, Vec<_>) =
        addrs.iter().copied().partition(|x| x.is_ipv6() == first_v6);
    a.reverse();
    b.reverse();
    let mut out = Vec::new();
    while !a.is_empty() || !b.is_empty() {
        out.extend(a.pop());
        out.extend(b.pop());
    }
    out.truncate(MAX_ATTEMPTS);
    out
}

type Connector = Arc<dyn Fn(&SocketAddr) -> io::Result<TcpStream> + Send + Sync>;

/// Happy-eyeballs connect (losing attempts are not waited for): start an attempt per address, the next one after `stagger` or at once
/// when the previous failed; the first success wins and late winners are dropped (closed). Every
/// attempt is bounded by `connect` itself, so the threads end on their own; the race as a whole
/// gives up at `deadline`.
fn race_connect(
    addrs: &[SocketAddr],
    stagger: Duration,
    deadline: Instant,
    connect: Connector,
) -> io::Result<TcpStream> {
    let addrs = interleave(addrs);
    match addrs.as_slice() {
        [] => return Err(io::Error::other("no address")),
        [one] => return connect(one),
        _ => {}
    }
    {
        let (tx, rx) = mpsc::channel();
        let (mut next, mut pending) = (0, 0);
        loop {
            if next < addrs.len() {
                let (tx, addr, connect) = (tx.clone(), addrs[next], connect.clone());
                let spawned = std::thread::Builder::new()
                    .name("update-connect".into())
                    .spawn(move || {
                        let _ = tx.send(connect(&addr));
                    });
                next += 1;
                match spawned {
                    Ok(_) => pending += 1,
                    Err(e) if pending == 0 && next >= addrs.len() => return Err(e),
                    Err(_) => {}
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let wait = if next < addrs.len() {
                stagger.min(left)
            } else {
                left
            };
            match rx.recv_timeout(wait) {
                Ok(Ok(sock)) => return Ok(sock),
                Ok(Err(e)) => {
                    pending -= 1;
                    if pending == 0 && next >= addrs.len() {
                        return Err(e);
                    }
                }
                Err(_) => {}
            }
        }
    }
}

/// An `http://host:port` forward proxy used through `CONNECT` for https targets.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Proxy {
    host: String,
    port: u16,
    /// Lowercase host or domain suffix (`*` for everything) with an optional port.
    no_proxy: Vec<(String, Option<u16>)>,
}

impl Proxy {
    fn from_env() -> Option<Proxy> {
        let var = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
        Proxy::parse(
            &var("HTTPS_PROXY")
                .or_else(|| var("https_proxy"))
                .or_else(|| var("ALL_PROXY"))
                .or_else(|| var("all_proxy"))?,
            &var("NO_PROXY")
                .or_else(|| var("no_proxy"))
                .unwrap_or_default(),
        )
    }

    fn parse(proxy: &str, no_proxy: &str) -> Option<Proxy> {
        let text = proxy.trim();
        let rest = match text.get(..7) {
            Some(scheme) if scheme.eq_ignore_ascii_case("http://") => &text[7..],
            _ => text,
        };
        if rest.contains("://") {
            return None;
        }
        // Reuse the URL parser (rejects credentials, bad ports and control bytes); loopback is fine.
        let u = Url::parse(&format!("http://{}", rest.trim_end_matches('/'))).ok()?;
        (u.path == "/").then(|| Proxy {
            host: u.host,
            port: if rest.contains(':') && !rest.ends_with(']') {
                u.port
            } else {
                3128
            },
            no_proxy: no_proxy
                .split([',', ' '])
                .filter_map(no_proxy_entry)
                .collect(),
        })
    }

    /// `NO_PROXY` semantics: an entry matches the host itself and its subdomains (`.example.com`,
    /// `*.example.com` and `example.com` alike); `host:port` only that port; `*` everything.
    /// CIDR ranges are not understood (they never match).
    fn bypasses(&self, host: &str, port: u16) -> bool {
        let host = host.to_ascii_lowercase();
        self.no_proxy.iter().any(|(e, p)| {
            p.is_none_or(|p| p == port)
                && (e == "*" || host == *e || host.ends_with(&format!(".{e}")))
        })
    }
}

fn no_proxy_entry(entry: &str) -> Option<(String, Option<u16>)> {
    let entry = entry.trim().to_ascii_lowercase();
    let (host, port) = if let Some(v6) = entry.strip_prefix('[') {
        let (addr, after) = v6.split_once(']')?;
        (addr, after.strip_prefix(':'))
    } else {
        match entry.split_once(':') {
            // Exactly one colon: host:port. More: a bare IPv6 address.
            Some((h, p)) if !p.contains(':') => (h, Some(p)),
            _ => (entry.as_str(), None),
        }
    };
    let port = match port {
        None => None,
        Some(p) => Some(p.parse::<u16>().ok()?),
    };
    let host = if host == "*" {
        host
    } else {
        host.trim_start_matches('*').trim_start_matches('.')
    };
    (!host.is_empty()).then(|| (host.to_owned(), port))
}

/// Ask the proxy for a tunnel to `host:port`. Reads the reply byte by byte so nothing of the
/// TLS stream that follows is consumed; any status but 2xx fails.
fn proxy_tunnel(
    mut sock: TcpStream,
    host: &str,
    port: u16,
    deadline: Instant,
) -> io::Result<TcpStream> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    write!(
        sock,
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n"
    )?;
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= 8192 || Instant::now() >= deadline {
            return Err(io::Error::other(
                "the proxy's reply is too long or too slow",
            ));
        }
        if sock.read(&mut byte)? == 0 {
            return Err(io::Error::other("the proxy closed the connection"));
        }
        head.push(byte[0]);
    }
    let line =
        String::from_utf8_lossy(head.split(|&b| b == b'\r').next().unwrap_or(b"")).into_owned();
    let mut parts = line.split(' ');
    match (parts.next(), parts.next()) {
        (Some(v), Some(code))
            if v.starts_with("HTTP/1.") && code.len() == 3 && code.starts_with('2') =>
        {
            Ok(sock)
        }
        _ => Err(io::Error::other(format!(
            "the proxy refused the tunnel ({})",
            line.chars().take(60).collect::<String>()
        ))),
    }
}

fn host_header(url: &Url) -> String {
    let host = if url.host.contains(':') {
        format!("[{}]", url.host)
    } else {
        url.host.clone()
    };
    let default = if url.https { 443 } else { 80 };
    if url.port == default {
        host
    } else {
        format!("{host}:{}", url.port)
    }
}

fn resolve(base: &Url, location: &str) -> Result<Url, Error> {
    if location.starts_with("http://") || location.starts_with("https://") {
        Url::parse(location)
    } else if location.starts_with('/') && !location.starts_with("//") {
        Url::parse(&format!("{}{location}", base.origin()))
    } else {
        Err(Error::Url("unsupported redirect target".into()))
    }
}

#[derive(Debug)]
struct TooLarge;
impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(TOO_LARGE_MARK)
    }
}
impl std::error::Error for TooLarge {}
const TOO_LARGE_MARK: &str = "response too large";

/// Name resolution or connecting failed: almost always no network. Says so in words a person can act on.
fn unreachable_err(host: &str, e: io::Error) -> Error {
    match io_err(e) {
        Error::Io(m) => Error::Io(format!(
            "Couldn't reach {host} ({m}). Check your internet connection."
        )),
        other => other,
    }
}

pub(crate) fn io_err(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
        _ => {
            let message = e.to_string();
            // A wrong clock is the usual reason a certificate of a well-known host is refused.
            if message.contains("certificate expired")
                || message.contains("certificate not valid yet")
            {
                Error::Tls(
                    "The secure connection was refused because this computer's date or time looks wrong. Check the clock."
                        .into(),
                )
            } else {
                Error::Io(message)
            }
        }
    }
}

struct Guard {
    ctl: TcpStream,
    deadline: Instant,
    io_timeout: Duration,
    /// Dropping this wakes the watchdog thread, which then exits without touching the socket.
    _cancel: std::sync::mpsc::Sender<()>,
}

impl Guard {
    fn new(ctl: TcpStream, deadline: Instant, io_timeout: Duration) -> Result<Guard, Error> {
        let victim = ctl.try_clone().map_err(io_err)?;
        let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
        let wait = deadline.saturating_duration_since(Instant::now());
        std::thread::Builder::new()
            .name("update-deadline".into())
            .spawn(move || {
                // Only a timeout means the deadline passed; a send or a drop means "done".
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    cancelled.recv_timeout(wait)
                {
                    let _ = victim.shutdown(std::net::Shutdown::Both);
                }
            })
            .map_err(io_err)?;
        Ok(Guard {
            ctl,
            deadline,
            io_timeout,
            _cancel: cancel,
        })
    }

    /// Called before each blocking read or write: the socket timeout never exceeds what is left of
    /// the overall deadline.
    fn arm(&self) -> Result<(), Error> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Error::Timeout)?;
        let t = remaining.min(self.io_timeout).max(Duration::from_millis(1));
        self.ctl.set_read_timeout(Some(t)).map_err(io_err)?;
        self.ctl.set_write_timeout(Some(t)).map_err(io_err)
    }
}

struct Head {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn summary(&self) -> Fetched {
        let number = |name| self.header(name).and_then(|v| v.trim().parse::<u64>().ok());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // GitHub sends `x-ratelimit-reset` on every response, so only an explicit `Retry-After`
        // or an exhausted quota makes a 403 a rate limit rather than a plain refusal.
        let explicit_wait = number("retry-after").is_some();
        let retry_after = number("retry-after")
            .or_else(|| number("x-ratelimit-reset").map(|reset| reset.saturating_sub(now).max(1)));
        let exhausted = number("x-ratelimit-remaining") == Some(0);
        Fetched {
            status: self.status,
            etag: self
                .header("etag")
                .filter(|v| v.len() <= 256 && v.bytes().all(|b| (b' '..0x7f).contains(&b)))
                .map(str::to_owned),
            content_length: number("content-length"),
            retry_after,
            rate_limited: self.status == 429
                || (self.status == 403 && (exhausted || explicit_wait)),
        }
    }
}

/// Reads one `\n`-terminated line of at most `cap` bytes (the terminator included).
fn read_line(
    reader: &mut BufReader<Box<dyn Stream>>,
    guard: &Guard,
    cap: usize,
) -> Result<String, Error> {
    let mut line = Vec::new();
    loop {
        guard.arm()?;
        let buf = reader.fill_buf().map_err(io_err)?;
        if buf.is_empty() {
            return Err(Error::Malformed("connection closed early".into()));
        }
        let (take, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        if line.len() + take > cap {
            return Err(Error::Malformed("header line too long".into()));
        }
        line.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if done {
            break;
        }
    }
    String::from_utf8(line)
        .map(|l| l.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|_| Error::Malformed("non-UTF-8 header".into()))
}

fn read_head(reader: &mut BufReader<Box<dyn Stream>>, guard: &Guard) -> Result<Head, Error> {
    let bad = |m: &str| Error::Malformed(m.to_owned());
    let status_line = read_line(reader, guard, 1024)?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or("");
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(bad("not an HTTP response"));
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|s| (100..600).contains(s))
        .ok_or_else(|| bad("bad status"))?;
    let mut headers = Vec::new();
    let mut total = status_line.len();
    loop {
        let line = read_line(reader, guard, 8 * 1024)?;
        total += line.len() + 2;
        if total > MAX_HEAD_BYTES || headers.len() > MAX_HEADERS {
            return Err(bad("response head too large"));
        }
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| bad("bad header"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(Head { status, headers })
}

fn read_plain(
    reader: &mut BufReader<Box<dyn Stream>>,
    guard: &Guard,
    length: Option<u64>,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), Error> {
    let mut remaining = length;
    let mut buf = vec![0_u8; 32 * 1024];
    loop {
        if remaining == Some(0) {
            return Ok(());
        }
        guard.arm()?;
        let want = remaining.map_or(buf.len(), |r| buf.len().min(r as usize));
        let n = match reader.read(&mut buf[..want]) {
            Ok(n) => n,
            // A TLS peer that closes without close_notify after a complete body is common;
            // when the length is known the check below still catches truncation.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && remaining.is_none() => 0,
            Err(e) => return Err(io_err(e)),
        };
        if n == 0 {
            return match remaining {
                Some(r) if r > 0 => Err(Error::Malformed("truncated body".into())),
                _ => Ok(()),
            };
        }
        if let Some(r) = remaining.as_mut() {
            *r -= n as u64;
        }
        sink(&buf[..n]).map_err(io_err)?;
    }
}

fn read_chunked(
    reader: &mut BufReader<Box<dyn Stream>>,
    guard: &Guard,
    sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> Result<(), Error> {
    let mut buf = vec![0_u8; 32 * 1024];
    loop {
        let line = read_line(reader, guard, 128)?;
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_text, 16)
            .ok()
            .filter(|_| !size_text.is_empty() && size_text.len() <= 16)
            .ok_or_else(|| Error::Malformed("bad chunk size".into()))?;
        if size == 0 {
            // Trailers, then the blank line that ends the message.
            for _ in 0..MAX_HEADERS {
                if read_line(reader, guard, 8 * 1024)?.is_empty() {
                    return Ok(());
                }
            }
            return Err(Error::Malformed("too many trailers".into()));
        }
        let mut left = size;
        while left > 0 {
            guard.arm()?;
            let want = buf.len().min(left as usize);
            let n = reader.read(&mut buf[..want]).map_err(io_err)?;
            if n == 0 {
                return Err(Error::Malformed("truncated chunk".into()));
            }
            left -= n as u64;
            sink(&buf[..n]).map_err(io_err)?;
        }
        if !read_line(reader, guard, 4)?.is_empty() {
            return Err(Error::Malformed("bad chunk terminator".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn the_token_is_for_the_api_host_over_tls_only() {
        let policy = Policy::default();
        let sends = |u: &str| policy.sends_auth(&Url::parse(u).unwrap());
        assert!(sends("https://api.github.com/repos/a/b/releases/latest"));
        for no in [
            "https://github.com/a/b/releases/download/v1/x",
            "https://objects.githubusercontent.com/x",
            "https://release-assets.githubusercontent.com/x",
            "https://api.github.com.evil.example/x",
            "https://evil.example/x",
            "http://api.github.com/x",
            "https://api.github.com:8443/x",
        ] {
            assert!(!sends(no), "{no}");
        }
    }

    #[test]
    fn parses_urls() {
        let u = Url::parse("https://api.github.com/repos/a/b?x=1#frag").unwrap();
        assert!(u.https && u.port == 443 && u.host == "api.github.com");
        assert_eq!(u.path, "/repos/a/b?x=1");
        let u = Url::parse("http://127.0.0.1:8080").unwrap();
        assert!(!u.https && u.port == 8080 && u.path == "/" && u.is_loopback());
        let u = Url::parse("http://[::1]:9/x").unwrap();
        assert!(u.is_loopback() && u.host == "::1" && u.port == 9);
        for bad in [
            "ftp://x/",
            "https://user@host/",
            "https:///x",
            "https://h:0/",
            "https://h:99999/",
            "https://h/a b",
            "javascript:alert(1)",
        ] {
            assert!(Url::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn policy_allows_only_github_https_and_the_test_origin() {
        let p = Policy {
            loopback_origin: Some(("127.0.0.1".into(), 4000)),
        };
        let ok = |s: &str| p.allows(&Url::parse(s).unwrap());
        assert!(ok("https://github.com/x"));
        assert!(ok("https://objects.githubusercontent.com/x"));
        assert!(ok("https://release-assets.githubusercontent.com/x"));
        assert!(ok("https://github-releases.githubusercontent.com/x"));
        assert!(!ok("https://raw.githubusercontent.com/x"));
        assert!(!ok("https://gist.githubusercontent.com/x"));
        assert!(!ok("https://user-images.githubusercontent.com/x"));
        assert!(!ok("https://objects.githubusercontent.com:8443/x"));
        assert!(ok("http://127.0.0.1:4000/x"));
        assert!(!ok("http://github.com/x"));
        assert!(!ok("https://evilgithub.com/x"));
        assert!(!ok("https://github.com.evil.example/x"));
        assert!(!ok("http://127.0.0.1:4001/x"));
        assert!(!ok("https://github.com:8443/x"));
        assert!(ok("https://github.com:443/x"));
        assert!(!Policy::default().allows(&Url::parse("http://127.0.0.1:4000/").unwrap()));
    }

    #[test]
    fn certificate_time_errors_point_at_the_clock() {
        use rustls::{CertificateError as C, pki_types::UnixTime};
        let t = |s| UnixTime::since_unix_epoch(Duration::from_secs(s));
        for cert in [
            C::ExpiredContext {
                time: t(2),
                not_after: t(1),
            },
            C::NotValidYetContext {
                time: t(1),
                not_before: t(2),
            },
        ] {
            let e = io::Error::new(
                io::ErrorKind::InvalidData,
                rustls::Error::InvalidCertificate(cert),
            );
            assert!(matches!(io_err(e), Error::Tls(m) if m.contains("clock")));
        }
        let other = io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer),
        );
        assert!(matches!(io_err(other), Error::Io(m) if m.contains("invalid peer certificate")));
    }

    #[test]
    fn plain_403_is_not_a_rate_limit_but_exhausted_quota_is() {
        let head = |status, headers: &[(&str, &str)]| Head {
            status,
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
        };
        let reset = ("x-ratelimit-reset", "99999999999");
        assert!(!head(403, &[reset]).summary().rate_limited);
        assert!(
            head(403, &[reset, ("x-ratelimit-remaining", "0")])
                .summary()
                .rate_limited
        );
        assert!(head(403, &[("retry-after", "60")]).summary().rate_limited);
        assert!(head(429, &[]).summary().rate_limited);
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    #[test]
    fn the_race_gives_up_at_the_deadline_even_if_attempts_hang() {
        let started = Instant::now();
        let err = race_connect(
            &[sa("[2001:db8::1]:443"), sa("192.0.2.1:443")],
            Duration::from_millis(50),
            Instant::now() + Duration::from_millis(300),
            Arc::new(|_| {
                std::thread::sleep(Duration::from_secs(3));
                Err(io::Error::from(io::ErrorKind::TimedOut))
            }),
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn address_families_alternate_starting_with_the_resolvers_first() {
        let order = interleave(&[
            sa("[2001:db8::1]:443"),
            sa("[2001:db8::2]:443"),
            sa("192.0.2.1:443"),
            sa("192.0.2.2:443"),
        ]);
        let want = [
            "[2001:db8::1]:443",
            "192.0.2.1:443",
            "[2001:db8::2]:443",
            "192.0.2.2:443",
        ];
        assert_eq!(order, want.map(sa));
        let v4_first = interleave(&[sa("192.0.2.1:1"), sa("192.0.2.2:1"), sa("[::1]:1")]);
        assert_eq!(v4_first, ["192.0.2.1:1", "[::1]:1", "192.0.2.2:1"].map(sa));
        assert!(interleave(&[]).is_empty());
    }

    #[test]
    fn a_stalled_first_address_does_not_delay_the_second() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let live = listener.local_addr().unwrap();
        let stalled = sa("[2001:db8::1]:443");
        let started = Instant::now();
        let sock = race_connect(
            &[stalled, live],
            Duration::from_millis(50),
            Instant::now() + Duration::from_secs(10),
            Arc::new(move |a| {
                if *a == stalled {
                    std::thread::sleep(Duration::from_secs(5)); // a black hole
                    Err(io::Error::from(io::ErrorKind::TimedOut))
                } else {
                    TcpStream::connect(a)
                }
            }),
        )
        .unwrap();
        assert_eq!(sock.peer_addr().unwrap(), live);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_refused_first_address_falls_through_at_once_and_all_failing_reports_an_error() {
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap() // closed again: connection refused
        };
        let live = TcpListener::bind("[::1]:0")
            .or_else(|_| TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let connect =
            Arc::new(|a: &SocketAddr| TcpStream::connect_timeout(a, Duration::from_secs(2)));
        let started = Instant::now();
        // A long stagger: success must come from the failure, not the timer.
        let sock = race_connect(
            &[dead, live.local_addr().unwrap()],
            Duration::from_secs(30),
            far(),
            connect.clone(),
        )
        .unwrap();
        assert_eq!(sock.peer_addr().unwrap(), live.local_addr().unwrap());
        assert!(started.elapsed() < Duration::from_secs(5));
        let err =
            race_connect(&[dead, dead], Duration::from_millis(10), far(), connect).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
        assert!(
            race_connect(
                &[],
                Duration::from_millis(10),
                far(),
                Arc::new(|_| unreachable!())
            )
            .is_err()
        );
    }

    #[test]
    fn proxy_settings_parse_and_honour_no_proxy() {
        let p = Proxy::parse(
            "http://proxy.example:8080/",
            "localhost, .Internal.example,10.0.0.1",
        )
        .unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.example", 8080));
        assert!(
            p.bypasses("localhost", 443)
                && p.bypasses("a.internal.example", 443)
                && p.bypasses("10.0.0.1", 443)
        );
        assert!(!p.bypasses("api.github.com", 443) && !p.bypasses("notinternal.example", 443));
        assert!(
            Proxy::parse("proxy:3128", "*")
                .unwrap()
                .bypasses("github.com", 443)
        );
        assert_eq!(Proxy::parse("http://p", "").unwrap().port, 3128);
        assert_eq!(Proxy::parse("http://[::1]:99", "").unwrap().host, "::1");
        for bad in [
            "https://p:1",
            "socks5://p:1",
            "http://user:pw@p:1",
            "http://p:1/x",
            "",
            "http://p:0",
        ] {
            assert!(Proxy::parse(bad, "").is_none(), "{bad}");
        }
    }

    #[test]
    fn proxy_env_edge_cases() {
        // Scheme case, trailing slash, whitespace, IPv6 literal with and without a port.
        let p = Proxy::parse("  HTTP://Proxy.Example:8080/ ", "").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.example", 8080));
        let p = Proxy::parse("http://[2001:db8::1]", "").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("2001:db8::1", 3128));
        let p = Proxy::parse("[2001:db8::1]:81", "").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("2001:db8::1", 81));
        for bad in ["HTTPS://p:1", "   ", "http://", "http://p:", "http://[::1"] {
            assert!(Proxy::parse(bad, "").is_none(), "{bad:?}");
        }

        let p = Proxy::parse(
            "p:1",
            "example.com:8443, *.Wild.test ,[2001:db8::5], 2001:db8::6 ,:80,,bad:port, .dot.test",
        )
        .unwrap();
        // host:port entries only match that port, subdomains included.
        assert!(p.bypasses("example.com", 8443) && p.bypasses("a.example.com", 8443));
        assert!(!p.bypasses("example.com", 443));
        // Wildcard and leading-dot forms match the domain and its subdomains.
        assert!(p.bypasses("x.wild.test", 443) && p.bypasses("WILD.test", 443));
        assert!(p.bypasses("x.dot.test", 443) && !p.bypasses("notwild.test", 443));
        // IPv6 literals, bracketed or bare.
        assert!(p.bypasses("2001:db8::5", 443) && p.bypasses("2001:db8::6", 443));
        assert!(!p.bypasses("2001:db8::7", 443));
        // A port-qualified wildcard and an empty-host entry are not a match-all.
        let q = Proxy::parse("p:1", "*:8080, :80").unwrap();
        assert!(q.bypasses("github.com", 8080) && !q.bypasses("github.com", 443));
    }

    #[test]
    fn a_proxy_reply_that_is_not_a_clean_2xx_is_refused() {
        for reply in [
            "HTTP/1.1 2 OK\r\n\r\n",
            "HTTP/1.1 2000 OK\r\n\r\n",
            "ICY 200 OK\r\n\r\n",
        ] {
            let (addr, h) = fake_proxy(reply);
            let sock = TcpStream::connect(addr).unwrap();
            assert!(proxy_tunnel(sock, "h", 443, far()).is_err(), "{reply:?}");
            let _ = h.join();
        }
    }

    #[test]
    fn connect_reply_limits_hold_for_endless_and_silent_proxies() {
        // Endless header bytes with no terminator: refused at the cap, not buffered forever.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let _ = s.write_all(&vec![b'a'; 100_000]);
        });
        let sock = TcpStream::connect(addr).unwrap();
        let err = proxy_tunnel(sock, "h", 443, far()).unwrap_err();
        assert!(err.to_string().contains("too long"), "{err}");
        let _ = h.join();

        // A proxy that accepts and says nothing: the read timeout / deadline ends it.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let (done, wait) = mpsc::channel::<()>();
        let h = std::thread::spawn(move || {
            let (_s, _) = l.accept().unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(5));
        });
        let sock = TcpStream::connect(addr).unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let started = Instant::now();
        assert!(proxy_tunnel(sock, "h", 443, far()).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(done);
        let _ = h.join();
    }

    /// A one-shot proxy on a local socket: answers the CONNECT with `reply`, then echoes what
    /// follows. Returns its address and the CONNECT head it saw.
    fn fake_proxy(reply: &'static str) -> (SocketAddr, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut head = Vec::new();
            let mut b = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                s.read_exact(&mut b).unwrap();
                head.push(b[0]);
            }
            s.write_all(reply.as_bytes()).unwrap();
            let mut rest = [0_u8; 4];
            if s.read_exact(&mut rest).is_ok() {
                let _ = s.write_all(&rest);
            }
            String::from_utf8(head).unwrap()
        });
        (addr, h)
    }

    #[test]
    fn connect_tunnel_succeeds_on_2xx_and_leaves_the_stream_untouched() {
        let (addr, seen) = fake_proxy("HTTP/1.1 200 Connection established\r\nVia: x\r\n\r\n");
        let sock = TcpStream::connect(addr).unwrap();
        let mut sock = proxy_tunnel(
            sock,
            "api.github.com",
            443,
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        sock.write_all(b"ping").unwrap();
        let mut echo = [0_u8; 4];
        sock.read_exact(&mut echo).unwrap();
        assert_eq!(&echo, b"ping");
        let head = seen.join().unwrap();
        assert!(
            head.starts_with("CONNECT api.github.com:443 HTTP/1.1\r\n"),
            "{head}"
        );
    }

    #[test]
    fn connect_tunnel_refusal_is_an_error_naming_the_proxy() {
        for reply in [
            "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
            "garbage\r\n\r\n",
        ] {
            let (addr, seen) = fake_proxy(reply);
            let sock = TcpStream::connect(addr).unwrap();
            let err = proxy_tunnel(
                sock,
                "api.github.com",
                443,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(err.to_string().contains("proxy"), "{err}");
            let _ = seen.join();
        }
    }

    #[test]
    fn the_client_reaches_a_loopback_origin_directly_even_with_a_proxy_set() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let mut client = Client::new(
            "t",
            Policy {
                loopback_origin: Some(("127.0.0.1".into(), port)),
            },
            Duration::from_secs(2),
            None,
        )
        .unwrap();
        client.proxy = Proxy::parse("http://127.0.0.1:9", "");
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0_u8; 512];
            let _ = s.read(&mut buf);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let mut body = Vec::new();
        let got = client
            .get(
                &format!("http://127.0.0.1:{port}/x"),
                &[],
                100,
                Duration::from_secs(5),
                &mut |c| {
                    body.extend_from_slice(c);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(got.status, 200);
        assert_eq!(body, b"ok");
        h.join().unwrap();
    }
}
