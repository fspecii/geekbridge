// SPDX-License-Identifier: GPL-3.0-or-later
//! Table tests for the "Report a problem" redaction and link builder, plus the two helpers that spawn OS tools
//! (browser opener, clipboard copy) driven through fake executables on a temporary PATH.

use bridge_core::report::{self, build, decode, encode, redact};

const R: &str = "<redacted>";

/// (input, must NOT survive, must survive)
const SECRETS: &[(&str, &str)] = &[
    ("mail alice@example.com sent", "alice@example.com"),
    (
        "(bob.smith+tag@sub.example.org)",
        "bob.smith+tag@sub.example.org",
    ),
    ("user=carol@example.com,", "carol@example.com"),
    ("path /home/alice/Documents/secret.txt end", "/home/alice"),
    ("at ~/Downloads/x.pdf", "~/Downloads/x.pdf"),
    ("file:///home/alice/x", "file:///home/alice/x"),
    ("C:\\Users\\Alice\\Desktop\\a.txt", "Alice"),
    ("D:/Users/Alice/a.txt", "Alice"),
    ("\\\\server\\share\\bob", "bob"),
    ("ip 192.168.1.20 up", "192.168.1.20"),
    ("peer 10.0.0.7:1716 closed", "10.0.0.7"),
    ("fe80::1c2b:3dff:fe4a:5b6c on wlan0", "fe80::1c2b"),
    ("2001:db8:85a3:0:0:8a2e:370:7334", "2001:db8"),
    ("[2001:db8::1]:1716", "2001:db8"),
    ("mac aa:bb:cc:dd:ee:ff paired", "aa:bb:cc:dd:ee:ff"),
    ("AA-BB-CC-DD-EE-FF is a mac", "AA-BB-CC-DD-EE-FF"),
    ("id 123e4567-e89b-12d3-a456-426614174000.", "123e4567"),
    ("token 0123456789abcdef0123456789abcdef", "0123456789abcdef"),
    ("sha deadbeefdeadbeefdeadbeef", "deadbeefdeadbeef"),
    ("call +49 170 1234567 now", "1234567"),
    ("call +1 (555) 123-4567 now", "4567"),
    ("tel 5551234567 x", "5551234567"),
    ("+442071234567", "2071234567"),
    ("PATH=/usr/local/bin:/home/alice/bin", "/home/alice"),
    ("\"/home/alice/My Files/x\"", "/home/alice"),
];

#[test]
fn personal_looking_tokens_never_survive() {
    for (input, secret) in SECRETS {
        let out = redact(input);
        assert!(!out.contains(secret), "{input:?} -> {out:?}");
        assert!(out.contains(R), "{input:?} -> {out:?}");
    }
}

#[test]
fn tokens_are_caught_wherever_they_sit_in_a_line() {
    for (input, secret) in SECRETS {
        for line in [
            input.to_string(),
            format!("start {input}"),
            format!("{input} end"),
            format!("a\t{input}\t\tb"),
            format!("first line\n{input}\nlast line"),
            format!("{input}\r\nnext"),
        ] {
            let out = redact(&line);
            assert!(!out.contains(secret), "{line:?} -> {out:?}");
        }
    }
}

#[test]
fn report_vocabulary_and_plain_numbers_are_kept() {
    for keep in [
        "GeekBridge desktop 0.9.1 on linux x86_64",
        "links: 1 trusted: 1",
        "port 1716 and 1717",
        "version 1.2.3",
        "feature clipboard: allowed",
        "kde connect protocol 8",
        "unix_ms 1700000000000",
        "wlan0 eth0 lo",
        "ratio 3/4 and and/or",
        "https://github.com/fspecii/geekbridge/issues/12",
        "0 1 2 3 4 5 6",
        "",
    ] {
        assert_eq!(redact(keep), keep, "{keep:?}");
    }
}

#[test]
fn redaction_is_idempotent_and_never_panics_on_odd_unicode() {
    let odd = [
        "\u{202e}alice@example.com\u{202c}",
        "عنوان /home/علي/ملف.txt",
        "名前 +81 90 1234 5678 です",
        "👩‍👩‍👧 192.168.0.1 🙂",
        "a\u{0}b /etc/passwd\u{0}",
        "e\u{301}@e\u{301}.com",
        "\u{fe0f}\u{200b}  \u{3000}x",
        &"x".repeat(100_000),
        &"/a".repeat(50_000),
        &"1 ".repeat(50_000),
        "@",
        "@@",
        "a@",
        "@b",
        ":",
        "::",
        ":::",
        "+",
        "+1",
        "-",
        "~",
        "~/",
        "/",
        "//",
        "\\",
    ];
    for t in odd {
        let once = redact(t);
        assert_eq!(redact(&once), once, "not idempotent for {t:.40?}");
    }
    assert!(!redact("\u{202e}alice@example.com").contains("alice@"));
    assert!(!redact("عنوان /home/علي/ملف.txt").contains("علي"));
    assert!(!redact("名前 +81 90 1234 5678 です").contains("1234"));
}

