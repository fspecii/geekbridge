// SPDX-License-Identifier: GPL-3.0-or-later
//! One fuzz target per parser. Every target takes arbitrary bytes, must not panic, must keep its
//! output bounded, and asserts the property its sanitiser promises (no control characters, no path
//! separators, ...). A violated property is a panic whose message starts with `ORACLE`.

use bridge_core::{calls_link, clipboard_wire, ipc, phone_link};
use bridge_kde::{
    Identity, Packet,
    identity::{self, DeviceId},
    packet::{LineReader, read_line_unbuffered},
    pairing::PairBody,
    qrpair::{QrPayload, TokenBook},
    share, sms,
};
use serde_json::{Map, Value};
use std::{
    fmt::Debug,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, OnceLock},
    time::Duration,
};

pub struct Target {
    pub name: &'static str,
    pub run: fn(&[u8]),
    /// Packet types used when generating whole packets.
    pub kinds: &'static [&'static str],
    /// Fixture packets whose `type` starts with one of these seed the corpus (`"*"` = all).
    pub fixture_prefixes: &'static [&'static str],
    /// Extra seeds that are not packets (QR text, vCards, release JSON, ...).
    pub builtin_seeds: fn() -> Vec<Vec<u8>>,
}

fn none() -> Vec<Vec<u8>> {
    Vec::new()
}

// ------------------------------------------------------------------------------------ helpers

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime")
    })
}

/// A runtime of its own for one hub input: the hubs spawn watcher tasks that never finish, and on the
/// shared runtime every input would leave one (and the service it holds) behind.
fn fresh_rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime")
}

fn strip_nl(input: &[u8]) -> &[u8] {
    input.strip_suffix(b"\n").unwrap_or(input)
}

fn oracle(ok: bool, what: impl FnOnce() -> String) {
    if !ok {
        panic!("ORACLE: {}", what());
    }
}

fn no_control(s: &str, what: &str, allow_newline: bool) {
    for c in s.chars() {
        oracle(
            !c.is_control() || (allow_newline && matches!(c, '\n' | '\t')),
            || format!("{what} contains control character {c:?} in {s:?}"),
        );
    }
}

/// Output must not be wildly larger than the input that produced it.
fn bounded<T: Debug>(out: &T, input_len: usize, what: &str) {
    let shown = format!("{out:?}");
    oracle(shown.len() <= 24 * input_len + 128 * 1024, || {
        format!(
            "{what}: output of {} bytes from {} input bytes",
            shown.len(),
            input_len
        )
    });
}

/// `(type, body)` of the input: a well-formed packet, or else a JSON object used directly as a
/// body (so body parsers are reachable even when the envelope is damaged).
fn kind_and_body(input: &[u8], kinds: &[&str]) -> (String, Map<String, Value>) {
    if let Ok(p) = Packet::from_line(strip_nl(input)) {
        return (p.kind, p.body);
    }
    let fallback = if kinds.is_empty() {
        "x"
    } else {
        kinds[input.first().copied().unwrap_or(0) as usize % kinds.len()]
    };
    match serde_json::from_slice::<Value>(strip_nl(input)) {
        Ok(Value::Object(mut o)) => {
            let kind = o
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| fallback.to_owned());
            match o.remove("body") {
                Some(Value::Object(b)) => (kind, b),
                _ => (kind, o),
            }
        }
        _ => (fallback.to_owned(), Map::new()),
    }
}

/// serde_json's default float parser is not correctly rounded, so a non-integer number may move by
/// an ulp when re-parsed from its shortest form; that is a property of the dependency, not a defect
/// here. Everything else must be identical.
fn same_modulo_floats(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if x.is_f64() && y.is_f64() => true,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_modulo_floats(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| same_modulo_floats(v, w)))
        }
        _ => a == b,
    }
}

fn text(input: &[u8]) -> String {
    String::from_utf8_lossy(input).into_owned()
}

fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{061C}' | '\u{FEFF}'
    )
}

