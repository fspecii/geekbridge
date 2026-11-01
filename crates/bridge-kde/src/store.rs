//! Trust store: which device ids are paired and which exact certificate each one
//! is pinned to. Entries are only created by a completed, locally approved
//! pairing; discovery or a handshake never writes here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::cert::CertInfo;
use crate::error::{KdeError, Result};
use crate::fsutil;
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

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    peers: Vec<FilePeer>,
}

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

/// JSON trust store at `<dir>/trusted_peers.json`, written atomically with
/// owner-only permissions (see [`fsutil`]). A corrupt or tampered file is an
/// error, never an empty store: nothing is silently trusted and nothing is
/// overwritten.
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
///   re-read fails (file corrupted, permissions loosened, replaced by a
///   link) the call fails; it never falls back to stale trust.
/// * The lock is advisory (`flock` / `LockFileEx`) and only coordinates users
///   of this type; it is released if the holder crashes. It is not reliable
///   on network file systems.
pub struct FileTrustStore {
    path: PathBuf,
    lock_path: PathBuf,
    state: Mutex<State>,
}

/// Last file contents seen and what they parsed to.
struct State {
    raw: Option<Vec<u8>>,
    peers: BTreeMap<DeviceId, TrustedPeer>,
}

impl FileTrustStore {
    pub fn open(dir: &Path) -> Result<Self> {
        fsutil::ensure_private_dir(dir)?;
        let path = dir.join("trusted_peers.json");
        let lock_path = dir.join("trusted_peers.json.lock");
        let mut state = State {
            raw: None,
            peers: BTreeMap::new(),
        };
        refresh(&path, &mut state)?;
        Ok(Self {
            path,
            lock_path,
            state: Mutex::new(state),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Serialise `peers`, replace the file, and return the bytes written.
    fn persist(&self, peers: &BTreeMap<DeviceId, TrustedPeer>) -> Result<Vec<u8>> {
        let file = FileFormat {
            version: 1,
            peers: peers
                .values()
                .map(|p| FilePeer {
                    device_id: p.device_id.to_string(),
                    name: p.name.clone(),
                    device_type: p.device_type.clone(),
                    protocol_version: p.protocol_version,
                    paired_at_unix: p.paired_at_unix,
                    cert_der_base64: B64.encode(p.cert.der()),
                    cert_sha256: p.cert.fingerprint_hex(),
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| KdeError::Store(e.to_string()))?;
        if bytes.len() as u64 > fsutil::MAX_STATE_FILE {
            // Would be written but could never be loaded again.
            return Err(KdeError::Store("trust store is full".into()));
        }
        fsutil::atomic_write_private(&self.path, &bytes)?;
        Ok(bytes)
    }

    /// Run a read-modify-write cycle under the cross-process lock.
    fn modify<T>(
        &self,
        f: impl FnOnce(&mut BTreeMap<DeviceId, TrustedPeer>) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        let mut guard = self.state.lock().unwrap();
        let _file_lock = fsutil::lock_exclusive(&self.lock_path)?;
        // Authoritative state, now that nobody else can write.
        refresh(&self.path, &mut guard)?;
        let mut next = guard.peers.clone();
        let out = f(&mut next)?;
        if out.is_some() {
            let raw = self.persist(&next)?;
            guard.raw = Some(raw);
            guard.peers = next;
        }
        Ok(out)
    }
}

/// Bring `state` in line with the file on disk, parsing only if it changed.
fn refresh(path: &Path, state: &mut State) -> Result<()> {
    match fsutil::read_private_file(path)? {
        None => {
            state.raw = None;
            state.peers.clear();
        }
        Some(bytes) => {
            if state.raw.as_deref() != Some(bytes.as_slice()) {
                state.peers = parse(path, &bytes)?;
                state.raw = Some(bytes);
            }
        }
    }
    Ok(())
}

fn parse(path: &Path, bytes: &[u8]) -> Result<BTreeMap<DeviceId, TrustedPeer>> {
    let corrupt = |reason: String| KdeError::StoreCorrupt {
        path: path.to_owned(),
        reason,
    };
    let file: FileFormat =
        serde_json::from_slice(bytes).map_err(|e| corrupt(format!("unreadable JSON ({e})")))?;
    if file.version != 1 {
        return Err(corrupt(format!("unsupported version {}", file.version)));
    }
    if file.peers.len() > MAX_PEERS {
        return Err(corrupt("too many entries".into()));
    }
    let mut out = BTreeMap::new();
    for p in file.peers {
        let id = DeviceId::parse(&p.device_id)
            .map_err(|_| corrupt("entry with an invalid device id".into()))?;
        let der = B64
            .decode(&p.cert_der_base64)
            .map_err(|_| corrupt(format!("{id}: bad certificate encoding")))?;
        let cert = CertInfo::parse(&der).map_err(|e| corrupt(format!("{id}: {e}")))?;
        if cert.fingerprint_hex() != p.cert_sha256 {
            return Err(corrupt(format!(
                "{id}: certificate does not match its recorded fingerprint"
            )));
        }
        if cert.common_name() != id.as_str() {
            return Err(corrupt(format!(
                "{id}: certificate common name differs from the device id"
            )));
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
            return Err(corrupt(format!("{id}: duplicate entry")));
        }
    }
    Ok(out)
}

impl TrustStore for FileTrustStore {
    fn get(&self, id: &DeviceId) -> Result<Option<TrustedPeer>> {
        let mut state = self.state.lock().unwrap();
        refresh(&self.path, &mut state)?;
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
        refresh(&self.path, &mut state)?;
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
    fn corrupt_files_are_errors_not_empty_stores() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        let s = FileTrustStore::open(&dir).unwrap();
        s.insert(peer('a')).unwrap();
        let path = s.path().to_owned();
        drop(s);
        let good = std::fs::read(&path).unwrap();
        for bad in [good[..good.len() / 2].to_vec(), b"{}".to_vec(), Vec::new()] {
            std::fs::write(&path, &bad).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                FileTrustStore::open(&dir),
                Err(KdeError::StoreCorrupt { .. })
            ));
            // The damaged file is left in place for the user.
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
    }

    #[test]
    fn tampered_certificate_or_fingerprint_is_rejected() {
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
        assert!(matches!(
            FileTrustStore::open(&dir),
            Err(KdeError::StoreCorrupt { .. })
        ));
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
