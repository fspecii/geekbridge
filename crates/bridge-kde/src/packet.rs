//! Newline-delimited JSON packets with hard size, depth and time bounds.
//!
//! Any framing or parse failure is fatal for the connection: the reader never
//! tries to resynchronise after an oversize or malformed line.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, timeout_at};

use crate::error::{KdeError, Result};

/// Nesting limit applied to every incoming packet.
pub const MAX_JSON_DEPTH: usize = 64;
const MAX_TYPE_LEN: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub id: i64,
    pub kind: String,
    pub body: Map<String, Value>,
    /// Present only when a payload follows. This crate does not implement
    /// payload sockets; the fields are validated and passed through.
    pub payload_size: Option<i64>,
    pub payload_transfer_info: Option<Map<String, Value>>,
}

impl Packet {
    pub fn new(kind: &str, body: Map<String, Value>) -> Self {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Self {
            id,
            kind: kind.to_owned(),
            body,
            payload_size: None,
            payload_transfer_info: None,
        }
    }

    /// Serialise as one compact JSON line including the trailing `\n`.
    pub fn to_line(&self) -> Vec<u8> {
        let mut obj = Map::new();
        obj.insert("id".into(), Value::from(self.id));
        obj.insert("type".into(), Value::from(self.kind.as_str()));
        obj.insert("body".into(), Value::Object(self.body.clone()));
        if let Some(size) = self.payload_size {
            obj.insert("payloadSize".into(), Value::from(size));
        }
        if let Some(info) = &self.payload_transfer_info {
            obj.insert("payloadTransferInfo".into(), Value::Object(info.clone()));
        }
        // Serialising a Value cannot fail; escapes keep the line free of raw newlines.
        let mut line = serde_json::to_vec(&Value::Object(obj)).expect("Value serialises");
        line.push(b'\n');
        line
    }

    /// Parse one line (without the trailing newline).
    pub fn from_line(line: &[u8]) -> Result<Self> {
        check_depth(line)?;
        let value: Value = serde_json::from_slice(line).map_err(|_| bad("not valid JSON"))?;
        let Value::Object(mut obj) = value else {
            return Err(bad("packet is not a JSON object"));
        };

        let kind = match obj.remove("type") {
            Some(Value::String(s)) if !s.is_empty() && s.len() <= MAX_TYPE_LEN => s,
            _ => return Err(bad("type missing, empty or not a string")),
        };
        let body = match obj.remove("body") {
            Some(Value::Object(m)) => m,
            _ => return Err(bad("body missing or not an object")),
        };
        let id = match obj.remove("id") {
            Some(Value::Number(n)) => n.as_i64().ok_or_else(|| bad("id is not an integer"))?,
            Some(Value::String(s)) if s.len() <= 19 => {
                s.parse().map_err(|_| bad("id is not an integer"))?
            }
            _ => return Err(bad("id missing or not an integer")),
        };
        let payload_size = match obj.remove("payloadSize") {
            None => None,
            Some(Value::Number(n)) => Some(
                n.as_i64()
                    .ok_or_else(|| bad("payloadSize is not an integer"))?,
            ),
            Some(_) => return Err(bad("payloadSize is not an integer")),
        };
        let payload_transfer_info = match obj.remove("payloadTransferInfo") {
            None => None,
            Some(Value::Object(m)) => Some(m),
            Some(_) => return Err(bad("payloadTransferInfo is not an object")),
        };
        if payload_size.is_some() && payload_transfer_info.is_none() {
            return Err(bad("payloadSize without payloadTransferInfo"));
        }
        if payload_size.is_some_and(|s| s < -1) {
            return Err(bad("payloadSize is out of range"));
        }
        Ok(Self {
            id,
            kind,
            body,
            payload_size,
            payload_transfer_info,
        })
    }
}

fn bad(msg: &str) -> KdeError {
    KdeError::MalformedPacket(msg.to_owned())
}

