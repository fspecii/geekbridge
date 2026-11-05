// SPDX-License-Identifier: GPL-3.0-or-later
//! Update checking against the latest GitHub Release.
//!
//! What it does, and what it deliberately does not:
//!
//! * One conditional `GET /repos/<repo>/releases/latest` (User-Agent set, ETag, size and time
//!   limits, rate-limit aware). No identifiers, no telemetry, no other host than GitHub.
//! * The release tag is compared as semver with the running build's version.
//! * An installer is offered for download only when an asset for this OS/architecture **and** a
//!   published SHA-256 (`SHA256SUMS`) both exist. The download goes into a fresh private
//!   directory, is hashed while it is written, and is deleted if the digest does not match.
//! * Nothing is executed, installed or replaced here. The caller shows the verified file's
//!   location and the user installs it.

mod dev;
mod http;
mod release;
mod version;

pub use dev::{DEV_CODE_BASE, DEV_TAG, DevManifest, MANIFEST_ASSET};
pub use release::{Asset, Platform, Release, select_asset, select_asset_with};
pub use version::Version;

/// The untrusted-input parsers, exposed only so `crates/bridge-fuzz` can drive them.
#[doc(hidden)]
pub mod fuzzing {
    pub use crate::http::{Policy, Url};
    pub use crate::release::{clean_notes, find_checksum, safe_file_name, url_allowed};
}

use bridge_core::ipc::{UpdateChannel, UpdateRelease, UpdateState, UpdateStatus};
use release::{MAX_ASSET_BYTES, clean_notes, find_checksum, find_sums_asset, safe_file_name};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Built-in release source; `GEEKBRIDGE_RELEASE_REPO=owner/name` at build time points a build at a
/// separate public releases-only repository (see docs/release-checklist.md).
pub const REPO: &str = match option_env!("GEEKBRIDGE_RELEASE_REPO") {
    Some(repo) => repo,
    None => "fspecii/geekbridge",
};
pub use bridge_core::ipc::UPDATE_NO_PUBLIC_RELEASE as NO_PUBLIC_RELEASE;
const API_BASE: &str = "https://api.github.com";
const MAX_API_BYTES: u64 = 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 64 * 1024;
const NOT_PUBLIC_COOLDOWN: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Url(String),
    Io(String),
    Tls(String),
    Timeout,
    Http(u16),
    RateLimited { retry_after_secs: u64 },
    TooLarge,
    Malformed(String),
    Disabled,
    Busy,
    NothingToDownload(&'static str),
    HashMismatch,
    SizeMismatch,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Url(m) | Error::Io(m) | Error::Tls(m) | Error::Malformed(m) => f.write_str(m),
            Error::Timeout => f.write_str("the server did not answer in time"),
            Error::Http(s) => write!(f, "GitHub answered HTTP {s}"),
            Error::RateLimited { retry_after_secs } => write!(
                f,
                "GitHub rate limit reached; trying again in about {} min",
                retry_after_secs.div_ceil(60)
            ),
            Error::TooLarge => f.write_str("the response was larger than allowed"),
            Error::Disabled => f.write_str("automatic update checks are turned off"),
            Error::Busy => f.write_str("an update operation is already running"),
            Error::NothingToDownload(why) => f.write_str(why),
            Error::HashMismatch => f.write_str(
                "the downloaded file does not match the published SHA-256; it was deleted",
            ),
            Error::SizeMismatch => {
                f.write_str("the downloaded file has the wrong size; it was deleted")
            }
        }
    }
}

impl std::error::Error for Error {}

/// A bearer token that never prints: `Debug` is redacted and there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Secret {
        Secret(value.into())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// `owner/name` as GitHub spells it: the only shapes that may end up in an API URL.
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

#[derive(Clone, Debug)]
pub struct Config {
    /// `https://api.github.com`; tests point this at a loopback mock server.
    pub base_url: String,
    /// `owner/name`; a separate public releases-only repository works as well as the main one.
    pub repo: String,
    /// Optional read-only token for a private repository (testers). Sent to `api.github.com`
    /// only; never stored, never logged.
    pub token: Option<Secret>,
    pub current_version: String,
    pub platform: Platform,
    /// Where settings, the ETag and the last answer persist. `None` keeps everything in memory.
    pub state_file: Option<PathBuf>,
    /// Parent of the private per-download directory. Defaults to a per-user state directory
    /// (`$XDG_STATE_HOME/geekbridge/updates`, `%LOCALAPPDATA%`, `~/Library/Application Support`),
    /// never the shared OS temp directory unless no user directory can be found.
    pub download_root: Option<PathBuf>,
    pub io_timeout: Duration,
    pub check_timeout: Duration,
    pub download_timeout: Duration,
    /// Delay before the startup check, so it never competes with start-up work.
    pub startup_delay: Duration,
    /// Time between automatic checks.
    pub interval: Duration,
    /// A startup check is skipped when the last check is younger than this.
    pub min_gap: Duration,
    /// How often the scheduler wakes to look at the clock.
    pub tick: Duration,
    /// Manual checks closer together than this are answered from the cache.
    pub manual_cooldown: Duration,
    /// Also offer experimental archives (`-gnu-`, `experimental`) when a release has no regular
    /// one for this system. Off unless `GEEKBRIDGE_UPDATE_EXPERIMENTAL=1`.
    pub experimental: bool,
    /// Operator override of the channel (`GEEKBRIDGE_UPDATE_CHANNEL`). `None`: the persisted choice
    /// made in the app's settings (default `stable`).
    pub channel: Option<UpdateChannel>,
    /// Source commit (12 hex digits) and commit count of the running build, embedded by `build.rs`;
    /// `None` for a build without a git checkout. The `dev` channel compares against them.
    pub build_commit: Option<String>,
    pub build_count: Option<u32>,
}