fn check_filename(name: &str) {
    no_control(name, "filename", false);
    oracle(!name.is_empty(), || "empty filename".into());
    oracle(!name.contains(['/', '\\']), || {
        format!("path separator in {name:?}")
    });
    oracle(!name.contains(['<', '>', ':', '"', '|', '?', '*']), || {
        format!("reserved character in {name:?}")
    });
    oracle(name != "." && name != "..", || format!("dot name {name:?}"));
    oracle(!name.starts_with('.'), || {
        format!("hidden file name {name:?}")
    });
    oracle(name.len() <= share::MAX_FILENAME_BYTES, || {
        format!("filename too long ({})", name.len())
    });
    oracle(!name.ends_with(['.', ' ']), || {
        format!("trailing dot or space (invalid on Windows) in {name:?}")
    });
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    oracle(
        !(matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.as_bytes()[3].is_ascii_digit())),
        || format!("Windows device name {name:?}"),
    );
}

// --------------------------------------------------------------------------------- KDE layer

fn packet_line(input: &[u8]) {
    let Ok(p) = Packet::from_line(strip_nl(input)) else {
        return;
    };
    bounded(&p.body, input.len(), "packet body");
    // Whatever parses re-serialises to one line that parses back to the same packet.
    let line = p.to_line();
    oracle(line.iter().filter(|b| **b == b'\n').count() == 1, || {
        "serialised packet is not one line".into()
    });
    let back = Packet::from_line(strip_nl(&line));
    oracle(
        back.as_ref().is_ok_and(|b| {
            b.kind == p.kind
                && same_modulo_floats(
                    &Value::Object(b.body.clone()),
                    &Value::Object(p.body.clone()),
                )
                && b.id == p.id
        }),
        || "packet does not round-trip".into(),
    );
}

fn identity_line(input: &[u8]) {
    // The plaintext identity line a connecting peer sends (after the newline split).
    let Ok(p) = Packet::from_line(strip_nl(input)) else {
        return;
    };
    if let Ok(id) = Identity::from_packet(&p) {
        no_control(&id.device_name, "identity device name", false);
        oracle(!id.device_name.is_empty(), || "empty device name".into());
        oracle(
            id.device_name.chars().count() <= identity::MAX_USER_NAME_CHARS,
            || "device name too long".into(),
        );
        oracle(!id.device_name.chars().any(is_invisible_format), || {
            format!("invisible format character in {:?}", id.device_name)
        });
        oracle(DeviceId::parse(id.device_id.as_str()).is_ok(), || {
            "invalid device id accepted".into()
        });
        let again = Identity::from_packet(&id.to_packet());
        oracle(again.is_ok(), || {
            "accepted identity does not survive re-encoding".into()
        });
    }
    let s = text(input);
    if let Some(n) = identity::sanitize_device_name(&s) {
        no_control(&n, "sanitised device name", false);
        oracle(!n.chars().any(is_invisible_format), || {
            format!("invisible format character in {n:?}")
        });
        oracle(n.chars().count() <= identity::MAX_USER_NAME_CHARS, || {
            "sanitised name too long".into()
        });
    }
    if let Ok(n) = identity::validate_device_name(&s) {
        no_control(&n, "validated device name", false);
    }
    let _ = DeviceId::parse(&s);
}

fn udp_discovery(input: &[u8]) {
    let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 8));
    if let Ok(d) = bridge_service::discovery::parse_identity(input, ip, 42) {
        no_control(&d.name, "discovered device name", false);
        oracle(d.name.chars().count() <= 64, || {
            "discovered name too long".into()
        });
        oracle(
            (32..=38).contains(&d.id.len())
                && d.id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            || format!("bad device id accepted: {:?}", d.id),
        );
        oracle(d.address == ip, || {
            "address is not the datagram source".into()
        });
        oracle((1716..=1764).contains(&d.tcp_port), || {
            "tcp port out of range".into()
        });
        oracle(
            d.incoming_capabilities.len() <= 128 && d.outgoing_capabilities.len() <= 128,
            || "too many capabilities".into(),
        );
        for c in d
            .incoming_capabilities
            .iter()
            .chain(&d.outgoing_capabilities)
        {
            oracle(
                c.len() <= 128 && c.bytes().all(|b| b.is_ascii_graphic()),
                || format!("bad capability {c:?}"),
            );
        }
        // The UDP path and the TCP identity path must agree on what a name may contain.
        oracle(!d.name.chars().any(is_invisible_format), || {
            format!(
                "UDP discovery keeps invisible format characters: {:?}",
                d.name
            )
        });
    }
    // Source addresses a datagram can never legitimately have.
    let _ = bridge_service::discovery::parse_identity(input, IpAddr::V4(Ipv4Addr::UNSPECIFIED), 42);
    let _ = bridge_service::discovery::parse_identity(
        input,
        IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)),
        42,
    );
}