/// Reject JSON nested deeper than [`MAX_JSON_DEPTH`] before handing it to a parser.
fn check_depth(line: &[u8]) -> Result<()> {
    let (mut depth, mut in_str, mut escaped) = (0usize, false, false);
    for &b in line {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_DEPTH {
                    return Err(bad("nesting too deep"));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Read one line a byte at a time, so nothing beyond the newline is consumed.
/// Used for the plaintext connection packet, after which the same socket is
/// handed to TLS. `limit` bounds the line length excluding the newline.
pub async fn read_line_unbuffered<R: AsyncRead + Unpin>(
    r: &mut R,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = timeout_at(deadline, r.read(&mut b))
            .await
            .map_err(|_| KdeError::Timeout("reading a line"))??;
        if n == 0 {
            return Err(KdeError::UnexpectedEof);
        }
        if b[0] == b'\n' {
            return Ok(line);
        }
        if line.len() >= limit {
            return Err(KdeError::LineTooLong { limit });
        }
        line.push(b[0]);
    }
}

/// Buffered line reader. Cancel-safe: dropping a `read_line` future never loses
/// bytes. After any error the reader is poisoned and refuses further reads.
pub struct LineReader<R> {
    inner: R,
    buf: Vec<u8>,
    scanned: usize,
    poisoned: bool,
}

impl<R: AsyncRead + Unpin> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            scanned: 0,
            poisoned: false,
        }
    }

    /// Next line without its newline; at most `limit` bytes long.
    pub async fn read_line(&mut self, limit: usize) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(KdeError::UnexpectedEof);
        }
        let r = self.read_line_inner(limit).await;
        if r.is_err() {
            self.poisoned = true;
        }
        r
    }

    pub async fn read_line_until(&mut self, limit: usize, deadline: Instant) -> Result<Vec<u8>> {
        match timeout_at(deadline, self.read_line(limit)).await {
            Ok(r) => r,
            Err(_) => {
                self.poisoned = true;
                Err(KdeError::Timeout("reading a line"))
            }
        }
    }

    async fn read_line_inner(&mut self, limit: usize) -> Result<Vec<u8>> {
        loop {
            if let Some(off) = self.buf[self.scanned..].iter().position(|&b| b == b'\n') {
                let end = self.scanned + off;
                if end > limit {
                    return Err(KdeError::LineTooLong { limit });
                }
                let mut line: Vec<u8> = self.buf.drain(..=end).collect();
                line.pop();
                self.scanned = 0;
                return Ok(line);
            }
            self.scanned = self.buf.len();
            if self.buf.len() > limit {
                return Err(KdeError::LineTooLong { limit });
            }
            let mut chunk = [0u8; 8192];
            let room = (limit + 1 - self.buf.len()).min(chunk.len());
            // TLS reports a peer that vanished without close_notify as UnexpectedEof.
            let n = match self.inner.read(&mut chunk[..room]).await {
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => 0,
                other => other?,
            };
            if n == 0 {
                return Err(KdeError::UnexpectedEof);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

pub async fn write_packet<W: AsyncWrite + Unpin>(
    w: &mut W,
    packet: &Packet,
    limit: usize,
    deadline: Instant,
) -> Result<()> {
    let line = packet.to_line();
    if line.len() > limit {
        return Err(KdeError::OutgoingTooLarge {
            size: line.len(),
            limit,
        });
    }
    timeout_at(deadline, async {
        w.write_all(&line).await?;
        w.flush().await
    })
    .await
    .map_err(|_| KdeError::Timeout("writing a packet"))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn round_trip() {
        let mut body = Map::new();
        body.insert("k".into(), Value::from("v\nwith newline"));
        let p = Packet::new("kdeconnect.ping", body);
        let line = p.to_line();
        assert_eq!(line.iter().filter(|&&b| b == b'\n').count(), 1);
        let back = Packet::from_line(&line[..line.len() - 1]).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn rejects_structurally_bad_packets() {
        let bad: &[&str] = &[
            r#"{"id":1,"type":"t"}"#,
            r#"{"id":1,"body":{}}"#,
            r#"{"id":1,"type":5,"body":{}}"#,
            r#"{"id":1,"type":"t","body":[]}"#,
            r#"{"type":"t","body":{}}"#,
            r#"{"id":1.5,"type":"t","body":{}}"#,
            r#"{"id":1,"type":"t","body":{},"payloadSize":5}"#,
            r#"{"id":1,"type":"t","body":{},"payloadSize":"5","payloadTransferInfo":{}}"#,
            r#"{"id":1,"type":"t","body":{},"payloadSize":-7,"payloadTransferInfo":{}}"#,
            r#"[1,2,3]"#,
            r#"not json"#,
            r#""#,
        ];
        for b in bad {
            assert!(
                Packet::from_line(b.as_bytes()).is_err(),
                "should reject {b}"
            );
        }
        let ok =
            r#"{"id":1,"type":"t","body":{},"payloadSize":-1,"payloadTransferInfo":{"port":1739}}"#;
        assert!(Packet::from_line(ok.as_bytes()).is_ok());
    }

    #[test]
    fn depth_limit() {
        let nest = |n: usize| {
            format!(
                r#"{{"id":1,"type":"t","body":{{"a":{}0{}}}}}"#,
                "[".repeat(n),
                "]".repeat(n)
            )
        };
        assert!(Packet::from_line(nest(60).as_bytes()).is_ok());
        assert!(Packet::from_line(nest(70).as_bytes()).is_err());
        // Brackets inside strings do not count.
        let s = format!(
            r#"{{"id":1,"type":"t","body":{{"a":"{}"}}}}"#,
            "[".repeat(500)
        );
        assert!(Packet::from_line(s.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn line_reader_limits_and_poison() {
        let data: &[u8] = b"short\nthis line is far too long\nafter\n";
        let mut r = LineReader::new(data);
        assert_eq!(r.read_line(10).await.unwrap(), b"short");
        assert!(matches!(
            r.read_line(10).await,
            Err(KdeError::LineTooLong { limit: 10 })
        ));
        // Poisoned: the tail is never parsed as the next line.
        assert!(r.read_line(100).await.is_err());
    }

    #[tokio::test]
    async fn line_reader_without_newline_is_bounded() {
        let data = vec![b'a'; 100_000];
        let mut r = LineReader::new(data.as_slice());
        assert!(matches!(
            r.read_line(1000).await,
            Err(KdeError::LineTooLong { .. })
        ));
        let mut r = LineReader::new(&b"abc"[..]);
        assert!(matches!(
            r.read_line(1000).await,
            Err(KdeError::UnexpectedEof)
        ));
    }

    #[tokio::test]
    async fn limit_is_exact() {
        let mut r = LineReader::new(&b"12345\n"[..]);
        assert_eq!(r.read_line(5).await.unwrap(), b"12345");
        let mut r = LineReader::new(&b"123456\n"[..]);
        assert!(r.read_line(5).await.is_err());
        let mut r = &b"12345\n"[..];
        assert_eq!(
            read_line_unbuffered(&mut r, 5, soon()).await.unwrap(),
            b"12345"
        );
        let mut r = &b"123456\n"[..];
        assert!(read_line_unbuffered(&mut r, 5, soon()).await.is_err());
    }

    #[tokio::test]
    async fn unbuffered_reader_leaves_following_bytes() {
        let mut r = &b"first\nsecond\n"[..];
        assert_eq!(
            read_line_unbuffered(&mut r, 100, soon()).await.unwrap(),
            b"first"
        );
        assert_eq!(r, b"second\n");
    }

    #[tokio::test(start_paused = true)]
    async fn slow_loris_hits_deadline() {
        let (mut a, b) = tokio::io::duplex(64);
        let mut r = LineReader::new(b);
        let d = Instant::now() + Duration::from_secs(3);
        let feeder = tokio::spawn(async move {
            for _ in 0..10 {
                a.write_all(b"x").await.unwrap();
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        });
        assert!(matches!(
            r.read_line_until(100, d).await,
            Err(KdeError::Timeout(_))
        ));
        feeder.abort();
    }
}
