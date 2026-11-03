// SPDX-License-Identifier: GPL-3.0-or-later
//! Trust store: which device ids are paired and which exact certificate each one
//! is pinned to. Entries are only created by a completed, locally approved
//! pairing; discovery or a handshake never writes here.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use bridge_core::statefile::{Corrupt, Loaded, Spec, Status};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::cert::CertInfo;
use crate::error::{KdeError, Result};
use crate::fsutil::{self, PrivateStateFile};
use crate::identity::DeviceId;

const MAX_PEERS: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedPeer {
    pub device_id: DeviceId,
    pub name: String,
    pub device_type: String,
    pub protocol_version: u32,
    pub cert: CertInfo,
    pub paired_at_unix: i64,
}

pub trait TrustStore: Send + Sync {
    fn get(&self, id: &DeviceId) -> Result<Option<TrustedPeer>>;
    /// Durably record a pairing. Must not report success unless the record
    /// would survive a crash.
    fn insert(&self, peer: TrustedPeer) -> Result<()>;
    fn remove(&self, id: &DeviceId) -> Result<bool>;
    fn list(&self) -> Result<Vec<TrustedPeer>>;
}

/// In-memory store for tests and embedding.
#[derive(Default)]
pub struct MemoryTrustStore {
    peers: Mutex<BTreeMap<DeviceId, TrustedPeer>>,
}

impl MemoryTrustStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TrustStore for MemoryTrustStore {
    fn get(&self, id: &DeviceId) -> Result<Option<TrustedPeer>> {
        Ok(self.peers.lock().unwrap().get(id).cloned())
    }
    fn insert(&self, peer: TrustedPeer) -> Result<()> {
        self.peers
            .lock()
            .unwrap()
            .insert(peer.device_id.clone(), peer);
        Ok(())
    }
    fn remove(&self, id: &DeviceId) -> Result<bool> {
        Ok(self.peers.lock().unwrap().remove(id).is_some())
    }
    fn list(&self) -> Result<Vec<TrustedPeer>> {
        Ok(self.peers.lock().unwrap().values().cloned().collect())
    }
}

/// One entry of `trusted_peers.json`. Unknown fields are ignored here and kept in the file
/// (see [`write_peers`]).
#[derive(Serialize, Deserialize)]
struct FilePeer {
    device_id: String,
    name: String,
    device_type: String,
    protocol_version: u32,
    paired_at_unix: i64,
    cert_der_base64: String,
    cert_sha256: String,
}

/// Format 1 is the first and only one so far. A future change that is not purely additive adds
/// its step to `migrations` and bumps `current`.
pub static TRUST_SPEC: Spec = Spec {
    name: "trusted_peers.json",
    current: 1,
    migrations: &[],
    validate: Some(validate_trust),
    // A damaged store pairs nobody (fail closed); the file is kept as `.corrupt`.
    on_corrupt: Corrupt::Reset,
};

fn validate_trust(body: &Map<String, Value>) -> std::result::Result<(), String> {
    parse_peers(body).map(|_| ())
}

/// JSON trust store at `<dir>/trusted_peers.json`, written atomically with
/// owner-only permissions (see [`fsutil`]) through [`bridge_core::statefile`]: versioned, with a
/// one-generation `.bak`. A damaged file falls back to the backup; with no usable backup the store
/// starts empty (nobody is trusted), says so in the diagnostics and keeps the damaged file as
/// `.corrupt`. A file from a *newer* build is read as far as it can be understood but never
/// rewritten (pairing and unpairing then fail with a clear error).
///
/// # Several processes (daemon + CLI)
///
/// * **Writers** (`insert`, `remove`) take an exclusive lock on
///   `<dir>/trusted_peers.json.lock` (waiting up to [`fsutil::LOCK_TIMEOUT`]),
///   re-read the file *under the lock*, apply their change to that fresh
///   state and replace the file atomically. Concurrent writers in different
///   processes (or different `FileTrustStore` values in one process) are
///   therefore serialised and cannot lose each other's updates.
/// * **Readers** (`get`, `list`) take no lock. Because the file is only ever
///   replaced by rename they see either the old or the new complete file, and
///   they re-read it when it changed, so a pairing added or revoked by another
///   process is honoured by a long-running daemon without a restart. If the
///   re-read fails (permissions loosened, replaced by a link) the call fails;
///   it never falls back to stale trust.
/// * The lock is advisory (`flock` / `LockFileEx`) and only coordinates users
///   of this type; it is released if the holder crashes. It is not reliable
///   on network file systems.
pub struct FileTrustStore {
    file: PrivateStateFile,
    state: Mutex<State>,
    /// Serialises read-modify-write cycles in this process (never held by readers).
    writer: Mutex<()>,
}

