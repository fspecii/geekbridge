// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's private state: its own TLS identity, the trust store, per-device feature consent
//! and the control token. Everything lives in one owner-only directory and is opened through
//! `bridge_kde::fsutil`, which refuses loose permissions and symlinks. If any piece cannot be
//! opened safely the protected API is *unavailable*; nothing falls back to weaker storage.

use bridge_core::ipc::{self, Feature, Token};
use bridge_core::statefile::{Corrupt, Spec, Status};
use bridge_kde::{
    DeviceCredentials, DeviceId, FileTrustStore, KdeError, TrustStore, fsutil,
    fsutil::PrivateStateFile,
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

const CONSENT_FILE: &str = "consent.json";
const MAX_CONSENT_DEVICES: usize = 1024;

/// Why the protected API cannot run. `code` and `message` are safe to show to any local process
/// (they are sent in the unauthenticated hello): no paths, no secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable {
    pub code: &'static str,
    pub message: &'static str,
}

impl Unavailable {
    fn from_kde(err: &KdeError) -> Self {
        match err {
            KdeError::InsecurePermissions { .. } => Self {
                code: "state_insecure",
                message: "the state directory or one of its files is not private to this user",
            },
            KdeError::UnsupportedPlatform(_) => Self {
                code: "platform_unsupported",
                message: "owner-only state storage is not implemented on this platform",
            },
            KdeError::StoreNewer { .. } => Self {
                code: "state_newer",
                message: "a state file was written by a newer version; it is left untouched and changes are not saved",
            },
            KdeError::StoreCorrupt { .. } => Self {
                code: "trust_store_corrupt",
                message: "the trust store is unreadable and was left untouched",
            },
            KdeError::Certificate(_) => Self {
                code: "identity_unusable",
                message: "the device identity could not be loaded and was left untouched",
            },
            _ => Self {
                code: "state_error",
                message: "the private state directory could not be opened",
            },
        }
    }
}

/// What to tell the operator when the state directory is refused as not private. The directory
/// holds the identity key and the control token, so the service never "fixes" loose permissions by
/// itself; this says why it refused and how to repair it. Goes to the terminal, never to clients.
pub fn insecure_hint(windows: bool, dir: &Path) -> String {
    let d = dir.display();
    if windows {
        return format!(
            "hint: {d} already existed and Windows gave it permissions inherited from its parent folder (typical for a folder made in %TEMP% or by Explorer), so other accounts may be able to read the identity key and token. Either pick a --state-dir that does not exist yet (GeekBridge creates it private to your account), or protect this one once with: icacls \"{d}\" /inheritance:r /grant:r \"%USERNAME%:(OI)(CI)F\" \"SYSTEM:(OI)(CI)F\"  (then start the service again; if it still refuses, delete the folder and let GeekBridge create it)."
        );
    }
    format!(
        "hint: {d} or a file in it is readable by other users. Fix with: chmod 700 \"{d}\" && chmod 600 \"{d}\"/*  (or choose a --state-dir that does not exist yet; GeekBridge creates it private)."
    )
}

pub struct ConsentStore {
    file: PrivateStateFile,
    /// The file is from a newer build: readable, never rewritten.
    newer: Option<u32>,
    /// In-memory view. Held only for instants, never across file I/O, so `status` is never delayed
    /// by a writer waiting on the file lock or an fsync.
    view: Mutex<View>,
    /// Serialises read-modify-write cycles (held across file I/O; readers never take it).
    writer: Mutex<()>,
}

#[derive(Clone)]
struct View {
    enabled: BTreeMap<String, Vec<Feature>>,
    /// Devices whose `debug` consent was switched on by default once (so turning it off sticks).
    debug_defaulted: BTreeSet<String>,
}

/// `consent.json`: `{"version": 1, "devices": {"<device id>": ["Sms", ...]}}`. Unknown fields and
/// feature names from a newer build are ignored for decisions and kept on rewrite. A damaged file
/// without a usable backup means every feature is off (consent is opt-in), never an error.
pub static CONSENT_SPEC: Spec = Spec {
    name: CONSENT_FILE,
    current: 1,
    migrations: &[],
    validate: Some(validate_consent),
    on_corrupt: Corrupt::Reset,
};

