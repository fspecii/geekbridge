// SPDX-License-Identifier: GPL-3.0-or-later
//! "Report a problem": turns the already-redacted diagnostics report into a prefilled GitHub
//! issue-form URL. Nothing here touches the network; the user's browser opens the link.
//!
//! Every name that must match `.github/ISSUE_TEMPLATE/bug_report.yml` lives in this one block
//! (`TEMPLATE_FILE`, `PARAM_*`, `FIELD_*`), and a test fails if the template stops declaring them.
//! The Android twin is `ReportLink.kt`; its test checks the same file.

/// Issue template (file name in `.github/ISSUE_TEMPLATE/`) the link selects.
pub const TEMPLATE_FILE: &str = "bug_report.yml";
pub const PARAM_TEMPLATE: &str = "template";
pub const PARAM_TITLE: &str = "title";
/// Issue-form field ids that receive a prefill (`?<id>=<text>`).
pub const FIELD_VERSION: &str = "version";
pub const FIELD_DESKTOP: &str = "desktop";
pub const FIELD_PHONE: &str = "phone";
pub const FIELD_LOGS: &str = "logs";
/// Every field id this code may prefill.
pub const PREFILLED_FIELDS: [&str; 4] = [FIELD_VERSION, FIELD_DESKTOP, FIELD_PHONE, FIELD_LOGS];

pub const DEFAULT_REPO: &str = "fspecii/geekbridge";
pub const REPO_ENV: &str = "GEEKBRIDGE_UPDATE_REPO";
/// Whole URL, well under the ~8 KB limits of browsers, proxies and GitHub.
pub const MAX_URL_BYTES: usize = 6000;
pub const DEFAULT_TITLE: &str = "[Bug] ";

/// `owner/name` as GitHub spells it (same rule as the updater's).
pub fn valid_repo(repo: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p.len() <= 100
            && p != "."
            && p != ".."
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| part(owner) && part(name))
}

/// The tracker to report to: `GEEKBRIDGE_UPDATE_REPO` if valid, else the built-in default.
pub fn repo_from_env() -> String {
    repo_from(std::env::var(REPO_ENV).ok().as_deref())
}

pub fn repo_from(override_repo: Option<&str>) -> String {
    override_repo
        .map(str::trim)
        .filter(|r| valid_repo(r))
        .unwrap_or(DEFAULT_REPO)
        .to_owned()
}

/// Second line of defence on top of the report's own redaction: replaces anything that looks like
/// an e-mail address, file path, IP/MAC address, UUID, long hex id or phone number. Names cannot be
/// recognised, so the reports never contain them in the first place.
pub fn redact(text: &str) -> String {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words: Vec<String> = Vec::new();
        let mut prev = String::new();
        let mut in_phone = false;
        for word in line.split_whitespace() {
            // "+49 170 1234567": the groups after a leading "+digits" belong to the same number.
            let digits_only = word
                .chars()
                .all(|c| c.is_ascii_digit() || "-()".contains(c));
            let starts_number = word
                .strip_prefix('+')
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()));
            let clean = if starts_number || (in_phone && digits_only) {
                in_phone = true;
                "<redacted>".to_owned()
            } else {
                in_phone = false;
                redact_word(word, &prev)
            };
            prev = word.to_owned();
            words.push(clean);
        }
        out.push(words.join(" "));
    }
    out.join("\n")
}

fn redact_word(word: &str, prev: &str) -> String {
    let core =
        word.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || "/\\~@:.+_-".contains(c)));
    let core = core.trim_end_matches(['.', ':']);
    if core.is_empty() || !is_sensitive(core, prev) {
        return word.to_owned();
    }
    word.replace(core, "<redacted>")
}

fn is_sensitive(core: &str, prev: &str) -> bool {
    let hex = core.chars().filter(char::is_ascii_hexdigit).count();
    let digits = core.chars().filter(char::is_ascii_digit).count();
    let email = core
        .split_once('@')
        .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty());
    let unix_path = (core.starts_with('/') && core[1..].contains('/')) || core.starts_with("~/");
    let win_path = core.contains('\\')
        || (core.len() > 2
            && core.as_bytes()[1] == b':'
            && matches!(core.as_bytes()[2], b'/' | b'\\'));
    let file_url = core.starts_with("file:");
    let (host, _) = core.split_once(':').unwrap_or((core, ""));
    let v4 = host.split('.').count() == 4
        && host
            .split('.')
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()));
    let colons = core.matches(':').count();
    let v6_or_mac = colons >= 2 && hex >= 4 && !core.contains("//");
    let uuid = {
        let lens: Vec<usize> = core.split('-').map(str::len).collect();
        lens == [8, 4, 4, 4, 12] && hex == 32
    };
    let long_hex = core.len() >= 16 && hex * 10 >= core.len() * 9;
    let timestamp = prev.ends_with("unix_ms");
    let phone = (core.starts_with('+') && digits >= 6) || (digits >= 7 && !timestamp);
    email || unix_path || win_path || file_url || v4 || v6_or_mac || uuid || long_hex || phone
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Inverse of [`encode`], for tests and the preview round-trip.
pub fn decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What the user is shown, and what the browser link carries: exactly these fields, nothing else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub title: String,
    /// `(field id, text)` in URL order, already redacted and shortened to fit.
    pub fields: Vec<(String, String)>,
    pub truncated: bool,
}