fn line_reader(input: &[u8]) {
    // First byte picks the limit and chunking; the rest is the stream.
    let (&head, rest) = input.split_first().unwrap_or((&0, &[]));
    let limit = [16usize, 255, 1024, 8192, 65_536][head as usize % 5];
    let chunk = 1 + (head as usize / 5) % 41;
    rt().block_on(async {
        let (mut tx, rx) = tokio::io::duplex(64);
        let data = rest.to_vec();
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            for c in data.chunks(chunk) {
                if tx.write_all(c).await.is_err() {
                    return;
                }
            }
        });
        let mut r = LineReader::new(rx);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut total = 0usize;
        while let Ok(line) = r.read_line_until(limit, deadline).await {
            oracle(line.len() <= limit, || {
                format!("line of {} bytes exceeds limit {limit}", line.len())
            });
            oracle(!line.contains(&b'\n'), || "line contains newline".into());
            total += line.len() + 1;
            let _ = Packet::from_line(&line);
        }
        oracle(total <= rest.len() + 1, || {
            "reader produced more bytes than it was given".into()
        });
        writer.abort();
    });
    // The unbuffered plaintext reader must agree on framing.
    rt().block_on(async {
        let mut r = rest;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        if let Ok(line) = read_line_unbuffered(&mut r, limit, deadline).await {
            oracle(line.len() <= limit && !line.contains(&b'\n'), || {
                "unbuffered line violates its limit".into()
            });
        }
    });
}

fn pairing(input: &[u8]) {
    let Ok(p) = Packet::from_line(strip_nl(input)) else {
        return;
    };
    let _ = PairBody::parse(&p);
    let (_, body) = (p.kind, p.body);
    let mut pk = Packet::new("kdeconnect.pair", body);
    pk.id = 1;
    if let Ok(b) = PairBody::parse(&pk)
        && let Some(t) = &b.token
    {
        oracle(bridge_kde::qrpair::token_shape_ok(t), || {
            "unshaped token accepted".into()
        });
    }
}

