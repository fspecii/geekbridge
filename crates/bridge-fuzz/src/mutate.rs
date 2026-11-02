// SPDX-License-Identifier: GPL-3.0-or-later
//! Structure-aware input generation and mutation.
//!
//! Four strategies, picked per input by [`next_input`]:
//! 1. byte-level mutation of a seed (bit flips, splices, truncation, repeated chunks, ...);
//! 2. field-level mutation of a seed parsed as JSON (huge numbers, negative sizes, deep nesting,
//!    oversized strings, control characters, type swaps, dropped and renamed keys);
//! 3. grammar generation of a whole packet from the key/type dictionary harvested from the seeds;
//! 4. a textual/encoding patch on top of 2 or 3 (duplicate keys, lone UTF-16 surrogate escapes,
//!    invalid UTF-8, UTF-16 re-encoding, CRLF, missing or doubled newline, NUL bytes).

use crate::rng::Rng;
use serde_json::{Map, Number, Value, json};

/// Inputs never grow past this (libFuzzer's `-max_len` plays the same role there).
pub const MAX_INPUT: usize = 1 << 20;

#[derive(Default, Clone)]
pub struct Dict {
    pub keys: Vec<String>,
    pub strings: Vec<String>,
}

/// Collect every object key and short string value that occurs in the seeds.
pub fn harvest(seeds: &[Vec<u8>]) -> Dict {
    fn walk(v: &Value, d: &mut Dict) {
        match v {
            Value::String(s)
                if s.len() <= 80 && !d.strings.contains(s) && d.strings.len() < 400 =>
            {
                d.strings.push(s.clone());
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, d)),
            Value::Object(o) => {
                for (k, x) in o {
                    if !d.keys.contains(k) && d.keys.len() < 400 {
                        d.keys.push(k.clone());
                    }
                    walk(x, d);
                }
            }
            _ => {}
        }
    }
    let mut d = Dict::default();
    for s in seeds {
        if let Ok(v) = serde_json::from_slice::<Value>(s) {
            walk(&v, &mut d);
        }
    }
    d
}

fn repeat_str(unit: &str, n: usize) -> String {
    unit.repeat(n)
}

/// A hostile string value.
pub fn evil_string(rng: &mut Rng, dict: &Dict) -> String {
    const NASTY: &[&str] = &[
        "",
        " ",
        "..",
        ".",
        "../../etc/passwd",
        "..\\..\\windows\\system32",
        "/abs/path",
        "C:\\x",
        "CON",
        "nul.txt",
        "COM1 .txt",
        "a/b\\c",
        "\u{0}",
        "a\u{0}b",
        "\u{1b}[31mred\u{1b}[0m",
        "\r\n",
        "line1\nline2\r\nline3",
        "\u{202e}gpj.exe",
        "\u{200b}\u{feff}",
        "\u{2028}\u{2029}\u{85}",
        "e\u{301}\u{301}\u{301}\u{301}\u{301}",
        "😀👨‍👩‍👧‍👦",
        "\u{10ffff}",
        "%00%2e%2e%2f",
        "%",
        "%zz",
        "geekbridge://pair?v=1&id=&fp=&t=&a=&n=",
        "-1",
        "1e999",
        "true",
        "null",
        "{}",
        "[]",
        "\"",
        "\\",
        "}{",
        "BEGIN:VCARD\nEND:VCARD",
        "BEGIN:VCARD\nVERSION:3.0\nFN:=\nTEL;ENCODING=QUOTED-PRINTABLE:=ZZ=\n=\nEND:VCARD",
        "0.0.0.0:1716",
        "[::1]:1716",
        "https://api.github.com/x",
        "javascript:alert(1)",
        "file:///etc/passwd",
    ];
    match rng.below(10) {
        0..=2 => rng.pick(NASTY).to_string(),
        3 if !dict.strings.is_empty() => rng.pick(&dict.strings).clone(),
        4 => {
            let n = *rng.pick(&[
                1usize, 255, 256, 257, 1023, 4096, 4097, 65_535, 65_536, 70_000,
            ]);
            repeat_str(
                rng.pick(&["a", "é", "日", "😀", "/", "\\", ".", " ", "\u{0}", "\n"]),
                n,
            )
        }
        5 => (0..rng.below(40))
            .map(|_| char::from_u32(rng.below(0x20) as u32).unwrap_or('\u{1}'))
            .collect(),
        6 => (0..rng.below(24))
            .map(|_| char::from_u32(rng.below(0x11_0000) as u32).unwrap_or('\u{fffd}'))
            .collect(),
        7 => {
            let mut s = rng.pick(NASTY).to_string();
            s.push_str(rng.pick(NASTY));
            s
        }
        8 => {
            let n = rng.below(70);
            (0..n)
                .map(|_| rng.pick(&['0', '1', 'a', 'f', 'Z', '-', '_', '.']))
                .collect()
        }
        _ => format!("{}{}", rng.pick(&["v", "V", ""]), rng.next_u64() % 1000),
    }
}

