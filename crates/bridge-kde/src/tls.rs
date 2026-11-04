// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS configuration with per-connection peer verifiers.
//!
//! KDE peers use self-signed certificates, so public-CA validation is useless
//! and a plain "accept anything" verifier would be a hole. Instead every
//! connection gets its own verifier bound to the device id the peer *claims*:
//!
//! * `Pinned`: the presented end-entity certificate must be byte-identical to
//!   the one stored at pairing. Anything else is a hard failure; there is no
//!   fallback to the unpaired path.
//! * `Inspect`: used only for peers we have no record of. The certificate is
//!   accepted into the handshake so it can be shown to the user and bound to a
//!   pending pairing, but the link carries no feature traffic.
//!
//! In both modes the certificate must parse, carry a common name equal to the claimed device id, and the TLS handshake
//! signature must verify against that certificate's key (so possession of the
//! private key is proven, not just a copy of the certificate).

use std::sync::{Arc, Mutex};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::NoServerSessionStorage;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, ClientConfig, DigitallySignedStruct, DistinguishedName, Error as TlsError,
    ServerConfig, SignatureScheme, SupportedProtocolVersion,
};

use crate::cert::{CertInfo, DeviceCredentials, provider};
use crate::error::{KdeError, Result};
use crate::identity::DeviceId;

/// Which TLS versions to offer. Android pins TLS 1.2; Qt/OpenSSL desktops can
/// do either. Never 1.3-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsVersions {
    Tls12Only,
    Tls12And13,
}