fn qrpair(input: &[u8]) {
    let s = text(input);
    if let Ok(q) = QrPayload::parse(&s) {
        no_control(&q.name, "QR device name", false);
        oracle(q.addresses.len() <= 16, || {
            format!("{} addresses accepted", q.addresses.len())
        });
        oracle(bridge_kde::qrpair::token_shape_ok(&q.token), || {
            "unshaped QR token".into()
        });
        // A name made of multi-byte characters can encode past the 700-byte payload cap, which parse refuses.
        let encoded = q.encode();
        if encoded.len() <= 700 {
            oracle(QrPayload::parse(&encoded).is_ok(), || {
                "accepted QR payload does not re-parse".into()
            });
        }
    }
    // Token redemption with an attacker-chosen string.
    let mut book = TokenBook::new();
    let (token, _) = book.issue_with(
        "0123456789abcdef0123456789abcdef",
        1000,
        Duration::from_secs(120),
    );
    oracle(book.redeem(&s, 1001).is_err() || s == token, || {
        "wrong token redeemed".into()
    });
    let _ = book.redeem_from(&s, 1001, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    let _ = bridge_kde::qrpair::token_shape_ok(&s);
}

fn share_parse(input: &[u8]) {
    let s = text(input);
    let name = share::sanitize_filename(&s);
    check_filename(&name);
    let _ = share::is_openable_url(&s);
    let Ok(p) = Packet::from_line(strip_nl(input)) else {
        // Also drive the filename through a packet built around the raw text.
        let pkt = share::file_packet(&s, 10, None, 1739);
        if let Ok(share::ShareContent::File(f)) = share::parse_share(&pkt, 1 << 20) {
            check_filename(&f.name);
        }
        return;
    };
    match share::parse_share(&p, 1 << 30) {
        Ok(share::ShareContent::File(f)) => {
            check_filename(&f.name);
            oracle(f.port != 0, || "port 0 accepted".into());
            oracle(f.size.is_none_or(|n| n > 0 && n <= 1 << 30), || {
                "size out of bounds accepted".into()
            });
        }
        Ok(share::ShareContent::Text(t)) => {
            no_control(&t, "shared text", true);
            oracle(t.len() <= share::MAX_TEXT_BYTES, || {
                "text over limit".into()
            });
        }
        Ok(share::ShareContent::Url(u)) => {
            no_control(&u, "shared url", false);
            oracle(u.len() <= share::MAX_URL_BYTES, || "url over limit".into());
        }
        _ => {}
    }
    // The reservation path is real file-system code: a hostile name may only ever land in the directory.
    if let Ok(dir) = tempfile::tempdir()
        && let Ok(r) = share::reserve_unique(dir.path(), &name)
    {
        oracle(r.path.parent() == Some(dir.path()), || {
            format!("reserved path escapes: {:?}", r.path)
        });
    }
}

fn sms_parse(input: &[u8]) {
    let (_, body) = kind_and_body(input, &["kdeconnect.sms.messages"]);
    if let Ok(m) = sms::parse_messages(&body) {
        bounded(&m.messages, input.len(), "sms");
        oracle(m.messages.len() <= input.len() / 2 + 1, || {
            "more messages than input could hold".into()
        });
    }
    let u = sms::parse_unread(&body);
    oracle(u.len() <= input.len() + 1, || "unread amplification".into());
    let _ = sms::parse_sims(&body);
}

// --------------------------------------------------------------------- bridge-core extension

fn phone_wire(input: &[u8]) {
    let (_, body) = kind_and_body(
        input,
        &[
            "geekbridge.calendar.sync",
            "kdeconnect.contacts.response_uids_timestamps",
        ],
    );
    if let Ok(u) = phone_link::parse_uids(&body) {
        oracle(u.len() <= phone_link::MAX_CONTACT_UIDS, || {
            format!("{} uids accepted", u.len())
        });
        bounded(&u, input.len(), "uids");
    }
    if let Ok(v) = phone_link::parse_vcards(&body) {
        bounded(&v, input.len(), "vcards");
        for (_, vc) in v.iter().take(50) {
            let _ = phone_link::contact_from_vcard(vc);
        }
    }
    if let Ok(c) = phone_link::parse_calendar_sync(&body) {
        let _ = c;
    }
    let _ = phone_link::parse_write_result(&body);
    let _ = phone_link::parse_sms_status(&body);
}

fn vcard(input: &[u8]) {
    let s = text(input);
    let stripped = phone_link::strip_vcard_blobs(&s);
    oracle(stripped.len() <= s.len() + 16, || {
        "vcard blob stripping grew the text".into()
    });
    if let Ok(c) = phone_link::contact_from_vcard(&s) {
        bounded(&format!("{:?}", c.display_name), input.len(), "contact");
    }
    let _ = phone_link::contact_revision(Some(&s), &s);
    let _ = bridge_core::agenda::vcard_rev(&s);
}

fn calls_wire(input: &[u8]) {
    let (_, body) = kind_and_body(
        input,
        &[
            "geekbridge.calls.state",
            "geekbridge.calls.request",
            "geekbridge.calls.result",
        ],
    );
    let _ = calls_link::LinkCallReport::parse(&body);
    let _ = calls_link::parse_request(&body);
    let _ = calls_link::parse_result(&body);
}

fn clipboard_parse(input: &[u8]) {
    let (kind, body) = kind_and_body(
        input,
        &[
            "kdeconnect.clipboard",
            "kdeconnect.clipboard.connect",
            "geekbridge.clipboard.item",
            "geekbridge.clipboard.none",
            "geekbridge.clipboard.pull",
        ],
    );
    if let Ok(c) = clipboard_wire::parse(&kind, &body, 1 << 16) {
        bounded(&c, input.len(), "clipboard");
    }
    let limits = clipboard_wire::Limits {
        max_text_bytes: 1 << 16,
        max_image_bytes: 1 << 20,
    };
    let _ = clipboard_wire::parse_item(&body, &limits);
    let _ = clipboard_wire::parse_none(&body);
    let _ = clipboard_wire::parse_pull(&body);
    let _ = clipboard_wire::sniff_image(input);
    let s = text(input);
    let _ = clipboard_wire::looks_like_url(&s);
    let _ = clipboard_wire::image_mime(&s);
}

fn messages_wire(input: &[u8]) {
    let Ok(w) = serde_json::from_slice::<bridge_core::messages::WireMessage>(strip_nl(input))
    else {
        return;
    };
    let _ = bridge_core::messages::Message::from_wire(w);
    let t = text(input);
    let i = bridge_core::messages::sms_segments(&t);
    oracle(
        i.segments == 0 || i.units <= i.segments * i.per_segment,
        || format!("sms_segments: {i:?} cannot hold its own text"),
    );
    oracle(i.remaining <= i.per_segment, || format!("{i:?}"));
}

// ------------------------------------------------------------------------------ control API

fn ipc_parse(input: &[u8]) {
    let body = strip_nl(input);
    if let Ok(r) = serde_json::from_slice::<ipc::Request>(body) {
        let again = serde_json::to_vec(&r)
            .ok()
            .and_then(|b| serde_json::from_slice::<ipc::Request>(&b).ok());
        oracle(again.as_ref() == Some(&r), || {
            "request does not round-trip".into()
        });
    }
    let _ = serde_json::from_slice::<ipc::Response>(body);
    let _ = serde_json::from_slice::<ipc::Event>(body);
    let _ = serde_json::from_slice::<ipc::EventPage>(body);
    let _ = ipc::Token::parse(input);
    if let Some((mac, rest)) = ipc::split_frame(input) {
        oracle(rest.len() < input.len() && mac.len() == 32, || {
            "split_frame returned out-of-range data".into()
        });
    }
    let s = text(input);
    let line = s.trim_end_matches('\n');
    if let Ok(ipc::Hello::Ready { nonce }) = ipc::parse_hello(line) {
        oracle(ipc::decode_hex::<32>(&nonce).is_some(), || {
            "unchecked nonce accepted".into()
        });
    }
    let _ = ipc::decode_hex::<32>(line);
    let _ = ipc::decode_hex::<16>(line);
    let hello = ipc::format_hello(&ipc::Hello::Unavailable {
        code: s.clone(),
        message: s.clone(),
    });
    oracle(hello.matches('\n').count() == 1 && hello.is_ascii(), || {
        format!("hello not one clean ASCII line: {hello:?}")
    });
    oracle(hello.len() <= 400, || "hello unbounded".into());
}

fn android_dispatch(input: &[u8]) {
    let Ok(request) = std::str::from_utf8(input) else {
        return;
    };
    // `transport_start` with a state directory would bind sockets and write files; it is covered by
    // the engine's own tests, and everything else about it (argument parsing) is reachable without
    // a usable directory.
    if request.contains("transport_start") && request.contains("state_dir") {
        return;
    }
    let d = geekbridge_android::Dispatcher::new();
    let out = d.handle(request);
    let v: Result<Value, _> = serde_json::from_str(&out);
    oracle(
        v.as_ref().is_ok_and(|v| {
            v.get("version").is_some() && v.get("ok").and_then(Value::as_bool).is_some()
        }),
        || format!("response is not a well-formed envelope: {out:.200}"),
    );
    oracle(out.len() <= 64 * 1024 + 8 * input.len(), || {
        format!(
            "response of {} bytes for {} request bytes",
            out.len(),
            input.len()
        )
    });
}

// ----------------------------------------------------------------------------------- updater

fn update_parse(input: &[u8]) {
    use bridge_update::fuzzing::{
        Policy, Url, clean_notes, find_checksum, safe_file_name, url_allowed,
    };
    use bridge_update::{Platform, Release, Version, select_asset};
    let s = text(input);
    if let Ok(r) = Release::parse(input) {
        let _ = r.version();
        for (os, arch) in [
            ("linux", "x86_64"),
            ("windows", "x86_64"),
            ("macos", "aarch64"),
            ("linux", "aarch64"),
        ] {
            let p = Platform {
                os: os.into(),
                arch: arch.into(),
            };
            if let Some(a) = select_asset(&r.assets, &p) {
                oracle(safe_file_name(&a.name), || {
                    format!("unsafe asset name selected: {:?}", a.name)
                });
            }
        }
        for a in r.assets.iter().take(20) {
            let _ = Url::parse(&a.browser_download_url);
            if let Some(u) = &a.url {
                let _ = Url::parse(u);
            }
        }
    }
    if let Some(d) = find_checksum(&s, "geekbridge-linux-x86_64.tar.gz") {
        oracle(
            d.len() == 64
                && d.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            || format!("bad digest {d:?}"),
        );
    }
    let first = s.lines().next().unwrap_or("");
    let _ = find_checksum(&s, first.split_whitespace().last().unwrap_or(""));
    let notes = clean_notes(&s);
    no_control(&notes, "release notes", true);
    oracle(notes.chars().count() <= 4096 + 1, || {
        "release notes unbounded".into()
    });
    if let Ok(u) = Url::parse(first) {
        oracle(
            !u.host.is_empty() && !u.host.contains(['/', '@', '?', '#', ' ']),
            || format!("bad host parsed: {:?}", u.host),
        );
        oracle(u.path.starts_with('/'), || {
            "path without leading slash".into()
        });
        let p = Policy::default();
        let _ = p.sends_auth(&u);
        let _ = p.allows(&u);
    }
    let _ = url_allowed(&Policy::default(), first);
    let _ = Version::parse(first);
    let _ = bridge_update::valid_repo(first);
    let _ = safe_file_name(first);
}

// ------------------------------------------------------------------------------- certificates

fn cert_parse(input: &[u8]) {
    // A peer's certificate arrives in the TLS handshake: hostile DER and PEM both land here.
    let _ = bridge_kde::CertInfo::parse(input);
    let _ = bridge_kde::DeviceCredentials::from_pem(input, 1_700_000_000);
}

// ---------------------------------------------------------------------- service packet hubs

/// A sequence of packets (one per line) delivered to one hub, as a hostile peer would.
fn lines(input: &[u8]) -> impl Iterator<Item = (String, Map<String, Value>)> + '_ {
    input
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .take(64)
        .map(|l| kind_and_body(l, &[]))
}

