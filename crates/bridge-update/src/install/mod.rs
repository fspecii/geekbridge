// SPDX-License-Identifier: GPL-3.0-or-later
//! "Install update" for the per-user Linux install (`install.sh` into `~/.local`).
//!
//! The design, threat model and failure analysis are in `docs/design/desktop-self-update.md`.
//! In short: the verified archive is unpacked into a staging directory inside the prefix, checked
//! as a whole, renamed to `lib/geekbridge/versions/<version>`, and made live by one atomic
//! `rename` of the `current` symlink. `bin/<name>` are symlinks to `current/bin/<name>`, the
//! previous version stays for `rollback`, and nothing restarts unless the caller asks and no call
//! is in progress. Package-manager installs and other systems are never modified.
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod gz;
#[cfg(target_os = "linux")]
mod tar;
#[cfg(all(test, target_os = "linux"))]
mod tests;

/// Executables of a release, in `scripts/release-binaries.txt` order.
pub const BINARIES: [&str; 9] = [
    "geekbridge",
    "geekbridge-service",
    "geekbridge-ctl",
    "geekbridge-calls",
    "geekbridge-messages",
    "geekbridge-clipboard",
    "geekbridge-agenda",
    "geekbridge-contacts",
    "geekbridge-share",
];
#[cfg(target_os = "linux")]
const DOCS: [&str; 3] = ["README.md", "LICENSE", "THIRD_PARTY_NOTICES.md"];
#[cfg(target_os = "linux")]
const SERVICE_UNIT: &str = "geekbridge-service.service";

/// What this copy of GeekBridge may do about an update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Support {
    /// Installed by `install.sh` under this prefix, which the user owns: `update install` works.
    UserInstall(PathBuf),
    /// Installed by a package manager or for all users: never modified from here.
    PackageManaged(String),
    /// Download and verify only; the user installs by hand (Windows, macOS, a build folder).
    DownloadOnly(String),
}

impl Support {
    pub fn can_install(&self) -> bool {
        matches!(self, Support::UserInstall(_))
    }

    /// One plain sentence for the Settings card and `geekbridge-ctl update status`.
    pub fn message(&self) -> String {
        match self {
            Support::UserInstall(p) => format!(
                "GeekBridge can install updates itself under {}.",
                p.display()
            ),
            Support::PackageManaged(why) => format!(
                "Update with your package manager. GeekBridge does not modify this installation ({why})."
            ),
            Support::DownloadOnly(why) => format!(
                "Download only: GeekBridge downloads and verifies the update, you install it yourself ({why})."
            ),
        }
    }
}

/// Where the running executable says GeekBridge is installed: `<prefix>/bin/<name>` (before the
/// first self-update) or `<prefix>/lib/geekbridge/versions/<v>/bin/<name>` (after it).
pub fn prefix_of_exe(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let dir = bin.parent()?;
    let named = |p: &Path, n: &str| p.file_name().is_some_and(|f| f == n);
    if let Some(versions) = dir.parent()
        && named(versions, "versions")
        && let Some(gb) = versions.parent()
        && named(gb, "geekbridge")
        && let Some(lib) = gb.parent()
        && named(lib, "lib")
    {
        return lib.parent().map(Path::to_path_buf);
    }
    Some(dir.to_path_buf())
}

