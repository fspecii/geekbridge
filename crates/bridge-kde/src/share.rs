// SPDX-License-Identifier: GPL-3.0-or-later
//! KDE Connect `share` plugin: packets, filename hygiene and safe file reception.
//!
//! * `kdeconnect.share.request` carries one of `filename` (+ `payloadSize` and
//!   `payloadTransferInfo.port`), `text`, or `url` in its body. File bytes travel on a separate
//!   payload socket, see [`crate::payload`].
//! * Peer-supplied file names are never used as paths: [`sanitize_filename`] reduces them to a single
//!   safe path component and [`reserve_unique`] creates the destination with `O_EXCL` semantics, so
//!   an existing file is never overwritten.
//! * Received bytes go to a hidden temporary file in the destination directory and are renamed over
//!   the reserved name only after the whole payload arrived, so a cancelled or truncated transfer
//!   never leaves a plausible-looking partial file behind.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::packet::Packet;
use crate::payload::{CancelToken, PayloadContext, PayloadError, PayloadListener, PayloadResult};

pub const SHARE_REQUEST: &str = "kdeconnect.share.request";
/// Sent by stock peers after a multi-file batch announcement; accepted and ignored.
pub const SHARE_REQUEST_UPDATE: &str = "kdeconnect.share.request.update";

pub const MAX_FILENAME_BYTES: usize = 200;
pub const MAX_TEXT_BYTES: usize = 48 * 1024;
pub const MAX_URL_BYTES: usize = 4096;
/// Default cap for a single received file.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShareError {
    #[error("malformed share packet: {0}")]
    Malformed(&'static str),
    #[error("file of {size} bytes exceeds the {limit} byte limit")]
    TooLarge { size: u64, limit: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOffer {
    /// Already sanitised: a single path component.
    pub name: String,
    /// `None` when the sender announced `-1` (unknown length).
    pub size: Option<u64>,
    pub port: u16,
    /// Milliseconds since the Unix epoch, as sent by the peer.
    pub last_modified_ms: Option<i64>,
    pub number_of_files: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareContent {
    File(FileOffer),
    Text(String),
    Url(String),
    /// `share.request.update`: batch bookkeeping with nothing to act on.
    Ignored,
}

/// Interpret an incoming share packet. Never touches the file system.
pub fn parse_share(packet: &Packet, max_file_bytes: u64) -> Result<ShareContent, ShareError> {
    if packet.kind == SHARE_REQUEST_UPDATE {
        return Ok(ShareContent::Ignored);
    }
    if packet.kind != SHARE_REQUEST {
        return Err(ShareError::Malformed("not a share packet"));
    }
    let body = &packet.body;
    if let Some(name) = body.get("filename") {
        let Value::String(name) = name else {
            return Err(ShareError::Malformed("filename is not a string"));
        };
        let size = match packet.payload_size {
            Some(-1) => None,
            Some(n) if n > 0 => Some(n as u64),
            Some(0) | None => return Err(ShareError::Malformed("file without a payload")),
            Some(_) => return Err(ShareError::Malformed("payload size out of range")),
        };
        if let Some(n) = size
            && n > max_file_bytes
        {
            return Err(ShareError::TooLarge {
                size: n,
                limit: max_file_bytes,
            });
        }
        let port = packet
            .payload_transfer_info
            .as_ref()
            .and_then(|i| i.get("port"))
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .ok_or(ShareError::Malformed("payloadTransferInfo.port missing"))?;
        return Ok(ShareContent::File(FileOffer {
            name: sanitize_filename(name),
            size,
            port,
            last_modified_ms: body.get("lastModified").and_then(Value::as_i64),
            number_of_files: body
                .get("numberOfFiles")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok()),
        }));
    }
    if let Some(text) = body.get("text") {
        return match text {
            Value::String(t) if !t.is_empty() && t.len() <= MAX_TEXT_BYTES => {
                Ok(ShareContent::Text(strip_controls(t, true)))
            }
            _ => Err(ShareError::Malformed(
                "text is empty, too long or not a string",
            )),
        };
    }
    if let Some(url) = body.get("url") {
        return match url {
            Value::String(u) if !u.is_empty() && u.len() <= MAX_URL_BYTES => {
                Ok(ShareContent::Url(strip_controls(u, false)))
            }
            _ => Err(ShareError::Malformed(
                "url is empty, too long or not a string",
            )),
        };
    }
    Err(ShareError::Malformed("no filename, text or url"))
}

fn strip_controls(s: &str, keep_newlines: bool) -> String {
    s.chars()
        .filter(|c| !c.is_control() || (keep_newlines && matches!(c, '\n' | '\t')))
        .collect()
}

/// `http`/`https` only: nothing else is ever offered to the system URL opener.
pub fn is_openable_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !url.chars().any(|c| c.is_control() || c.is_whitespace())
        && url.len() <= MAX_URL_BYTES
}

/// Outgoing file announcement.
pub fn file_packet(name: &str, size: u64, last_modified_ms: Option<i64>, port: u16) -> Packet {
    let mut body = Map::new();
    body.insert("filename".into(), Value::from(sanitize_filename(name)));
    body.insert("open".into(), Value::from(false));
    if let Some(ms) = last_modified_ms {
        body.insert("lastModified".into(), Value::from(ms));
    }
    body.insert("numberOfFiles".into(), Value::from(1));
    body.insert("totalPayloadSize".into(), Value::from(size));
    let mut p = Packet::new(SHARE_REQUEST, body);
    p.payload_size = Some(i64::try_from(size).unwrap_or(i64::MAX));
    let mut info = Map::new();
    info.insert("port".into(), Value::from(port));
    p.payload_transfer_info = Some(info);
    p
}

pub fn text_packet(text: &str) -> Packet {
    let mut body = Map::new();
    body.insert("text".into(), Value::from(text));
    Packet::new(SHARE_REQUEST, body)
}

pub fn url_packet(url: &str) -> Packet {
    let mut body = Map::new();
    body.insert("url".into(), Value::from(url));
    Packet::new(SHARE_REQUEST, body)
}

/// Reduce a peer-supplied name to one safe path component.
pub fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut name = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    if name.starts_with('.') {
        name.insert(0, '_');
    }
    if name.is_empty() {
        name = "file".into();
    }
    // Cut to length first (one byte of headroom for the `_` below): the cut can leave a trailing
    // space or dot, and a space-padded `CON` + 250 spaces + `x` becomes the device name `CON`.
    name = truncate_name(&name, MAX_FILENAME_BYTES - 1);
    name.truncate(name.trim_end_matches(['.', ' ']).len());
    // Windows device names are reserved with or without an extension.
    // Trailing spaces and dots before the extension do not stop `CON .txt` from being the device.
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        name.insert(0, '_');
    }
    name
}