impl Config {
    /// The real thing: GitHub over https for the running build.
    pub fn github(current_version: &str) -> Config {
        Config {
            base_url: API_BASE.to_owned(),
            repo: REPO.to_owned(),
            token: None,
            current_version: current_version.to_owned(),
            platform: Platform::current(),
            state_file: None,
            download_root: None,
            io_timeout: Duration::from_secs(15),
            check_timeout: Duration::from_secs(30),
            download_timeout: Duration::from_secs(30 * 60),
            startup_delay: Duration::from_secs(15),
            interval: Duration::from_secs(24 * 3600),
            min_gap: Duration::from_secs(600),
            tick: Duration::from_secs(30),
            manual_cooldown: Duration::from_secs(30),
            experimental: false,
            channel: None,
            build_commit: option_env!("GEEKBRIDGE_BUILD_COMMIT").map(str::to_owned),
            build_count: option_env!("GEEKBRIDGE_BUILD_COUNT").and_then(|c| c.parse().ok()),
        }
    }

    /// [`Config::github`] plus the operator's overrides: `GEEKBRIDGE_UPDATE_REPO` (`owner/name`,
    /// ignored with a warning if malformed) and `GEEKBRIDGE_UPDATE_TOKEN` (read-only token for a
    /// private repository, kept in memory only).
    pub fn github_from_env(current_version: &str) -> Config {
        let mut cfg = Config::github(current_version);
        if let Ok(repo) = std::env::var("GEEKBRIDGE_UPDATE_REPO") {
            let repo = repo.trim();
            if valid_repo(repo) {
                cfg.repo = repo.to_owned();
            } else if !repo.is_empty() {
                eprintln!("GeekBridge: ignoring GEEKBRIDGE_UPDATE_REPO (expected owner/name)");
            }
        }
        if let Ok(text) = std::env::var("GEEKBRIDGE_UPDATE_CHANNEL") {
            match UpdateChannel::parse(&text) {
                Some(channel) => cfg.channel = Some(channel),
                None if !text.trim().is_empty() => {
                    eprintln!(
                        "GeekBridge: ignoring GEEKBRIDGE_UPDATE_CHANNEL (expected stable or dev)"
                    );
                }
                None => {}
            }
        }
        cfg.experimental = std::env::var("GEEKBRIDGE_UPDATE_EXPERIMENTAL")
            .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        cfg.token = std::env::var("GEEKBRIDGE_UPDATE_TOKEN")
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .map(Secret::new);
        cfg
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Cached {
    version: String,
    tag: String,
    notes: String,
    release_url: String,
    asset: Option<Asset>,
    sha256: Option<String>,
    /// From the `dev` channel (`version` is then `dev-<commit>`).
    #[serde(default)]
    dev: bool,
    /// Dev channel only: decided at fetch time against the running build's commit and count.
    #[serde(default)]
    dev_newer: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct Persisted {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    channel: UpdateChannel,
    #[serde(default)]
    skipped_version: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    /// The running version the cached release (asset, checksum) was prepared for. The cached
    /// offer depends on it (only a release newer than the running build gets an asset), so after
    /// an upgrade or downgrade the ETag is not sent and the release is fetched again.
    #[serde(default)]
    checked_for: Option<String>,
    #[serde(default)]
    last_checked_unix: Option<u64>,
    #[serde(default)]
    release: Option<Cached>,
    /// The last check got 404/403: no public release is reachable (not "up to date").
    #[serde(default)]
    no_public_release: bool,
    /// Rate-limit pause (unix seconds). Persisted so a restart loop of the service cannot ignore it.
    #[serde(default)]
    backoff_until: Option<u64>,
    /// When the scheduler last gave up on a check. Persisted for the same reason: without it every
    /// restart after a failed check would ask GitHub again 15 s after start.
    #[serde(default)]
    last_attempt_unix: Option<u64>,
}

/// A stored pause further ahead than this is a clock that was set forward and back: ignored.
const MAX_BACKOFF_SECS: u64 = 24 * 3600;

fn yes() -> bool {
    true
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            enabled: true,
            channel: UpdateChannel::Stable,
            skipped_version: None,
            etag: None,
            checked_for: None,
            last_checked_unix: None,
            release: None,
            no_public_release: false,
            backoff_until: None,
            last_attempt_unix: None,
        }
    }
}

struct Downloaded {
    dir: PathBuf,
    file: PathBuf,
    version: String,
    sha256: String,
}

#[derive(Default)]
struct Runtime {
    persisted: Persisted,
    checking: bool,
    downloading: bool,
    error: Option<String>,
    downloaded: Option<Downloaded>,
    progress: (u64, u64),
}

#[allow(clippy::large_enum_variant)]
enum Fetch {
    NotModified,
    NoRelease,
    /// 404/403: private repository, wrong name, or nothing published.
    NotPublic,
    Fresh {
        etag: Option<String>,
        cached: Cached,
    },
}

pub struct Updater {
    cfg: Config,
    current: Version,
    client: http::Client,
    state: Mutex<Runtime>,
}

/// Clears a `Runtime` busy flag when dropped, so a panicking worker cannot leave "checking" or
/// "downloading" set forever.
struct FlagGuard<'a> {
    up: &'a Updater,
    flag: fn(&mut Runtime) -> &mut bool,
}

impl<'a> FlagGuard<'a> {
    fn new(up: &'a Updater, flag: fn(&mut Runtime) -> &mut bool) -> Self {
        Self { up, flag }
    }
}