impl Link {
    /// Plain-text view of everything the link will carry, for the preview screen and clipboard.
    pub fn preview(&self) -> String {
        let mut o = format!("Title: {}\n", self.title);
        for (id, text) in &self.fields {
            o.push_str(&format!("\n[{id}]\n{text}\n"));
        }
        o
    }
}

fn url_for(repo: &str, title: &str, fields: &[(String, String)]) -> String {
    let mut url = format!(
        "https://github.com/{repo}/issues/new?{PARAM_TEMPLATE}={}&{PARAM_TITLE}={}",
        encode(TEMPLATE_FILE),
        encode(title)
    );
    for (id, text) in fields {
        url.push('&');
        url.push_str(id);
        url.push('=');
        url.push_str(&encode(text));
    }
    url
}

/// Builds the link. `short_fields` are small `(id, text)` prefills (versions, OS); `logs` is the
/// diagnostics report, which is shortened from the middle if the URL would exceed
/// [`MAX_URL_BYTES`]. Everything is passed through [`redact`] first.
pub fn build(repo: &str, title: &str, short_fields: &[(&str, &str)], logs: &str) -> Link {
    let title: String = redact(title).chars().take(120).collect();
    let mut fields: Vec<(String, String)> = short_fields
        .iter()
        .filter(|(id, text)| PREFILLED_FIELDS.contains(id) && !text.trim().is_empty())
        .map(|(id, text)| ((*id).to_owned(), redact(text).chars().take(200).collect()))
        .collect();
    let lines: Vec<String> = redact(logs)
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    let (mut head, mut tail) = (lines.len().div_ceil(2), lines.len() / 2);
    let mut truncated = false;
    loop {
        let mut text = lines[..head].join("\n");
        if truncated {
            text.push_str(&format!(
                "\n[... {} lines left out to fit the link; run `geekbridge-ctl diagnose` for all ...]",
                lines.len() - head - tail
            ));
        }
        if tail > 0 {
            text.push('\n');
            text.push_str(&lines[lines.len() - tail..].join("\n"));
        }
        let mut all = fields.clone();
        if !lines.is_empty() {
            all.push((FIELD_LOGS.to_owned(), text));
        }
        let url = url_for(repo, &title, &all);
        if url.len() <= MAX_URL_BYTES || (head == 0 && tail == 0) {
            fields = all;
            return Link {
                url,
                title,
                fields,
                truncated,
            };
        }
        truncated = true;
        if head >= tail && head > 0 {
            head -= 1;
        } else {
            tail = tail.saturating_sub(1);
        }
    }
}