impl TlsVersions {
    fn list(self) -> Vec<&'static SupportedProtocolVersion> {
        match self {
            Self::Tls12Only => vec![&rustls::version::TLS12],
            Self::Tls12And13 => vec![&rustls::version::TLS12, &rustls::version::TLS13],
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum PeerPolicy {
    Pinned(CertInfo),
    Inspect,
}

#[derive(Debug, Clone)]
pub(crate) enum VerifyFailure {
    Certificate(String),
    PinMismatch,
}

#[derive(Debug)]
pub(crate) struct PeerVerifier {
    policy: PeerPolicy,
    expected: DeviceId,
    algs: WebPkiSupportedAlgorithms,
    failure: Arc<Mutex<Option<VerifyFailure>>>,
}

impl PeerVerifier {
    pub(crate) fn new(
        policy: PeerPolicy,
        expected: DeviceId,
    ) -> (Arc<Self>, Arc<Mutex<Option<VerifyFailure>>>) {
        let failure = Arc::new(Mutex::new(None));
        let v = Arc::new(Self {
            policy,
            expected,
            algs: provider().signature_verification_algorithms,
            failure: failure.clone(),
        });
        (v, failure)
    }

    fn fail(&self, f: VerifyFailure, e: CertificateError) -> TlsError {
        *self.failure.lock().unwrap() = Some(f);
        TlsError::InvalidCertificate(e)
    }

    fn check(
        &self,
        end_entity: &CertificateDer<'_>,
        _now: UnixTime,
    ) -> std::result::Result<(), TlsError> {
        if let PeerPolicy::Pinned(pinned) = &self.policy
            && pinned.der() != end_entity.as_ref()
        {
            return Err(self.fail(
                VerifyFailure::PinMismatch,
                CertificateError::ApplicationVerificationFailure,
            ));
        }
        let info = CertInfo::parse(end_entity.as_ref()).map_err(|e| {
            self.fail(
                VerifyFailure::Certificate(e.to_string()),
                CertificateError::BadEncoding,
            )
        })?;
        if info.common_name() != self.expected.as_str() {
            return Err(self.fail(
                VerifyFailure::Certificate(
                    "certificate common name does not match the claimed device id".into(),
                ),
                CertificateError::NotValidForName,
            ));
        }
        // The validity period is not checked: the certificate is self-signed and, once paired, must
        // equal the pinned one byte for byte, so its dates carry no trust. Checking them only made a
        // phone or computer with a wrong clock (never set, year 2020) unable to connect to anyone.
        // Pairing has its own clock guard (the +-1800 s timestamp window on the pair request).
        Ok(())
    }
}

impl ServerCertVerifier for PeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, TlsError> {
        self.check(end_entity, now)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for PeerVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    /// A TLS server (= TCP initiator) always requires the peer certificate:
    /// without it there is nothing to pin or to bind a pairing to.
    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, TlsError> {
        self.check(end_entity, now)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// Presents our single certificate in either role.
#[derive(Debug)]
struct FixedCert(Arc<CertifiedKey>);

impl rustls::server::ResolvesServerCert for FixedCert {
    fn resolve(&self, _hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

impl rustls::client::ResolvesClientCert for FixedCert {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        sigschemes: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        self.0.key.choose_scheme(sigschemes).map(|_| self.0.clone())
    }

    fn has_certs(&self) -> bool {
        true
    }
}

fn tls_err(e: TlsError) -> KdeError {
    KdeError::Tls(e.to_string())
}

/// TLS server config (our side initiated the TCP connection).
pub(crate) fn server_config(
    creds: &DeviceCredentials,
    verifier: Arc<PeerVerifier>,
    versions: TlsVersions,
) -> Result<Arc<ServerConfig>> {
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&versions.list())
        .map_err(tls_err)?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(FixedCert(creds.certified_key())));
    cfg.session_storage = Arc::new(NoServerSessionStorage {});
    cfg.send_tls13_tickets = 0;
    Ok(Arc::new(cfg))
}

/// TLS client config (our side accepted the TCP connection).
pub(crate) fn client_config(
    creds: &DeviceCredentials,
    verifier: Arc<PeerVerifier>,
    versions: TlsVersions,
) -> Result<Arc<ClientConfig>> {
    let mut cfg = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&versions.list())
        .map_err(tls_err)?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(Arc::new(FixedCert(creds.certified_key())));
    cfg.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(cfg))
}

/// Convert a failed handshake into the most specific error we know.
pub(crate) fn handshake_error(
    err: std::io::Error,
    failure: &Mutex<Option<VerifyFailure>>,
    expected: &DeviceId,
) -> KdeError {
    match failure.lock().unwrap().clone() {
        Some(VerifyFailure::PinMismatch) => KdeError::PinMismatch {
            device_id: expected.to_string(),
        },
        Some(VerifyFailure::Certificate(m)) => KdeError::Certificate(m),
        None => KdeError::Tls(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: u64) -> UnixTime {
        UnixTime::since_unix_epoch(std::time::Duration::from_secs((year - 1970) * 365 * 86400))
    }

    #[test]
    fn a_wrong_clock_does_not_stop_a_pinned_or_new_peer_certificate() {
        let id = DeviceId::parse(&"e".repeat(32)).unwrap();
        let creds = DeviceCredentials::generate_with_id(&id).unwrap();
        let der = CertificateDer::from(creds.cert().der().to_vec());
        for policy in [
            PeerPolicy::Pinned(creds.cert().clone()),
            PeerPolicy::Inspect,
        ] {
            let (v, _) = PeerVerifier::new(policy, id.clone());
            for year in [1970, 2020, 2026, 2060, 2200] {
                v.check(&der, at(year))
                    .unwrap_or_else(|e| panic!("clock {year}: {e}"));
            }
        }
    }

    #[test]
    fn the_pin_and_the_claimed_name_still_decide_whatever_the_clock_says() {
        let id = DeviceId::parse(&"e".repeat(32)).unwrap();
        let other = DeviceId::parse(&"f".repeat(32)).unwrap();
        let mine = DeviceCredentials::generate_with_id(&id).unwrap();
        let stranger = DeviceCredentials::generate_with_id(&id).unwrap();
        let (pinned, failure) =
            PeerVerifier::new(PeerPolicy::Pinned(mine.cert().clone()), id.clone());
        let swapped = CertificateDer::from(stranger.cert().der().to_vec());
        assert!(pinned.check(&swapped, at(2020)).is_err());
        assert!(matches!(
            *failure.lock().unwrap(),
            Some(VerifyFailure::PinMismatch)
        ));
        let (wrong_name, _) = PeerVerifier::new(PeerPolicy::Inspect, other);
        let der = CertificateDer::from(mine.cert().der().to_vec());
        assert!(wrong_name.check(&der, at(2020)).is_err());
    }
}