fn truncate_name(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if name.len() - i <= 16 && i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    let mut keep = max - ext.len();
    while !stem.is_char_boundary(keep) {
        keep -= 1;
    }
    format!("{}{}", &stem[..keep], ext)
}

/// A destination name reserved in a directory.
#[derive(Debug)]
pub struct Reserved {
    pub path: PathBuf,
}

/// Create an empty file with a name that did not exist, trying `name (1).ext`, `name (2).ext`, ...
/// `create_new` makes the check-and-create atomic, so two concurrent transfers (or a planted file)
/// can never be overwritten.
pub fn reserve_unique(dir: &Path, name: &str) -> io::Result<Reserved> {
    let name = sanitize_filename(name);
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    for n in 0..1000u32 {
        let candidate = if n == 0 {
            name.clone()
        } else {
            let suffix = format!(" ({n})");
            let room = MAX_FILENAME_BYTES.saturating_sub(suffix.len() + ext.len());
            let mut keep = stem.len().min(room);
            while !stem.is_char_boundary(keep) {
                keep -= 1;
            }
            format!("{}{suffix}{ext}", &stem[..keep])
        };
        let path = dir.join(&candidate);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(Reserved { path }),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with this name",
    ))
}

fn temp_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut short = name;
    let mut end = short.len().min(64);
    while !short.is_char_boundary(end) {
        end -= 1;
    }
    short.truncate(end);
    // The truncated name alone is not unique (two long names can share a 64-byte prefix), so a
    // per-process counter plus pid and clock make every temp path distinct.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let token = format!("{:x}-{seq:x}-{nanos:x}", std::process::id());
    final_path.with_file_name(format!(".gb-{token}-{short}.part"))
}