fn hub_phone_data(input: &[u8]) {
    use bridge_service::phone_data::{Caps, PhoneData};
    let hub_rt = fresh_rt();
    let _g = hub_rt.enter();
    let hub = PhoneData::new();
    let caps = Caps {
        contacts: true,
        calendar: true,
        calendar_write: true,
        sms_read: true,
        sms_send: true,
        sms_status: true,
    };
    let (_rx, gen_) = hub.link_up(
        "0123456789abcdef0123456789abcdef",
        "Phone",
        caps,
        (true, true, true),
    );
    for (kind, body) in lines(input) {
        let _ = hub.on_packet("0123456789abcdef0123456789abcdef", &kind, &body);
    }
    hub.link_down("0123456789abcdef0123456789abcdef", gen_);
}

fn hub_calls(input: &[u8]) {
    use bridge_service::call_control::CallHub;
    let hub_rt = fresh_rt();
    let _g = hub_rt.enter();
    let hub = CallHub::new();
    let (_rx, gen_) = hub.link_up_generation("0123456789abcdef0123456789abcdef", "Phone", true);
    for (kind, body) in lines(input) {
        let _ = hub.on_packet("0123456789abcdef0123456789abcdef", &kind, &body);
        let _ = hub.status();
    }
    hub.link_down_generation("0123456789abcdef0123456789abcdef", gen_);
}

