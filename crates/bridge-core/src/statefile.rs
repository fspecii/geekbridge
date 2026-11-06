// SPDX-License-Identifier: GPL-3.0-or-later
//! Versioned, crash-safe JSON state files. One implementation for every persisted JSON document
//! (see `docs/state-formats.md` for the policy this enforces).
//!
//! * Every document is a JSON object with an integer `version`. A file without one is version 1.
//! * [`Spec::migrations`] holds the upgrade steps in order (`migrations[i]` turns version `i + 1`
//!   into `i + 2`); [`StateFile::load`] is the single entry point that applies them.
//! * Fields a reader does not know are kept in the loaded object and written back untouched, so a
//!   newer additive field survives a round trip through an older build.
//! * A file with a *newer* version than this build knows is never rewritten ([`Status::Newer`],
//!   [`Error::ReadOnly`]); callers run degraded and say so.
//! * Writes go through [`StateIo::write_atomic`] (temp file, fsync, rename) under a cross-process
//!   lock, after the previous good file was copied to `<file>.bak` (one generation).
//! * A damaged file falls back to `.bak`; with no usable backup the [`Corrupt`] policy decides
//!   between an error and a fresh state. A damaged file is copied to `<file>.corrupt` before it is
//!   replaced. Nothing here panics on bad input.

use serde_json::{Map, Value};
use std::{
    fmt::{self, Display},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Largest document read back; larger files count as damaged.
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;

pub type Migration = fn(&mut Map<String, Value>) -> Result<(), String>;
pub type Validator = fn(&Map<String, Value>) -> Result<(), String>;

/// What to do when neither the file nor its backup can be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corrupt {
    /// Report an error; the file stays where it is.
    Fail,
    /// Continue with a fresh (empty) document and a diagnostic. The damaged file is preserved as
    /// `<file>.corrupt` as soon as something is written.
    Reset,
}

/// Static description of one document type.
pub struct Spec {
    pub name: &'static str,
    /// Version this build writes.
    pub current: u32,
    /// `migrations[i]` upgrades version `i + 1` to `i + 2`; length must be `current - 1`.
    pub migrations: &'static [Migration],
    /// Semantic check on the migrated document; an error marks the file damaged.
    pub validate: Option<Validator>,
    pub on_corrupt: Corrupt,
}

/// Storage backend. `bridge-kde` provides an owner-only one, [`PlainIo`] is the plain-std one.
pub trait StateIo {
    type Err: Display;
    /// `Ok(None)` when the file does not exist.
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, Self::Err>;
    /// Replace `path` atomically and durably.
    fn write_atomic(&self, path: &Path, data: &[u8]) -> Result<(), Self::Err>;
    /// Exclusive cross-process lock on `lock_path`, released when the guard drops.
    fn lock(&self, lock_path: &Path) -> Result<Box<dyn Send>, Self::Err>;
    /// Can this read failure be treated like a damaged file (fall back to the backup)? Security
    /// refusals (loose permissions, symlinks) must say no.
    fn is_recoverable(&self, err: &Self::Err) -> bool;
}

#[derive(Debug)]
pub enum Error<E> {
    Io(E),
    /// The file is damaged and the [`Corrupt::Fail`] policy applies.
    Corrupt {
        path: PathBuf,
        reason: String,
    },
    /// The file was written by a newer build; it is kept as is and may not be written.
    ReadOnly {
        path: PathBuf,
        found: u32,
        known: u32,
    },
}

impl<E: Display> Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Corrupt { path, reason } => {
                write!(f, "{} is damaged: {reason}", path.display())
            }
            Self::ReadOnly { path, found, known } => write!(
                f,
                "{} was written by a newer version (format {found}, this build knows {known}); \
                 it is left untouched and changes are not saved until you update",
                path.display()
            ),
        }
    }
}
impl<E: fmt::Debug + Display> std::error::Error for Error<E> {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// No file yet.
    Missing,
    /// Read as is.
    Current,
    /// Read and upgraded in memory from this version (written on the next save).
    Migrated { from: u32 },
    /// The file was damaged; the backup was used.
    Recovered { reason: String },
    /// Neither file nor backup was usable; started empty.
    Reset { reason: String },
    /// A newer build wrote the file. `body` holds its raw object for read-only use.
    Newer { found: u32 },
}

#[derive(Clone, Debug)]
pub struct Loaded {
    /// The document at [`Spec::current`] (when `status` is not `Newer`), unknown fields included.
    pub body: Map<String, Value>,
    pub status: Status,
    /// Bytes of the file the document came from (for change detection).
    pub raw: Option<Vec<u8>>,
}

