// SPDX-License-Identifier: GPL-3.0-or-later
//! Device certificates: parsing peer certificates, generating our own, and
//! persisting the private identity.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::crypto::{CryptoProvider, ring as ring_provider};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::sign::CertifiedKey;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::error::{KdeError, Result};
use crate::fsutil;
use crate::identity::DeviceId;

pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring_provider::default_provider())
}

/// The facts about a certificate this crate relies on. Parsing is deliberately
/// lenient about extensions (stock peers emit none) and strict about structure.
#[derive(Clone, PartialEq, Eq)]
pub struct CertInfo {
    der: Vec<u8>,
    common_name: String,
    spki_der: Vec<u8>,
    not_before: i64,
    not_after: i64,
}

impl fmt::Debug for CertInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertInfo")
            .field("cn", &self.common_name)
            .field("sha256", &self.fingerprint_hex())
            .finish()
    }
}

impl CertInfo {
    pub fn parse(der: &[u8]) -> Result<Self> {
        let bad = |m: &str| KdeError::Certificate(m.to_owned());
        let (rest, cert) =
            X509Certificate::from_der(der).map_err(|_| bad("not a valid X.509 certificate"))?;
        if !rest.is_empty() {
            return Err(bad("trailing bytes after certificate"));
        }
        let mut cns = cert.subject().iter_common_name();
        let cn = cns
            .next()
            .ok_or_else(|| bad("subject has no common name"))?;
        if cns.next().is_some() {
            return Err(bad("subject has several common names"));
        }
        let common_name = cn
            .as_str()
            .map_err(|_| bad("common name is not a string"))?
            .to_owned();
        let validity = cert.validity();
        Ok(Self {
            der: der.to_vec(),
            common_name,
            spki_der: cert.tbs_certificate.subject_pki.raw.to_vec(),
            not_before: validity.not_before.timestamp(),
            not_after: validity.not_after.timestamp(),
        })
    }

    pub fn der(&self) -> &[u8] {
        &self.der
    }

    pub fn common_name(&self) -> &str {
        &self.common_name
    }

    /// DER `SubjectPublicKeyInfo`, the input of the pairing verification key.
    pub fn spki_der(&self) -> &[u8] {
        &self.spki_der
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(&self.der).into()
    }

    /// Lower-case colon-free hex SHA-256 of the DER certificate.
    pub fn fingerprint_hex(&self) -> String {
        hex(&self.fingerprint())
    }

    pub fn check_validity(&self, now_unix: i64) -> Result<()> {
        if now_unix < self.not_before {
            return Err(KdeError::Certificate("certificate is not yet valid".into()));
        }
        if now_unix > self.not_after {
            return Err(KdeError::Certificate("certificate has expired".into()));
        }
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Our own certificate and signing key. The private key is only ever held as a
/// rustls signing object (no raw bytes after construction) and `Debug` never
/// prints it.
#[derive(Clone)]
pub struct DeviceCredentials {
    device_id: DeviceId,
    info: CertInfo,
    certified: Arc<CertifiedKey>,
}

impl fmt::Debug for DeviceCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCredentials")
            .field("device_id", &self.device_id)
            .field("cert", &self.info)
            .field("key", &"<redacted>")
            .finish()
    }
}

/// PEM text of a freshly generated identity. Contains the private key.
struct GeneratedIdentity {
    pem: zeroize::Zeroizing<String>,
}

impl DeviceCredentials {
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub fn cert(&self) -> &CertInfo {
        &self.info
    }

    pub(crate) fn certified_key(&self) -> Arc<CertifiedKey> {
        self.certified.clone()
    }

    /// Generate a new identity with a random device id (not persisted).
    pub fn generate() -> Result<Self> {
        let id = random_device_id()?;
        let generated = generate_pem(&id, time::OffsetDateTime::now_utc())?;
        Self::from_pem(generated.pem.as_bytes(), unix_now())
    }

    /// Parse PEM holding exactly one certificate and one private key.
    /// Fails if the key does not belong to the certificate, the common name is
    /// not a valid device id, or the certificate is outside its validity.
    pub fn from_pem(pem: &[u8], now_unix: i64) -> Result<Self> {
        Self::parse_pem(pem, Some(now_unix))
    }

