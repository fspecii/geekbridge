//! End-to-end flows against a local mock "GitHub" over plain HTTP on loopback.

use bridge_core::ipc::UpdateState;
use bridge_update::{Config, Error, Platform, Updater};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Clone)]
enum Reply {
    Ok {
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    Status {
        code: u16,
        headers: Vec<(String, String)>,
    },
    Chunked(Vec<Vec<u8>>),
    /// Promises `claimed` bytes but sends `sent`, then closes.
    Short {
        claimed: usize,
        sent: Vec<u8>,
    },
    /// Claims a Content-Length and streams that many bytes of `b'x'`.
    Endless {
        len: usize,
    },
    Raw(Vec<u8>),
}

#[derive(Default)]
struct Seen {
    hits: HashMap<String, usize>,
    last_headers: HashMap<String, HashMap<String, String>>,
}

struct Mock {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Seen>>,
}

impl Mock {
    fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let (r, s) = (routes.clone(), seen.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let (r, s) = (r.clone(), s.clone());
                std::thread::spawn(move || serve(conn, &r, &s));
            }
        });
        Mock { port, routes, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn set(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.to_owned(), reply);
    }

    fn hits(&self, path: &str) -> usize {
        self.seen
            .lock()
            .unwrap()
            .hits
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    fn header(&self, path: &str, name: &str) -> Option<String> {
        self.seen
            .lock()
            .unwrap()
            .last_headers
            .get(path)?
            .get(name)
            .cloned()
    }
}

fn serve(mut conn: TcpStream, routes: &Mutex<HashMap<String, Reply>>, seen: &Mutex<Seen>) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut byte = [0_u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match conn.read(&mut byte) {
            Ok(1) => buf.push(byte[0]),
            _ => return,
        }
    }
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines = text.split("\r\n");
    let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    {
        let mut seen = seen.lock().unwrap();
        *seen.hits.entry(path.clone()).or_default() += 1;
        seen.last_headers.insert(path.clone(), headers);
    }
    let reply = routes.lock().unwrap().get(&path).cloned();
    let head = |code: u16, extra: &[(String, String)], more: &str| {
        let mut h = format!("HTTP/1.1 {code} X\r\n{more}");
        for (k, v) in extra {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        h.push_str("\r\n");
        h.into_bytes()
    };
    match reply {
        None => {
            let _ = conn.write_all(&head(404, &[], "Content-Length: 0\r\n"));
        }
        Some(Reply::Ok { headers, body }) => {
            let _ = conn.write_all(&head(
                200,
                &headers,
                &format!("Content-Length: {}\r\n", body.len()),
            ));
            let _ = conn.write_all(&body);
        }
        Some(Reply::Status { code, headers }) => {
            let _ = conn.write_all(&head(code, &headers, "Content-Length: 0\r\n"));
        }
        Some(Reply::Chunked(chunks)) => {
            let _ = conn.write_all(&head(200, &[], "Transfer-Encoding: chunked\r\n"));
            for c in chunks {
                let _ = conn.write_all(format!("{:x}\r\n", c.len()).as_bytes());
                let _ = conn.write_all(&c);
                let _ = conn.write_all(b"\r\n");
            }
            let _ = conn.write_all(b"0\r\n\r\n");
        }
        Some(Reply::Short { claimed, sent }) => {
            let _ = conn.write_all(&head(200, &[], &format!("Content-Length: {claimed}\r\n")));
            let _ = conn.write_all(&sent);
        }
        Some(Reply::Endless { len }) => {
            let _ = conn.write_all(&head(200, &[], &format!("Content-Length: {len}\r\n")));
            let block = vec![b'x'; 64 * 1024];
            let mut left = len;
            while left > 0 {
                let n = left.min(block.len());
                if conn.write_all(&block[..n]).is_err() {
                    break;
                }
                left -= n;
            }
        }
        Some(Reply::Raw(bytes)) => {
            let _ = conn.write_all(&bytes);
        }
    }
}

const LATEST: &str = "/repos/fspecii/geekbridge/releases/latest";
const ASSET: &str = "geekbridge-linux-x86_64.tar.gz";

fn sha_hex(data: &[u8]) -> String {
    bridge_core::ipc::encode_hex(&Sha256::digest(data))
}