/// Remove what a process that died mid-download left behind: `.gb-*.part` files nobody has touched for
/// `min_idle`, and the zero-byte reserved file each of them belonged to. Returns how many `.part`
/// files were removed. A live transfer keeps writing, so its temp file is never old enough.
pub fn sweep_partials(dir: &Path, min_idle: std::time::Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name
            .strip_prefix(".gb-")
            .and_then(|r| r.strip_suffix(".part"))
        else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let idle = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .unwrap_or_default();
        if !meta.is_file() || idle < min_idle {
            continue;
        }
        if std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
            // `.gb-<pid>-<seq>-<nanos>-<reserved name>.part`: the reserved name is what is left.
            if let Some(reserved) = rest.splitn(4, '-').nth(3) {
                let path = dir.join(reserved);
                if std::fs::metadata(&path).is_ok_and(|m| m.is_file() && m.len() == 0) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
    removed
}

/// Download one offered file into `dir` and return its final path.
///
/// On any failure (including cancellation) neither the temporary file nor the reserved name
/// remains.
pub async fn receive_file(
    ctx: &PayloadContext,
    offer: &FileOffer,
    dir: &Path,
    max_file_bytes: u64,
    progress: &mut (dyn FnMut(u64) + Send),
    cancel: &CancelToken,
) -> PayloadResult<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let reserved = reserve_unique(dir, &offer.name)?;
    let tmp = temp_path(&reserved.path);
    let _ = std::fs::remove_file(&tmp);
    let result = async {
        let std_file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let mut out = tokio::fs::File::from_std(std_file);
        ctx.receive(
            offer.port,
            offer.size,
            max_file_bytes,
            &mut out,
            progress,
            cancel,
        )
        .await?;
        let std_file = out.into_std().await;
        if let Some(t) = offer.last_modified_ms.and_then(sane_mtime) {
            let _ = std_file.set_modified(t);
        }
        drop(std_file);
        // Replaces our own empty reservation atomically.
        std::fs::rename(&tmp, &reserved.path)?;
        Ok::<(), PayloadError>(())
    }
    .await;
    match result {
        Ok(()) => Ok(reserved.path),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&reserved.path);
            Err(e)
        }
    }
}

fn sane_mtime(ms: i64) -> Option<SystemTime> {
    let ms = u64::try_from(ms).ok().filter(|m| *m > 0)?;
    let t = UNIX_EPOCH + Duration::from_millis(ms);
    (t < SystemTime::now() + Duration::from_secs(86_400)).then_some(t)
}

/// A local file chosen for sending.
#[derive(Debug)]
pub struct OutgoingFile {
    pub file: tokio::fs::File,
    pub name: String,
    pub size: u64,
    pub last_modified_ms: Option<i64>,
}

/// Open `path` for sending. Only regular, non-empty files within `max_file_bytes` qualify.
pub async fn open_outgoing(path: &Path, max_file_bytes: u64) -> io::Result<OutgoingFile> {
    let file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "only regular files can be sent",
        ));
    }
    if meta.len() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty files cannot be sent",
        ));
    }
    if meta.len() > max_file_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file is larger than the transfer limit",
        ));
    }
    let name = path
        .file_name()
        .map(|n| sanitize_filename(&n.to_string_lossy()))
        .unwrap_or_else(|| "file".into());
    let last_modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok());
    Ok(OutgoingFile {
        file,
        name,
        size: meta.len(),
        last_modified_ms,
    })
}