/// Support for the running executable. `GEEKBRIDGE_HOME` is not consulted: `$HOME` decides what
/// "the user's own" means.
pub fn support_here() -> Support {
    #[cfg(target_os = "linux")]
    {
        let Ok(exe) = std::env::current_exe() else {
            return Support::DownloadOnly("cannot tell where GeekBridge is installed".into());
        };
        let text = exe.to_string_lossy();
        let exe = PathBuf::from(text.strip_suffix(" (deleted)").unwrap_or(&text));
        match prefix_of_exe(&exe) {
            Some(prefix) => support_for(
                &prefix,
                std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            ),
            None => Support::DownloadOnly(
                "this copy does not run from an installed bin directory".into(),
            ),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Support::DownloadOnly(format!(
            "self-update is built for Linux only; this is {}",
            std::env::consts::OS
        ))
    }
}

/// Support for installing under `prefix`. `home`: the prefix must lie inside it (`None` skips the
/// check, for an explicit `--prefix`).
pub fn support_for(prefix: &Path, home: Option<&Path>) -> Support {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (prefix, home);
        Support::DownloadOnly("self-update is built for Linux only".into())
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if prefix.join("share/geekbridge/package-managed").exists() {
            return Support::PackageManaged("the package marks its files as its own".into());
        }
        let real = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
        if let Some(home) = home
            && !home.as_os_str().is_empty()
            && !real.starts_with(std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()))
        {
            return Support::PackageManaged(format!(
                "{} is outside your home directory",
                prefix.display()
            ));
        }
        let euid = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u32>().ok())
            });
        if euid == Some(0) {
            // Root would replace files other people run; a per-user install is the user's own business.
            return Support::DownloadOnly(
                "self-update does not run as root; use your own account or the package manager"
                    .into(),
            );
        }
        let mine = |dir: &Path| {
            std::fs::metadata(dir)
                .is_ok_and(|m| m.is_dir() && Some(m.uid()) == euid && m.mode() & 0o200 != 0)
        };
        if !mine(&real.join("bin")) || !(mine(&real.join("lib")) || mine(&real)) {
            return Support::PackageManaged(format!("{} is not writable by you", prefix.display()));
        }
        // The links in bin/ are relative (`../lib/geekbridge/...`): a bin/ that is itself a link
        // would resolve them somewhere else. And a directory anyone can write to lets anyone swap
        // the links that the user then runs.
        let plain_dir = |dir: &Path| {
            std::fs::symlink_metadata(dir).map_or(true, |m| m.is_dir() && m.mode() & 0o002 == 0)
        };
        if !plain_dir(&real.join("bin")) || !plain_dir(&real.join("lib/geekbridge")) {
            return Support::DownloadOnly(format!(
                "{}/bin or lib/geekbridge is a link or writable by everyone",
                prefix.display()
            ));
        }
        Support::UserInstall(real)
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use super::{BINARIES, DOCS, SERVICE_UNIT, gz, tar};
    use crate::Version;
    use sha2::{Digest, Sha256};
    use std::{
        collections::BTreeSet,
        fs,
        io::{Read, Write},
        os::unix::{
            fs::{DirBuilderExt, OpenOptionsExt, symlink},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    const MIN_BINARY: u64 = 64 * 1024;
    const LIMITS: tar::Limits = tar::Limits {
        entries: 64,
        file_bytes: 192 << 20,
        total_bytes: 512 << 20,
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Error {
        /// This installation must not be changed from here.
        Unsupported(String),
        /// A rule refused the update (downgrade, pre-release, version mismatch, ...).
        Refused(String),
        /// Another install is running.
        Busy,
        /// A call is in progress (or could not be ruled out): the service is not restarted.
        CallInProgress(String),
        Io(String),
        /// The archive is damaged or hostile.
        Archive(String),
        /// The unpacked release failed validation.
        Invalid(String),
        /// The service did not come back on the new version; it was rolled back.
        Unhealthy(String),
        /// Test hook: the process "died" at this point.
        Interrupted,
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Error::Busy => f.write_str("another update is running"),
                Error::Interrupted => f.write_str("interrupted"),
                Error::Unsupported(s)
                | Error::Refused(s)
                | Error::CallInProgress(s)
                | Error::Io(s)
                | Error::Archive(s)
                | Error::Invalid(s)
                | Error::Unhealthy(s) => f.write_str(s),
            }
        }
    }
    impl std::error::Error for Error {}

    fn io(what: &str, e: impl std::fmt::Display) -> Error {
        Error::Io(format!("{what}: {e}"))
    }

    /// Crash points, for the interrupted-swap tests.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Step {
        Staged,
        Migrated,
        CurrentLinked,
        Relink(usize),
        Renamed,
        PreviousSet,
        Committed,
    }

    pub struct Request<'a> {
        pub prefix: &'a Path,
        /// The downloaded archive and what the release published for it.
        pub archive: &'a Path,
        pub sha256: &'a str,
        pub size: u64,
        /// Release tag, `vX.Y.Z`.
        pub tag: &'a str,
        pub current_version: &'a str,
        /// Verify, unpack and validate, then remove everything again.
        pub dry_run: bool,
        /// `update-state.json`, where the outcome is recorded.
        pub state_file: Option<&'a Path>,
        /// Require ELF executables for this CPU. Only tests switch it off.
        pub strict_binaries: bool,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Report {
        pub version: String,
        pub previous: Option<String>,
        pub dry_run: bool,
        /// The pre-existing `install.sh` files were turned into the versioned layout.
        pub migrated: bool,
    }

    pub(super) struct Layout {
        pub(super) bin: PathBuf,
        pub(super) root: PathBuf,
    }
    impl Layout {
        pub(super) fn new(prefix: &Path) -> Layout {
            Layout {
                bin: prefix.join("bin"),
                root: prefix.join("lib/geekbridge"),
            }
        }
        fn versions(&self) -> PathBuf {
            self.root.join("versions")
        }
        pub(super) fn link_name(&self, which: &str) -> Option<String> {
            let target = fs::read_link(self.root.join(which)).ok()?;
            let name = target.file_name()?.to_str()?.to_owned();
            (target.parent()? == Path::new("versions")).then_some(name)
        }
        fn runnable(&self, version: &str) -> bool {
            fs::metadata(self.versions().join(version).join("bin/geekbridge-service"))
                .is_ok_and(|m| m.is_file())
        }
    }

    /// The install lock: `flock(2)` on `lib/geekbridge/.lock`. The kernel drops it when the holder
    /// dies, so there is no stale lock to take over (and no pid reuse or takeover race).
    pub(super) struct Lock(PathBuf, #[allow(dead_code)] fs::File);
    impl Drop for Lock {
        fn drop(&mut self) {
            // Unlink while still holding the lock; `lock` re-checks the path after locking, so a
            // waiter that opened the old file does not end up holding a lock nobody else sees.
            let _ = fs::remove_file(&self.0);
            // A run that changed nothing leaves no empty directories behind either.
            if let Some(root) = self.0.parent()
                && fs::remove_dir(root).is_ok()
                && let Some(lib) = root.parent()
            {
                let _ = fs::remove_dir(lib);
            }
        }
    }
    pub(super) fn lock(root: &Path) -> Result<Lock, Error> {
        use std::os::unix::{fs::MetadataExt, io::AsRawFd};
        fs::create_dir_all(root).map_err(|e| io("cannot create the install directory", e))?;
        let path = root.join(".lock");
        for _ in 0..4 {
            let mut f = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .mode(0o600)
                .open(&path)
                .map_err(|e| io("cannot lock the install directory", e))?;
            // SAFETY: plain flock(2) on a descriptor this function owns.
            if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(Error::Busy);
            }
            let same = match (f.metadata(), fs::symlink_metadata(&path)) {
                (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
                _ => false,
            };
            if same {
                f.set_len(0).ok();
                let _ = write!(f, "{}", std::process::id());
                return Ok(Lock(path, f));
            }
        }
        Err(Error::Busy)
    }

    fn sync_dir(p: &Path) {
        let _ = fs::File::open(p).and_then(|f| f.sync_all());
    }

    /// Atomically make `dir/name` a symlink to `target` (replacing a file or another link).
    fn set_link(dir: &Path, name: &str, target: &str) -> Result<(), Error> {
        let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
        let _ = fs::remove_file(&tmp);
        symlink(target, &tmp).map_err(|e| io("cannot create a link", e))?;
        fs::rename(&tmp, dir.join(name)).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            io("cannot replace a link", e)
        })
    }

    fn bin_target(name: &str) -> String {
        format!("../lib/geekbridge/current/bin/{name}")
    }

    /// Is `bin/<name>` something this module may replace: missing, a plain file `install.sh` wrote
    /// (already copied into the versioned layout), or one of its own links? Anything else (a link
    /// to the user's own build, a directory) belongs to the user and is never overwritten.
    fn replaceable(l: &Layout, name: &str) -> bool {
        match fs::symlink_metadata(l.bin.join(name)) {
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            Ok(m) if m.is_file() => true,
            Ok(m) if m.is_symlink() => {
                fs::read_link(l.bin.join(name)).is_ok_and(|t| t.starts_with("../lib/geekbridge/"))
            }
            Ok(_) => false,
        }
    }

    fn safe_version(v: &str) -> bool {
        !v.is_empty()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
    }

    struct Stage(Option<PathBuf>);
    impl Stage {
        fn new(root: &Path) -> Result<Stage, Error> {
            let p = root.join(format!(".staging-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            // Private from the first instant: nothing else may slip a file in beside ours.
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&p)
                .map_err(|e| io("cannot create the staging directory", e))?;
            Ok(Stage(Some(p)))
        }
        fn path(&self) -> &Path {
            self.0.as_deref().unwrap_or(Path::new("/nonexistent"))
        }
        /// Leave the directory behind, as a crash would.
        fn forget(&mut self) {
            self.0 = None;
        }
    }
    impl Drop for Stage {
        fn drop(&mut self) {
            if let Some(p) = &self.0 {
                let _ = fs::remove_dir_all(p);
            }
        }
    }

    fn check_versions(req: &Request) -> Result<Version, Error> {
        let target = req
            .tag
            .strip_prefix('v')
            .and_then(Version::parse)
            .filter(|v| format!("v{v}") == req.tag)
            .ok_or_else(|| {
                Error::Refused(format!("'{}' is not a release tag (vX.Y.Z)", req.tag))
            })?;
        if target.is_prerelease() {
            return Err(Error::Refused(format!(
                "{} is a pre-release; the stable channel never installs one",
                req.tag
            )));
        }
        let current = Version::parse(req.current_version).ok_or_else(|| {
            Error::Refused("the running version is not a semantic version".into())
        })?;
        if target <= current {
            return Err(Error::Refused(format!(
                "{} is not newer than the running v{current}; going back is `update rollback`",
                req.tag
            )));
        }
        Ok(target)
    }

    /// Copy `src` to `dst`, hashing on the way; both size and SHA-256 must match what was published.
    fn copy_verified(src: &Path, dst: &Path, sha256: &str, size: u64) -> Result<(), Error> {
        // Non-blocking open: a FIFO at this path must not hang the installer.
        let mut from = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(src)
            .map_err(|e| io("cannot open the downloaded archive", e))?;
        // A pipe or device would block or never end; the size check below needs a regular file.
        if !from.metadata().is_ok_and(|m| m.is_file()) {
            return Err(Error::Archive(
                "the downloaded archive is not a regular file".into(),
            ));
        }
        let mut to = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dst)
            .map_err(|e| io("cannot copy the archive into staging", e))?;
        let (mut h, mut n, mut buf) = (Sha256::new(), 0_u64, vec![0_u8; 64 * 1024]);
        loop {
            let k = from
                .read(&mut buf)
                .map_err(|e| io("reading the archive", e))?;
            if k == 0 {
                break;
            }
            n += k as u64;
            if n > size {
                break;
            }
            h.update(&buf[..k]);
            to.write_all(&buf[..k])
                .map_err(|e| io("copying the archive", e))?;
        }
        to.sync_all().map_err(|e| io("copying the archive", e))?;
        if n != size {
            return Err(Error::Archive(format!(
                "the archive is {n} bytes, the release published {size}"
            )));
        }
        if bridge_core::ipc::encode_hex(&h.finalize()) != sha256.to_ascii_lowercase() {
            return Err(Error::Archive(
                "the archive does not match its published SHA-256".into(),
            ));
        }
        Ok(())
    }

    fn elf_ok(path: &Path) -> bool {
        let mut head = [0_u8; 20];
        if !fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut head))
            .is_ok()
        {
            return false;
        }
        let machine = u16::from_le_bytes([head[18], head[19]]);
        head[..4] == *b"\x7fELF"
            && head[4] == 2
            && match std::env::consts::ARCH {
                "x86_64" => machine == 62,
                "aarch64" => machine == 183,
                _ => true,
            }
    }

    /// `geekbridge-service --version` of the staged binary: "geekbridge-service X.Y.Z".
    fn staged_version(bin: &Path) -> Result<String, String> {
        // Its own process group, so a timeout takes down whatever it started too; the working
        // directory is the staging tree, not wherever the user happened to run ctl from.
        let cwd = bin
            .parent()
            .and_then(Path::parent)
            .unwrap_or(Path::new("/"));
        // ETXTBSY: another thread's fork can still hold the just-written file open for a moment.
        let mut tries = 0;
        let mut child = loop {
            let spawned = Command::new(bin)
                .arg("--version")
                .env_clear()
                .current_dir(cwd)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            match spawned {
                Err(e) if e.raw_os_error() == Some(26) && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("cannot run it: {e}")),
                Ok(c) => break c,
            }
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => break,
                Ok(Some(status)) => return Err(format!("it exited with {status}")),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    // SAFETY: signalling the process group this function created.
                    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("it did not answer within 10 seconds".into());
                }
            }
        }
        let mut text = String::new();
        if let Some(out) = child.stdout.take() {
            let _ = out.take(4096).read_to_string(&mut text);
        }
        let mut words = text.lines().next().unwrap_or("").split_whitespace();
        match (words.next(), words.next(), words.next()) {
            (Some("geekbridge-service"), Some(v), None) => Ok(v.to_owned()),
            _ => Err("its --version answer is not 'geekbridge-service X.Y.Z'".into()),
        }
    }

    fn validate(
        tree: &Path,
        members: &[tar::Member],
        target: &Version,
        strict: bool,
    ) -> Result<(), Error> {
        for name in BINARIES {
            let Some(m) = members.iter().find(|m| m.name == name) else {
                return Err(Error::Invalid(format!("the release lacks {name}")));
            };
            let path = tree.join("bin").join(name);
            let meta = fs::symlink_metadata(&path).map_err(|e| io(name, e))?;
            if !meta.is_file() || meta.len() != m.size || meta.len() < MIN_BINARY && strict {
                return Err(Error::Invalid(format!(
                    "{name} is not a regular, plausible file"
                )));
            }
            if !m.exec {
                return Err(Error::Invalid(format!("{name} is not marked executable")));
            }
            if strict && !elf_ok(&path) {
                return Err(Error::Invalid(format!(
                    "{name} is not an executable for this computer ({})",
                    std::env::consts::ARCH
                )));
            }
        }
        let said = staged_version(&tree.join("bin/geekbridge-service"))
            .map_err(|e| Error::Invalid(format!("geekbridge-service --version: {e}")))?;
        if Version::parse(&said).as_ref() != Some(target) || said.starts_with('v') {
            return Err(Error::Invalid(format!(
                "the release is tagged v{target} but its geekbridge-service says {said}"
            )));
        }
        Ok(())
    }

    /// Turn an `install.sh` layout (regular files in `bin/`) into the versioned one. Idempotent.
    fn ensure_layout(
        l: &Layout,
        running: &str,
        hook: &mut dyn FnMut(Step) -> bool,
    ) -> Result<bool, Error> {
        if fs::symlink_metadata(l.root.join("current")).is_ok() {
            return Ok(false);
        }
        let ver = Version::parse(running)
            .map(|v| v.to_string())
            .filter(|v| safe_version(v))
            .ok_or_else(|| {
                Error::Refused("the running version is not usable as a directory name".into())
            })?;
        let tmp = l.root.join(format!(".staging-mig-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("bin"))
            .map_err(|e| io("cannot create the staging directory", e))?;
        let mut present = Vec::new();
        for name in BINARIES {
            let src = l.bin.join(name);
            match fs::symlink_metadata(&src) {
                Ok(m) if m.is_file() => {
                    fs::copy(&src, tmp.join("bin").join(name)).map_err(|e| io(name, e))?;
                    present.push(name);
                }
                Ok(_) => {
                    let _ = fs::remove_dir_all(&tmp);
                    return Err(Error::Unsupported(format!(
                        "{} is not a plain file that install.sh wrote (a link you made?), so it is left alone",
                        src.display()
                    )));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(name, e)),
            }
        }
        if !present.contains(&"geekbridge-service") {
            let _ = fs::remove_dir_all(&tmp);
            return Err(Error::Unsupported(format!(
                "no geekbridge-service in {}; is this the prefix install.sh used?",
                l.bin.display()
            )));
        }
        let vdir = l.versions().join(&ver);
        let _ = fs::remove_dir_all(&vdir);
        fs::create_dir_all(l.versions()).map_err(|e| io("versions", e))?;
        fs::rename(&tmp, &vdir).map_err(|e| io("cannot place the current version", e))?;
        if !hook(Step::Migrated) {
            return Err(Error::Interrupted);
        }
        set_link(&l.root, "current", &format!("versions/{ver}"))?;
        sync_dir(&l.root);
        if !hook(Step::CurrentLinked) {
            return Err(Error::Interrupted);
        }
        for (i, name) in present.iter().enumerate() {
            if !hook(Step::Relink(i)) {
                return Err(Error::Interrupted);
            }
            set_link(&l.bin, name, &bin_target(name))?;
        }
        Ok(true)
    }

    pub(super) fn record(
        state_file: Option<&Path>,
        outcome: &str,
        version: &str,
        previous: Option<&str>,
        detail: &str,
    ) {
        let Some(path) = state_file else { return };
        let entry = serde_json::json!({
            "outcome": outcome,
            "version": version,
            "previous": previous,
            "detail": detail.chars().take(300).collect::<String>(),
            "unix": crate::now_unix(),
        });
        let r = crate::state_file(path).update(|body| {
            body.insert("last_install".into(), entry);
            true
        });
        if let Err(e) = r {
            bridge_core::statefile::note(format!(
                "update-state.json: install outcome not saved: {e}"
            ));
        }
    }

    /// The outcome of the last install, rollback or recovery, as one line.
    pub fn last_outcome(state_file: &Path) -> Option<String> {
        let loaded = crate::state_file(state_file).load().ok()?;
        let e = loaded.body.get("last_install")?;
        // The file is plain JSON a person could edit: show it as one harmless line.
        let s = |k: &str| {
            e.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .take(300)
                .collect::<String>()
        };
        let detail = s("detail");
        Some(format!(
            "{} {}{}{}",
            s("outcome"),
            s("version"),
            e.get("unix")
                .and_then(|u| u.as_u64())
                .map_or(String::new(), |u| format!(" (unix time {u})")),
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ))
    }

    fn prune(l: &Layout) {
        let keep: BTreeSet<String> = ["current", "previous"]
            .iter()
            .filter_map(|w| l.link_name(w))
            .collect();
        if let Ok(dir) = fs::read_dir(l.versions()) {
            for e in dir.flatten() {
                if !keep.contains(e.file_name().to_string_lossy().as_ref()) {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }

    /// The version `current` points at, when the prefix has been through a self-update.
    pub fn installed_version(prefix: &Path) -> Option<String> {
        Layout::new(prefix).link_name("current")
    }

    pub fn install(req: &Request) -> Result<Report, Error> {
        let result = run(req, &mut |_| true);
        if let Err(e) = &result
            && !req.dry_run
            && !matches!(e, Error::Busy | Error::Interrupted)
        {
            record(
                req.state_file,
                "failed",
                req.tag.trim_start_matches('v'),
                None,
                &e.to_string(),
            );
        }
        result
    }

    pub(super) fn run(req: &Request, hook: &mut dyn FnMut(Step) -> bool) -> Result<Report, Error> {
        let target = check_versions(req)?;
        let l = Layout::new(req.prefix);
        if !l.bin.is_dir() {
            return Err(Error::Unsupported(format!(
                "{} does not exist",
                l.bin.display()
            )));
        }
        let _lock = lock(&l.root)?;
        recover_locked(&l, None);
        if let Some(name) = BINARIES.iter().find(|n| !replaceable(&l, n)) {
            return Err(Error::Unsupported(format!(
                "{} is not a plain file that install.sh wrote (a link you made?), so it is left alone",
                l.bin.join(name).display()
            )));
        }
        let mut stage = Stage::new(&l.root)?;
        let tree = stage.path().join("tree");
        let archive = stage.path().join("archive.tar.gz");
        for d in ["bin", "doc"] {
            fs::create_dir_all(tree.join(d)).map_err(|e| io("staging", e))?;
        }
        copy_verified(req.archive, &archive, req.sha256, req.size)?;
        let file = fs::File::open(&archive).map_err(|e| io("staging", e))?;
        let reader = gz::Gunzip::new(file, LIMITS.total_bytes + (4 << 20));
        let members = tar::extract(reader, &LIMITS, &mut |name| {
            if BINARIES.contains(&name) {
                Some(tree.join("bin").join(name))
            } else {
                DOCS.contains(&name).then(|| tree.join("doc").join(name))
            }
        })
        .map_err(Error::Archive)?;
        let _ = fs::remove_file(&archive);
        validate(&tree, &members, &target, req.strict_binaries)?;
        if !hook(Step::Staged) {
            stage.forget();
            return Err(Error::Interrupted);
        }
        let ver = target.to_string();
        if req.dry_run {
            return Ok(Report {
                version: ver,
                previous: l.link_name("current"),
                dry_run: true,
                migrated: false,
            });
        }
        let crash = |stage: &mut Stage| {
            stage.forget();
            Err(Error::Interrupted)
        };
        let migrated = match ensure_layout(&l, req.current_version, hook) {
            Ok(m) => m,
            Err(Error::Interrupted) => {
                stage.forget();
                return Err(Error::Interrupted);
            }
            Err(e) => return Err(e),
        };
        let mut previous = l.link_name("current");
        if previous.as_deref() == Some(ver.as_str()) {
            // An earlier run committed and died before finishing: only the tail is left to do.
            previous = l.link_name("previous");
        } else {
            let vdir = l.versions().join(&ver);
            let _ = fs::remove_dir_all(&vdir);
            fs::create_dir_all(l.versions()).map_err(|e| io("versions", e))?;
            fs::rename(&tree, &vdir).map_err(|e| io("cannot place the new version", e))?;
            sync_dir(&l.versions());
            if !hook(Step::Renamed) {
                return crash(&mut stage);
            }
            if let Some(old) = &previous {
                set_link(&l.root, "previous", &format!("versions/{old}"))?;
            }
            if !hook(Step::PreviousSet) {
                return crash(&mut stage);
            }
            set_link(&l.root, "current", &format!("versions/{ver}"))?;
            sync_dir(&l.root);
            if !hook(Step::Committed) {
                return crash(&mut stage);
            }
        }
        // Committed. From here on a failure leaves the new version live.
        for (i, name) in BINARIES.iter().enumerate() {
            if !hook(Step::Relink(i)) {
                return crash(&mut stage);
            }
            let up_to_date =
                fs::read_link(l.bin.join(name)).is_ok_and(|t| t == Path::new(&bin_target(name)));
            if !up_to_date {
                set_link(&l.bin, name, &bin_target(name))?;
            }
        }
        prune(&l);
        record(req.state_file, "installed", &ver, previous.as_deref(), "");
        Ok(Report {
            version: ver,
            previous,
            dry_run: false,
            migrated,
        })
    }

    /// Go back to the previous version (and keep the one being left as the new "previous", so a
    /// second rollback goes forward again).
    pub fn rollback(prefix: &Path, state_file: Option<&Path>) -> Result<Report, Error> {
        let l = Layout::new(prefix);
        let _lock = lock(&l.root)?;
        recover_locked(&l, None);
        let (Some(cur), Some(prev)) = (l.link_name("current"), l.link_name("previous")) else {
            return Err(Error::Refused(
                "there is no previous version to go back to".into(),
            ));
        };
        if cur == prev || !l.runnable(&prev) {
            return Err(Error::Refused(
                "there is no previous version to go back to".into(),
            ));
        }
        set_link(&l.root, "current", &format!("versions/{prev}"))?;
        set_link(&l.root, "previous", &format!("versions/{cur}"))?;
        sync_dir(&l.root);
        for name in BINARIES {
            if fs::read_link(l.bin.join(name)).map_or(true, |t| t != Path::new(&bin_target(name)))
                && replaceable(&l, name)
                && l.versions().join(&prev).join("bin").join(name).is_file()
            {
                set_link(&l.bin, name, &bin_target(name))?;
            }
        }
        record(state_file, "rolled_back", &prev, Some(&cur), "");
        Ok(Report {
            version: prev,
            previous: Some(cur),
            dry_run: false,
            migrated: false,
        })
    }

    /// Clean up after an interrupted install and make sure `current` works. Safe to call at every
    /// start (the service does); a no-op when there is nothing to do or an install is running.
    pub fn recover(prefix: &Path, state_file: Option<&Path>) -> Vec<String> {
        let l = Layout::new(prefix);
        // Nothing to do when this prefix never saw a self-update, and package files are never touched.
        if !l.root.is_dir() || prefix.join("share/geekbridge/package-managed").exists() {
            return Vec::new();
        }
        let Ok(_lock) = lock(&l.root) else {
            return Vec::new();
        };
        recover_locked(&l, state_file)
    }

    /// `.staging-<pid>` and `.staging-mig-<pid>`: what this module creates, nothing else of the user's.
    fn ours_staging(name: &str) -> bool {
        name.strip_prefix(".staging-").is_some_and(|rest| {
            let pid = rest.strip_prefix("mig-").unwrap_or(rest);
            !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())
        })
    }

    /// `.<current|previous|binary>.tmp-<pid>`, the half-made links of `set_link`.
    fn ours_tmp_link(name: &str) -> bool {
        name.strip_prefix('.')
            .and_then(|r| r.split_once(".tmp-"))
            .is_some_and(|(n, pid)| {
                (n == "current" || n == "previous" || BINARIES.contains(&n))
                    && !pid.is_empty()
                    && pid.bytes().all(|b| b.is_ascii_digit())
            })
    }

    fn recover_locked(l: &Layout, state_file: Option<&Path>) -> Vec<String> {
        let mut notes = Vec::new();
        for dir in [&l.root, &l.bin] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if ours_staging(&name) {
                    let _ = fs::remove_dir_all(e.path());
                } else if ours_tmp_link(&name) {
                    let _ = fs::remove_file(e.path());
                } else {
                    continue;
                }
                notes.push(format!("removed leftover {name} of an interrupted update"));
            }
        }
        if let Some(cur) = l.link_name("current")
            && !l.runnable(&cur)
            && let Some(prev) = l.link_name("previous")
            && l.runnable(&prev)
            && set_link(&l.root, "current", &format!("versions/{prev}")).is_ok()
        {
            notes.push(format!("v{cur} was unusable; v{prev} is current again"));
            record(
                state_file,
                "recovered",
                &prev,
                Some(&cur),
                "current was unusable",
            );
        }
        let before = fs::read_dir(l.versions()).map_or(0, Iterator::count);
        prune(l);
        let after = fs::read_dir(l.versions()).map_or(0, Iterator::count);
        if after < before {
            notes.push("removed an unreferenced version of an interrupted update".into());
        }
        notes
    }

    /// What the restart step needs to know about the running service.
    pub trait Probe {
        /// Is a call ringing or live? An error means "cannot tell", which is treated as yes.
        fn calls_active(&self) -> Result<bool, String>;
        /// Version the service reports now; `None` when it does not answer.
        fn running_version(&self) -> Option<String>;
        /// The existing mechanism of install.sh: the systemd user unit.
        fn restart(&self) -> Result<(), String> {
            let ok = |args: &[&str]| {
                Command::new("systemctl")
                    .arg("--user")
                    .args(args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            };
            if !ok(&["show-environment"]) {
                return Err("systemd user services are not available here; restart geekbridge-service yourself".into());
            }
            if ok(&["try-restart", SERVICE_UNIT]) {
                Ok(())
            } else {
                Err(format!(
                    "systemctl --user try-restart {SERVICE_UNIT} failed"
                ))
            }
        }
    }

    /// The real probe: the service's control API.
    pub struct IpcProbe {
        pub port: u16,
        pub token: bridge_core::ipc::Token,
    }
    impl Probe for IpcProbe {
        fn calls_active(&self) -> Result<bool, String> {
            use bridge_core::ipc::{Request, Response, call};
            match call(self.port, &self.token, &Request::Calls) {
                Ok(Response::Calls(c)) => Ok(!c.view.rows.is_empty()),
                Ok(_) => Err("unexpected reply".into()),
                Err(e) => Err(e.to_string()),
            }
        }
        fn running_version(&self) -> Option<String> {
            use bridge_core::ipc::{Request, Response, call};
            match call(self.port, &self.token, &Request::UpdateStatus) {
                Ok(Response::Update(s)) => Some(s.current_version),
                _ => None,
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Restarted {
        /// The service was not running; the new version starts the next time.
        NotRunning,
        /// Restarted and answering on the expected version.
        Restarted,
    }

    /// Restart the service onto `expect` unless a call is in progress. When it does not answer on
    /// that version within `wait`, go back (if `auto_rollback`) and restart again.
    pub fn restart(
        probe: &dyn Probe,
        prefix: &Path,
        expect: &str,
        wait: Duration,
        auto_rollback: bool,
        state_file: Option<&Path>,
    ) -> Result<Restarted, Error> {
        if probe.running_version().is_none() {
            return Ok(Restarted::NotRunning);
        }
        match probe.calls_active() {
            Ok(false) => {}
            Ok(true) => {
                return Err(Error::CallInProgress(
                    "a call is in progress, so the service is not restarted; finish the call and try again".into(),
                ));
            }
            Err(e) => {
                return Err(Error::CallInProgress(format!(
                    "cannot tell whether a call is in progress ({e}), so the service is not restarted"
                )));
            }
        }
        probe.restart().map_err(Error::Unsupported)?;
        let step = (wait / 20).clamp(Duration::from_millis(10), Duration::from_millis(500));
        let deadline = Instant::now() + wait;
        loop {
            if probe.running_version().as_deref() == Some(expect) {
                return Ok(Restarted::Restarted);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(step);
        }
        if !auto_rollback {
            return Err(Error::Unhealthy(format!(
                "the service did not come back on v{expect}; `geekbridge-ctl update rollback` goes back"
            )));
        }
        let back = rollback(prefix, state_file)?;
        let _ = probe.restart();
        record(
            state_file,
            "rolled_back_unhealthy",
            &back.version,
            Some(expect),
            "the new version did not start",
        );
        Err(Error::Unhealthy(format!(
            "the service did not come back on v{expect}; went back to v{} and restarted it",
            back.version
        )))
    }
}