fn evil_number(rng: &mut Rng) -> Value {
    match rng.below(14) {
        0 => json!(0),
        1 => json!(-1),
        2 => json!(1),
        3 => json!(i64::MAX),
        4 => json!(i64::MIN),
        5 => json!(u64::MAX),
        6 => json!(i32::MAX as i64 + 1),
        7 => json!(i32::MIN as i64 - 1),
        8 => json!(65_535),
        9 => json!(65_536),
        10 => json!(1u64 << 53),
        11 => {
            Number::from_f64(*rng.pick(&[1e308, -1e308, 5e-324, 0.5, -0.0, 1.7976931348623157e308]))
                .map_or(Value::Null, Value::Number)
        }
        12 => json!(rng.next_u64() as i64),
        _ => json!(rng.below(200_000) as i64 - 100_000),
    }
}

fn deep(rng: &mut Rng, depth: usize) -> Value {
    let mut v = Value::Null;
    for _ in 0..depth {
        v = if rng.chance(2) {
            Value::Array(vec![v])
        } else {
            let mut m = Map::new();
            m.insert("k".into(), v);
            Value::Object(m)
        };
    }
    v
}

/// A hostile JSON value of any type.
pub fn evil_value(rng: &mut Rng, dict: &Dict) -> Value {
    match rng.below(14) {
        0 => Value::Null,
        1 => Value::Bool(rng.chance(2)),
        2..=4 => evil_number(rng),
        5..=7 => Value::String(evil_string(rng, dict)),
        8 => {
            let d = *rng.pick(&[2usize, 8, 31, 32, 33, 64, 127, 128, 129, 200, 400]);
            deep(rng, d)
        }
        9 => Value::Array(
            (0..*rng.pick(&[0usize, 1, 2, 100, 1000, 5000]))
                .map(|_| evil_number(rng))
                .collect(),
        ),
        10 => {
            let mut m = Map::new();
            for i in 0..*rng.pick(&[0usize, 1, 2, 300, 2000]) {
                m.insert(format!("k{i}"), evil_number(rng));
            }
            Value::Object(m)
        }
        11 => Value::Array(
            (0..rng.below(5))
                .map(|_| Value::String(evil_string(rng, dict)))
                .collect(),
        ),
        12 => {
            let mut m = Map::new();
            for _ in 0..rng.below(4) {
                let k = if dict.keys.is_empty() {
                    "x".to_owned()
                } else {
                    rng.pick(&dict.keys).clone()
                };
                m.insert(k, evil_value_shallow(rng, dict));
            }
            Value::Object(m)
        }
        _ => Value::String(evil_string(rng, dict)),
    }
}

fn evil_value_shallow(rng: &mut Rng, dict: &Dict) -> Value {
    match rng.below(4) {
        0 => evil_number(rng),
        1 => Value::Null,
        _ => Value::String(evil_string(rng, dict)),
    }
}