    /// [`Self::from_pem`]; with `None` the validity period is not checked (to tell a damaged file
    /// from a good key whose certificate the clock currently calls expired).
    fn parse_pem(pem: &[u8], now_unix: Option<i64>) -> Result<Self> {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem)
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| {
                KdeError::Certificate("identity file has an unreadable certificate".into())
            })?;
        let [cert] = <[CertificateDer<'static>; 1]>::try_from(certs).map_err(|_| {
            KdeError::Certificate("identity file must contain exactly one certificate".into())
        })?;
        let key = PrivateKeyDer::from_pem_slice(pem).map_err(|_| {
            KdeError::Certificate("identity file has no readable private key".into())
        })?;
        let info = CertInfo::parse(cert.as_ref())?;
        let device_id = DeviceId::parse(info.common_name())?;
        if let Some(now_unix) = now_unix {
            info.check_validity(now_unix).map_err(|e| {
                KdeError::Certificate(format!(
                    "own certificate unusable ({e}); it is never rotated silently"
                ))
            })?;
        }
        let signing = provider()
            .key_provider
            .load_private_key(key)
            .map_err(|_| KdeError::Certificate("unsupported private key".into()))?;
        let certified = CertifiedKey::new(vec![cert], signing);
        certified.keys_match().map_err(|_| {
            KdeError::Certificate("private key does not match the certificate".into())
        })?;
        Ok(Self {
            device_id,
            info,
            certified: Arc::new(certified),
        })
    }

    /// Load `<dir>/identity.pem`, or create it atomically with mode 0600 if absent.
    ///
    /// A good identity is never regenerated: one whose certificate the clock calls expired is an
    /// error, not a reason to rotate. A one-generation copy lives in `identity.pem.bak` (owner-only
    /// like the original). If the file is missing or its *content* is damaged (empty, truncated,
    /// not a certificate and key) the backup is restored; only when there is no usable backup is a
    /// new identity generated (the damaged file is kept as `identity.pem.corrupt`, and a diagnostics
    /// line says that every device must be paired again). An unreadable file is an error.
    pub fn load_or_create(dir: &Path) -> Result<(Self, bool)> {
        fsutil::ensure_private_dir(dir)?;
        // Serialize the check/create sequence across daemon, CLI and mobile startup.
        // Atomic rename alone cannot stop two callers from returning different identities.
        let _lock = fsutil::lock_exclusive(&dir.join("identity.pem.lock"))?;
        let path = identity_path(dir);
        let bak = bridge_core::statefile::backup_path(&path);
        let now = unix_now();
        let mut damaged = None;
        match fsutil::read_private_file(&path) {
            Ok(Some(bytes)) => match Self::from_pem(&bytes, now) {
                Ok(c) => {
                    // Keep the rollback copy current (installs from before it existed get one).
                    if fsutil::read_private_file(&bak).ok().flatten().as_deref()
                        != Some(bytes.as_slice())
                    {
                        let _ = fsutil::atomic_write_private(&bak, &bytes);
                    }
                    return Ok((c, false));
                }
                // Good key, wrong clock (or similar): never rotate.
                Err(e) if Self::parse_pem(&bytes, None).is_ok() => return Err(e),
                Err(e) => damaged = Some((Some(bytes), e.to_string())),
            },
            Ok(None) => {}
            // Unreadable is not the same as gone: the key may be fine, so do not replace it.
            Err(e @ (KdeError::Io(_) | KdeError::InsecurePermissions { .. })) => {
                if let Ok(Some(b)) = fsutil::read_private_file(&bak)
                    && let Ok(c) = Self::from_pem(&b, now)
                {
                    bridge_core::statefile::note(format!(
                        "identity.pem is unreadable ({e}); using identity.pem.bak for this run"
                    ));
                    return Ok((c, false));
                }
                return Err(e);
            }
            Err(e) => return Err(e),
        }
        if let Ok(Some(b)) = fsutil::read_private_file(&bak)
            && let Ok(c) = Self::from_pem(&b, now)
        {
            if let Some((Some(bad), _)) = &damaged {
                let _ =
                    fsutil::atomic_write_private(&bridge_core::statefile::corrupt_path(&path), bad);
            }
            fsutil::atomic_write_private(&path, &b)?;
            bridge_core::statefile::note(match &damaged {
                Some((_, why)) => format!(
                    "identity.pem was damaged ({why}); restored from identity.pem.bak, pairings are kept"
                ),
                None => {
                    "identity.pem was missing; restored from identity.pem.bak, pairings are kept"
                        .into()
                }
            });
            return Ok((c, false));
        }
        if let Some((bytes, why)) = damaged {
            let blank = bytes
                .as_ref()
                .is_none_or(|b| b.iter().all(u8::is_ascii_whitespace));
            if !blank && let Some(b) = &bytes {
                let _ =
                    fsutil::atomic_write_private(&bridge_core::statefile::corrupt_path(&path), b);
            }
            bridge_core::statefile::note(format!(
                "identity.pem was damaged ({why}) and no backup exists; generated a new identity. \
                 Every device must be paired again{}",
                if blank {
                    ""
                } else {
                    " (the damaged file is kept as identity.pem.corrupt)"
                }
            ));
        }
        let id = random_device_id()?;
        let generated = generate_pem(&id, time::OffsetDateTime::now_utc())?;
        fsutil::atomic_write_private(&path, generated.pem.as_bytes())?;
        let _ = fsutil::atomic_write_private(&bak, generated.pem.as_bytes());
        Ok((Self::from_pem(generated.pem.as_bytes(), unix_now())?, true))
    }

    /// Test fixtures only: PEM (certificate + private key) for `id` that stays valid for
    /// `valid_years`, so golden files do not expire under the test suite.
    #[doc(hidden)]
    pub fn fixture_pem(id: &DeviceId, valid_years: i64) -> Result<String> {
        let now = time::OffsetDateTime::now_utc();
        let g = generate_pem_between(
            id,
            now - time::Duration::days(1),
            now + time::Duration::days(365 * valid_years),
        )?;
        Ok(g.pem.as_str().to_owned())
    }

    /// Test/dev helper: generate with a chosen device id.
    pub fn generate_with_id(id: &DeviceId) -> Result<Self> {
        let generated = generate_pem(id, time::OffsetDateTime::now_utc())?;
        Self::from_pem(generated.pem.as_bytes(), unix_now())
    }
}