fn release_json(mock: &Mock, tag: &str, with_sums: bool, size: usize) -> Vec<u8> {
    let base = mock.url();
    let mut assets = vec![
        serde_json::json!({"name": ASSET, "size": size,
            "browser_download_url": format!("{base}/dl/{ASSET}")}),
        serde_json::json!({"name": "geekbridge-windows-x86_64.zip", "size": 10,
            "browser_download_url": format!("{base}/dl/win.zip")}),
        serde_json::json!({"name": "geekbridge-android.apk", "size": 10,
            "browser_download_url": format!("{base}/dl/app.apk")}),
    ];
    if with_sums {
        assets.push(serde_json::json!({"name": "SHA256SUMS", "size": 100,
            "browser_download_url": format!("{base}/dl/SHA256SUMS")}));
    }
    serde_json::to_vec(&serde_json::json!({
        "tag_name": tag,
        "html_url": format!("{base}/release/{tag}"),
        "body": "Notes \u{1b}[31mhere",
        "assets": assets,
    }))
    .unwrap()
}

fn json_ok(body: Vec<u8>) -> Reply {
    Reply::Ok {
        headers: vec![("ETag".into(), "\"abc\"".into())],
        body,
    }
}

struct Rig {
    mock: Mock,
    dir: tempfile::TempDir,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            mock: Mock::start(),
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self, current: &str) -> Config {
        let mut cfg = Config::github(current);
        cfg.base_url = self.mock.url();
        cfg.platform = Platform {
            os: "linux".into(),
            arch: "x86_64".into(),
        };
        cfg.state_file = Some(self.dir.path().join("update.json"));
        let root = self.dir.path().join("dl");
        std::fs::create_dir_all(&root).unwrap();
        cfg.download_root = Some(root);
        cfg.io_timeout = Duration::from_secs(5);
        cfg.check_timeout = Duration::from_secs(10);
        cfg.download_timeout = Duration::from_secs(20);
        cfg.manual_cooldown = Duration::ZERO;
        cfg
    }

    fn updater(&self, current: &str) -> Arc<Updater> {
        Updater::new(self.config(current)).unwrap()
    }

    fn publish(&self, tag: &str, payload: &[u8], sums: Option<String>) {
        self.mock.set(
            LATEST,
            json_ok(release_json(&self.mock, tag, sums.is_some(), payload.len())),
        );
        self.mock.set(
            &format!("/dl/{ASSET}"),
            Reply::Ok {
                headers: vec![],
                body: payload.to_vec(),
            },
        );
        if let Some(sums) = sums {
            self.mock.set(
                "/dl/SHA256SUMS",
                Reply::Ok {
                    headers: vec![],
                    body: sums.into_bytes(),
                },
            );
        }
    }

    fn download_root_entries(&self) -> usize {
        std::fs::read_dir(self.dir.path().join("dl"))
            .unwrap()
            .count()
    }
}

fn good_sums(payload: &[u8]) -> String {
    format!(
        "{}  {ASSET}\n{}  geekbridge-windows-x86_64.zip\n",
        sha_hex(payload),
        "0".repeat(64)
    )
}

#[test]
fn newer_release_is_offered_downloaded_and_verified() {
    let rig = Rig::new();
    let payload = vec![7_u8; 100_000];
    rig.publish("v0.2.0", &payload, Some(good_sums(&payload)));
    let up = rig.updater("0.1.0");
    assert_eq!(up.status().state, UpdateState::Idle);
    up.check(true).unwrap();
    let st = up.status();
    assert_eq!(st.state, UpdateState::Available);
    assert!(st.newer && st.downloadable && !st.skipped);
    let latest = st.latest.unwrap();
    assert_eq!(latest.version, "0.2.0");
    assert_eq!(latest.asset_name.as_deref(), Some(ASSET));
    assert_eq!(latest.sha256, Some(sha_hex(&payload)));
    assert!(!latest.notes.contains('\u{1b}'));
    assert!(
        rig.mock
            .header(LATEST, "user-agent")
            .unwrap()
            .starts_with("geekbridge-updater/0.1.0")
    );

    let path = up.download().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    assert_eq!(path.file_name().unwrap(), ASSET);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "download dir must be private");
    }
    let st = up.status();
    assert_eq!(st.state, UpdateState::Downloaded);
    assert_eq!(st.downloaded_path.as_deref(), path.to_str());
    assert_eq!(st.downloaded_bytes, payload.len() as u64);
}

#[test]
fn verified_installer_rehashes_before_use() {
    let rig = Rig::new();
    let payload = vec![9_u8; 4096];
    rig.publish(
        "v0.2.0",
        &payload,
        Some(format!("{}  {ASSET}\n", sha_hex(&payload))),
    );
    let up = rig.updater("0.1.0");
    assert!(up.verified_installer().is_err(), "nothing downloaded yet");
    up.check(true).unwrap();
    let path = up.download().unwrap();
    assert_eq!(up.verified_installer().unwrap(), path);

    // The user (or anything else) swaps the file after the download.
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    std::fs::write(&path, b"tampered").unwrap();
    assert_eq!(up.verified_installer(), Err(Error::HashMismatch));
}