/// Mutate one random node of a JSON tree in place.
fn mutate_value(rng: &mut Rng, v: &mut Value, dict: &Dict, depth: usize) {
    let descend = depth < 8 && rng.below(3) != 0;
    match v {
        Value::Object(m) if descend && !m.is_empty() => {
            let n = rng.below(m.len());
            let key = m.keys().nth(n).cloned().unwrap_or_default();
            match rng.below(8) {
                0 => {
                    m.remove(&key);
                }
                1 => {
                    if let Some(val) = m.remove(&key) {
                        let new = if dict.keys.is_empty() || rng.chance(3) {
                            evil_string(rng, dict)
                        } else {
                            rng.pick(&dict.keys).clone()
                        };
                        m.insert(new, val);
                    }
                }
                2 => {
                    // Same key, another type.
                    m.insert(key, evil_value(rng, dict));
                }
                _ => {
                    if let Some(child) = m.get_mut(&key) {
                        mutate_value(rng, child, dict, depth + 1);
                    }
                }
            }
        }
        Value::Object(m) => {
            let k = if dict.keys.is_empty() || rng.chance(4) {
                evil_string(rng, dict)
            } else {
                rng.pick(&dict.keys).clone()
            };
            if m.len() < 4000 {
                m.insert(k, evil_value(rng, dict));
            }
        }
        Value::Array(a) if descend && !a.is_empty() => {
            let i = rng.below(a.len());
            match rng.below(4) {
                0 => {
                    a.remove(i);
                }
                1 => {
                    let c = a[i].clone();
                    for _ in 0..rng.below(50) {
                        a.push(c.clone());
                    }
                }
                _ => mutate_value(rng, &mut a[i], dict, depth + 1),
            }
        }
        Value::Array(a) => {
            if a.len() < 4000 {
                a.push(evil_value(rng, dict));
            }
        }
        Value::Number(n) if rng.chance(2) => {
            if let Some(i) = n.as_i64() {
                *v = json!(i.wrapping_add(*rng.pick(&[-1i64, 1, -1000, 1000])));
            } else {
                *v = evil_number(rng);
            }
        }
        _ => *v = evil_value(rng, dict),
    }
}

// --------------------------------------------------------------------------------------- bytes

const INTERESTING: &[&[u8]] = &[
    b"\n",
    b"\r\n",
    b"\0",
    b"\"",
    b"\\",
    b"{",
    b"}",
    b"[",
    b"]",
    b"\\ud800",
    b"\\udc00",
    b"\\ud83d\\ude00",
    b"\\u0000",
    b"\xff",
    b"\xc0\x80",
    b"\xed\xa0\x80",
    b"\xf4\x90\x80\x80",
    b"\xef\xbb\xbf",
    b"\xe2\x80\xae",
    b"9999999999999999999999",
    b"-9223372036854775809",
    b"1e999",
    b"-1e999",
    b"NaN",
    b"Infinity",
    b",,",
    b":",
];

pub fn mutate_bytes(rng: &mut Rng, input: &[u8], other: &[u8]) -> Vec<u8> {
    let mut b = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        if b.len() > MAX_INPUT {
            b.truncate(MAX_INPUT);
        }
        match rng.below(12) {
            0 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] = rng.next_u64() as u8;
            }
            2 => {
                let at = rng.below(b.len() + 1);
                let ins = rng.pick(INTERESTING);
                b.splice(at..at, ins.iter().copied());
            }
            3 if !b.is_empty() => {
                let a = rng.below(b.len());
                let n = rng.below(b.len() - a + 1).min(64);
                b.drain(a..a + n);
            }
            4 if !b.is_empty() => {
                // Repeat a chunk many times.
                let a = rng.below(b.len());
                let n = (1 + rng.below(32)).min(b.len() - a);
                let chunk = b[a..a + n].to_vec();
                let times = *rng.pick(&[2usize, 16, 256, 4096, 30_000]);
                let tail = b.split_off(a);
                for _ in 0..times {
                    if b.len() + chunk.len() > MAX_INPUT {
                        break;
                    }
                    b.extend_from_slice(&chunk);
                }
                b.extend_from_slice(&tail);
            }
            5 if !b.is_empty() => b.truncate(rng.below(b.len())),
            6 if !other.is_empty() => {
                // Splice a slice of another seed in.
                let a = rng.below(other.len());
                let n = rng.below(other.len() - a + 1).min(200);
                let at = rng.below(b.len() + 1);
                b.splice(at..at, other[a..a + n].iter().copied());
            }
            7 => b.extend((0..rng.below(64)).map(|_| rng.next_u64() as u8)),
            8 if b.len() > 1 => {
                let i = rng.below(b.len());
                let j = rng.below(b.len());
                b.swap(i, j);
            }
            9 => {
                b.pop();
            }
            10 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] = b[i].wrapping_add(*rng.pick(&[1u8, 255]));
            }
            _ => {
                let at = rng.below(b.len() + 1);
                b.insert(
                    at,
                    *rng.pick(&[0u8, 0x0a, 0x0d, 0x1b, 0x7f, 0x80, 0xff, b'"', b'\\']),
                );
            }
        }
    }
    b
}