fn validate_consent(body: &Map<String, Value>) -> Result<(), String> {
    let Some(Value::Object(devices)) = body.get("devices") else {
        return Err("no devices object".into());
    };
    if devices.len() > MAX_CONSENT_DEVICES {
        return Err("too many entries".into());
    }
    for (id, v) in devices {
        DeviceId::parse(id).map_err(|_| "invalid device id".to_string())?;
        if !v.is_array() {
            return Err("feature list is not an array".into());
        }
    }
    Ok(())
}

fn known_features(body: &Map<String, Value>) -> BTreeMap<String, Vec<Feature>> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(devices)) = body.get("devices") {
        for (id, v) in devices {
            let mut list: Vec<Feature> = v
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|f| serde_json::from_value(f.clone()).ok())
                .collect();
            list.sort();
            list.dedup();
            if !list.is_empty() {
                out.insert(id.clone(), list);
            }
        }
    }
    out
}

impl ConsentStore {
    pub fn open(dir: &Path) -> Result<Self, KdeError> {
        let file = PrivateStateFile::new(fsutil::PrivateIo, dir.join(CONSENT_FILE), &CONSENT_SPEC);
        let loaded = file.load()?;
        let newer = match loaded.status {
            Status::Newer { found } => Some(found),
            _ => None,
        };
        let debug_defaulted = loaded
            .body
            .get("debug_defaulted")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .filter(|id| DeviceId::parse(id).is_ok())
            .map(str::to_owned)
            .collect();
        Ok(Self {
            file,
            newer,
            view: Mutex::new(View {
                enabled: known_features(&loaded.body),
                debug_defaulted,
            }),
            writer: Mutex::new(()),
        })
    }

    /// The directory the consent file lives in (the service's private state directory).
    pub fn dir(&self) -> Option<&Path> {
        self.file.path().parent()
    }

    fn view(&self) -> MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn writer(&self) -> MutexGuard<'_, ()> {
        self.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A paired phone that advertises the debug packets (only a debug build does) gets the `debug` consent
    /// switched on, once. Later changes by the user are never overridden. True if it was switched on now.
    pub fn default_debug_on(&self, device_id: &str) -> Result<bool, KdeError> {
        let _writer = self.writer();
        let mut next = self.view().clone();
        if next.debug_defaulted.contains(device_id) {
            return Ok(false);
        }
        let entry = next.enabled.entry(device_id.to_owned()).or_default();
        let already = entry.contains(&Feature::Debug);
        if !already {
            entry.push(Feature::Debug);
            entry.sort();
        }
        next.debug_defaulted.insert(device_id.to_owned());
        self.persist(&next.enabled, &next.debug_defaulted, false)?;
        *self.view() = next;
        Ok(!already)
    }

    /// Consent is opt-in: anything not recorded is off.
    pub fn is_enabled(&self, device_id: &str, feature: Feature) -> bool {
        self.view()
            .enabled
            .get(device_id)
            .is_some_and(|f| f.contains(&feature))
    }

    pub fn set(&self, device_id: &str, feature: Feature, enabled: bool) -> Result<(), KdeError> {
        let _writer = self.writer();
        let mut next = self.view().clone();
        let entry = next.enabled.entry(device_id.to_owned()).or_default();
        entry.retain(|f| *f != feature);
        if enabled {
            entry.push(feature);
            entry.sort();
        }
        if entry.is_empty() {
            next.enabled.remove(device_id);
        }
        if next.enabled.len() > MAX_CONSENT_DEVICES {
            return Err(KdeError::Store("consent store is full".into()));
        }
        self.persist(&next.enabled, &next.debug_defaulted, false)?;
        *self.view() = next;
        Ok(())
    }

