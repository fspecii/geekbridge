//! The daemon's private state: its own TLS identity, the trust store, per-device feature consent
//! and the control token. Everything lives in one owner-only directory and is opened through
//! `bridge_kde::fsutil`, which refuses loose permissions and symlinks. If any piece cannot be
//! opened safely the protected API is *unavailable*; nothing falls back to weaker storage.

use bridge_core::ipc::{self, Feature, Token};
use bridge_kde::{DeviceCredentials, DeviceId, FileTrustStore, KdeError, TrustStore, fsutil};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
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
    path: PathBuf,
    enabled: BTreeMap<String, Vec<Feature>>,
}

#[derive(Serialize, Deserialize)]
struct ConsentFile {
    version: u32,
    devices: BTreeMap<String, Vec<Feature>>,
}

impl ConsentStore {
    pub fn open(dir: &Path) -> Result<Self, KdeError> {
        let path = dir.join(CONSENT_FILE);
        let enabled = match fsutil::read_private_file(&path)? {
            None => BTreeMap::new(),
            Some(bytes) => {
                let corrupt = |reason: &str| KdeError::StoreCorrupt {
                    path: path.clone(),
                    reason: reason.into(),
                };
                let file: ConsentFile =
                    serde_json::from_slice(&bytes).map_err(|_| corrupt("unreadable JSON"))?;
                if file.version != 1 || file.devices.len() > MAX_CONSENT_DEVICES {
                    return Err(corrupt("unsupported version or too many entries"));
                }
                for id in file.devices.keys() {
                    DeviceId::parse(id).map_err(|_| corrupt("invalid device id"))?;
                }
                file.devices
            }
        };
        Ok(Self { path, enabled })
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
        self.persist(&next)?;
        self.enabled = next;
        Ok(())
    }

    pub fn forget(&mut self, device_id: &str) -> Result<(), KdeError> {
        if !self.enabled.contains_key(device_id) {
            return Ok(());
        }
        let mut next = self.enabled.clone();
        next.remove(device_id);
        self.persist(&next)?;
        self.enabled = next;
        Ok(())
    }

    fn persist(&self, devices: &BTreeMap<String, Vec<Feature>>) -> Result<(), KdeError> {
        let bytes = serde_json::to_vec_pretty(&ConsentFile {
            version: 1,
            devices: devices.clone(),
        })
        .map_err(|e| KdeError::Store(e.to_string()))?;
        fsutil::atomic_write_private(&self.path, &bytes)
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
            TokenProblem::Invalid => (
                Unavailable {
                    code: "token_invalid",
                    message: "the control token file is malformed; delete it to create a new one",
                },
                KdeError::Store("control token file is malformed".into()),
            ),
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
    Invalid,
    Random,
}

fn load_or_create_token(dir: &Path) -> Result<Token, TokenProblem> {
    let _lock =
        fsutil::lock_exclusive(&dir.join("control-token.lock")).map_err(TokenProblem::Kde)?;
    let path = ipc::token_path(dir);
    match fsutil::read_private_file(&path).map_err(TokenProblem::Kde)? {
        Some(bytes) => Token::parse(&bytes).map_err(|_| TokenProblem::Invalid),
        None => {
            let mut bytes = [0_u8; ipc::TOKEN_BYTES];
            getrandom::fill(&mut bytes).map_err(|_| TokenProblem::Random)?;
            let token = Token::from_bytes(bytes);
            let contents = token.to_file_contents();
            fsutil::atomic_write_private(&path, contents.as_bytes()).map_err(TokenProblem::Kde)?;
            Ok(token)
        }
    }
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
    fn malformed_token_is_reported_not_regenerated() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        PrivateState::open(&dir).unwrap_or_else(|e| panic!("{}", e.1));
        let token = ipc::token_path(&dir);
        std::fs::write(&token, b"not hex").unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            PrivateState::open(&dir).err().unwrap().0.code,
            "token_invalid"
        );
        assert_eq!(std::fs::read(&token).unwrap(), b"not hex");
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