/// The link for a computer-side report: version and OS prefilled, the redacted diagnostics as logs.
pub fn desktop_link(repo: &str, report: &str, app_version: &str) -> Link {
    let os = format!(
        "{} {} (add how GeekBridge was installed)",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    build(
        repo,
        DEFAULT_TITLE,
        &[
            (
                FIELD_VERSION,
                &format!("desktop {app_version}; phone: see Diagnostics & about"),
            ),
            (FIELD_DESKTOP, &os),
        ],
        report,
    )
}

/// Only an https link to github.com is ever handed to the system opener.
pub fn openable(url: &str) -> bool {
    url.starts_with("https://github.com/") && url.bytes().all(|b| b.is_ascii_graphic())
}

/// Opens `url` in the default browser. The URL is one argv entry of the platform opener, never part
/// of a shell command line. `false` when it is not an https GitHub link or no opener works.
pub fn open_in_browser(url: &str) -> bool {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    if !openable(url) {
        return false;
    }
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", windows)))]
    let program = "xdg-open";
    let Ok(mut child) = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match child.try_wait() {
            // explorer.exe reports 1 even when it opened the link.
            Ok(Some(status)) => return status.success() || cfg!(windows),
            Ok(None) => std::thread::sleep(Duration::from_millis(40)),
            Err(_) => return false,
        }
    }
    // Still running: the browser is starting. Reap it later so no zombie is left behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

/// Best-effort clipboard copy through the platform tool (for the CLI; the app uses egui's).
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let tools: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard", "-i"]),
        ("xsel", &["--clipboard", "--input"]),
        ("pbcopy", &[]),
        ("clip", &[]),
    ];
    for (program, args) in tools {
        let Ok(mut child) = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut i| i.write_all(text.as_bytes()).is_ok());
        // xclip/wl-copy keep running to serve the selection; do not wait for them.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if wrote {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = include_str!("../../../.github/ISSUE_TEMPLATE/bug_report.yml");

    fn template_ids() -> Vec<String> {
        TEMPLATE
            .lines()
            .filter_map(|l| l.trim().strip_prefix("id:"))
            .map(|v| v.trim().to_owned())
            .collect()
    }

    #[test]
    fn prefilled_fields_exist_in_the_issue_template() {
        let ids = template_ids();
        for f in PREFILLED_FIELDS {
            assert!(
                ids.iter().any(|i| i == f),
                "bug_report.yml has no field id `{f}`: {ids:?}"
            );
        }
    }

    #[test]
    fn template_file_exists_and_logs_field_is_a_textarea() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.github/ISSUE_TEMPLATE")
            .join(TEMPLATE_FILE);
        assert!(path.is_file(), "{path:?}");
        let at = TEMPLATE.find(&format!("id: {FIELD_LOGS}")).unwrap();
        assert!(
            TEMPLATE[..at]
                .rsplit("- type:")
                .next()
                .unwrap()
                .trim_start()
                .starts_with("textarea")
        );
    }

    #[test]
    fn repo_override_is_validated() {
        assert_eq!(repo_from(None), DEFAULT_REPO);
        assert_eq!(repo_from(Some(" me/fork ")), "me/fork");
        assert_eq!(repo_from(Some("a/b/c")), DEFAULT_REPO);
        assert_eq!(repo_from(Some("evil.com/../x")), DEFAULT_REPO);
        assert_eq!(repo_from(Some("")), DEFAULT_REPO);
    }

    #[test]
    fn url_shape_and_roundtrip() {
        let l = build(
            "fspecii/geekbridge",
            "[Bug] pairing & more",
            &[(FIELD_VERSION, "geekbridge 0.1.0")],
            "line one\nline two: ünï",
        );
        assert!(l.url.starts_with(
            "https://github.com/fspecii/geekbridge/issues/new?template=bug_report.yml&title=%5BBug%5D%20pairing%20%26%20more&version="
        ));
        assert!(!l.truncated);
        let logs = l.url.split("&logs=").nth(1).unwrap();
        assert_eq!(decode(logs), "line one\nline two: ünï");
        assert!(!l.url.contains(' ') && !l.url.contains('\n'));
    }

    #[test]
    fn unknown_field_ids_are_never_sent() {
        let l = build("a/b", "t", &[("secret", "x"), (FIELD_PHONE, "Pixel")], "");
        assert!(!l.url.contains("secret"));
        assert_eq!(l.fields.len(), 1);
    }

    #[test]
    fn long_report_is_capped_and_keeps_both_ends() {
        let mut lines = vec!["== Versions ==".to_owned()];
        lines.extend((0..400).map(|i| format!("filler line {i} with some words")));
        lines.push("== Last errors == connect_failed (timed out)".into());
        let l = build("a/b", "t", &[], &lines.join("\n"));
        assert!(l.url.len() <= MAX_URL_BYTES, "{}", l.url.len());
        assert!(l.truncated);
        let logs = &l.fields[0].1;
        assert!(logs.starts_with("== Versions =="));
        assert!(logs.ends_with("timed out)"));
        assert!(logs.contains("lines left out"));
    }

    #[test]
    fn redact_removes_personal_looking_tokens() {
        let s = redact(
            "mail me at alice@example.com from /home/alice/.config/x or C:\\Users\\Alice\\f \
             ip 192.168.1.20:1716 v6 fe80::1234:abcd phone +49 1701234567 id 3f2a9c1e-1111-2222-3333-444455556666 \
             dev 0123456789abcdef0123 mac aa:bb:cc:dd:ee:ff 01234567890 ok unix_ms 1730000000000",
        );
        for bad in [
            "alice",
            "example.com",
            "/home",
            "Users",
            "192.168",
            "fe80",
            "1701234567",
            "3f2a9c1e",
            "0123456789abcdef",
            "aa:bb",
            "01234567890",
        ] {
            assert!(!s.contains(bad), "{bad} leaked: {s}");
        }
        assert!(s.contains("unix_ms 1730000000000"), "{s}");
        assert!(s.contains("ok"));
    }

    #[test]
    fn only_github_https_links_are_opened() {
        assert!(openable("https://github.com/a/b/issues/new?x=1"));
        assert!(!openable("http://github.com/a/b"));
        assert!(!openable("https://evil.example/x"));
        assert!(!openable("https://github.com/a b"));
        assert!(!open_in_browser("file:///etc/passwd"));
    }

    #[test]
    fn redact_keeps_report_vocabulary() {
        let line = "service: 0.1.0 (ipc 1) host: linux x86_64 device-1: type phone, link connected";
        assert_eq!(redact(line), line);
    }
}