/// Last file contents seen and what they parsed to.
struct State {
    raw: Option<Vec<u8>>,
    peers: BTreeMap<DeviceId, TrustedPeer>,
    /// Format of a file from a newer build, if that is what is on disk.
    newer: Option<u32>,
}

impl State {
    fn apply(&mut self, loaded: &Loaded) {
        self.raw = loaded.raw.clone();
        self.newer = match loaded.status {
            Status::Newer { found } => Some(found),
            _ => None,
        };
        // A newer file is read leniently: if it no longer looks like ours, trust nobody.
        self.peers = parse_peers(&loaded.body).unwrap_or_default();
    }
}

impl FileTrustStore {
    pub fn open(dir: &Path) -> Result<Self> {
        fsutil::ensure_private_dir(dir)?;
        let file = PrivateStateFile::new(
            fsutil::PrivateIo,
            dir.join("trusted_peers.json"),
            &TRUST_SPEC,
        );
        let mut state = State {
            raw: None,
            peers: BTreeMap::new(),
            newer: None,
        };
        refresh(&file, &mut state)?;
        Ok(Self {
            file,
            state: Mutex::new(state),
            writer: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Run a read-modify-write cycle under the cross-process lock.
    fn modify<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<DeviceId, TrustedPeer>) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        // One writer at a time inside this process; readers only ever take `state`, and only
        // briefly, so a writer stuck on the file lock or an fsync never stalls them.
        let _writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let locked = self.file.lock()?;
        // Authoritative state, now that nobody else can write.
        let loaded = locked.load()?;
        let current = {
            let mut guard = self.state.lock().unwrap();
            guard.apply(&loaded);
            if let Some(found) = guard.newer {
                return Err(KdeError::StoreNewer {
                    path: self.file.path().to_owned(),
                    found,
                });
            }
            guard.peers.clone()
        };
        let mut next = current.clone();
        let out = f(&mut next)?;
        if out.is_some() {
            if next.len() > MAX_PEERS {
                return Err(KdeError::Store("trust store is full".into()));
            }
            let mut body = loaded.body;
            write_peers(&mut body, &next);
            // Removals refresh the backup too, so a damaged primary cannot bring a revoked device
            // back from an older generation.
            if next.len() < current.len() {
                locked.save_and_refresh_backup(&body)?;
            } else {
                locked.save(&body)?;
            }
            let mut guard = self.state.lock().unwrap();
            guard.raw = None; // re-read (and re-validated) on next use
            guard.peers = next;
        }
        Ok(out)
    }
}

/// Bring `state` in line with the file on disk, parsing only if it changed.
fn refresh(file: &PrivateStateFile, state: &mut State) -> Result<()> {
    if let Ok(Some(bytes)) = fsutil::read_private_file(file.path())
        && state.raw.as_deref() == Some(bytes.as_slice())
    {
        return Ok(());
    }
    let loaded = file.load()?;
    state.apply(&loaded);
    Ok(())
}

/// Write `peers` into the document's `peers` array, keeping any fields of existing entries that
/// this build does not know.
fn write_peers(body: &mut Map<String, Value>, peers: &BTreeMap<DeviceId, TrustedPeer>) {
    let mut old: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    if let Some(Value::Array(a)) = body.get("peers") {
        for v in a {
            if let Value::Object(m) = v
                && let Some(id) = m.get("device_id").and_then(Value::as_str)
            {
                old.insert(id.to_owned(), m.clone());
            }
        }
    }
    let list = peers
        .values()
        .map(|p| {
            let mut m = old.remove(p.device_id.as_str()).unwrap_or_default();
            let known = FilePeer {
                device_id: p.device_id.to_string(),
                name: p.name.clone(),
                device_type: p.device_type.clone(),
                protocol_version: p.protocol_version,
                paired_at_unix: p.paired_at_unix,
                cert_der_base64: B64.encode(p.cert.der()),
                cert_sha256: p.cert.fingerprint_hex(),
            };
            if let Ok(Value::Object(k)) = serde_json::to_value(&known) {
                m.extend(k);
            }
            Value::Object(m)
        })
        .collect();
    body.insert("peers".into(), Value::Array(list));
}

fn parse_peers(
    body: &Map<String, Value>,
) -> std::result::Result<BTreeMap<DeviceId, TrustedPeer>, String> {
    let list: Vec<FilePeer> = body
        .get("peers")
        .cloned()
        .ok_or_else(|| "no peers list".to_string())
        .and_then(|v| serde_json::from_value(v).map_err(|e| format!("unreadable peers ({e})")))?;
    if list.len() > MAX_PEERS {
        return Err("too many entries".into());
    }
    let mut out = BTreeMap::new();
    for p in list {
        let id = DeviceId::parse(&p.device_id)
            .map_err(|_| "entry with an invalid device id".to_string())?;
        let der = B64
            .decode(&p.cert_der_base64)
            .map_err(|_| format!("{id}: bad certificate encoding"))?;
        let cert = CertInfo::parse(&der).map_err(|e| format!("{id}: {e}"))?;
        if cert.fingerprint_hex() != p.cert_sha256 {
            return Err(format!(
                "{id}: certificate does not match its recorded fingerprint"
            ));
        }
        if cert.common_name() != id.as_str() {
            return Err(format!(
                "{id}: certificate common name differs from the device id"
            ));
        }
        let peer = TrustedPeer {
            device_id: id.clone(),
            name: p.name,
            device_type: p.device_type,
            protocol_version: p.protocol_version,
            cert,
            paired_at_unix: p.paired_at_unix,
        };
        if out.insert(id.clone(), peer).is_some() {
            return Err(format!("{id}: duplicate entry"));
        }
    }
    Ok(out)
}

impl TrustStore for FileTrustStore {
    fn get(&self, id: &DeviceId) -> Result<Option<TrustedPeer>> {
        let mut state = self.state.lock().unwrap();
        refresh(&self.file, &mut state)?;
        Ok(state.peers.get(id).cloned())
    }