    pub fn forget(&self, device_id: &str) -> Result<(), KdeError> {
        let _writer = self.writer();
        let mut next = self.view().clone();
        if !next.enabled.contains_key(device_id) && !next.debug_defaulted.contains(device_id) {
            return Ok(());
        }
        next.enabled.remove(device_id);
        next.debug_defaulted.remove(device_id);
        self.persist(&next.enabled, &next.debug_defaulted, true)?;
        *self.view() = next;
        Ok(())
    }

    fn persist(
        &self,
        devices: &BTreeMap<String, Vec<Feature>>,
        defaulted: &BTreeSet<String>,
        // A forgotten phone must not come back from the `.bak` copy (and its id must not linger there).
        refresh_backup: bool,
    ) -> Result<(), KdeError> {
        let locked = self.file.lock()?;
        let loaded = locked.load()?;
        if let Some(found) = self.newer.or(match loaded.status {
            Status::Newer { found } => Some(found),
            _ => None,
        }) {
            return Err(KdeError::StoreNewer {
                path: self.file.path().to_owned(),
                found,
            });
        }
        let mut body = loaded.body;
        let old = match body.remove("devices") {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        let mut out = Map::new();
        for (id, features) in devices {
            // Feature names this build does not know stay as they were.
            let mut list: Vec<Value> = old
                .get(id)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|f| serde_json::from_value::<Feature>((*f).clone()).is_err())
                .cloned()
                .collect();
            for f in features {
                if let Ok(v) = serde_json::to_value(f) {
                    list.push(v);
                }
            }
            out.insert(id.clone(), Value::Array(list));
        }
        body.insert("devices".into(), Value::Object(out));
        if defaulted.is_empty() {
            body.remove("debug_defaulted");
        } else {
            body.insert("debug_defaulted".into(), serde_json::json!(defaulted));
        }
        if refresh_backup {
            locked.save_and_refresh_backup(&body)?;
        } else {
            locked.save(&body)?;
        }
        Ok(())
    }
}

pub struct PrivateState {
    pub dir: PathBuf,
    pub credentials: Arc<DeviceCredentials>,
    pub store: Arc<dyn TrustStore>,
    pub consents: ConsentStore,
    pub token: Token,
}

impl PrivateState {
    /// Open (creating on first run) every private file. Existing files are never regenerated.
    pub fn open(dir: &Path) -> Result<Self, (Unavailable, KdeError)> {
        let fail = |e: KdeError| (Unavailable::from_kde(&e), e);
        fsutil::ensure_private_dir(dir).map_err(fail)?;
        // Temporary files of writers that were killed mid-save.
        fsutil::sweep_dead_temps(dir);
        let (credentials, _created) = DeviceCredentials::load_or_create(dir).map_err(fail)?;
        let store = FileTrustStore::open(dir).map_err(fail)?;
        let consents = ConsentStore::open(dir).map_err(fail)?;
        let token = load_or_create_token(dir).map_err(|e| match e {
            TokenProblem::Kde(e) => fail(e),
            TokenProblem::Random => (
                Unavailable {
                    code: "random_unavailable",
                    message: "the system random source failed",
                },
                KdeError::Store("system random source failed".into()),
            ),
        })?;
        Ok(Self {
            dir: dir.to_owned(),
            credentials: Arc::new(credentials),
            store: Arc::new(store),
            consents,
            token,
        })
    }
}

enum TokenProblem {
    Kde(KdeError),
    Random,
}

fn load_or_create_token(dir: &Path) -> Result<Token, TokenProblem> {
    let _lock =
        fsutil::lock_exclusive(&dir.join("control-token.lock")).map_err(TokenProblem::Kde)?;
    let path = ipc::token_path(dir);
    // The token only guards the local control API and holds nothing worth recovering: a missing,
    // empty or damaged file is replaced (clients re-read it), with a diagnostics line.
    if let Some(bytes) = fsutil::read_private_file(&path).map_err(TokenProblem::Kde)? {
        match Token::parse(&bytes) {
            Ok(token) => return Ok(token),
            Err(_) => bridge_core::statefile::note(
                "ipc-token was damaged; created a new control token".into(),
            ),
        }
    }
    let mut bytes = [0_u8; ipc::TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| TokenProblem::Random)?;
    let token = Token::from_bytes(bytes);
    let contents = token.to_file_contents();
    fsutil::atomic_write_private(&path, contents.as_bytes()).map_err(TokenProblem::Kde)?;
    Ok(token)
}