pub fn identity_path(dir: &Path) -> PathBuf {
    dir.join("identity.pem")
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn random_bytes(buf: &mut [u8]) -> Result<()> {
    provider()
        .secure_random
        .fill(buf)
        .map_err(|_| KdeError::Certificate("system random source failed".into()))
}

fn random_device_id() -> Result<DeviceId> {
    let mut b = [0u8; 16];
    random_bytes(&mut b)?;
    DeviceId::parse(&hex(&b))
}

fn generate_pem(id: &DeviceId, now: time::OffsetDateTime) -> Result<GeneratedIdentity> {
    // Same validity convention as stock peers: a year of backdating, ten years forward.
    generate_pem_between(
        id,
        now - time::Duration::days(365),
        now + time::Duration::days(3650),
    )
}

fn generate_pem_between(
    id: &DeviceId,
    not_before: time::OffsetDateTime,
    not_after: time::OffsetDateTime,
) -> Result<GeneratedIdentity> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256, SerialNumber,
    };
    let gen_err = |_| KdeError::Certificate("certificate generation failed".into());
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(gen_err)?;
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, id.as_str());
    dn.push(DnType::OrganizationalUnitName, "GeekBridge");
    dn.push(DnType::OrganizationName, "GeekBridge");
    params.distinguished_name = dn;
    params.not_before = not_before;
    params.not_after = not_after;
    let mut serial = [0u8; 16];
    random_bytes(&mut serial)?;
    serial[0] &= 0x7f;
    serial[0] |= 0x01; // positive, non-zero
    params.serial_number = Some(SerialNumber::from_slice(&serial));
    let cert = params.self_signed(&key).map_err(gen_err)?;
    let mut pem = zeroize::Zeroizing::new(String::new());
    pem.push_str(&cert.pem());
    pem.push_str(&key.serialize_pem());
    Ok(GeneratedIdentity { pem })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_is_self_consistent() {
        let c = DeviceCredentials::generate().unwrap();
        assert_eq!(c.cert().common_name(), c.device_id().as_str());
        assert!(!format!("{c:?}").contains("PRIVATE"));
        assert!(format!("{c:?}").contains("redacted"));
        assert_eq!(c.cert().fingerprint_hex().len(), 64);
        assert!(!c.cert().spki_der().is_empty());
    }

    #[test]
    fn key_must_match_certificate() {
        let a = generate_pem(
            &DeviceId::parse(&"a".repeat(32)).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
        let b = generate_pem(
            &DeviceId::parse(&"b".repeat(32)).unwrap(),
            time::OffsetDateTime::now_utc(),
        )
        .unwrap();
        let cert_a = a
            .pem
            .split("-----BEGIN PRIVATE KEY-----")
            .next()
            .unwrap()
            .to_owned();
        let key_b = format!(
            "-----BEGIN PRIVATE KEY-----{}",
            b.pem.split("-----BEGIN PRIVATE KEY-----").nth(1).unwrap()
        );
        let mixed = format!("{cert_a}{key_b}");
        assert!(DeviceCredentials::from_pem(mixed.as_bytes(), unix_now()).is_err());
    }

    #[test]
    fn expired_or_future_own_certificate_is_refused() {
        let id = DeviceId::parse(&"c".repeat(32)).unwrap();
        let old = generate_pem(
            &id,
            time::OffsetDateTime::now_utc() - time::Duration::days(4000),
        )
        .unwrap();
        assert!(DeviceCredentials::from_pem(old.pem.as_bytes(), unix_now()).is_err());
        let fresh = generate_pem(&id, time::OffsetDateTime::now_utc()).unwrap();
        assert!(
            DeviceCredentials::from_pem(fresh.pem.as_bytes(), unix_now() - 3 * 365 * 86400)
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn identity_survives_damage_through_its_backup() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        let (first, created) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert!(created);
        let path = identity_path(&dir);
        let bak = bridge_core::statefile::backup_path(&path);
        assert_eq!(std::fs::read(&path).unwrap(), std::fs::read(&bak).unwrap());
        let good = std::fs::read(&path).unwrap();
        for bad in [&b""[..], b"-----BEGIN CERT", b"garbage\n"] {
            std::fs::write(&path, bad).unwrap();
            let (again, created) = DeviceCredentials::load_or_create(&dir).unwrap();
            assert!(!created);
            assert_eq!(again.cert().fingerprint(), first.cert().fingerprint());
            assert_eq!(std::fs::read(&path).unwrap(), good, "primary repaired");
        }
        std::fs::remove_file(&path).unwrap();
        let (again, _) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert_eq!(again.cert().fingerprint(), first.cert().fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn damaged_identity_without_backup_is_replaced_loudly_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        let (first, _) = DeviceCredentials::load_or_create(&dir).unwrap();
        let path = identity_path(&dir);
        std::fs::remove_file(bridge_core::statefile::backup_path(&path)).unwrap();
        std::fs::write(&path, b"half a key").unwrap();
        let (second, created) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert!(created);
        assert_ne!(second.cert().fingerprint(), first.cert().fingerprint());
        assert_eq!(
            std::fs::read(bridge_core::statefile::corrupt_path(&path)).unwrap(),
            b"half a key"
        );
        assert!(
            bridge_core::statefile::notes()
                .iter()
                .any(|n| n.contains("paired again"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_expired_but_intact_identity_is_never_rotated() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        fsutil::ensure_private_dir(&dir).unwrap();
        let id = DeviceId::parse(&"d".repeat(32)).unwrap();
        let old = generate_pem(
            &id,
            time::OffsetDateTime::now_utc() - time::Duration::days(4000),
        )
        .unwrap();
        let path = identity_path(&dir);
        fsutil::atomic_write_private(&path, old.pem.as_bytes()).unwrap();
        assert!(matches!(
            DeviceCredentials::load_or_create(&dir),
            Err(KdeError::Certificate(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), old.pem.as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_initialization_returns_one_persisted_identity() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("state");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let barrier = barrier.clone();
                    let dir = dir.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        let (credentials, created) =
                            DeviceCredentials::load_or_create(&dir).unwrap();
                        (credentials.cert().fingerprint(), created)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|(_, created)| *created).count(), 1);
        assert!(
            results
                .iter()
                .all(|(fingerprint, _)| *fingerprint == results[0].0)
        );
        let (stored, created) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert!(!created);
        assert_eq!(stored.cert().fingerprint(), results[0].0);
    }

    #[test]
    fn load_or_create_persists_and_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("id");
        let (a, created) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert!(created);
        let (b, created) = DeviceCredentials::load_or_create(&dir).unwrap();
        assert!(!created);
        assert_eq!(a.cert().der(), b.cert().der());
        let on_disk = std::fs::read_to_string(identity_path(&dir)).unwrap();
        assert!(on_disk.contains("PRIVATE KEY"));
    }

    #[test]
    fn rejects_garbage_certificates() {
        assert!(CertInfo::parse(b"").is_err());
        assert!(CertInfo::parse(&[0x30, 0x03, 0x01, 0x01, 0x00]).is_err());
        let c = DeviceCredentials::generate().unwrap();
        let mut padded = c.cert().der().to_vec();
        padded.push(0);
        assert!(CertInfo::parse(&padded).is_err());
    }
}
