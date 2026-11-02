// SPDX-License-Identifier: GPL-3.0-or-later
//! Handshake and link abuse cases that `loopback.rs` does not reach: dialling ourselves, a stored protocol
//! version newer than the one claimed (downgrade), and a trusted link refusing reserved or malformed outgoing
//! and incoming packets. The peer is a hand-written rustls endpoint, not `bridge_kde`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bridge_kde::*;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring as ring_provider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsConnector;

async fn within<F: std::future::Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out")
}

fn prov() -> Arc<CryptoProvider> {
    Arc::new(ring_provider::default_provider())
}

#[derive(Debug)]
struct AcceptAll(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAll {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

struct RawPeer {
    id: String,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

fn raw_peer(c: char) -> RawPeer {
    let id = c.to_string().repeat(32);
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, &id);
    params.distinguished_name = dn;
    let cert = params.self_signed(&key).unwrap();
    RawPeer {
        id,
        cert: cert.der().clone(),
        key: PrivatePkcs8KeyDer::from(key.serialize_der()),
    }
}

fn config() -> TransportConfig {
    TransportConfig {
        address_policy: AddressPolicy {
            local_networks: true,
            loopback: true,
        },
        timeouts: Timeouts {
            connect: Duration::from_secs(5),
            plaintext_identity: Duration::from_secs(3),
            tls_handshake: Duration::from_secs(5),
            secure_identity: Duration::from_secs(3),
            write: Duration::from_secs(3),
        },
        ..TransportConfig::default()
    }
}

fn endpoint(store: Arc<dyn TrustStore>) -> Arc<Endpoint> {
    Arc::new(
        Endpoint::new(
            Arc::new(DeviceCredentials::generate().unwrap()),
            "Desk",
            DeviceType::Desktop,
            vec!["kdeconnect.ping".into()],
            vec!["kdeconnect.ping".into()],
            store,
            config(),
        )
        .unwrap(),
    )
}

fn record(p: &RawPeer, protocol_version: u32) -> TrustedPeer {
    TrustedPeer {
        device_id: DeviceId::parse(&p.id).unwrap(),
        name: "Raw Phone".into(),
        device_type: "phone".into(),
        protocol_version,
        cert: CertInfo::parse(p.cert.as_ref()).unwrap(),
        paired_at_unix: 1,
    }
}

fn identity_line(id: &str, extra: Value) -> Vec<u8> {
    let mut body = json!({
        "deviceId": id, "deviceName": "Raw Phone", "protocolVersion": 8, "deviceType": "phone",
        "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": ["kdeconnect.ping"]
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    format!(
        "{}\n",
        json!({"id": 1, "type": "kdeconnect.identity", "body": body})
    )
    .into_bytes()
}

async fn read_json_line<R: tokio::io::AsyncRead + Unpin>(r: &mut R) -> Value {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        let n = tokio::time::timeout(Duration::from_secs(5), r.read(&mut b))
            .await
            .expect("peer line in time")
            .unwrap();
        assert_eq!(n, 1, "unexpected EOF");
        if b[0] == b'\n' {
            return serde_json::from_slice(&line).unwrap();
        }
        line.push(b[0]);
    }
}

async fn listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

type RawTls = tokio_rustls::client::TlsStream<TcpStream>;

/// The phone side of a link we dialled: accept the TCP connection, read our plaintext identity, run the TLS
/// client with the phone's certificate and exchange secure identities.
async fn phone_accepts_our_dial(l: &TcpListener, p: &RawPeer) -> RawTls {
    let (mut tcp, addr) = l.accept().await.unwrap();
    let _plain = read_json_line(&mut tcp).await;
    let cfg = Arc::new(
        ClientConfig::builder_with_provider(prov())
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAll(prov())))
            .with_client_auth_cert(vec![p.cert.clone()], PrivateKeyDer::from(p.key.clone_key()))
            .unwrap(),
    );
    let mut tls = TlsConnector::from(cfg)
        .connect(ServerName::IpAddress(addr.ip().into()), tcp)
        .await
        .expect("TLS client handshake");
    tls.write_all(&identity_line(&p.id, json!({})))
        .await
        .unwrap();
    let _secure = read_json_line(&mut tls).await;
    tls
}

#[tokio::test]
async fn dialling_our_own_device_id_is_refused_before_any_trust_lookup() {
    let ep = endpoint(Arc::new(MemoryTrustStore::new()));
    let own = ep.local_identity().device_id.clone();
    let (l, addr) = listener().await;
    let _keep = l; // the TCP connect succeeds; the refusal comes before a byte is written
    let err = within(ep.connect(addr, &own)).await.err().unwrap();
    assert!(matches!(err, KdeError::TargetMismatch(_)), "{err}");
}

#[tokio::test]
async fn stored_protocol_version_newer_than_claimed_is_a_downgrade_and_refused() {
    let store = Arc::new(MemoryTrustStore::new());
    let phone = raw_peer('d');
    store.insert(record(&phone, 9)).unwrap();
    let ep = endpoint(store.clone());
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.err().unwrap()
        }
    });
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    let me = ep.local_identity().device_id.to_string();
    tcp.write_all(&identity_line(
        &phone.id,
        json!({"targetDeviceId": me, "targetProtocolVersion": 8}),
    ))
    .await
    .unwrap();
    let err = within(acc).await.unwrap();
    assert!(
        matches!(
            err,
            KdeError::ProtocolDowngrade {
                stored: 9,
                claimed: 8
            }
        ),
        "{err}"
    );
    // Nothing was changed or unpinned by the refused attempt, and no TLS bytes were offered.
    assert_eq!(
        store
            .get(&DeviceId::parse(&phone.id).unwrap())
            .unwrap()
            .unwrap()
            .protocol_version,
        9
    );
    let mut b = [0u8; 8];
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut b)).await,
        Ok(Ok(0) | Err(_))
    ));
}