impl Loaded {
    pub fn read_only(&self) -> bool {
        matches!(self.status, Status::Newer { .. })
    }

    /// A one-line message for anything out of the ordinary, `None` when the file was fine.
    pub fn diagnostic(&self, name: &str) -> Option<String> {
        match &self.status {
            Status::Missing | Status::Current => None,
            Status::Migrated { from } => Some(format!(
                "{name}: upgraded from format {from} (a copy of the old file is kept as {name}.bak)"
            )),
            Status::Recovered { reason } => Some(format!(
                "{name}: file was damaged ({reason}); restored from {name}.bak"
            )),
            Status::Reset { reason } => Some(format!(
                "{name}: file was damaged ({reason}) and no backup was usable; started empty, \
                 the damaged file is kept as {name}.corrupt"
            )),
            Status::Newer { found } => Some(format!(
                "{name}: file is format {found}, newer than this build understands; \
                 running read-only and leaving it untouched"
            )),
        }
    }
}

static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
const MAX_NOTES: usize = 32;

/// Record a diagnostics line (also printed to stderr once). Used for anything a user may need to
/// know after an upgrade, downgrade or recovery; shown by the service's diagnostics.
pub fn note(line: String) {
    let mut g = NOTES.lock().unwrap_or_else(|e| e.into_inner());
    if g.contains(&line) {
        return;
    }
    eprintln!("GeekBridge: {line}");
    if g.len() >= MAX_NOTES {
        g.remove(0);
    }
    g.push(line);
}

/// Everything [`note`] recorded in this process.
pub fn notes() -> Vec<String> {
    NOTES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn backup_path(path: &Path) -> PathBuf {
    with_suffix(path, ".bak")
}
pub fn corrupt_path(path: &Path) -> PathBuf {
    with_suffix(path, ".corrupt")
}
pub fn lock_path(path: &Path) -> PathBuf {
    with_suffix(path, ".lock")
}

enum Interpreted {
    Ok {
        body: Map<String, Value>,
        from: u32,
    },
    Newer {
        body: Map<String, Value>,
        found: u32,
    },
}

fn interpret(spec: &Spec, bytes: &[u8]) -> Result<Interpreted, String> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err("empty file".into());
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("unreadable JSON ({e})"))?;
    let Value::Object(mut body) = value else {
        return Err("not a JSON object".into());
    };
    let found = match body.get("version") {
        None => 1,
        // Beyond u32 is a far-future format, not damage.
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or("invalid version field")?
            .min(u64::from(u32::MAX)) as u32,
    };
    if found > spec.current {
        return Ok(Interpreted::Newer { body, found });
    }
    debug_assert_eq!(spec.migrations.len() as u32 + 1, spec.current);
    for v in found..spec.current {
        let step = spec
            .migrations
            .get(v as usize - 1)
            .ok_or_else(|| format!("no migration from format {v}"))?;
        step(&mut body).map_err(|e| format!("migration from format {v} failed: {e}"))?;
    }
    body.insert("version".into(), Value::from(spec.current));
    if let Some(validate) = spec.validate {
        validate(&body)?;
    }
    Ok(Interpreted::Ok { body, from: found })
}

pub struct StateFile<I: StateIo> {
    io: I,
    path: PathBuf,
    spec: &'static Spec,
}

impl<I: StateIo> StateFile<I> {
    pub fn new(io: I, path: impl Into<PathBuf>, spec: &'static Spec) -> Self {
        Self {
            io,
            path: path.into(),
            spec,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn spec(&self) -> &'static Spec {
        self.spec
    }

    fn name(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.spec.name.to_owned())
    }

    /// The single read entry point: reads, migrates in memory, recovers from the backup.
    pub fn load(&self) -> Result<Loaded, Error<I::Err>> {
        let loaded = self.load_inner()?;
        if let Some(line) = loaded.diagnostic(&self.name()) {
            note(line);
        }
        Ok(loaded)
    }