    fn insert(&self, peer: TrustedPeer) -> Result<()> {
        self.modify(|peers| {
            if peers.len() >= MAX_PEERS && !peers.contains_key(&peer.device_id) {
                return Err(KdeError::Store("trust store is full".into()));
            }
            peers.insert(peer.device_id.clone(), peer);
            Ok(Some(()))
        })?;
        Ok(())
    }

    fn remove(&self, id: &DeviceId) -> Result<bool> {
        Ok(self
            .modify(|peers| Ok(peers.remove(id).map(|_| ())))?
            .is_some())
    }

    fn list(&self) -> Result<Vec<TrustedPeer>> {
        let mut state = self.state.lock().unwrap();
        refresh(&self.file, &mut state)?;
        Ok(state.peers.values().cloned().collect())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::cert::DeviceCredentials;
    use std::os::unix::fs::PermissionsExt;

    fn peer(c: char) -> TrustedPeer {
        let id = DeviceId::parse(&c.to_string().repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        TrustedPeer {
            device_id: id,
            name: "Phone".into(),
            device_type: "phone".into(),
            protocol_version: 8,
            cert: creds.cert().clone(),
            paired_at_unix: 1,
        }
    }

    /// A writer stuck on the cross-process lock (another process, or a slow fsync) must not stop
    /// readers: the daemon answers `status` from `list()` on an async worker.
    #[test]
    fn readers_are_not_blocked_by_a_writer_waiting_for_the_file_lock() {
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = Arc::new(FileTrustStore::open(&dir).unwrap());
        s.insert(peer('a')).unwrap();
        let lock = fsutil::lock_exclusive(&bridge_core::statefile::lock_path(s.path())).unwrap();
        let writer = {
            let s = s.clone();
            std::thread::spawn(move || s.insert(peer('b')))
        };
        std::thread::sleep(Duration::from_millis(300)); // the writer is now waiting on the lock
        let started = Instant::now();
        assert_eq!(s.list().unwrap().len(), 1);
        assert!(s.get(&peer('a').device_id).unwrap().is_some());
        let took = started.elapsed();
        drop(lock);
        writer.join().unwrap().unwrap();
        assert!(took < Duration::from_millis(200), "readers waited {took:?}");
        assert_eq!(s.list().unwrap().len(), 2);
    }

    #[test]
    fn persists_across_reopen_with_private_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        let p = peer('a');
        s.insert(p.clone()).unwrap();
        s.insert(peer('b')).unwrap();
        drop(s);
        let s = FileTrustStore::open(&dir).unwrap();
        assert_eq!(s.get(&p.device_id).unwrap().unwrap(), p);
        assert_eq!(s.list().unwrap().len(), 2);
        let mode = std::fs::metadata(s.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(s.remove(&p.device_id).unwrap());
        assert!(!s.remove(&p.device_id).unwrap());
        drop(s);
        assert_eq!(FileTrustStore::open(&dir).unwrap().list().unwrap().len(), 1);
    }

    #[test]
    fn damaged_file_recovers_from_backup_else_trusts_nobody_and_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        drop(s);
        let good = std::fs::read(&path).unwrap();
        let damage = |bad: &[u8]| {
            std::fs::write(&path, bad).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        // No backup yet (only one write so far): nobody is trusted, the file is left alone.
        for bad in [good[..good.len() / 2].to_vec(), b"{}".to_vec(), Vec::new()] {
            damage(&bad);
            assert!(
                FileTrustStore::open(&dir)
                    .unwrap()
                    .list()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
        // Restore, write again so a backup exists, then damage the primary.
        damage(&good);
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('b')).unwrap();
        drop(s);
        for bad in [&b""[..], b"\0\0", b"{\"version\":1,\"peers\":3}"] {
            damage(bad);
            let s = FileTrustStore::open(&dir).unwrap();
            assert_eq!(
                s.list().unwrap().len(),
                1,
                "the backup holds the pairing from before"
            );
            // The next write repairs the primary and keeps the damaged file.
            s.insert(peer('c')).unwrap();
            assert_eq!(s.list().unwrap().len(), 2);
            assert_eq!(
                std::fs::read(path.with_extension("json.corrupt")).unwrap(),
                bad
            );
            drop(s);
            damage(bad);
            std::fs::remove_file(path.with_extension("json.corrupt")).unwrap();
        }
    }

    #[test]
    fn a_revoked_device_is_not_trusted_again_when_the_primary_is_damaged() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        s.insert(peer('b')).unwrap();
        let revoked = peer('a').device_id;
        assert!(s.remove(&revoked).unwrap());
        let path = s.path().to_owned();
        drop(s);
        for bad in [&b""[..], b"{", b"\0\0"] {
            std::fs::write(&path, bad).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let s = FileTrustStore::open(&dir).unwrap();
            assert!(
                s.get(&revoked).unwrap().is_none(),
                "revoked device trusted again"
            );
            assert_eq!(s.list().unwrap().len(), 1);
        }
    }

    #[test]
    fn an_unreadable_file_is_left_intact_and_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        let good = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            return; // running as root: permissions do not apply
        }
        assert!(s.insert(peer('b')).is_err(), "must not write over it");
        assert!(FileTrustStore::open(&dir).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), good);
        assert_eq!(FileTrustStore::open(&dir).unwrap().list().unwrap().len(), 1);
    }