/// 128 random bits as hex, for challenge ids and the like.
pub fn random_id() -> String {
    let mut bytes = [0_u8; 16];
    // A failing system RNG is unrecoverable; refusing to continue beats predictable ids.
    getrandom::fill(&mut bytes).expect("system random source failed");
    ipc::encode_hex(&bytes)
}

pub fn random_nonce() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("system random source failed");
    ipc::encode_hex(&bytes)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn concurrent_clients_reuse_one_control_token() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let tokens = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = barrier.clone();
                    let dir = dir.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        load_or_create_token(&dir)
                            .unwrap_or_else(|_| panic!("token initialization failed"))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(tokens.iter().all(|token| token == &tokens[0]));
    }

    #[test]
    fn creates_private_state_once_and_reuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let first = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
        let second = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
        assert_eq!(first.token, second.token);
        assert_eq!(
            first.credentials.device_id(),
            second.credentials.device_id()
        );
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&ipc::token_path(&dir)), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn the_insecure_hint_names_the_fix_for_each_system() {
        let w = insecure_hint(true, Path::new(r"C:\Temp\gb"));
        assert!(w.contains("icacls") && w.contains("/inheritance:r") && w.contains(r"C:\Temp\gb"));
        assert!(w.contains("does not exist yet") && w.contains("%TEMP%"));
        let u = insecure_hint(false, Path::new("/tmp/gb"));
        assert!(u.contains("chmod 700") && !u.contains("icacls"));
    }

    #[test]
    fn insecure_directory_or_token_makes_the_protected_state_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
        let token = ipc::token_path(&dir);
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = PrivateState::open(&dir).err().unwrap().0;
        assert_eq!(err.code, "state_insecure");
        assert!(!err.message.contains(tmp.path().to_str().unwrap()));
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            PrivateState::open(&dir).err().unwrap().0.code,
            "state_insecure"
        );
    }

    #[test]
    fn damaged_token_is_replaced_not_a_brick() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let first = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
        let token = ipc::token_path(&dir);
        for bad in [&b"not hex"[..], b""] {
            std::fs::write(&token, bad).unwrap();
            std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
            let again = PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
            assert_ne!(again.token, first.token);
            assert_eq!(
                again.credentials.device_id(),
                first.credentials.device_id(),
                "the identity is untouched"
            );
        }
    }

    /// `status` reads consents on an async worker; a writer waiting for the file lock (or an fsync)
    /// must not hold those reads up.
    #[test]
    fn consent_reads_are_not_blocked_by_a_writer_waiting_for_the_file_lock() {
        use std::time::{Duration, Instant};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = "c4".repeat(16);
        let store = Arc::new(ConsentStore::open(&dir).unwrap());
        let lock =
            fsutil::lock_exclusive(&bridge_core::statefile::lock_path(store.file.path())).unwrap();
        let writer = {
            let (store, id) = (store.clone(), id.clone());
            std::thread::spawn(move || store.set(&id, Feature::Sms, true))
        };
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        assert!(!store.is_enabled(&id, Feature::Sms));
        let took = started.elapsed();
        drop(lock);
        writer.join().unwrap().unwrap();
        assert!(took < Duration::from_millis(200), "reads waited {took:?}");
        assert!(store.is_enabled(&id, Feature::Sms));
    }

    #[test]
    fn debug_consent_defaults_on_once_and_the_users_off_sticks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = "c3".repeat(16);
        let store = ConsentStore::open(&dir).unwrap();
        assert!(
            !store.is_enabled(&id, Feature::Debug),
            "off until a debug phone shows up"
        );
        assert!(store.default_debug_on(&id).unwrap());
        assert!(store.is_enabled(&id, Feature::Debug));
        store.set(&id, Feature::Debug, false).unwrap();
        // A later connection of the same phone, even after a restart, must not switch it back on.
        assert!(!store.default_debug_on(&id).unwrap());
        let again = ConsentStore::open(&dir).unwrap();
        assert!(!again.default_debug_on(&id).unwrap());
        assert!(!again.is_enabled(&id, Feature::Debug));
        // Unpairing forgets it, so a re-paired debug phone starts on again.
        again.forget(&id).unwrap();
        assert!(again.default_debug_on(&id).unwrap());
        assert!(again.is_enabled(&id, Feature::Debug));
    }

    #[test]
    fn consent_file_keeps_unknown_data_and_never_clobbers_a_newer_format() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = "b2".repeat(16);
        let path = dir.join(CONSENT_FILE);
        let put = |text: String| {
            fsutil::atomic_write_private(&path, text.as_bytes()).unwrap();
        };
        // The golden format-1 file (plus a feature name and a field only a later build knows).
        put(format!(
            r#"{{"version":1,"extra":{{"a":1}},"devices":{{"{id}":["sms","clipboard","hologram"]}}}}"#
        ));
        let store = ConsentStore::open(&dir).unwrap();
        assert!(store.is_enabled(&id, Feature::Sms));
        assert!(store.is_enabled(&id, Feature::Clipboard));
        store.set(&id, Feature::Contacts, true).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["extra"]["a"], 1);
        let list = v["devices"][&id].as_array().unwrap();
        assert!(list.iter().any(|f| f == "hologram"), "{list:?}");
        assert!(list.iter().any(|f| f == "contacts"));
        // Newer format: readable, refuses to write, file untouched.
        put(format!(r#"{{"version":4,"devices":{{"{id}":["sms"]}}}}"#));
        let before = std::fs::read(&path).unwrap();
        let store = ConsentStore::open(&dir).unwrap();
        assert!(store.is_enabled(&id, Feature::Sms));
        assert!(matches!(
            store.set(&id, Feature::Contacts, true),
            Err(KdeError::StoreNewer { found: 4, .. })
        ));
        assert!(
            !store.is_enabled(&id, Feature::Contacts),
            "memory unchanged on failure"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        // Damage falls back to the previous generation, else to "everything off".
        put(format!(r#"{{"version":1,"devices":{{"{id}":["sms"]}}}}"#));
        let store = ConsentStore::open(&dir).unwrap();
        store.set(&id, Feature::Contacts, true).unwrap();
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let store = ConsentStore::open(&dir).unwrap();
        assert!(store.is_enabled(&id, Feature::Sms) && !store.is_enabled(&id, Feature::Contacts));
        let _ = std::fs::remove_file(dir.join("consent.json.bak"));
        let store = ConsentStore::open(&dir).unwrap();
        assert!(!store.is_enabled(&id, Feature::Sms));
    }

    #[test]
    fn consent_defaults_off_persists_and_forgets() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = "a".repeat(32);
        let store = ConsentStore::open(&dir).unwrap();
        for feature in Feature::ALL {
            assert!(!store.is_enabled(&id, feature));
        }
        store.set(&id, Feature::Sms, true).unwrap();
        assert!(
            ConsentStore::open(&dir)
                .unwrap()
                .is_enabled(&id, Feature::Sms)
        );
        assert!(
            !ConsentStore::open(&dir)
                .unwrap()
                .is_enabled(&id, Feature::Contacts)
        );
        store.set(&id, Feature::Sms, false).unwrap();
        assert!(
            !ConsentStore::open(&dir)
                .unwrap()
                .is_enabled(&id, Feature::Sms)
        );
        store.set(&id, Feature::Clipboard, true).unwrap();
        store.forget(&id).unwrap();
        assert!(
            !ConsentStore::open(&dir)
                .unwrap()
                .is_enabled(&id, Feature::Clipboard)
        );
        // The one-generation backup must not keep the forgotten phone either.
        let bak = std::fs::read_to_string(dir.join("consent.json.bak")).unwrap();
        assert!(!bak.contains(&id), "backup still names the phone: {bak}");
    }
}