#[tokio::test]
async fn trusted_link_refuses_reserved_and_inconsistent_outgoing_packets() {
    let store = Arc::new(MemoryTrustStore::new());
    let phone = raw_peer('e');
    store.insert(record(&phone, 8)).unwrap();
    let ep = endpoint(store);
    let (l, addr) = listener().await;
    let peer = DeviceId::parse(&phone.id).unwrap();
    let dial = tokio::spawn({
        let ep = ep.clone();
        async move { ep.connect(addr, &peer).await }
    });
    let mut tls = within(phone_accepts_our_dial(&l, &phone)).await;
    let Handshake::Trusted(mut link) = within(dial).await.unwrap().unwrap() else {
        panic!("a pinned peer must produce a trusted link");
    };

    let mut with_size = Packet::new("kdeconnect.share.request", Default::default());
    with_size.payload_size = Some(10);
    let mut with_info = Packet::new("kdeconnect.share.request", Default::default());
    with_info.payload_transfer_info = Some(json!({"port": 1739}).as_object().unwrap().clone());
    let plain = Packet::new("kdeconnect.ping", Default::default());
    for reserved in ["kdeconnect.pair", "kdeconnect.identity"] {
        let p = Packet::new(reserved, Default::default());
        assert!(
            matches!(link.send(&p).await, Err(KdeError::MalformedPacket(_))),
            "send {reserved}"
        );
        assert!(
            matches!(
                link.send_with_payload(&p).await,
                Err(KdeError::MalformedPacket(_))
            ),
            "send_with_payload {reserved}"
        );
    }
    // A packet that announces a payload must go through send_with_payload, and the reverse.
    assert!(matches!(
        link.send(&with_size).await,
        Err(KdeError::MalformedPacket(_))
    ));
    assert!(matches!(
        link.send(&with_info).await,
        Err(KdeError::MalformedPacket(_))
    ));
    assert!(matches!(
        link.send_with_payload(&plain).await,
        Err(KdeError::MalformedPacket(_))
    ));
    let mut half = with_size.clone();
    half.payload_transfer_info = None;
    assert!(matches!(
        link.send_with_payload(&half).await,
        Err(KdeError::MalformedPacket(_))
    ));

    // None of the refused packets reached the wire: the next line the phone sees is the ping we do send.
    link.send(&plain).await.unwrap();
    assert_eq!(read_json_line(&mut tls).await["type"], "kdeconnect.ping");
}

#[tokio::test]
async fn trusted_link_treats_a_second_identity_packet_as_fatal() {
    let store = Arc::new(MemoryTrustStore::new());
    let phone = raw_peer('f');
    store.insert(record(&phone, 8)).unwrap();
    let ep = endpoint(store);
    let (l, addr) = listener().await;
    let peer = DeviceId::parse(&phone.id).unwrap();
    let dial = tokio::spawn({
        let ep = ep.clone();
        async move { ep.connect(addr, &peer).await }
    });
    let mut tls = within(phone_accepts_our_dial(&l, &phone)).await;
    let Handshake::Trusted(mut link) = within(dial).await.unwrap().unwrap() else {
        panic!("expected a trusted link");
    };
    tls.write_all(&identity_line(&phone.id, json!({})))
        .await
        .unwrap();
    let err = within(link.recv()).await.unwrap_err();
    assert!(matches!(err, KdeError::MalformedPacket(_)), "{err}");
}