    fn load_inner(&self) -> Result<Loaded, Error<I::Err>> {
        let reason = match self.io.read(&self.path) {
            Ok(None) => {
                return Ok(Loaded {
                    body: Map::new(),
                    status: Status::Missing,
                    raw: None,
                });
            }
            Ok(Some(bytes)) => match interpret(self.spec, &bytes) {
                Ok(Interpreted::Ok { body, from }) => {
                    let status = if from == self.spec.current {
                        Status::Current
                    } else {
                        Status::Migrated { from }
                    };
                    return Ok(Loaded {
                        body,
                        status,
                        raw: Some(bytes),
                    });
                }
                Ok(Interpreted::Newer { body, found }) => {
                    return Ok(Loaded {
                        body,
                        status: Status::Newer { found },
                        raw: Some(bytes),
                    });
                }
                Err(reason) => reason,
            },
            Err(e) if self.io.is_recoverable(&e) => format!("unreadable ({e})"),
            Err(e) => return Err(Error::Io(e)),
        };
        // Damaged: the backup, if it is a good file of a format we know.
        if let Ok(Some(bytes)) = self.io.read(&backup_path(&self.path))
            && let Ok(Interpreted::Ok { body, .. }) = interpret(self.spec, &bytes)
        {
            return Ok(Loaded {
                body,
                status: Status::Recovered { reason },
                raw: None,
            });
        }
        match self.spec.on_corrupt {
            Corrupt::Fail => Err(Error::Corrupt {
                path: self.path.clone(),
                reason,
            }),
            Corrupt::Reset => Ok(Loaded {
                body: Map::new(),
                status: Status::Reset { reason },
                raw: None,
            }),
        }
    }

    /// Take the cross-process write lock.
    pub fn lock(&self) -> Result<Locked<'_, I>, Error<I::Err>> {
        let guard = self.io.lock(&lock_path(&self.path)).map_err(Error::Io)?;
        Ok(Locked {
            file: self,
            _guard: guard,
        })
    }

    /// Read-modify-write under the lock: `f` edits the freshly loaded document (unknown fields
    /// included) and returns whether to save. Returns what was loaded (before `f`).
    pub fn update(
        &self,
        f: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<Loaded, Error<I::Err>> {
        let locked = self.lock()?;
        let loaded = locked.load()?;
        if loaded.read_only() {
            return Err(self.read_only_error(&loaded));
        }
        let mut body = loaded.body.clone();
        if f(&mut body) {
            locked.save(&body)?;
        }
        Ok(loaded)
    }

    fn read_only_error(&self, loaded: &Loaded) -> Error<I::Err> {
        Error::ReadOnly {
            path: self.path.clone(),
            found: match loaded.status {
                Status::Newer { found } => found,
                _ => 0,
            },
            known: self.spec.current,
        }
    }
}

/// A [`StateFile`] with its write lock held.
pub struct Locked<'a, I: StateIo> {
    file: &'a StateFile<I>,
    _guard: Box<dyn Send>,
}

impl<I: StateIo> Locked<'_, I> {
    pub fn load(&self) -> Result<Loaded, Error<I::Err>> {
        self.file.load()
    }

    /// Write `body` as the new document. Refuses (and leaves everything alone) when the file on
    /// disk is from a newer build. The previous good file becomes `.bak` first; a damaged one is
    /// kept as `.corrupt` and never replaces the backup.
    pub fn save(&self, body: &Map<String, Value>) -> Result<(), Error<I::Err>> {
        let f = self.file;
        let io = &f.io;
        match io.read(&f.path) {
            Ok(Some(raw)) => match interpret(f.spec, &raw) {
                Ok(Interpreted::Newer { found, .. }) => {
                    return Err(Error::ReadOnly {
                        path: f.path.clone(),
                        found,
                        known: f.spec.current,
                    });
                }
                Ok(Interpreted::Ok { .. }) => {
                    io.write_atomic(&backup_path(&f.path), &raw)
                        .map_err(Error::Io)?;
                }
                Err(_) => {
                    // Keep the evidence; never let a damaged file push out the good backup.
                    let _ = io.write_atomic(&corrupt_path(&f.path), &raw);
                }
            },
            Ok(None) => {}
            Err(e) if io.is_recoverable(&e) => {}
            Err(e) => return Err(Error::Io(e)),
        }
        let mut out = body.clone();
        out.insert("version".into(), Value::from(f.spec.current));
        let bytes = serde_json::to_vec_pretty(&out).map_err(|_| Error::Corrupt {
            path: f.path.clone(),
            reason: "document cannot be serialised".into(),
        })?;
        io.write_atomic(&f.path, &bytes).map_err(Error::Io)
    }
}

impl<I: StateIo> Locked<'_, I> {
    /// [`Locked::save`], then make the backup the same as the new file. For changes that must not
    /// be undone by a fall-back to an older generation (a revoked trust record).
    pub fn save_and_refresh_backup(&self, body: &Map<String, Value>) -> Result<(), Error<I::Err>> {
        self.save(body)?;
        let f = self.file;
        if let Some(raw) = f.io.read(&f.path).map_err(Error::Io)? {
            f.io.write_atomic(&backup_path(&f.path), &raw)
                .map_err(Error::Io)?;
        }
        Ok(())
    }
}