/// Announce-then-serve helper: bind the payload port, build the packet to send on the link and
/// return the listener to run after the packet was written.
pub async fn prepare_send(
    ctx: &PayloadContext,
    out: &OutgoingFile,
) -> PayloadResult<(Packet, PayloadListener)> {
    let listener = ctx.listen().await?;
    let packet = file_packet(&out.name, out.size, out.last_modified_ms, listener.port());
    Ok((packet, listener))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_names() {
        for (raw, want) in [
            ("../../etc/passwd", "passwd"),
            ("..\\..\\boot.ini", "boot.ini"),
            ("a/b/c.txt", "c.txt"),
            ("", "file"),
            ("..", "file"),
            (".", "file"),
            ("/", "file"),
            (".bashrc", "_.bashrc"),
            ("name. . ", "name"),
            ("CON", "_CON"),
            ("nul.txt", "_nul.txt"),
            ("com1", "_com1"),
            ("CON .txt", "_CON .txt"),
            ("aux.", "_aux"),
            ("Lpt9.log", "_Lpt9.log"),
            ("CON:stream", "CON_stream"),
            ("report.txt:Zone.Identifier", "report.txt_Zone.Identifier"),
            ("a:b*c?.txt", "a_b_c_.txt"),
            ("tab\there\n.txt", "tab_here_.txt"),
            ("ok name (1).tar.gz", "ok name (1).tar.gz"),
        ] {
            assert_eq!(sanitize_filename(raw), want, "{raw:?}");
        }
        let long = format!("{}.jpeg", "é".repeat(300));
        let out = sanitize_filename(&long);
        assert!(out.len() <= MAX_FILENAME_BYTES && out.ends_with(".jpeg"));
    }

    #[test]
    fn names_differing_only_by_case_never_overwrite_each_other() {
        // Case-insensitive on Windows and default macOS volumes, case-sensitive on most Linux ones.
        // Either way the second reservation must be a different path, and the file must survive.
        let dir = tempfile::tempdir().unwrap();
        let first = reserve_unique(dir.path(), "Photo.JPG").unwrap();
        std::fs::write(&first.path, b"one").unwrap();
        let second = reserve_unique(dir.path(), "photo.jpg").unwrap();
        assert_ne!(first.path, second.path);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"one");
    }

    #[test]
    fn sweep_removes_dead_partials_and_their_empty_reservation_only() {
        let dir = tempfile::tempdir().unwrap();
        let reserved = reserve_unique(dir.path(), "movie.mkv").unwrap();
        let part = temp_path(&reserved.path);
        std::fs::write(&part, b"half").unwrap();
        let keep_real = dir.path().join("kept.bin");
        std::fs::write(&keep_real, b"data").unwrap();
        // A part file whose reservation holds real data: the part goes, the data stays.
        let other = dir.path().join(".gb-1-2-3-kept.bin.part");
        std::fs::write(&other, b"x").unwrap();
        // Recently written: a transfer in progress.
        assert_eq!(
            sweep_partials(dir.path(), std::time::Duration::from_secs(600)),
            0
        );
        assert!(part.exists());
        assert_eq!(sweep_partials(dir.path(), std::time::Duration::ZERO), 2);
        assert!(!part.exists() && !reserved.path.exists() && !other.exists());
        assert_eq!(std::fs::read(&keep_real).unwrap(), b"data");
    }

    #[test]
    fn temp_path_survives_multibyte_at_truncation_boundary() {
        // Byte 64 falls inside the two-byte `é`; this used to panic in String::truncate.
        let name = format!("{}é.bin", "a".repeat(63));
        let tmp = temp_path(Path::new("/tmp").join(&name).as_path());
        let n = tmp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with(".gb-") && n.ends_with(".part"), "{n}");
        assert!(n.len() < 255);
    }

    #[test]
    fn temp_paths_differ_for_names_sharing_a_long_prefix() {
        let prefix = "p".repeat(100);
        let a = temp_path(
            Path::new("/tmp")
                .join(format!("{prefix}-one.bin"))
                .as_path(),
        );
        let b = temp_path(
            Path::new("/tmp")
                .join(format!("{prefix}-two.bin"))
                .as_path(),
        );
        let a2 = temp_path(
            Path::new("/tmp")
                .join(format!("{prefix}-one.bin"))
                .as_path(),
        );
        assert_ne!(a, b);
        assert_ne!(a, a2);
    }

    #[test]
    fn reserve_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"keep").unwrap();
        let r1 = reserve_unique(dir.path(), "a.txt").unwrap();
        let r2 = reserve_unique(dir.path(), "../a.txt").unwrap();
        assert_eq!(r1.path.file_name().unwrap(), "a (1).txt");
        assert_eq!(r2.path.file_name().unwrap(), "a (2).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"keep");
        assert!(r2.path.starts_with(dir.path()));
    }

    #[test]
    fn parses_offers_and_text() {
        let p = file_packet("../x.bin", 10, Some(1_700_000_000_000), 1740);
        let Ok(ShareContent::File(o)) = parse_share(&p, 100) else {
            panic!()
        };
        assert_eq!((o.name.as_str(), o.size, o.port), ("x.bin", Some(10), 1740));
        assert_eq!(
            parse_share(&p, 5),
            Err(ShareError::TooLarge { size: 10, limit: 5 })
        );
        let mut bad = p.clone();
        bad.payload_transfer_info = None;
        bad.payload_size = Some(10);
        assert!(parse_share(&bad, 100).is_err());
        let mut zero = p.clone();
        zero.payload_size = Some(0);
        assert!(parse_share(&zero, 100).is_err());
        assert_eq!(
            parse_share(&text_packet("hi\u{0}"), 1),
            Ok(ShareContent::Text("hi".into()))
        );
        assert_eq!(
            parse_share(&url_packet("https://a.b/c"), 1),
            Ok(ShareContent::Url("https://a.b/c".into()))
        );
        assert!(parse_share(&Packet::new(SHARE_REQUEST, Map::new()), 1).is_err());
    }

    #[test]
    fn only_web_urls_open() {
        assert!(is_openable_url("https://example.com/a?b=c"));
        assert!(is_openable_url("HTTP://example.com"));
        for bad in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "https://a b",
            "smb://x",
        ] {
            assert!(!is_openable_url(bad), "{bad}");
        }
    }
}