#[test]
fn hash_mismatch_is_rejected_and_nothing_is_left_behind() {
    let rig = Rig::new();
    let payload = vec![1_u8; 5000];
    let sums = format!("{}  {ASSET}\n", sha_hex(b"something else"));
    rig.publish("v0.2.0", &payload, Some(sums));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    assert_eq!(up.download(), Err(Error::HashMismatch));
    assert_eq!(rig.download_root_entries(), 0);
    let st = up.status();
    assert_eq!(st.state, UpdateState::Available);
    assert!(st.downloaded_path.is_none());
    assert!(st.error.unwrap().contains("SHA-256"));
}

#[test]
fn missing_checksum_means_no_download() {
    let rig = Rig::new();
    let payload = vec![1_u8; 100];
    rig.publish("v0.2.0", &payload, None);
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    let st = up.status();
    assert!(st.newer && !st.downloadable);
    assert!(matches!(up.download(), Err(Error::NothingToDownload(_))));
    assert_eq!(rig.mock.hits(&format!("/dl/{ASSET}")), 0);

    // A checksum file that does not list the asset is the same as none.
    rig.publish(
        "v0.2.0",
        &payload,
        Some(format!("{}  other.zip\n", "a".repeat(64))),
    );
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    assert!(!up.status().downloadable);
}

#[test]
fn same_or_older_version_is_up_to_date() {
    let rig = Rig::new();
    rig.publish("v0.1.0", b"x", Some(good_sums(b"x")));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    let st = up.status();
    assert_eq!(st.state, UpdateState::UpToDate);
    assert!(!st.newer && !st.downloadable);
    // The checksum of an installer we do not need is not fetched.
    assert_eq!(rig.mock.hits("/dl/SHA256SUMS"), 0);
    assert!(matches!(up.download(), Err(Error::NothingToDownload(_))));
}

#[test]
fn conditional_request_reuses_the_cached_release() {
    let rig = Rig::new();
    let payload = vec![3_u8; 64];
    rig.publish("v0.2.0", &payload, Some(good_sums(&payload)));
    rig.updater("0.1.0").check(true).unwrap();
    assert!(rig.mock.header(LATEST, "if-none-match").is_none());

    rig.mock.set(
        LATEST,
        Reply::Status {
            code: 304,
            headers: vec![],
        },
    );
    // A fresh process reads the persisted ETag and release.
    let up = rig.updater("0.1.0");
    assert_eq!(up.status().state, UpdateState::Available);
    up.check(true).unwrap();
    assert_eq!(
        rig.mock.header(LATEST, "if-none-match").as_deref(),
        Some("\"abc\"")
    );
    let st = up.status();
    assert_eq!(st.state, UpdateState::Available);
    assert!(st.downloadable && st.error.is_none());
    // The same cache seen from a newer build is simply "up to date".
    assert_eq!(rig.updater("0.2.0").status().state, UpdateState::UpToDate);
}

#[test]
fn manual_checks_are_throttled() {
    let rig = Rig::new();
    rig.publish("v0.1.0", b"x", None);
    let mut cfg = rig.config("0.1.0");
    cfg.manual_cooldown = Duration::from_secs(60);
    let up = Updater::new(cfg).unwrap();
    up.check(true).unwrap();
    up.check(true).unwrap();
    assert_eq!(rig.mock.hits(LATEST), 1);
}

#[test]
fn no_release_published_is_up_to_date() {
    let rig = Rig::new();
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    assert_eq!(up.status().state, UpdateState::UpToDate);
    assert!(up.status().latest.is_none());
}