#[test]
fn lone_punctuation_and_short_forms_are_not_mistaken_for_secrets() {
    for keep in ["@", "a@", "@b", ":", "::", "+", "-", "/", "~", "a/b"] {
        assert_eq!(redact(keep), keep, "{keep:?}");
    }
}

#[test]
fn builder_redacts_every_field_and_drops_unknown_or_blank_ones() {
    let link = build(
        "fspecii/geekbridge",
        "[Bug] crash for alice@example.com",
        &[
            ("version", "desktop 1.0 on /home/alice"),
            ("desktop", "linux 10.1.2.3"),
            ("phone", "   "),
            ("evil", "injected"),
            ("title", "x"),
            ("template", "x"),
        ],
        "line one 192.168.1.2\n\nline two /tmp/x/y  \nmac aa:bb:cc:dd:ee:ff",
    );
    let all = format!("{} {:?} {}", link.title, link.fields, decode(&link.url));
    for secret in ["alice", "10.1.2.3", "192.168.1.2", "/tmp/x/y", "aa:bb:cc"] {
        assert!(!all.contains(secret), "{secret} leaked: {all}");
    }
    let ids: Vec<&str> = link.fields.iter().map(|(i, _)| i.as_str()).collect();
    assert_eq!(ids, ["version", "desktop", "logs"]);
    assert!(!link.url.contains("injected") && !link.url.contains("evil"));
    assert!(!link.truncated);
    let logs = &link.fields[2].1;
    assert_eq!(logs.lines().count(), 3, "blank lines dropped: {logs:?}");
    assert!(
        link.preview()
            .starts_with("Title: [Bug] crash for <redacted>\n")
    );
}

#[test]
fn title_and_short_fields_are_capped_and_the_url_always_fits() {
    let long = "word ".repeat(400);
    let link = build("o/r", &long, &[("version", &long), ("desktop", &long)], "");
    assert!(link.title.chars().count() <= 120);
    assert!(link.fields.iter().all(|(_, t)| t.chars().count() <= 200));
    assert!(
        link.fields.iter().all(|(i, _)| i != "logs"),
        "no logs field when empty"
    );
    assert!(link.url.len() <= report::MAX_URL_BYTES);

    // Multi-byte logs (4-byte emoji, RTL) are cut on line boundaries, never in a character.
    for line in ["🙂🙂🙂🙂🙂🙂🙂🙂 status ok", "שלום עולם status ok", "x"] {
        let logs: String = (0..2000).map(|i| format!("{line} {i}\n")).collect();
        let l = build("o/r", "t", &[("version", "v")], &logs);
        assert!(l.truncated);
        assert!(l.url.len() <= report::MAX_URL_BYTES, "{}", l.url.len());
        let text = &l.fields.last().unwrap().1;
        assert!(text.contains("lines left out"), "{text:.200}");
        assert_eq!(decode(&encode(text)), *text);
    }

    // One single enormous line: the builder drops down to nothing rather than loop or exceed the cap.
    let l = build("o/r", "t", &[], &"y".repeat(50_000));
    assert!(l.url.len() <= report::MAX_URL_BYTES);
}

#[test]
fn encode_decode_round_trip_and_malformed_escapes() {
    for t in ["", "plain", "a b&c=d#e?f", "ünï 🙂 שלום", "%41", "\n\r\t\0"] {
        assert_eq!(decode(&encode(t)), t);
    }
    assert!(encode("a b").contains("%20"));
    assert_eq!(encode("AZaz09-_.~"), "AZaz09-_.~");
    // Truncated or non-hex escapes are passed through, not dropped or panicked on.
    for t in ["%", "%4", "%zz", "100%", "a%2", "%%41"] {
        let _ = decode(t);
    }
    assert_eq!(decode("%zz"), "%zz");
    assert_eq!(
        decode("%E2%9C"),
        "\u{fffd}",
        "invalid UTF-8 becomes a replacement"
    );
}