// ------------------------------------------------------------------------------- text patches

fn find_all(hay: &[u8], needle: u8) -> Vec<usize> {
    hay.iter()
        .enumerate()
        .filter(|(_, b)| **b == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Encoding- and framing-level damage applied to a serialised document.
pub fn patch_text(rng: &mut Rng, mut b: Vec<u8>, dict: &Dict) -> Vec<u8> {
    match rng.below(14) {
        0 => {
            // Lone or reversed UTF-16 surrogate escapes inside a string.
            let quotes = find_all(&b, b'"');
            if !quotes.is_empty() {
                let at = quotes[rng.below(quotes.len())] + 1;
                let esc: &[u8] = rng.pick(&[
                    &b"\\ud800"[..],
                    b"\\udc00",
                    b"\\udc00\\ud800",
                    b"\\ud800\\u0041",
                    b"\\ud83d",
                    b"\\uD800\\uDC00",
                    b"\\u0000",
                    b"\\u001b",
                ]);
                b.splice(at..at, esc.iter().copied());
            }
        }
        1 => {
            // Duplicate key with a different value, right after an opening brace.
            if let Some(&at) = find_all(&b, b'{').first() {
                let k = if dict.keys.is_empty() {
                    "type".to_owned()
                } else {
                    rng.pick(&dict.keys).clone()
                };
                let dup = format!(
                    "{}:{},",
                    Value::String(k),
                    serde_json::to_string(&evil_value_shallow(rng, dict)).unwrap_or_default()
                );
                b.splice(at + 1..at + 1, dup.into_bytes());
            }
        }
        2 => {
            // Invalid UTF-8 inside the document.
            let at = rng.below(b.len() + 1);
            let bad: &[u8] = rng.pick(&[
                &b"\xff"[..],
                b"\xc0\x80",
                b"\xed\xa0\x80",
                b"\xf8\x88\x80\x80\x80",
                b"\xe2\x82",
            ]);
            b.splice(at..at, bad.iter().copied());
        }
        3 => {
            // Whole document as UTF-16 (with or without BOM) or UTF-32-ish: mixed encodings.
            let text = String::from_utf8_lossy(&b).into_owned();
            let le = rng.chance(2);
            let mut out = Vec::new();
            if rng.chance(2) {
                out.extend_from_slice(if le { &[0xff, 0xfe] } else { &[0xfe, 0xff] });
            }
            for u in text.encode_utf16() {
                out.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
            }
            b = out;
        }
        4 => {
            // CRLF framing, in the middle or at the end.
            let at = if rng.chance(2) {
                b.len()
            } else {
                rng.below(b.len() + 1)
            };
            b.splice(at..at, b"\r\n".iter().copied());
        }
        5 => {
            b.extend_from_slice(b"\n");
            b.extend_from_slice(b"\n{\"id\":1}\n");
        }
        6 => b.truncate(b.len().saturating_sub(1 + rng.below(8))),
        7 => {
            b.splice(0..0, b"\xef\xbb\xbf".iter().copied());
        }
        8 => {
            let at = rng.below(b.len() + 1);
            let n = 1 + rng.below(8);
            b.splice(at..at, std::iter::repeat_n(0u8, n));
        }
        9 => {
            // Number literal serde_json may or may not accept.
            let lit: &[u8] = rng.pick(&[
                &b"1e999"[..],
                b"-0",
                b"18446744073709551616",
                b"-9223372036854775809",
                b"0.1e1",
                b"01",
                b"+1",
                b".5",
                b"1.",
                b"0x10",
            ]);
            let digits: Vec<usize> = b
                .iter()
                .enumerate()
                .filter(|(_, c)| c.is_ascii_digit())
                .map(|(i, _)| i)
                .collect();
            if let Some(&i) = digits.get(rng.below(digits.len().max(1))) {
                b.splice(i..i + 1, lit.iter().copied());
            }
        }
        10 => {
            // Deeply nested raw prefix (a parser with no depth guard overflows its stack).
            let n = *rng.pick(&[100usize, 1000, 20_000, 200_000]);
            let open = if rng.chance(2) { b'[' } else { b'{' };
            let mut v = vec![open; n];
            if open == b'{' {
                v = b"{\"a\":".repeat(n / 5 + 1);
            }
            v.extend_from_slice(&b);
            b = v;
        }
        11 => {
            // Padding whitespace to the size limits.
            let n = *rng.pick(&[
                1usize, 255, 256, 257, 4095, 4096, 16_383, 16_384, 16_385, 65_535, 65_536, 131_073,
            ]);
            let at = rng.below(b.len() + 1);
            b.splice(at..at, std::iter::repeat_n(b' ', n.min(MAX_INPUT)));
        }
        12 => {
            // Unterminated string.
            b.extend_from_slice(b"\"abc");
        }
        _ => {
            let a = rng.below(b.len() + 1);
            b.truncate(a);
        }
    }
    b.truncate(MAX_INPUT);
    b
}

// ------------------------------------------------------------------------------------ grammar

/// A whole packet built from nothing but the dictionary and the target's packet types.
pub fn generate_packet(rng: &mut Rng, kinds: &[&str], dict: &Dict) -> Vec<u8> {
    let mut obj = Map::new();
    let id = if rng.chance(4) {
        evil_value(rng, dict)
    } else {
        json!(1_700_000_000_000_i64 + rng.below(1000) as i64)
    };
    obj.insert("id".into(), id);
    let kind = if kinds.is_empty() || rng.chance(8) {
        Value::String(evil_string(rng, dict))
    } else {
        Value::String((*rng.pick(kinds)).to_owned())
    };
    obj.insert("type".into(), kind);
    let mut body = Map::new();
    for _ in 0..rng.below(8) {
        let k = if dict.keys.is_empty() || rng.chance(6) {
            evil_string(rng, dict)
        } else {
            rng.pick(&dict.keys).clone()
        };
        let v = if rng.chance(3) && !dict.strings.is_empty() {
            Value::String(rng.pick(&dict.strings).clone())
        } else {
            evil_value(rng, dict)
        };
        body.insert(k, v);
    }
    obj.insert(
        "body".into(),
        if rng.chance(20) {
            evil_value(rng, dict)
        } else {
            Value::Object(body)
        },
    );
    if rng.chance(4) {
        obj.insert("payloadSize".into(), evil_number(rng));
        obj.insert(
            "payloadTransferInfo".into(),
            if rng.chance(8) {
                evil_value(rng, dict)
            } else {
                json!({ "port": evil_number(rng) })
            },
        );
    }
    let mut out = serde_json::to_vec(&Value::Object(obj)).unwrap_or_default();
    out.push(b'\n');
    out
}

/// Pick a strategy and produce the next input for a target.
pub fn next_input(rng: &mut Rng, seeds: &[Vec<u8>], dict: &Dict, kinds: &[&str]) -> Vec<u8> {
    if seeds.is_empty() {
        return generate_packet(rng, kinds, dict);
    }
    let seed = rng.pick(seeds);
    let other = rng.pick(seeds);
    let out = match rng.below(10) {
        0..=2 => mutate_bytes(rng, seed, other),
        3..=6 => match serde_json::from_slice::<Value>(seed.strip_suffix(b"\n").unwrap_or(seed)) {
            Ok(mut v) => {
                for _ in 0..1 + rng.below(3) {
                    mutate_value(rng, &mut v, dict, 0);
                }
                let mut out = serde_json::to_vec(&v).unwrap_or_default();
                out.push(b'\n');
                if rng.chance(3) {
                    patch_text(rng, out, dict)
                } else {
                    out
                }
            }
            Err(_) => mutate_bytes(rng, seed, other),
        },
        7 | 8 => {
            let g = generate_packet(rng, kinds, dict);
            if rng.chance(3) {
                patch_text(rng, g, dict)
            } else {
                g
            }
        }
        _ => {
            // Replay a seed unchanged (plus an optional patch): the happy paths must stay covered.
            if rng.chance(2) {
                seed.clone()
            } else {
                patch_text(rng, seed.clone(), dict)
            }
        }
    };
    if out.len() > MAX_INPUT {
        out[..MAX_INPUT].to_vec()
    } else {
        out
    }
}
