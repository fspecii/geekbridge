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
    sync::Arc,
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

pub struct ConsentStore {
    file: PrivateStateFile,
    enabled: BTreeMap<String, Vec<Feature>>,
    /// The file is from a newer build: readable, never rewritten.
    newer: Option<u32>,
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
            enabled: known_features(&loaded.body),
            file,
            newer,
            debug_defaulted,
        })
    }

    /// The directory the consent file lives in (the service's private state directory).
    pub fn dir(&self) -> Option<&Path> {
        self.file.path().parent()
    }

    /// A paired phone that advertises the debug packets (only a debug build does) gets the `debug` consent
    /// switched on, once. Later changes by the user are never overridden. True if it was switched on now.
    pub fn default_debug_on(&mut self, device_id: &str) -> Result<bool, KdeError> {
        if self.debug_defaulted.contains(device_id) {
            return Ok(false);
        }
        let mut next = self.enabled.clone();
        let entry = next.entry(device_id.to_owned()).or_default();
        let already = entry.contains(&Feature::Debug);
        if !already {
            entry.push(Feature::Debug);
            entry.sort();
        }
        let mut seen = self.debug_defaulted.clone();
        seen.insert(device_id.to_owned());
        self.persist(&next, &seen)?;
        self.enabled = next;
        self.debug_defaulted = seen;
        Ok(!already)
    }

    /// Consent is opt-in: anything not recorded is off.
    pub fn is_enabled(&self, device_id: &str, feature: Feature) -> bool {
        self.enabled
            .get(device_id)
            .is_some_and(|f| f.contains(&feature))
    }

    pub fn set(
        &mut self,
        device_id: &str,
        feature: Feature,
        enabled: bool,
    ) -> Result<(), KdeError> {
        let mut next = self.enabled.clone();
        let entry = next.entry(device_id.to_owned()).or_default();
        entry.retain(|f| *f != feature);
        if enabled {
            entry.push(feature);
            entry.sort();
        }
        if entry.is_empty() {
            next.remove(device_id);
        }
        if next.len() > MAX_CONSENT_DEVICES {
            return Err(KdeError::Store("consent store is full".into()));
        }
        self.persist(&next, &self.debug_defaulted)?;
        self.enabled = next;
        Ok(())
    }

    pub fn forget(&mut self, device_id: &str) -> Result<(), KdeError> {
        if !self.enabled.contains_key(device_id) && !self.debug_defaulted.contains(device_id) {
            return Ok(());
        }
        let mut next = self.enabled.clone();
        next.remove(device_id);
        let mut seen = self.debug_defaulted.clone();
        seen.remove(device_id);
        self.persist(&next, &seen)?;
        self.enabled = next;
        self.debug_defaulted = seen;
        Ok(())
    }

    fn persist(
        &self,
        devices: &BTreeMap<String, Vec<Feature>>,
        defaulted: &BTreeSet<String>,
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
        locked.save(&body)?;
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

    #[test]
    fn debug_consent_defaults_on_once_and_the_users_off_sticks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = "c3".repeat(16);
        let mut store = ConsentStore::open(&dir).unwrap();
        assert!(
            !store.is_enabled(&id, Feature::Debug),
            "off until a debug phone shows up"
        );
        assert!(store.default_debug_on(&id).unwrap());
        assert!(store.is_enabled(&id, Feature::Debug));
        store.set(&id, Feature::Debug, false).unwrap();
        // A later connection of the same phone, even after a restart, must not switch it back on.
        assert!(!store.default_debug_on(&id).unwrap());
        let mut again = ConsentStore::open(&dir).unwrap();
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
        let mut store = ConsentStore::open(&dir).unwrap();
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
        let mut store = ConsentStore::open(&dir).unwrap();
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
        let mut store = ConsentStore::open(&dir).unwrap();
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
        let mut store = ConsentStore::open(&dir).unwrap();
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
    }
}