#[test]
fn repo_names_follow_github_rules() {
    for ok in [
        "a/b",
        "A-b_c.d/e.f",
        "fspecii/geekbridge",
        &format!("{0}/{0}", "x".repeat(100)),
    ] {
        assert!(report::valid_repo(ok), "{ok}");
    }
    for bad in [
        "",
        "a",
        "/b",
        "a/",
        "a/b/c",
        "./b",
        "a/..",
        "../b",
        "a b/c",
        "a/b c",
        "ä/b",
        "a/b?x=1",
        "a/b#f",
        "a\\b",
        &format!("{}/b", "x".repeat(101)),
        "a/b\n",
    ] {
        assert!(!report::valid_repo(bad), "{bad:?}");
    }
    assert_eq!(report::repo_from(Some("  o/r  ")), "o/r");
    assert_eq!(report::repo_from(Some("bad repo")), report::DEFAULT_REPO);
    assert_eq!(report::repo_from(None), report::DEFAULT_REPO);
}

#[test]
fn only_plain_github_https_links_are_openable() {
    assert!(report::openable("https://github.com/o/r/issues/new?a=1"));
    for bad in [
        "http://github.com/o/r",
        "https://github.com.evil.com/o",
        "https://evil.com/https://github.com/",
        "https://github.com/o r",
        "https://github.com/o/r\n",
        "https://github.com/ö",
        "file:///etc/passwd",
        "",
    ] {
        // github.com.evil.com does start with the prefix only if a slash follows github.com
        let expect =
            bad.starts_with("https://github.com/") && bad.bytes().all(|b| b.is_ascii_graphic());
        assert_eq!(report::openable(bad), expect, "{bad:?}");
    }
    assert!(!report::openable("https://github.com.evil.com/o"));
    assert!(!report::open_in_browser("http://example.com"));
    assert!(!report::open_in_browser("https://github.com/o r"));
}

// The stubs stand in for xdg-open / xclip / wl-copy; macOS uses `open` and `pbcopy`, Windows neither.
#[cfg(target_os = "linux")]
mod os_tools {
    use super::report;
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    fn stub(dir: &Path, name: &str, body: &str) {
        // Write elsewhere and rename: rewriting a script that is still running fails with ETXTBSY.
        let p = dir.join(name);
        let tmp = dir.join(format!(".{name}.new"));
        fs::write(&tmp, format!("#!/bin/sh\nPATH=/usr/bin:/bin\n{body}\n")).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).unwrap();
        fs::rename(&tmp, &p).unwrap();
    }

    fn wait_for(path: &Path) -> String {
        for _ in 0..200 {
            if let Ok(s) = fs::read_to_string(path)
                && !s.is_empty()
            {
                return s;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        panic!("{} never written", path.display());
    }

    /// PATH is process-wide, so every scenario runs in this one test.
    #[test]
    fn browser_opener_and_clipboard_tools_are_run_with_one_argument_and_failures_are_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let rec = tmp.path().join("rec");
        // SAFETY: single test in this process touches the environment.
        unsafe { std::env::set_var("PATH", &bin) };

        // No opener and no clipboard tool installed.
        assert!(!report::open_in_browser(
            "https://github.com/o/r/issues/new?a=b"
        ));
        assert!(!report::copy_to_clipboard("text"));

        // Opener succeeds: URL is exactly one argv entry (spaces/metacharacters cannot split it).
        stub(
            &bin,
            "xdg-open",
            &format!("printf '%s|' \"$#\" \"$1\" > {}", rec.display()),
        );
        assert!(report::open_in_browser(
            "https://github.com/o/r/issues/new?a=b&c=$(touch$IFS/tmp/x)"
        ));
        assert_eq!(
            wait_for(&rec),
            "1|https://github.com/o/r/issues/new?a=b&c=$(touch$IFS/tmp/x)|"
        );

        // Opener fails fast: reported as failure. Opener hangs: assumed to be starting the browser.
        stub(&bin, "xdg-open", "exit 3");
        assert!(!report::open_in_browser("https://github.com/o/r"));
        stub(&bin, "xdg-open", "exec sleep 4");
        let t = std::time::Instant::now();
        assert!(report::open_in_browser("https://github.com/o/r"));
        assert!(t.elapsed() < std::time::Duration::from_secs(3));

        // Clipboard: first tool that accepts stdin wins and receives the text verbatim (odd Unicode included).
        let text = "line1\nأهلاً 🙂 \u{0} end";
        stub(
            &bin,
            "wl-copy",
            &format!("cat > {}", rec.with_extension("wl").display()),
        );
        let _ = fs::remove_file(&rec);
        assert!(report::copy_to_clipboard(text));
        assert_eq!(
            wait_for(&rec.with_extension("wl")).as_bytes(),
            text.as_bytes()
        );

        // wl-copy missing: falls through to the next tool.
        fs::remove_file(bin.join("wl-copy")).unwrap();
        stub(
            &bin,
            "xclip",
            &format!("cat > {}", rec.with_extension("xc").display()),
        );
        assert!(report::copy_to_clipboard("second"));
        assert_eq!(wait_for(&rec.with_extension("xc")), "second");
    }
}