fn hub_clipboard(input: &[u8]) {
    use bridge_service::clipboard::{ClipboardService, LinkCaps, MemoryClipboard};
    let hub_rt = fresh_rt();
    let _g = hub_rt.enter();
    let backend = Arc::new(MemoryClipboard::default());
    let svc = ClipboardService::new(backend.clone(), "local-device");
    let caps = LinkCaps {
        stock: true,
        pull: true,
    };
    let (_rx, gen_) =
        svc.link_up_generation("0123456789abcdef0123456789abcdef", "Phone", caps, true);
    for (kind, body) in lines(input) {
        let port = body
            .get("port")
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok());
        let _ = svc.on_packet("0123456789abcdef0123456789abcdef", &kind, &body, port);
    }
    if let Some(t) = backend.text() {
        oracle(t.len() <= 4 * input.len() + 4096, || {
            "clipboard text amplified".into()
        });
    }
    svc.link_down_generation("0123456789abcdef0123456789abcdef", gen_);
}

// ---------------------------------------------------------------------------------- registry

const PHONE_KINDS: &[&str] = &[
    "kdeconnect.contacts.response_uids_timestamps",
    "kdeconnect.contacts.response_vcards",
    "geekbridge.calendar.sync",
    "geekbridge.calendar.write_result",
    "geekbridge.sms.status",
    "kdeconnect.sms.messages",
];