#[test]
fn malformed_and_oversized_api_responses_are_errors() {
    let rig = Rig::new();
    let up = rig.updater("0.1.0");
    type Check = fn(&Error) -> bool;
    let cases: Vec<(Reply, Check)> = vec![
        (json_ok(b"<html>not json</html>".to_vec()), |e| {
            matches!(e, Error::Malformed(_))
        }),
        (json_ok(br#"{"tag_name":"latest"}"#.to_vec()), |e| {
            matches!(e, Error::Malformed(_))
        }),
        (Reply::Raw(b"garbage\r\n\r\n".to_vec()), |e| {
            matches!(e, Error::Malformed(_))
        }),
        (
            Reply::Raw(b"HTTP/1.1 200 OK\r\nContent-Length: 5000000\r\n\r\n".to_vec()),
            |e| *e == Error::TooLarge,
        ),
        (
            Reply::Endless {
                len: 3 * 1024 * 1024,
            },
            |e| *e == Error::TooLarge,
        ),
        (
            Reply::Chunked(vec![vec![b'{'; 600 * 1024], vec![b'x'; 600 * 1024]]),
            |e| *e == Error::TooLarge,
        ),
        (
            Reply::Short {
                claimed: 100,
                sent: b"{}".to_vec(),
            },
            |e| matches!(e, Error::Malformed(_)),
        ),
        (
            Reply::Status {
                code: 500,
                headers: vec![],
            },
            |e| *e == Error::Http(500),
        ),
        (
            Reply::Raw(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZZZ\r\n".to_vec()),
            |e| matches!(e, Error::Malformed(_)),
        ),
        (
            Reply::Raw(
                b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 2\r\n\r\n{}"
                    .to_vec(),
            ),
            |e| matches!(e, Error::Malformed(_)),
        ),
    ];
    for (i, (reply, check)) in cases.into_iter().enumerate() {
        rig.mock.set(LATEST, reply);
        let err = up.check(true).expect_err(&format!("case {i} must fail"));
        assert!(check(&err), "case {i}: {err:?}");
        assert!(up.status().error.is_some());
        assert!(
            up.status().latest.is_none(),
            "case {i} must not invent a release"
        );
    }
}

#[test]
fn chunked_api_response_is_understood() {
    let rig = Rig::new();
    let body = release_json(&rig.mock, "v0.1.0", false, 1);
    let (a, b) = body.split_at(40);
    rig.mock
        .set(LATEST, Reply::Chunked(vec![a.to_vec(), b.to_vec()]));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    assert_eq!(up.status().latest.unwrap().version, "0.1.0");
}

#[test]
fn oversized_or_truncated_downloads_fail_cleanly() {
    let rig = Rig::new();
    let payload = vec![9_u8; 4096];
    rig.publish("v0.2.0", &payload, Some(good_sums(&payload)));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();

    // Server sends more than the release declared.
    rig.mock.set(
        &format!("/dl/{ASSET}"),
        Reply::Endless {
            len: 10 * 1024 * 1024,
        },
    );
    assert_eq!(up.download(), Err(Error::TooLarge));
    assert_eq!(rig.download_root_entries(), 0);

    // Server stops early.
    rig.mock.set(
        &format!("/dl/{ASSET}"),
        Reply::Short {
            claimed: 4096,
            sent: vec![9_u8; 100],
        },
    );
    assert!(matches!(up.download(), Err(Error::Malformed(_))));
    assert_eq!(rig.download_root_entries(), 0);

    // Declared length matches the hash of a different (shorter) body.
    rig.mock.set(
        &format!("/dl/{ASSET}"),
        Reply::Ok {
            headers: vec![],
            body: vec![9_u8; 4095],
        },
    );
    assert_eq!(up.download(), Err(Error::SizeMismatch));
    assert_eq!(rig.download_root_entries(), 0);

    // And the good path still works afterwards.
    rig.mock.set(
        &format!("/dl/{ASSET}"),
        Reply::Ok {
            headers: vec![],
            body: payload.clone(),
        },
    );
    assert!(up.download().is_ok());
}

#[test]
fn rate_limit_backs_off_without_hitting_the_network() {
    let rig = Rig::new();
    rig.mock.set(
        LATEST,
        Reply::Status {
            code: 403,
            headers: vec![
                ("X-RateLimit-Remaining".into(), "0".into()),
                ("Retry-After".into(), "600".into()),
            ],
        },
    );
    let up = rig.updater("0.1.0");
    assert!(matches!(
        up.check(false),
        Err(Error::RateLimited {
            retry_after_secs: 600
        })
    ));
    assert_eq!(rig.mock.hits(LATEST), 1);
    assert!(matches!(up.check(false), Err(Error::RateLimited { .. })));
    assert!(matches!(up.check(true), Err(Error::RateLimited { .. })));
    assert_eq!(rig.mock.hits(LATEST), 1, "must not retry while backing off");
    assert!(up.status().error.unwrap().contains("rate limit"));
}

#[test]
fn redirects_stay_inside_the_policy() {
    let rig = Rig::new();
    let payload = vec![5_u8; 300];
    rig.publish("v0.2.0", &payload, Some(good_sums(&payload)));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();

    // Same-origin redirect is followed.
    rig.mock.set(
        &format!("/dl/{ASSET}"),
        Reply::Status {
            code: 302,
            headers: vec![("Location".into(), "/real".into())],
        },
    );
    rig.mock.set(
        "/real",
        Reply::Ok {
            headers: vec![],
            body: payload.clone(),
        },
    );
    assert!(up.download().is_ok());

    // A redirect to an arbitrary host or to plain http is refused.
    for target in [
        "https://evil.example/x",
        "http://github.com/x",
        "ftp://x/y",
        "//evil.example/x",
    ] {
        rig.mock.set(
            &format!("/dl/{ASSET}"),
            Reply::Status {
                code: 302,
                headers: vec![("Location".into(), target.into())],
            },
        );
        assert!(matches!(up.download(), Err(Error::Url(_))), "{target}");
        assert_eq!(
            rig.download_root_entries(),
            0,
            "a new attempt replaces the earlier download and leaves nothing on failure"
        );
    }
}

#[test]
fn skip_and_disable_are_respected_and_persist() {
    let rig = Rig::new();
    let payload = vec![5_u8; 30];
    rig.publish("v0.2.0", &payload, Some(good_sums(&payload)));
    let up = rig.updater("0.1.0");
    up.set_enabled(false);
    assert_eq!(up.check(false), Err(Error::Disabled));
    assert_eq!(rig.mock.hits(LATEST), 0);
    up.check(true).unwrap(); // explicit user action still works
    assert_eq!(rig.mock.hits(LATEST), 1);

    up.skip("v0.2.0").unwrap();
    assert!(up.skip("banana").is_err());
    let again = rig.updater("0.1.0");
    let st = again.status();
    assert!(!st.enabled && st.skipped && st.newer);

    // A newer release is not covered by the earlier skip.
    rig.publish("v0.3.0", &payload, Some(good_sums(&payload)));
    again.check(true).unwrap();
    let st = again.status();
    assert!(!st.skipped && st.latest.unwrap().version == "0.3.0");
}

#[test]
fn background_scheduler_checks_at_startup_when_enabled() {
    let rig = Rig::new();
    rig.publish("v0.1.0", b"x", None);
    let mut cfg = rig.config("0.1.0");
    cfg.startup_delay = Duration::ZERO;
    cfg.tick = Duration::from_millis(20);
    let up = Updater::new(cfg).unwrap();
    up.spawn_scheduler();
    for _ in 0..200 {
        if up.status().last_checked_unix.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(up.status().state, UpdateState::UpToDate);
    // Younger than the interval: no repeated requests.
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rig.mock.hits(LATEST), 1);

    let rig = Rig::new();
    rig.publish("v0.1.0", b"x", None);
    let mut cfg = rig.config("0.1.0");
    cfg.startup_delay = Duration::ZERO;
    cfg.tick = Duration::from_millis(20);
    let up = Updater::new(cfg).unwrap();
    up.set_enabled(false);
    up.spawn_scheduler();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        rig.mock.hits(LATEST),
        0,
        "disabled means no background traffic"
    );
}

