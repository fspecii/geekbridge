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

mod http;
mod release;
mod version;

pub use release::{Asset, Platform, Release, select_asset};
pub use version::Version;

use bridge_core::ipc::{UpdateRelease, UpdateState, UpdateStatus};
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

pub const REPO: &str = "fspecii/geekbridge";
const API_BASE: &str = "https://api.github.com";
const MAX_API_BYTES: u64 = 1024 * 1024;
const MAX_SUMS_BYTES: u64 = 64 * 1024;
const MAX_STATE_BYTES: u64 = 256 * 1024;

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

#[derive(Clone, Debug)]
pub struct Config {
    /// `https://api.github.com`; tests point this at a loopback mock server.
    pub base_url: String,
    pub repo: String,
    pub current_version: String,
    pub platform: Platform,
    /// Where settings, the ETag and the last answer persist. `None` keeps everything in memory.
    pub state_file: Option<PathBuf>,
    /// Parent of the private per-download directory. Defaults to the OS temp directory.
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
}

impl Config {
    /// The real thing: GitHub over https for the running build.
    pub fn github(current_version: &str) -> Config {
        Config {
            base_url: API_BASE.to_owned(),
            repo: REPO.to_owned(),
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
        }
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
}

#[derive(Debug, Serialize, Deserialize)]
struct Persisted {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    skipped_version: Option<String>,
    #[serde(default)]
    etag: Option<String>,
    #[serde(default)]
    last_checked_unix: Option<u64>,
    #[serde(default)]
    release: Option<Cached>,
}

fn yes() -> bool {
    true
}

impl Default for Persisted {
    fn default() -> Self {
        Persisted {
            enabled: true,
            skipped_version: None,
            etag: None,
            last_checked_unix: None,
            release: None,
        }
    }
}

struct Downloaded {
    dir: PathBuf,
    file: PathBuf,
    version: String,
}

#[derive(Default)]
struct Runtime {
    persisted: Persisted,
    checking: bool,
    downloading: bool,
    error: Option<String>,
    downloaded: Option<Downloaded>,
    progress: (u64, u64),
    backoff_until: Option<u64>,
}

enum Fetch {
    NotModified,
    NoRelease,
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
        let client = http::Client::new(
            &format!("geekbridge-updater/{}", cfg.current_version),
            policy,
            cfg.io_timeout,
        )?;
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
        if let Some(path) = &self.cfg.state_file {
            let Ok(json) = serde_json::to_vec_pretty(&rt.persisted) else {
                return;
            };
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, path).is_err() {
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    pub fn status(&self) -> UpdateStatus {
        let rt = self.lock();
        let cached = rt.persisted.release.as_ref();
        let newer = cached
            .and_then(|c| Version::parse(&c.version))
            .is_some_and(|v| v > self.current);
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
        } else if rt.persisted.last_checked_unix.is_some() {
            UpdateState::UpToDate
        } else {
            UpdateState::Idle
        };
        UpdateStatus {
            enabled: rt.persisted.enabled,
            current_version: self.cfg.current_version.clone(),
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

    pub fn set_enabled(&self, enabled: bool) {
        let mut rt = self.lock();
        rt.persisted.enabled = enabled;
        self.persist(&rt);
    }

    /// Hide the offer for `version`; a later release is offered again.
    pub fn skip(&self, version: &str) -> Result<(), Error> {
        let v = Version::parse(version)
            .ok_or_else(|| Error::Malformed("not a version number".into()))?;
        let mut rt = self.lock();
        rt.persisted.skipped_version = Some(v.to_string());
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
        if let Some(until) = rt.backoff_until.filter(|u| *u > now) {
            let err = Error::RateLimited {
                retry_after_secs: until - now,
            };
            rt.error = Some(err.to_string());
            return Err(err);
        }
        if manual
            && rt.error.is_none()
            && rt
                .persisted
                .last_checked_unix
                .is_some_and(|t| now.saturating_sub(t) < self.cfg.manual_cooldown.as_secs())
        {
            return Ok(None);
        }
        rt.checking = true;
        rt.error = None;
        let etag = rt
            .persisted
            .release
            .as_ref()
            .and_then(|_| rt.persisted.etag.clone());
        Ok(Some(etag))
    }

    fn finish_check(&self, etag: Option<&str>) -> Result<(), Error> {
        let result = self.fetch_latest(etag);
        let mut rt = self.lock();
        rt.checking = false;
        let now = now_unix();
        match result {
            Ok(Fetch::NotModified) => rt.persisted.last_checked_unix = Some(now),
            Ok(Fetch::NoRelease) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.release = None;
                rt.persisted.etag = None;
            }
            Ok(Fetch::Fresh { etag, cached }) => {
                rt.persisted.last_checked_unix = Some(now);
                rt.persisted.etag = etag;
                rt.persisted.release = Some(cached);
            }
            Err(e) => {
                if let Error::RateLimited { retry_after_secs } = &e {
                    rt.backoff_until = Some(now + (*retry_after_secs).clamp(60, 24 * 3600));
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
            404 => return Ok(Fetch::NoRelease),
            _ if fetched.rate_limited => {
                return Err(Error::RateLimited {
                    retry_after_secs: fetched.retry_after.unwrap_or(3600),
                });
            }
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
        let policy = &self.client.policy;
        let release_url = if release::url_allowed(policy, &release.html_url) {
            release.html_url.clone()
        } else {
            format!(
                "https://github.com/{}/releases/tag/{}",
                self.cfg.repo, release.tag_name
            )
        };
        let mut cached = Cached {
            version: version.to_string(),
            tag: release.tag_name.clone(),
            notes: clean_notes(release.body.as_deref().unwrap_or("")),
            release_url: if release_url.len() <= 512 {
                release_url
            } else {
                String::new()
            },
            asset: None,
            sha256: None,
        };
        if version > self.current {
            cached.asset = select_asset(&release.assets, &self.cfg.platform)
                .filter(|a| release::url_allowed(policy, &a.browser_download_url))
                .cloned();
            if let (Some(asset), Some(sums)) = (&cached.asset, find_sums_asset(&release.assets))
                && release::url_allowed(policy, &sums.browser_download_url)
            {
                let (f, text) = self.get_bytes(
                    &sums.browser_download_url,
                    "application/octet-stream",
                    MAX_SUMS_BYTES,
                    None,
                )?;
                if f.status == 200 {
                    cached.sha256 = std::str::from_utf8(&text)
                        .ok()
                        .and_then(|t| find_checksum(t, &asset.name));
                }
            }
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
        let cached = rt
            .persisted
            .release
            .clone()
            .filter(|c| Version::parse(&c.version).is_some_and(|v| v > self.current))
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
        let result = self.download_verified(&asset, &sha);
        let mut rt = self.lock();
        rt.downloading = false;
        match result {
            Ok((dir, file)) => {
                rt.downloaded = Some(Downloaded {
                    dir,
                    file: file.clone(),
                    version: cached.version,
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
        let root = self
            .cfg
            .download_root
            .clone()
            .unwrap_or_else(std::env::temp_dir);
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
                &asset.browser_download_url,
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
                let due = {
                    let rt = this.lock();
                    let gap = if first {
                        this.cfg.min_gap
                    } else {
                        this.cfg.interval
                    };
                    rt.persisted.enabled
                        && rt
                            .persisted
                            .last_checked_unix
                            .is_none_or(|t| now_unix().saturating_sub(t) >= gap.as_secs())
                };
                if due {
                    let _ = this.check(false);
                    // A failed check retries at the next interval, not on every tick.
                    let mut rt = this.lock();
                    if rt.error.is_some() {
                        rt.persisted.last_checked_unix = Some(now_unix());
                    }
                }
                first = false;
                drop(this);
                std::thread::sleep(tick);
            }
        });
    }
}

fn load_persisted(path: &Path) -> Option<Persisted> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_STATE_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn make_private_dir(root: &Path) -> Result<PathBuf, Error> {
    for _ in 0..8 {
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|_| Error::Io("no random source".into()))?;
        let dir = root.join(format!(
            "geekbridge-update-{}",
            bridge_core::ipc::encode_hex(&random)
        ));
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
