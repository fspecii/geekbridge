//! A deliberately small blocking HTTP/1.1 GET client (rustls + the bundled Mozilla roots).
//!
//! Only what the updater needs: GET, `Connection: close`, identity encoding, Content-Length or
//! chunked bodies, bounded redirects, a hard body cap and an overall deadline. Plain `http://` is
//! accepted only for loopback hosts, which is how the tests talk to a local mock server.

use crate::Error;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{IpAddr, TcpStream, ToSocketAddrs},
    sync::Arc,
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
        let mut url = Url::parse(url)?;
        for _ in 0..=MAX_REDIRECTS {
            if !self.policy.allows(&url) {
                return Err(Error::Url(format!("refusing to contact {}", url.origin())));
            }
            let (mut reader, ctl) = self.connect(&url, deadline)?;
            let guard = Guard {
                ctl,
                deadline,
                io_timeout: self.io_timeout,
            };
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
        let addrs: Vec<_> = (url.host.as_str(), url.port)
            .to_socket_addrs()
            .map_err(io_err)?
            .collect();
        let mut last = Error::Io("no address".into());
        for addr in addrs {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Timeout)?;
            match TcpStream::connect_timeout(&addr, remaining.min(self.io_timeout)) {
                Ok(sock) => {
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
                    return Ok((BufReader::with_capacity(16 * 1024, stream), ctl));
                }
                Err(e) => last = io_err(e),
            }
        }
        Err(last)
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

pub(crate) fn io_err(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Error::Timeout,
        _ => Error::Io(e.to_string()),
    }
}

struct Guard {
    ctl: TcpStream,
    deadline: Instant,
    io_timeout: Duration,
}

impl Guard {
    /// Called before each blocking read or write: the socket timeout never exceeds what is left of
    /// the overall deadline.
    fn arm(&self) -> Result<(), Error> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Error::Timeout)?;
        let t = remaining.min(self.io_timeout);
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
}
