// SPDX-License-Identifier: GPL-3.0-or-later
//! A strict tar reader for the release archive: regular files and directories only.
//!
//! The archive is untrusted until the whole of it has been read, so this refuses everything that
//! is not plainly `geekbridge-linux-<arch>/<file>`: links of any kind, devices, pax/GNU extension
//! records, absolute or `..` paths, odd characters, duplicates, nested directories, and sizes or
//! counts beyond the limits. Nothing is ever written outside the destination paths the caller
//! hands out, and every file is created new (`O_EXCL`), never followed through a link.
use std::{
    collections::HashSet,
    fs::OpenOptions,
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
};

pub struct Limits {
    pub entries: usize,
    pub file_bytes: u64,
    pub total_bytes: u64,
}

pub struct Member {
    pub name: String,
    pub size: u64,
    pub exec: bool,
}

fn octal(field: &[u8]) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return Err("a number in the archive uses an unsupported encoding".into());
    }
    let text: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let text = text.trim_matches(|c| c == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    // `from_str_radix` would also take a leading '+'; a header field is octal digits only.
    if !text.bytes().all(|b| matches!(b, b'0'..=b'7')) {
        return Err("a number in the archive is not octal".to_owned());
    }
    u64::from_str_radix(text, 8).map_err(|_| "a number in the archive is not octal".to_owned())
}

fn shown(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(60)
        .map(|b| {
            if b.is_ascii_graphic() {
                *b as char
            } else {
                '?'
            }
        })
        .collect()
}

fn text_field(field: &[u8]) -> &[u8] {
    &field[..field.iter().position(|b| *b == 0).unwrap_or(field.len())]
}

/// `top/file` or `top/` -> (top, Some(file) | None). Anything else is an error.
fn split_path(raw: &[u8]) -> Result<(String, Option<String>), String> {
    let name = std::str::from_utf8(raw)
        .map_err(|_| format!("entry name '{}' is not UTF-8", shown(raw)))?;
    if name.is_empty() || name.starts_with('/') {
        return Err(format!("entry name '{}' is empty or absolute", shown(raw)));
    }
    let mut parts = Vec::new();
    for c in name.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "entry '{}' escapes the archive directory",
                    shown(raw)
                ));
            }
            c if c.len() > 100 || c.chars().any(|ch| ch.is_control() || ch == '\\') => {
                return Err(format!(
                    "entry name '{}' has unsupported characters",
                    shown(raw)
                ));
            }
            c => parts.push(c),
        }
    }
    match parts.as_slice() {
        [top] => Ok(((*top).to_owned(), None)),
        [top, file] => Ok(((*top).to_owned(), Some((*file).to_owned()))),
        _ => Err(format!(
            "entry '{}' is not directly inside the top directory",
            shown(raw)
        )),
    }
}

/// Unpack `r`. `dest(name)` says where a member goes, or `None` to read and discard it (still
/// counted against the limits). Returns the members seen, in archive order.
pub fn extract(
    mut r: impl Read,
    limits: &Limits,
    dest: &mut dyn FnMut(&str) -> Option<PathBuf>,
) -> Result<Vec<Member>, String> {
    let eof = |e: io::Error| -> String {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            "the archive is truncated".to_owned()
        } else {
            e.to_string()
        }
    };
    let (mut members, mut seen, mut total) = (Vec::new(), HashSet::new(), 0_u64);
    let mut top: Option<String> = None;
    let mut header = [0_u8; 512];
    loop {
        r.read_exact(&mut header).map_err(eof)?;
        if header.iter().all(|b| *b == 0) {
            break;
        }
        let sum: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        if octal(&header[148..156])? != sum {
            return Err("an archive header is damaged (checksum)".into());
        }
        if !header[257..262].eq(b"ustar") {
            return Err("the archive is not in ustar/GNU tar format".into());
        }
        let mut raw = text_field(&header[..100]).to_vec();
        if &header[257..265] == b"ustar\x0000" && header[345] != 0 {
            let mut p = text_field(&header[345..500]).to_vec();
            p.push(b'/');
            p.extend(raw);
            raw = p;
        }
        let kind = header[156];
        let (this_top, file) = split_path(&raw)?;
        match &top {
            Some(t) if *t != this_top => {
                return Err("the archive has more than one top directory".into());
            }
            _ => top = Some(this_top),
        }
        let size = octal(&header[124..136])?;
        match (kind, &file) {
            (b'5', None) if size == 0 => {
                // The top directory itself: once, and it counts like any other entry.
                if !seen.insert(String::new()) || seen.len() > limits.entries {
                    return Err(format!("entry '{}' appears twice", shown(&raw)));
                }
                continue;
            }
            (b'0' | 0, Some(_)) if !raw.ends_with(b"/") => {}
            (b'0' | 0 | b'5', _) => {
                return Err(format!("entry '{}' has an unexpected shape", shown(&raw)));
            }
            (k, _) => {
                let what = match k {
                    b'1' => "a hard link",
                    b'2' => "a symbolic link",
                    b'3' | b'4' | b'6' => "a device or pipe",
                    b'x' | b'g' | b'L' | b'K' => "an extension record",
                    _ => "an unsupported type",
                };
                return Err(format!("entry '{}' is {what}", shown(&raw)));
            }
        }
        let name = file.unwrap_or_default();
        if !seen.insert(name.clone()) {
            return Err(format!("entry '{}' appears twice", shown(&raw)));
        }
        if seen.len() > limits.entries {
            return Err("the archive has too many entries".into());
        }
        total = total.saturating_add(size);
        if size > limits.file_bytes || total > limits.total_bytes {
            return Err(format!("entry '{}' is too large", shown(&raw)));
        }
        let exec = octal(&header[100..108])? & 0o100 != 0;
        let mut body = (&mut r).take(size);
        let copied = match dest(&name) {
            Some(path) => {
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(if exec { 0o755 } else { 0o644 })
                    .open(&path)
                    .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
                let n = io::copy(&mut body, &mut f).map_err(eof)?;
                f.flush()
                    .and_then(|()| f.sync_all())
                    .map_err(|e| e.to_string())?;
                n
            }
            None => io::copy(&mut body, &mut io::sink()).map_err(eof)?,
        };
        if copied != size {
            return Err("the archive is truncated".into());
        }
        let pad = (512 - size % 512) % 512;
        io::copy(&mut (&mut r).take(pad), &mut io::sink()).map_err(eof)?;
        members.push(Member { name, size, exec });
    }
    // The rest must be zero padding; reading it to the end also lets the gzip layer check its trailer.
    let mut rest = [0_u8; 4096];
    let mut budget = 1 << 20;
    loop {
        let n = r.read(&mut rest).map_err(eof)?;
        if n == 0 {
            break;
        }
        budget -= n.min(budget);
        if budget == 0 || rest[..n].iter().any(|b| *b != 0) {
            return Err("unexpected data after the end of the archive".into());
        }
    }
    Ok(members)
}