impl Drop for FlagGuard<'_> {
    fn drop(&mut self) {
        *(self.flag)(&mut self.up.lock()) = false;
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Updater {
    pub fn new(cfg: Config) -> Result<Arc<Updater>, Error> {
        let current = Version::parse(&cfg.current_version)
            .ok_or_else(|| Error::Malformed("the running version is not semver".into()))?;
        let base = http::Url::parse(&cfg.base_url)?;
        let policy = http::Policy {
            loopback_origin: base.is_loopback().then(|| (base.host.clone(), base.port)),
        };
        if !policy.allows(&base) {
            return Err(Error::Url("update base URL is not permitted".into()));
        }
        if !valid_repo(&cfg.repo) {
            return Err(Error::Url("update repository must be owner/name".into()));
        }
        let client = http::Client::new(
            &format!("geekbridge-updater/{}", cfg.current_version),
            policy,
            cfg.io_timeout,
            cfg.token.as_ref().map(|t| t.0.clone()),
        )?;
        // Leftovers of crashed or abandoned downloads (up to 512 MiB each). Recent ones may belong
        // to another running instance, so only old directories go.
        sweep_stale_downloads(&download_root(&cfg), STALE_DOWNLOAD_AGE);
        let persisted = cfg
            .state_file
            .as_deref()
            .and_then(load_persisted)
            .unwrap_or_default();
        Ok(Arc::new(Updater {
            current,
            client,
            state: Mutex::new(Runtime {
                persisted,
                ..Runtime::default()
            }),
            cfg,
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Runtime> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self, rt: &Runtime) {
        if let Some(path) = &self.cfg.state_file
            && let Ok(serde_json::Value::Object(known)) = serde_json::to_value(&rt.persisted)
        {
            // Only our fields are replaced: anything a newer build added stays in the file, and a
            // file from a newer format is left untouched. A failed save is only a missed cache.
            let result = state_file(path).update(|body| {
                body.extend(known);
                true
            });
            if let Err(e) = result {
                bridge_core::statefile::note(format!("update-state.json: not saved: {e}"));
            }
        }
    }

    pub fn status(&self) -> UpdateStatus {
        let rt = self.lock();
        let cached = self.offer(&rt);
        let newer = cached.is_some_and(|c| self.is_newer(c));
        let downloaded = rt
            .downloaded
            .as_ref()
            .filter(|d| cached.is_some_and(|c| c.version == d.version));
        let state = if rt.downloading {
            UpdateState::Downloading
        } else if rt.checking {
            UpdateState::Checking
        } else if downloaded.is_some() {
            UpdateState::Downloaded
        } else if newer {
            UpdateState::Available
        } else if rt.persisted.no_public_release {
            UpdateState::NoPublicRelease
        } else if rt.persisted.last_checked_unix.is_some() {
            UpdateState::UpToDate
        } else {
            UpdateState::Idle
        };
        UpdateStatus {
            enabled: rt.persisted.enabled,
            current_version: self.cfg.current_version.clone(),
            channel: self.channel_of(&rt),
            current_commit: self.cfg.build_commit.clone(),
            state,
            latest: cached.map(|c| UpdateRelease {
                version: c.version.clone(),
                tag: c.tag.clone(),
                notes: c.notes.clone(),
                release_url: c.release_url.clone(),
                asset_name: c.asset.as_ref().map(|a| a.name.clone()),
                asset_size: c.asset.as_ref().map(|a| a.size),
                sha256: c.sha256.clone(),
            }),
            newer,
            skipped: cached
                .zip(rt.persisted.skipped_version.as_ref())
                .is_some_and(|(c, s)| c.version == *s),
            downloadable: newer && cached.is_some_and(|c| c.asset.is_some() && c.sha256.is_some()),
            last_checked_unix: rt.persisted.last_checked_unix,
            downloaded_bytes: rt.progress.0,
            download_total: rt.progress.1,
            downloaded_path: downloaded.map(|d| d.file.display().to_string()),
            error: rt.error.clone(),
        }
    }

    fn channel_of(&self, rt: &Runtime) -> UpdateChannel {
        self.cfg.channel.unwrap_or(rt.persisted.channel)
    }

    /// The cached offer, if it belongs to the channel being followed.
    fn offer<'a>(&self, rt: &'a Runtime) -> Option<&'a Cached> {
        let dev = self.channel_of(rt) == UpdateChannel::Dev;
        rt.persisted.release.as_ref().filter(|c| c.dev == dev)
    }

    fn is_newer(&self, c: &Cached) -> bool {
        if c.dev {
            c.dev_newer
        } else {
            Version::parse(&c.version).is_some_and(|v| v > self.current)
        }
    }

    /// What the cached answer (and its ETag) was prepared for: the running build and the channel.
    fn check_key(&self, rt: &Runtime) -> String {
        match self.channel_of(rt) {
            UpdateChannel::Stable => self.cfg.current_version.clone(),
            UpdateChannel::Dev => format!(
                "dev:{}:{}:{}",
                self.cfg.current_version,
                self.cfg.build_commit.as_deref().unwrap_or("-"),
                self.cfg
                    .build_count
                    .map_or_else(|| "-".to_owned(), |n| n.to_string())
            ),
        }
    }

    /// Follow `channel` from now on (persisted). The other channel's offer, ETag and skip choice
    /// are dropped, so the next check starts clean. Ignored when `GEEKBRIDGE_UPDATE_CHANNEL`
    /// pins the channel.
    pub fn set_channel(&self, channel: UpdateChannel) {
        let mut rt = self.lock();
        if rt.persisted.channel == channel {
            return;
        }
        rt.persisted.channel = channel;
        rt.persisted.release = None;
        rt.persisted.etag = None;
        rt.persisted.checked_for = None;
        rt.persisted.skipped_version = None;
        rt.persisted.last_checked_unix = None;
        rt.persisted.no_public_release = false;
        rt.error = None;
        rt.progress = (0, 0);
        if let Some(old) = rt.downloaded.take() {
            let _ = std::fs::remove_dir_all(old.dir);
        }
        self.persist(&rt);
    }

    pub fn set_enabled(&self, enabled: bool) {
        let mut rt = self.lock();
        rt.persisted.enabled = enabled;
        self.persist(&rt);
    }

    /// Hide the offer for `version`; a later release is offered again.
    pub fn skip(&self, version: &str) -> Result<(), Error> {
        let skipped = if dev::valid_label(version) {
            version.to_ascii_lowercase()
        } else {
            Version::parse(version)
                .ok_or_else(|| Error::Malformed("not a version number".into()))?
                .to_string()
        };
        let mut rt = self.lock();
        rt.persisted.skipped_version = Some(skipped);
        self.persist(&rt);
        Ok(())
    }

    /// Run a check on a background thread and return at once; poll [`Updater::status`]. The
    /// state is `Checking` by the time this returns. Problems are reported through `status`.
    pub fn start_check(self: &Arc<Self>, manual: bool) {
        if let Ok(Some(etag)) = self.begin_check(manual) {
            let this = self.clone();
            std::thread::spawn(move || {
                let _ = this.finish_check(etag.as_deref());
            });
        }
    }

    /// Download on a background thread; poll [`Updater::status`]. The state is `Downloading` by
    /// the time this returns.
    pub fn start_download(self: &Arc<Self>) -> Result<(), Error> {
        let job = self.begin_download()?;
        let this = self.clone();
        std::thread::spawn(move || {
            let _ = this.finish_download(job);
        });
        Ok(())
    }

    /// Query the latest release. `manual` means the user asked: it works even with automatic
    /// checks off, but is still throttled to one network request per cooldown.
    pub fn check(&self, manual: bool) -> Result<(), Error> {
        match self.begin_check(manual)? {
            None => Ok(()),
            Some(etag) => self.finish_check(etag.as_deref()),
        }
    }

    /// `Ok(None)`: answered from the cache. `Ok(Some(etag))`: a request must now be made.
    fn begin_check(&self, manual: bool) -> Result<Option<Option<String>>, Error> {
        let mut rt = self.lock();
        if rt.checking {
            return Err(Error::Busy);
        }
        if !manual && !rt.persisted.enabled {
            return Err(Error::Disabled);
        }
        let now = now_unix();
        if let Some(until) = rt
            .persisted
            .backoff_until
            .filter(|u| *u > now && *u - now <= MAX_BACKOFF_SECS)
        {
            let err = Error::RateLimited {
                retry_after_secs: until - now,
            };
            rt.error = Some(err.to_string());
            return Err(err);
        }
        // A repository without a public release is asked again no more than every few minutes.
        let cooldown = if rt.persisted.no_public_release {
            self.cfg.manual_cooldown.max(NOT_PUBLIC_COOLDOWN)
        } else {
            self.cfg.manual_cooldown
        };
        // A stored time ahead of the clock (clock set back) counts as "never": it must not freeze checks.
        if manual
            && rt.error.is_none()
            && rt
                .persisted
                .last_checked_unix
                .filter(|t| *t <= now)
                .is_some_and(|t| now - t < cooldown.as_secs())
        {
            return Ok(None);
        }
        rt.checking = true;
        rt.error = None;
        let etag = rt
            .persisted
            .release
            .as_ref()
            .filter(|_| rt.persisted.checked_for.as_deref() == Some(self.check_key(&rt).as_str()))
            .and_then(|_| rt.persisted.etag.clone());
        Ok(Some(etag))
    }

    fn finish_check(&self, etag: Option<&str>) -> Result<(), Error> {
        // Declared before the lock below so the flag is cleared after it is released, also on panic.
        let _reset = FlagGuard::new(self, |rt| &mut rt.checking);
        let (channel, key) = {
            let rt = self.lock();
            (self.channel_of(&rt), self.check_key(&rt))
        };
        let result = match channel {
            UpdateChannel::Stable => self.fetch_latest(etag),
            UpdateChannel::Dev => self.fetch_dev(etag),
        };
        let mut rt = self.lock();
        rt.checking = false;
        let now = now_unix();
        match result {
            Ok(Fetch::NotModified) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.no_public_release = false;
            }
            Ok(Fetch::NoRelease) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.release = None;
                rt.persisted.etag = None;
                rt.persisted.no_public_release = false;
            }
            Ok(Fetch::NotPublic) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.release = None;
                rt.persisted.etag = None;
                rt.persisted.no_public_release = true;
            }
            Ok(Fetch::Fresh { etag, cached }) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.no_public_release = false;
                rt.persisted.etag = etag;
                rt.persisted.checked_for = Some(key);
                rt.persisted.release = Some(cached);
            }
            Err(e) => {
                if let Error::RateLimited { retry_after_secs } = &e {
                    rt.persisted.backoff_until =
                        Some(now + (*retry_after_secs).clamp(60, MAX_BACKOFF_SECS));
                }
                rt.error = Some(e.to_string());
                self.persist(&rt);
                return Err(e);
            }
        }
        self.persist(&rt);
        Ok(())
    }

    fn get_bytes(
        &self,
        url: &str,
        accept: &str,
        max: u64,
        etag: Option<&str>,
    ) -> Result<(http::Fetched, Vec<u8>), Error> {
        let mut headers = vec![("Accept", accept), ("X-GitHub-Api-Version", "2022-11-28")];
        if let Some(etag) = etag {
            headers.push(("If-None-Match", etag));
        }
        let mut body = Vec::new();
        let fetched = self
            .client
            .get(url, &headers, max, self.cfg.check_timeout, &mut |c| {
                body.extend_from_slice(c);
                Ok(())
            })?;
        Ok((fetched, body))
    }

    fn fetch_latest(&self, etag: Option<&str>) -> Result<Fetch, Error> {
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.repo
        );
        let (fetched, body) =
            self.get_bytes(&url, "application/vnd.github+json", MAX_API_BYTES, etag)?;
        match fetched.status {
            200 => {}
            304 if etag.is_some() => return Ok(Fetch::NotModified),
            _ if fetched.rate_limited => {
                return Err(Error::RateLimited {
                    retry_after_secs: fetched.retry_after.unwrap_or(3600),
                });
            }
            // `releases/latest` never returns a pre-release, so a repository whose only release is
            // `v0.1.0-rc.1` answers 404 as well. One more request tells that apart from a private
            // repository (404/403 for anonymous callers): "up to date" versus "no public release".
            403 | 404 => return self.probe_release_list(),
            status => return Err(Error::Http(status)),
        }
        let release = Release::parse(&body)?;
        if release.draft || release.prerelease {
            return Ok(Fetch::NoRelease);
        }
        let version = release
            .version()
            .ok_or_else(|| Error::Malformed("the release tag is not a version".into()))?;
        if version.is_prerelease() {
            return Ok(Fetch::NoRelease);
        }
        let mut cached = Cached {
            version: version.to_string(),
            tag: release.tag_name.clone(),
            notes: clean_notes(release.body.as_deref().unwrap_or("")),
            release_url: self.release_page(&release),
            asset: None,
            sha256: None,
            dev: false,
            dev_newer: false,
        };
        if version > self.current {
            self.attach_installer(&mut cached, &release)?;
        }
        // Without the checksum the offer is unusable. Do not pin it behind an ETag: a later
        // check must re-fetch (the checksum file may have been briefly unavailable).
        let etag = if cached.asset.is_some() && cached.sha256.is_none() {
            None
        } else {
            fetched.etag
        };
        Ok(Fetch::Fresh { etag, cached })
    }

    fn release_page(&self, release: &Release) -> String {
        let url = if release::url_allowed(&self.client.policy, &release.html_url) {
            release.html_url.clone()
        } else {
            format!(
                "https://github.com/{}/releases/tag/{}",
                self.cfg.repo, release.tag_name
            )
        };
        if url.len() <= 512 { url } else { String::new() }
    }

    /// Pick this system's archive and look up its published SHA-256.
    fn attach_installer(&self, cached: &mut Cached, release: &Release) -> Result<(), Error> {
        let policy = &self.client.policy;
        cached.asset =
            select_asset_with(&release.assets, &self.cfg.platform, self.cfg.experimental)
                .filter(|a| release::url_allowed(policy, &a.browser_download_url))
                .cloned();
        if let (Some(asset), Some(sums)) = (&cached.asset, find_sums_asset(&release.assets))
            && release::url_allowed(policy, &sums.browser_download_url)
        {
            let (f, text) = self.get_bytes(
                self.fetch_url(sums),
                "application/octet-stream",
                MAX_SUMS_BYTES,
                None,
            )?;
            if f.rate_limited {
                // Back off like any other rate-limited answer instead of silently offering nothing.
                return Err(Error::RateLimited {
                    retry_after_secs: f.retry_after.unwrap_or(3600),
                });
            }
            if f.status == 200 {
                cached.sha256 = std::str::from_utf8(&text)
                    .ok()
                    .and_then(|t| find_checksum(t, &asset.name));
            }
            // GitHub computes its own `digest` for every upload. Two sources that disagree
            // mean one of them was tampered with or is stale: offer nothing.
            if let (Some(sums), Some(api)) = (&cached.sha256, asset.api_digest())
                && *sums != api
            {
                return Err(Error::Malformed(
                    "GitHub's digest for the installer and SHA256SUMS disagree; not offered".into(),
                ));
            }
        }
        Ok(())
    }

    /// The `dev` channel: the rolling prerelease tagged `dev`, described by `dev-manifest.json`.
    /// One conditional request for the release, one small one for the manifest (and the checksum
    /// file only when a newer dev build has an archive for this system).
    fn fetch_dev(&self, etag: Option<&str>) -> Result<Fetch, Error> {
        let url = format!(
            "{}/repos/{}/releases/tags/{}",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.repo,
            DEV_TAG
        );
        let (fetched, body) =
            self.get_bytes(&url, "application/vnd.github+json", MAX_API_BYTES, etag)?;
        match fetched.status {
            200 => {}
            304 if etag.is_some() => return Ok(Fetch::NotModified),
            _ if fetched.rate_limited => {
                return Err(Error::RateLimited {
                    retry_after_secs: fetched.retry_after.unwrap_or(3600),
                });
            }
            // No `dev` release yet, or a private repository: told apart by the release list.
            403 | 404 => return self.probe_release_list(),
            status => return Err(Error::Http(status)),
        }
        let release = Release::parse(&body)?;
        if release.draft || release.tag_name != DEV_TAG {
            return Ok(Fetch::NoRelease);
        }
        let policy = &self.client.policy;
        let manifest_asset = release
            .asset(MANIFEST_ASSET)
            .filter(|a| {
                a.size <= dev::MAX_MANIFEST_BYTES
                    && release::url_allowed(policy, &a.browser_download_url)
            })
            .ok_or_else(|| Error::Malformed("the dev release has no dev-manifest.json".into()))?;
        let (f, text) = self.get_bytes(
            self.fetch_url(manifest_asset),
            "application/octet-stream",
            dev::MAX_MANIFEST_BYTES,
            None,
        )?;
        if f.status != 200 {
            return Err(Error::Http(f.status));
        }
        let manifest = DevManifest::parse(&text)?;
        let newer = manifest.newer_than(self.cfg.build_commit.as_deref(), self.cfg.build_count);
        let mut notes = clean_notes(&manifest.notes);
        if !manifest.built_at.is_empty() {
            notes = format!("Built {}\n{notes}", manifest.built_at);
        }
        let mut cached = Cached {
            version: manifest.label(),
            tag: release.tag_name.clone(),
            notes,
            release_url: self.release_page(&release),
            asset: None,
            sha256: None,
            dev: true,
            dev_newer: newer,
        };
        if newer {
            self.attach_installer(&mut cached, &release)?;
        }
        let etag = if cached.asset.is_some() && cached.sha256.is_none() {
            None
        } else {
            fetched.etag
        };
        Ok(Fetch::Fresh { etag, cached })
    }

    /// `releases/latest` said 404/403: is the repository reachable at all? A readable release
    /// list (even an empty one, or one holding only pre-releases) means there is simply no stable
    /// release to offer; an unreadable one means private or misspelt.
    fn probe_release_list(&self) -> Result<Fetch, Error> {
        let url = format!(
            "{}/repos/{}/releases?per_page=30",
            self.cfg.base_url.trim_end_matches('/'),
            self.cfg.repo
        );
        let (fetched, body) =
            self.get_bytes(&url, "application/vnd.github+json", MAX_API_BYTES, None)?;
        match fetched.status {
            200 => match serde_json::from_slice::<serde_json::Value>(&body) {
                Ok(serde_json::Value::Array(_)) => Ok(Fetch::NoRelease),
                _ => Err(Error::Malformed("release list is not understood".into())),
            },
            _ if fetched.rate_limited => Err(Error::RateLimited {
                retry_after_secs: fetched.retry_after.unwrap_or(3600),
            }),
            403 | 404 => Ok(Fetch::NotPublic),
            status => Err(Error::Http(status)),
        }
    }

    /// Where to fetch `asset` from. With a token (private repository) the API URL is used: it
    /// redirects to a pre-signed CDN URL, which never receives the token. Otherwise the plain
    /// public download URL.
    fn fetch_url<'a>(&self, asset: &'a Asset) -> &'a str {
        match &asset.url {
            Some(api)
                if self.cfg.token.is_some() && release::url_allowed(&self.client.policy, api) =>
            {
                api
            }
            _ => &asset.browser_download_url,
        }
    }

    /// Download the offered installer into a fresh private directory and verify its SHA-256.
    /// On any failure the partial file and its directory are removed.
    pub fn download(&self) -> Result<PathBuf, Error> {
        let job = self.begin_download()?;
        self.finish_download(job)
    }

    fn begin_download(&self) -> Result<(Cached, Asset, String), Error> {
        let mut rt = self.lock();
        if rt.downloading {
            return Err(Error::Busy);
        }
        let cached = self
            .offer(&rt)
            .filter(|c| self.is_newer(c))
            .cloned()
            .ok_or(Error::NothingToDownload("no newer release is known"))?;
        let asset = cached.asset.clone().ok_or(Error::NothingToDownload(
            "the release has no installer for this system",
        ))?;
        let sha = cached.sha256.clone().ok_or(Error::NothingToDownload(
            "the release publishes no SHA-256 for this installer, so it will not be downloaded",
        ))?;
        if !safe_file_name(&asset.name) || asset.size == 0 || asset.size > MAX_ASSET_BYTES {
            return Err(Error::NothingToDownload(
                "the installer description is not usable",
            ));
        }
        rt.downloading = true;
        rt.error = None;
        rt.progress = (0, asset.size);
        if let Some(old) = rt.downloaded.take() {
            let _ = std::fs::remove_dir_all(old.dir);
        }
        Ok((cached, asset, sha))
    }

    fn finish_download(&self, job: (Cached, Asset, String)) -> Result<PathBuf, Error> {
        let (cached, asset, sha) = job;
        let _reset = FlagGuard::new(self, |rt| &mut rt.downloading);
        let result = self.download_verified(&asset, &sha);
        let mut rt = self.lock();
        rt.downloading = false;
        match result {
            Ok((dir, file)) => {
                touch_dir(&dir);
                rt.downloaded = Some(Downloaded {
                    dir,
                    file: file.clone(),
                    version: cached.version,
                    sha256: sha,
                });
                Ok(file)
            }
            Err(e) => {
                rt.progress = (0, 0);
                rt.error = Some(e.to_string());
                Err(e)
            }
        }
    }

    fn download_verified(
        &self,
        asset: &Asset,
        expected: &str,
    ) -> Result<(PathBuf, PathBuf), Error> {
        let root = download_root(&self.cfg);
        let dir = make_private_dir(&root)?;
        let outcome = (|| {
            let file_path = dir.join(&asset.name);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file_path)
                .map_err(|e| Error::Io(e.to_string()))?;
            let mut hasher = Sha256::new();
            let mut written = 0_u64;
            let fetched = self.client.get(
                self.fetch_url(asset),
                &[("Accept", "application/octet-stream")],
                asset.size,
                self.cfg.download_timeout,
                &mut |chunk| {
                    hasher.update(chunk);
                    file.write_all(chunk)?;
                    written += chunk.len() as u64;
                    self.lock().progress.0 = written;
                    Ok(())
                },
            )?;
            if fetched.status != 200 {
                return Err(Error::Http(fetched.status));
            }
            file.flush()
                .and_then(|()| file.sync_all())
                .map_err(|e| Error::Io(e.to_string()))?;
            drop(file);
            if written != asset.size {
                return Err(Error::SizeMismatch);
            }
            let digest = bridge_core::ipc::encode_hex(&hasher.finalize());
            if digest != expected.to_ascii_lowercase() {
                return Err(Error::HashMismatch);
            }
            if let Ok(meta) = std::fs::metadata(&file_path) {
                let mut perms = meta.permissions();
                perms.set_readonly(true);
                let _ = std::fs::set_permissions(&file_path, perms);
            }
            Ok(file_path)
        })();
        match outcome {
            Ok(file) => Ok((dir, file)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }

    /// Re-hash the downloaded installer right now and return its path only if it still matches
    /// the published SHA-256. Anything that opens or launches the file must go through this: the
    /// path was shown to the user, who may have moved or edited the file since the download.
    pub fn verified_installer(&self) -> Result<PathBuf, Error> {
        let (file, expected) = {
            let rt = self.lock();
            // Same rule as `status()`: an installer for a release that is no longer the cached one is stale.
            let cached = self.offer(&rt);
            let d = rt
                .downloaded
                .as_ref()
                .filter(|d| cached.is_some_and(|c| c.version == d.version))
                .ok_or(Error::NothingToDownload("no installer has been downloaded"))?;
            (d.file.clone(), d.sha256.clone())
        };
        if let Some(dir) = file.parent() {
            touch_dir(dir);
        }
        let mut f = std::fs::File::open(&file).map_err(|e| Error::Io(e.to_string()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0_u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf).map_err(|e| Error::Io(e.to_string()))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if bridge_core::ipc::encode_hex(&hasher.finalize()) == expected.to_ascii_lowercase() {
            Ok(file)
        } else {
            Err(Error::HashMismatch)
        }
    }

    /// Is an automatic check due? `first` is the startup check (shorter gap).
    fn check_due(&self, first: bool) -> bool {
        let rt = self.lock();
        let gap = if first {
            self.cfg.min_gap
        } else {
            self.cfg.interval
        };
        let now = now_unix();
        let last = rt
            .persisted
            .last_checked_unix
            .into_iter()
            .chain(rt.persisted.last_attempt_unix)
            .filter(|t| *t <= now) // a time ahead of the clock (clock set back) is ignored
            .max();
        rt.persisted.enabled && last.is_none_or(|t| now - t >= gap.as_secs())
    }

    fn scheduler_tick(&self, first: bool) {
        // Keep a downloaded installer looking recent: another instance's startup sweep removes
        // old directories, and an installer kept for days is still in use.
        if let Some(d) = self.lock().downloaded.as_ref() {
            touch_dir(&d.dir);
        }
        if self.check_due(first) {
            let _ = self.check(false);
            // A failed check retries at the next interval, not on every tick. This is remembered
            // apart from `last_checked_unix`, which only a successful answer may set: otherwise a
            // failure would make the status read "up to date".
            let mut rt = self.lock();
            if rt.error.is_some() {
                rt.persisted.last_attempt_unix = Some(now_unix());
                self.persist(&rt);
            }
        }
    }

    /// Background schedule: one check shortly after start (unless one just happened), then every
    /// `interval`. Stops by itself once the last `Arc` is gone.
    pub fn spawn_scheduler(self: &Arc<Self>) {
        let weak: Weak<Updater> = Arc::downgrade(self);
        let startup_delay = self.cfg.startup_delay;
        let tick = self.cfg.tick;
        std::thread::spawn(move || {
            std::thread::sleep(startup_delay);
            let mut first = true;
            loop {
                let Some(this) = weak.upgrade() else { return };
                this.scheduler_tick(first);
                first = false;
                drop(this);
                std::thread::sleep(tick);
            }
        });
    }
}

/// `update-state.json`: the updater's settings, ETag and last answer. Format 1 is the fields of
/// [`Persisted`] (older files have no `version` and count as 1). It is only a cache plus the
/// "automatic checks" switch, so a damaged file without backup just starts from the defaults.
pub static STATE_SPEC: bridge_core::statefile::Spec = bridge_core::statefile::Spec {
    name: "update-state.json",
    current: 1,
    migrations: &[],
    validate: None,
    on_corrupt: bridge_core::statefile::Corrupt::Reset,
};

fn state_file(path: &Path) -> bridge_core::statefile::StateFile<bridge_core::statefile::PlainIo> {
    bridge_core::statefile::StateFile::new(bridge_core::statefile::PlainIo, path, &STATE_SPEC)
}

fn load_persisted(path: &Path) -> Option<Persisted> {
    let loaded = state_file(path).load().ok()?;
    serde_json::from_value(serde_json::Value::Object(loaded.body)).ok()
}

const DIR_PREFIX: &str = "geekbridge-update-";
const STALE_DOWNLOAD_AGE: Duration = Duration::from_secs(3600);

fn download_root(cfg: &Config) -> PathBuf {
    cfg.download_root
        .clone()
        .or_else(user_state_dir)
        .unwrap_or_else(std::env::temp_dir)
}

fn user_state_dir() -> Option<PathBuf> {
    let env = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Application Support")
    } else {
        env("XDG_STATE_HOME").or_else(|| env("HOME").map(|h| h.join(".local/state")))?
    };
    Some(base.join("geekbridge").join("updates"))
}