    #[test]
    fn tampered_certificate_or_fingerprint_trusts_nobody() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        drop(s);
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["peers"][0]["cert_sha256"] = serde_json::Value::from("00".repeat(32));
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            FileTrustStore::open(&dir)
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn newer_format_is_read_but_never_rewritten() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        let p = peer('a');
        s.insert(p.clone()).unwrap();
        let path = s.path().to_owned();
        drop(s);
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["version"] = 7.into();
        v["peers"][0]["future_field"] = "keep me".into();
        let text = serde_json::to_vec(&v).unwrap();
        std::fs::write(&path, &text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let s = FileTrustStore::open(&dir).unwrap();
        assert_eq!(
            s.get(&p.device_id).unwrap().unwrap(),
            p,
            "still trusted, read-only"
        );
        assert!(matches!(
            s.insert(peer('b')),
            Err(KdeError::StoreNewer { found: 7, .. })
        ));
        assert!(matches!(
            s.remove(&p.device_id),
            Err(KdeError::StoreNewer { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), text);
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        let mut v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        v["top_level_extra"] = serde_json::json!({"x": 1});
        v["peers"][0]["nickname"] = "kept".into();
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        s.insert(peer('b')).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["top_level_extra"]["x"], 1);
        assert_eq!(v["peers"][0]["nickname"], "kept");
        assert_eq!(v["peers"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn loosened_permissions_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        drop(s);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            FileTrustStore::open(&dir),
            Err(KdeError::InsecurePermissions { .. })
        ));
    }
}