/// Plain-std backend for non-secret files: no permission checks and no cross-process lock (use it
/// only for files with a single writer process).
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainIo;

impl StateIo for PlainIo {
    type Err = io::Error;

    fn read(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let f = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut buf = Vec::new();
        f.take(MAX_STATE_BYTES + 1).read_to_end(&mut buf)?;
        if buf.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file is too large",
            ));
        }
        Ok(Some(buf))
    }

    fn write_atomic(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        write_atomic_plain(path, data)
    }

    fn lock(&self, _: &Path) -> io::Result<Box<dyn Send>> {
        Ok(Box::new(()))
    }

    fn is_recoverable(&self, _: &io::Error) -> bool {
        true
    }
}

/// Temp file in the same directory, fsync, rename, fsync of the directory.
pub fn write_atomic_plain(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad file name"))?;
    let tmp = dir.join(format!(".{name}.tmp.{}", std::process::id()));
    let result = (|| {
        // These files often sit next to owner-only secrets in the same private state directory
        // (identity, control token, trust store); the process umask alone must not decide whether
        // they end up world- or group-readable.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        slow_disk_hook();
        failpoint("before_rename");
        std::fs::rename(&tmp, path)?;
        #[cfg(unix)]
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Test hook: with `GB_FAILPOINT=<name>` set the process aborts at that point, simulating a crash.
/// Compiled out unless the `failpoints` feature is on.
#[cfg(feature = "failpoints")]
pub fn failpoint(name: &str) {
    if std::env::var("GB_FAILPOINT").is_ok_and(|v| v == name) {
        std::process::abort();
    }
}
#[cfg(not(feature = "failpoints"))]
#[inline(always)]
pub fn failpoint(_name: &str) {}

static WRITE_DELAY_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Test hook: make every durable state-file write sleep this long before its rename, as a saturated
/// disk would (soak 5 saw fsyncs take 35 s). Zero, the default, costs one relaxed load per write.
/// Not behind a feature: a dev-dependency feature would leak into `cargo build --release --example`.
pub fn set_write_delay_ms(ms: u32) {
    WRITE_DELAY_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// Called by every atomic writer between the fsync of the data and the rename.
pub fn slow_disk_hook() {
    let ms = WRITE_DELAY_MS.load(std::sync::atomic::Ordering::Relaxed);
    if ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V1: Spec = Spec {
        name: "t.json",
        current: 1,
        migrations: &[],
        validate: None,
        on_corrupt: Corrupt::Reset,
    };

    fn add_c(b: &mut Map<String, Value>) -> Result<(), String> {
        let a = b.get("a").and_then(Value::as_u64).ok_or("no a")?;
        b.insert("c".into(), Value::from(a * 2));
        Ok(())
    }
    // A fake future format: v2 derives `c` from `a`.
    static V2: Spec = Spec {
        name: "t.json",
        current: 2,
        migrations: &[add_c],
        validate: None,
        on_corrupt: Corrupt::Fail,
    };

    fn sf(dir: &Path, spec: &'static Spec) -> StateFile<PlainIo> {
        StateFile::new(PlainIo, dir.join("t.json"), spec)
    }

    #[test]
    fn missing_then_roundtrip_keeps_unknown_fields() {
        let d = tempfile::tempdir().unwrap();
        let s = sf(d.path(), &V1);
        assert_eq!(s.load().unwrap().status, Status::Missing);
        std::fs::write(s.path(), br#"{"a":1,"future":{"x":[1,2]}}"#).unwrap();
        let l = s.load().unwrap();
        assert_eq!(l.status, Status::Current, "no version means 1");
        s.update(|b| {
            b.insert("a".into(), json!(5));
            true
        })
        .unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(s.path()).unwrap()).unwrap();
        assert_eq!(v, json!({"version":1,"a":5,"future":{"x":[1,2]}}));
        assert!(backup_path(s.path()).exists());
    }

    /// `PlainIo` is used for files that sit next to owner-only secrets in the same private state
    /// directory (e.g. update-checker and "what's new" state next to the control token and trust
    /// store). They should not be left group/other-readable by the process umask.
    #[cfg(unix)]
    #[test]
    fn plain_io_writes_owner_only_files() {
        use std::os::unix::fs::PermissionsExt;

        let d = tempfile::tempdir().unwrap();
        let s = sf(d.path(), &V1);
        s.update(|b| {
            b.insert("a".into(), json!(1));
            true
        })
        .unwrap();
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state file should be owner-only, got {mode:o}");
    }

    #[test]
    fn migrations_run_in_order_and_back_up_the_old_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("t.json"), br#"{"version":1,"a":4}"#).unwrap();
        let s = sf(d.path(), &V2);
        let l = s.load().unwrap();
        assert_eq!(l.status, Status::Migrated { from: 1 });
        assert_eq!(l.body["c"], json!(8));
        assert_eq!(l.body["version"], json!(2));
        s.update(|_| true).unwrap();
        let bak: Value =
            serde_json::from_slice(&std::fs::read(backup_path(s.path())).unwrap()).unwrap();
        assert_eq!(bak["version"], json!(1), "rollback copy is the old format");
    }

    #[test]
    fn newer_file_is_never_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let text = br#"{"version":9,"a":1,"z":true}"#;
        std::fs::write(d.path().join("t.json"), text).unwrap();
        let s = sf(d.path(), &V1);
        let l = s.load().unwrap();
        assert!(l.read_only());
        assert!(matches!(
            s.update(|b| {
                b.clear();
                true
            }),
            Err(Error::ReadOnly { found: 9, .. })
        ));
        assert_eq!(std::fs::read(s.path()).unwrap(), text);
        assert!(!backup_path(s.path()).exists());
    }

    #[test]
    fn damaged_files_fall_back_to_backup_then_policy() {
        let d = tempfile::tempdir().unwrap();
        let s = sf(d.path(), &V1);
        s.update(|b| {
            b.insert("a".into(), json!(1));
            true
        })
        .unwrap();
        s.update(|b| {
            b.insert("a".into(), json!(2));
            true
        })
        .unwrap();
        for bad in [&b""[..], b"{", b"[1]", b"\0\0\0", br#"{"version":"x"}"#] {
            std::fs::write(s.path(), bad).unwrap();
            let l = s.load().unwrap();
            assert!(matches!(l.status, Status::Recovered { .. }), "{bad:?}");
            assert_eq!(l.body["a"], json!(1), "one generation back");
        }
        // Saving from the recovered state keeps the damaged file and the good backup.
        std::fs::write(s.path(), b"{").unwrap();
        s.update(|b| {
            b.insert("a".into(), json!(3));
            true
        })
        .unwrap();
        assert_eq!(std::fs::read(corrupt_path(s.path())).unwrap(), b"{");
        assert_eq!(s.load().unwrap().body["a"], json!(3));
        // No backup: Reset policy starts fresh, Fail policy errors.
        let d2 = tempfile::tempdir().unwrap();
        std::fs::write(d2.path().join("t.json"), b"{").unwrap();
        assert!(matches!(
            sf(d2.path(), &V1).load().unwrap().status,
            Status::Reset { .. }
        ));
        assert!(matches!(
            sf(d2.path(), &V2).load(),
            Err(Error::Corrupt { .. })
        ));
        assert_eq!(std::fs::read(d2.path().join("t.json")).unwrap(), b"{");
    }

    #[test]
    fn a_version_beyond_u32_is_newer_not_damaged() {
        let d = tempfile::tempdir().unwrap();
        let text = br#"{"version":99999999999,"a":1}"#;
        std::fs::write(d.path().join("t.json"), text).unwrap();
        let s = sf(d.path(), &V1);
        assert!(s.load().unwrap().read_only());
        assert!(matches!(s.update(|_| true), Err(Error::ReadOnly { .. })));
        assert_eq!(std::fs::read(s.path()).unwrap(), text);
    }

    #[test]
    fn failing_migration_counts_as_damage_and_leaves_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("t.json"), br#"{"version":1}"#).unwrap();
        assert!(matches!(
            sf(d.path(), &V2).load(),
            Err(Error::Corrupt { .. })
        ));
        assert_eq!(
            std::fs::read(d.path().join("t.json")).unwrap(),
            br#"{"version":1}"#
        );
    }

    #[test]
    fn stale_temp_file_does_not_disturb_the_real_one() {
        let d = tempfile::tempdir().unwrap();
        let s = sf(d.path(), &V1);
        s.update(|b| {
            b.insert("a".into(), json!(1));
            true
        })
        .unwrap();
        std::fs::write(d.path().join(".t.json.tmp.99999"), b"{\"a\":").unwrap();
        assert_eq!(s.load().unwrap().body["a"], json!(1));
        s.update(|b| {
            b.insert("a".into(), json!(2));
            true
        })
        .unwrap();
        assert_eq!(s.load().unwrap().body["a"], json!(2));
    }
}