fn vcard_seeds() -> Vec<Vec<u8>> {
    [
        "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\nTEL;TYPE=CELL:+1 555 0100\r\nEMAIL:ada@example.org\r\nREV:20240101T000000Z\r\nEND:VCARD\r\n",
        "BEGIN:VCARD\nVERSION:2.1\nFN;ENCODING=QUOTED-PRINTABLE:=C3=89milie\nTEL:1\nNOTE:line one\\nline two\\, with \\; escapes\n continued\nPHOTO;ENCODING=BASE64;JPEG:/9j/4AAQSkZJRgABAQEASABIAAD/\n AAAA\nEND:VCARD\n",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

fn qr_seeds() -> Vec<Vec<u8>> {
    let q = QrPayload {
        device_id: "0123456789abcdef0123456789abcdef".into(),
        name: "Desk PC".into(),
        cert_sha256: "ab".repeat(32),
        addresses: vec![
            "192.168.1.5:1716".parse().unwrap(),
            "[fe80::1]:1716".parse().unwrap(),
        ],
        token: "0123456789abcdef0123456789abcdef".into(),
    };
    vec![
        q.encode().into_bytes(),
        b"geekbridge://pair?v=1&id=x&fp=y&t=z&a=1.2.3.4%3A1716&n=N".to_vec(),
        b"https://example.org".to_vec(),
    ]
}

fn name_seeds() -> Vec<Vec<u8>> {
    [
        "report.pdf",
        "../../../etc/passwd",
        "C:\\Windows\\system32\\cmd.exe",
        "CON.txt",
        ".bashrc",
        "a\u{202e}fdp.exe",
        "x ",
        "nul",
        "dir/..\\..\\x",
        &"é".repeat(150),
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

fn udp_seeds() -> Vec<Vec<u8>> {
    vec![br#"{"id":1700000000000,"type":"kdeconnect.identity","body":{"deviceId":"0123456789abcdef0123456789abcdef","deviceName":"Phone","deviceType":"phone","tcpPort":1716,"protocolVersion":8,"incomingCapabilities":["kdeconnect.ping"],"outgoingCapabilities":["kdeconnect.ping"]}}
"#
    .to_vec()]
}

fn ipc_seeds() -> Vec<Vec<u8>> {
    [
        r#"{"op":"status"}"#,
        r#"{"op":"events","after":0}"#,
        r#"{"op":"connect","device_id":"0123456789abcdef0123456789abcdef","endpoint":{"address":"192.168.1.9","port":1716}}"#,
        r#"{"op":"approve","challenge_id":"abc"}"#,
        "GB-IPC 1 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "GB-IPC 1 unavailable state_dir_unusable cannot open state",
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef {\"op\":\"status\"}",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

fn dispatch_seeds() -> Vec<Vec<u8>> {
    [
        r#"{"version":1,"action":"transport_status"}"#,
        r#"{"version":1,"action":"pairing_parse_qr","text":"geekbridge://pair?v=1"}"#,
        r#"{"version":1,"action":"transport_connect","host":"192.168.1.5","port":1716,"device_id":"0123456789abcdef0123456789abcdef"}"#,
        r#"{"version":1,"action":"transport_start","state_dir":"/nonexistent","device_name":"x"}"#,
        r#"{"version":1,"action":"transport_wait","since":0,"timeout_ms":0}"#,
        r#"{"version":1,"action":"transport_ping","connection_id":1}"#,
        r#"{"version":1,"action":"nope"}"#,
        r#"{"version":1,"action":"transport_stop"}"#,
        r#"{"version":1,"action":"transport_pair_request","connection_id":1,"mode":"await"}"#,
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect()
}

fn update_seeds() -> Vec<Vec<u8>> {
    let release = br#"{"tag_name":"v1.2.3","name":"1.2.3","body":"Notes\n- a","draft":false,"prerelease":false,"assets":[{"name":"geekbridge-linux-x86_64.tar.gz","browser_download_url":"https://github.com/o/r/releases/download/v1.2.3/geekbridge-linux-x86_64.tar.gz","url":"https://api.github.com/repos/o/r/releases/assets/1","size":1000},{"name":"SHA256SUMS","browser_download_url":"https://github.com/o/r/releases/download/v1.2.3/SHA256SUMS","size":200}]}"#;
    vec![
        release.to_vec(),
        b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  geekbridge-linux-x86_64.tar.gz\nffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff *./other.zip\n".to_vec(),
        b"https://api.github.com/repos/owner/name/releases/latest".to_vec(),
        b"v1.2.3-rc.1+build.5".to_vec(),
    ]
}

macro_rules! t {
    ($name:literal, $run:expr, $kinds:expr, $fx:expr, $seeds:expr) => {
        Target {
            name: $name,
            run: $run,
            kinds: $kinds,
            fixture_prefixes: $fx,
            builtin_seeds: $seeds,
        }
    };
}

pub static TARGETS: &[Target] = &[
    t!(
        "packet_line",
        packet_line,
        &["kdeconnect.ping", "kdeconnect.identity", "kdeconnect.pair"],
        &["*"],
        none
    ),
    t!(
        "identity_line",
        identity_line,
        &["kdeconnect.identity"],
        &["kdeconnect.identity"],
        name_seeds
    ),
    t!(
        "udp_discovery",
        udp_discovery,
        &["kdeconnect.identity"],
        &["kdeconnect.identity"],
        udp_seeds
    ),
    t!("line_reader", line_reader, &[], &["*"], none),
    t!(
        "pairing",
        pairing,
        &["kdeconnect.pair"],
        &["kdeconnect.pair"],
        none
    ),
    t!("qrpair", qrpair, &[], &[], qr_seeds),
    t!(
        "share",
        share_parse,
        &[
            "kdeconnect.share.request",
            "kdeconnect.share.request.update"
        ],
        &["kdeconnect.share"],
        name_seeds
    ),
    t!(
        "sms",
        sms_parse,
        &["kdeconnect.sms.messages"],
        &["kdeconnect.sms"],
        none
    ),
    t!(
        "phone_wire",
        phone_wire,
        PHONE_KINDS,
        &[
            "kdeconnect.contacts",
            "geekbridge.calendar",
            "geekbridge.sms",
            "kdeconnect.sms"
        ],
        none
    ),
    t!("vcard", vcard, &[], &[], vcard_seeds),
    t!(
        "calls_wire",
        calls_wire,
        &[
            "geekbridge.calls.state",
            "geekbridge.calls.request",
            "geekbridge.calls.result"
        ],
        &["geekbridge.calls"],
        none
    ),
    t!(
        "clipboard",
        clipboard_parse,
        &[
            "kdeconnect.clipboard",
            "kdeconnect.clipboard.connect",
            "geekbridge.clipboard.item",
            "geekbridge.clipboard.none",
            "geekbridge.clipboard.pull"
        ],
        &["kdeconnect.clipboard", "geekbridge.clipboard"],
        none
    ),
    t!(
        "messages_wire",
        messages_wire,
        &[],
        &["kdeconnect.sms"],
        none
    ),
    t!("ipc", ipc_parse, &[], &[], ipc_seeds),
    t!(
        "android_dispatch",
        android_dispatch,
        &[],
        &[],
        dispatch_seeds
    ),
    t!("update", update_parse, &[], &[], update_seeds),
    t!("cert", cert_parse, &[], &[], none),
    t!(
        "hub_phone_data",
        hub_phone_data,
        PHONE_KINDS,
        &[
            "kdeconnect.contacts",
            "geekbridge.calendar",
            "geekbridge.sms",
            "kdeconnect.sms"
        ],
        none
    ),
    t!(
        "hub_calls",
        hub_calls,
        &["geekbridge.calls.state", "geekbridge.calls.result"],
        &["geekbridge.calls"],
        none
    ),
    t!(
        "hub_clipboard",
        hub_clipboard,
        &[
            "kdeconnect.clipboard",
            "kdeconnect.clipboard.connect",
            "geekbridge.clipboard.item",
            "geekbridge.clipboard.none"
        ],
        &["kdeconnect.clipboard", "geekbridge.clipboard"],
        none
    ),
];

pub fn by_name(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|t| t.name == name)
}