/// Mark a download directory as in use (its modified time is what the sweep goes by).
fn touch_dir(dir: &Path) {
    if let Ok(f) = open_dir_for_times(dir) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// A handle on a directory through which its times can be set. Windows refuses to open a
/// directory as a file without `FILE_FLAG_BACKUP_SEMANTICS`, and setting times needs
/// `FILE_WRITE_ATTRIBUTES`; Unix opens directories read-only as usual.
fn open_dir_for_times(dir: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .access_mode(0x0100) // FILE_WRITE_ATTRIBUTES
            .share_mode(0x7) // read | write | delete: never block another instance
            .custom_flags(0x0200_0000) // FILE_FLAG_BACKUP_SEMANTICS
            .open(dir)
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open(dir)
    }
}

/// Remove `geekbridge-update-*` directories under `root` last modified more than `min_age` ago.
fn sweep_stale_downloads(root: &Path, min_age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(DIR_PREFIX) {
            continue;
        }
        // `symlink_metadata`: never follow a link out of the root.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= min_age);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn make_private_dir(root: &Path) -> Result<PathBuf, Error> {
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(root).map_err(|e| Error::Io(e.to_string()))?;
    }
    for _ in 0..8 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|_| Error::Io("no random source".into()))?;
        let dir = root.join(format!(
            "{DIR_PREFIX}{}",
            bridge_core::ipc::encode_hex(&random)
        ));
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(e.to_string())),
        }
    }
    Err(Error::Io(
        "cannot create a private download directory".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_removes_only_old_update_dirs() {
        let root = tempfile::tempdir().unwrap();
        let stale = root.path().join("geekbridge-update-deadbeef");
        let other = root.path().join("keep-me");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("installer.bin"), b"x").unwrap();
        std::fs::create_dir_all(&other).unwrap();

        // Fresh directories survive a one-hour threshold.
        sweep_stale_downloads(root.path(), Duration::from_secs(3600));
        assert!(stale.exists());

        sweep_stale_downloads(root.path(), Duration::ZERO);
        assert!(!stale.exists());
        assert!(other.exists(), "unrelated directories are never touched");
    }

    #[test]
    fn a_touched_download_directory_survives_the_sweep() {
        let root = tempfile::tempdir().unwrap();
        let used = root.path().join("geekbridge-update-used");
        let old = root.path().join("geekbridge-update-old");
        std::fs::create_dir_all(&used).unwrap();
        std::fs::create_dir_all(&old).unwrap();
        let ancient = SystemTime::now() - Duration::from_secs(7200);
        for d in [&used, &old] {
            open_dir_for_times(d)
                .unwrap()
                .set_modified(ancient)
                .unwrap();
        }
        // The other instance still uses `used`: it refreshes the directory.
        touch_dir(&used);
        sweep_stale_downloads(root.path(), STALE_DOWNLOAD_AGE);
        assert!(used.exists(), "a recently used installer must stay");
        assert!(!old.exists());
    }

    #[test]
    fn a_new_running_version_drops_the_cached_etag() {
        let up = Updater::new(Config::github("0.1.0")).unwrap();
        let cached = Cached {
            version: "0.2.0".into(),
            tag: "v0.2.0".into(),
            notes: String::new(),
            release_url: String::new(),
            asset: None,
            sha256: None,
            dev: false,
            dev_newer: false,
        };
        {
            let mut rt = up.lock();
            rt.persisted.release = Some(cached);
            rt.persisted.etag = Some("\"abc\"".into());
            // Prepared while 0.3.0 was running (a downgrade since): no asset was attached then.
            rt.persisted.checked_for = Some("0.3.0".into());
        }
        assert_eq!(up.begin_check(true).unwrap(), Some(None));
        up.lock().checking = false;
        up.lock().persisted.checked_for = Some("0.1.0".into());
        assert_eq!(up.begin_check(true).unwrap(), Some(Some("\"abc\"".into())));
    }

    #[test]
    fn busy_flags_clear_when_the_worker_panics() {
        let up = Updater::new(Config::github("0.1.0")).unwrap();
        up.lock().checking = true;
        up.lock().downloading = true;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _a = FlagGuard::new(&up, |rt| &mut rt.checking);
            let _b = FlagGuard::new(&up, |rt| &mut rt.downloading);
            panic!("worker died");
        }));
        assert!(r.is_err());
        assert!(!up.lock().checking && !up.lock().downloading);
    }

    #[test]
    fn a_failed_scheduled_check_does_not_claim_up_to_date() {
        let mut cfg = Config::github("0.1.0");
        cfg.base_url = "http://127.0.0.1:1".into(); // loopback, nothing listening
        cfg.io_timeout = Duration::from_millis(200);
        cfg.check_timeout = Duration::from_millis(200);
        let up = Updater::new(cfg).unwrap();
        up.scheduler_tick(true);
        let st = up.status();
        assert!(st.error.is_some(), "the failure is reported");
        assert_ne!(st.state, UpdateState::UpToDate);
        assert!(st.last_checked_unix.is_none());
        // ... and it is not retried on every tick.
        assert!(!up.check_due(false));
    }

    #[test]
    fn an_installer_for_a_superseded_release_is_not_handed_out() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("installer.bin");
        std::fs::write(&file, b"old").unwrap();
        let sha = bridge_core::ipc::encode_hex(&Sha256::digest(b"old"));
        let up = Updater::new(Config::github("0.1.0")).unwrap();
        let cached = |version: &str| Cached {
            version: version.into(),
            tag: format!("v{version}"),
            notes: String::new(),
            release_url: String::new(),
            asset: None,
            sha256: None,
            dev: false,
            dev_newer: false,
        };
        {
            let mut rt = up.lock();
            rt.persisted.release = Some(cached("0.2.0"));
            rt.downloaded = Some(Downloaded {
                dir: dir.path().to_path_buf(),
                file: file.clone(),
                version: "0.2.0".into(),
                sha256: sha,
            });
        }
        assert_eq!(up.verified_installer().unwrap(), file);
        // A newer release is cached: the old download is hidden from status and must not be openable either.
        up.lock().persisted.release = Some(cached("0.3.0"));
        assert!(matches!(
            up.verified_installer(),
            Err(Error::NothingToDownload(_))
        ));
    }

    #[test]
    fn a_last_check_time_in_the_future_does_not_freeze_or_hammer_checks() {
        let up = Updater::new(Config::github("0.1.0")).unwrap();
        // The wall clock was set back after a check: the stored time is a year ahead.
        let future = now_unix() + 365 * 24 * 3600;
        {
            let mut rt = up.lock();
            rt.persisted.last_checked_unix = Some(future);
            rt.persisted.release = None;
        }
        // Never checked "recently": the scheduler must be due, and a manual check must hit the network.
        assert!(
            up.check_due(false),
            "a future timestamp must not postpone checks"
        );
        assert!(matches!(up.begin_check(true), Ok(Some(_))));
        up.lock().checking = false;
        // After a failed attempt the retry is spaced by the interval, not made on every tick.
        up.lock().persisted.last_attempt_unix = Some(now_unix());
        assert!(
            !up.check_due(false),
            "a failed attempt must not be retried every tick"
        );
    }

    #[test]
    fn a_rate_limit_pause_is_persisted_and_a_wild_one_is_ignored() {
        // The pause and the last failed attempt are saved fields, so a restarted service honours them.
        let saved = Persisted {
            backoff_until: Some(123),
            last_attempt_unix: Some(45),
            ..Persisted::default()
        };
        let back: Persisted =
            serde_json::from_value(serde_json::to_value(&saved).unwrap()).unwrap();
        assert_eq!((back.backoff_until, back.last_attempt_unix), (Some(123), Some(45)));
        // An older state file has neither field.
        let old: Persisted = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert_eq!((old.backoff_until, old.last_attempt_unix), (None, None));

        let up = Updater::new(Config::github("0.1.0")).unwrap();
        up.lock().persisted.backoff_until = Some(now_unix() + 600);
        assert!(matches!(
            up.begin_check(true),
            Err(Error::RateLimited { .. })
        ));
        // A stored pause more than a day ahead (clock set forward, then back) must not freeze checks.
        up.lock().persisted.backoff_until = Some(now_unix() + 3 * MAX_BACKOFF_SECS);
        assert!(matches!(up.begin_check(true), Ok(Some(_))));
    }

    #[test]
    fn default_download_root_is_not_the_shared_temp_dir() {
        let cfg = Config::github("0.1.0");
        let root = download_root(&cfg);
        if let Some(dir) = user_state_dir() {
            assert_eq!(root, dir);
        }
        assert!(root.is_absolute());
    }
}