#[test]
fn non_github_base_urls_are_refused() {
    for bad in [
        "http://example.com",
        "https://example.com",
        "http://api.github.com",
    ] {
        let mut cfg = Config::github("0.1.0");
        cfg.base_url = bad.into();
        assert!(Updater::new(cfg).is_err(), "{bad}");
    }
    let mut cfg = Config::github("not-a-version");
    cfg.base_url = "https://api.github.com".into();
    assert!(Updater::new(cfg).is_err());
}

#[test]
fn prerelease_tag_is_never_offered_even_if_not_flagged() {
    let rig = Rig::new();
    let payload = vec![4_u8; 32];
    rig.publish("v0.2.0-rc.1", &payload, Some(good_sums(&payload)));
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    let st = up.status();
    assert!(!st.newer && !st.downloadable && st.latest.is_none());
}

#[test]
fn a_release_without_checksum_is_not_pinned_behind_its_etag() {
    let rig = Rig::new();
    let payload = vec![5_u8; 32];
    rig.publish("v0.2.0", &payload, None);
    rig.updater("0.1.0").check(true).unwrap();
    // The checksum file shows up without the release JSON changing its ETag: the next check must
    // re-fetch instead of answering 304 from a cache that has no checksum.
    let up = rig.updater("0.1.0");
    up.check(true).unwrap();
    assert!(rig.mock.header(LATEST, "if-none-match").is_none());
}
