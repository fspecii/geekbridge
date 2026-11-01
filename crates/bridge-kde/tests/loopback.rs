//! Loopback integration tests with generated certificates.
//!
//! The "peer" in most tests is a hand-written raw rustls endpoint that does not
//! use `bridge_kde` at all, so a role mix-up or framing bug in the crate cannot
//! be mirrored on both sides. Nothing here talks to a real KDE Connect device.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge_kde::*;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, ring as ring_provider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, DigitallySignedStruct, DistinguishedName as RDn, ServerConfig, SignatureScheme,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio_rustls::{TlsAcceptor, TlsConnector};

async fn within<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(30), f)
        .await
        .expect("test timed out")
}

// ---------------------------------------------------------------------------
// Raw peer (no bridge_kde)
// ---------------------------------------------------------------------------

struct RawPeer {
    id: String,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

fn gen_cert(cn: &str) -> (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>) {
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, cn);
    params.distinguished_name = dn;
    let cert = params.self_signed(&key).unwrap();
    (
        cert.der().clone(),
        PrivatePkcs8KeyDer::from(key.serialize_der()),
    )
}

fn raw_peer(c: char) -> RawPeer {
    let id = c.to_string().repeat(32);
    let (cert, key) = gen_cert(&id);
    RawPeer { id, cert, key }
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

impl ClientCertVerifier for AcceptAll {
    fn root_hint_subjects(&self) -> &[RDn] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
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

#[derive(Clone, Copy)]
enum V {
    Tls12,
    Both,
}

fn versions(v: V) -> Vec<&'static rustls::SupportedProtocolVersion> {
    match v {
        V::Tls12 => vec![&rustls::version::TLS12],
        V::Both => vec![&rustls::version::TLS12, &rustls::version::TLS13],
    }
}

/// The raw peer presents `cert`/`key` (which may deliberately not belong together).
fn raw_client_cfg(
    cert: &CertificateDer<'static>,
    key: &PrivatePkcs8KeyDer<'static>,
    v: V,
) -> Arc<ClientConfig> {
    Arc::new(
        ClientConfig::builder_with_provider(prov())
            .with_protocol_versions(&versions(v))
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAll(prov())))
            .with_client_auth_cert(vec![cert.clone()], PrivateKeyDer::from(key.clone_key()))
            .unwrap(),
    )
}

fn raw_server_cfg(
    cert: &CertificateDer<'static>,
    key: &PrivatePkcs8KeyDer<'static>,
    v: V,
) -> Arc<ServerConfig> {
    Arc::new(
        ServerConfig::builder_with_provider(prov())
            .with_protocol_versions(&versions(v))
            .unwrap()
            .with_client_cert_verifier(Arc::new(AcceptAll(prov())))
            .with_single_cert(vec![cert.clone()], PrivateKeyDer::from(key.clone_key()))
            .unwrap(),
    )
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;

async fn raw_read_line<R: AsyncRead + Unpin>(r: &mut R) -> Option<Value> {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        match tokio::time::timeout(Duration::from_secs(5), r.read(&mut b)).await {
            Ok(Ok(1)) => {
                if b[0] == b'\n' {
                    return Some(serde_json::from_slice(&line).expect("peer sent valid JSON"));
                }
                line.push(b[0]);
            }
            _ => return None,
        }
    }
}

/// Like `raw_read_line` but gives up quickly: used to assert that nothing arrives.
async fn raw_try_read_line<R: AsyncRead + Unpin>(r: &mut R, wait: Duration) -> Option<Value> {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        match tokio::time::timeout(wait, r.read(&mut b)).await {
            Ok(Ok(1)) => {
                if b[0] == b'\n' {
                    return Some(serde_json::from_slice(&line).unwrap());
                }
                line.push(b[0]);
            }
            _ => return None,
        }
    }
}

async fn raw_send<W: AsyncWrite + Unpin>(w: &mut W, kind: &str, body: Value) {
    let line = format!("{}\n", json!({"id": 1, "type": kind, "body": body}));
    w.write_all(line.as_bytes()).await.unwrap();
    w.flush().await.unwrap();
}

fn identity_body(id: &str, name: &str, extra: Value) -> Value {
    let mut b = json!({
        "deviceId": id, "deviceName": name, "protocolVersion": 8, "deviceType": "phone",
        "incomingCapabilities": ["kdeconnect.ping"], "outgoingCapabilities": ["kdeconnect.ping"]
    });
    if let Value::Object(e) = extra {
        b.as_object_mut().unwrap().extend(e);
    }
    b
}

#[derive(Default, Clone)]
struct Opts {
    /// Device id to send in the secure identity instead of the real one.
    secure_id: Option<String>,
    /// Send an oversize secure identity.
    secure_oversize: bool,
    /// Skip the secure identity entirely.
    secure_silent: bool,
    /// `targetDeviceId` to put in the plaintext identity (initiator only).
    target_override: Option<String>,
    /// Raw plaintext line to send instead of an identity (initiator only).
    plaintext_raw: Option<Vec<u8>>,
}

struct Session {
    io: BoxIo,
    plaintext: Option<Value>,
    secure: Option<Value>,
    version: String,
    seen_cert: Vec<u8>,
}

/// Raw peer acting as TCP **acceptor** (TLS client), i.e. what a phone does when we dial it.
async fn raw_as_acceptor(listener: &TcpListener, p: &RawPeer, v: V, o: &Opts) -> Session {
    let (mut tcp, addr) = listener.accept().await.unwrap();
    let plaintext = raw_read_line(&mut tcp).await;
    let name = ServerName::IpAddress(addr.ip().into());
    let tls = TlsConnector::from(raw_client_cfg(&p.cert, &p.key, v))
        .connect(name, tcp)
        .await
        .expect("TLS client handshake");
    let (version, seen_cert) = {
        let c = tls.get_ref().1;
        (
            format!("{:?}", c.protocol_version().unwrap()),
            c.peer_certificates().unwrap()[0].as_ref().to_vec(),
        )
    };
    let mut io: BoxIo = Box::new(tls);
    finish_raw_identity(&mut io, p, o).await;
    let secure = raw_read_line(&mut io).await;
    Session {
        io,
        plaintext,
        secure,
        version,
        seen_cert,
    }
}

async fn finish_raw_identity(io: &mut BoxIo, p: &RawPeer, o: &Opts) {
    if o.secure_silent {
        return;
    }
    let id = o.secure_id.clone().unwrap_or_else(|| p.id.clone());
    let name = if o.secure_oversize {
        "N".repeat(9000)
    } else {
        "Raw Phone".to_string()
    };
    raw_send(
        io,
        "kdeconnect.identity",
        identity_body(&id, &name, json!({})),
    )
    .await;
}

/// Raw peer acting as TCP **initiator** (TLS server), i.e. what a phone does after seeing our broadcast.
async fn raw_as_initiator(addr: SocketAddr, p: &RawPeer, target: &str, v: V, o: &Opts) -> Session {
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    if let Some(raw) = &o.plaintext_raw {
        tcp.write_all(raw).await.unwrap();
    } else {
        // Like stock Android, targetProtocolVersion is a JSON *string*.
        let t = o
            .target_override
            .clone()
            .unwrap_or_else(|| target.to_string());
        raw_send(
            &mut tcp,
            "kdeconnect.identity",
            identity_body(
                &p.id,
                "Raw Phone",
                json!({"targetDeviceId": t, "targetProtocolVersion": "8"}),
            ),
        )
        .await;
    }
    let tls = TlsAcceptor::from(raw_server_cfg(&p.cert, &p.key, v))
        .accept(tcp)
        .await
        .expect("TLS server handshake");
    let (version, seen_cert) = {
        let c = tls.get_ref().1;
        (
            format!("{:?}", c.protocol_version().unwrap()),
            c.peer_certificates().unwrap()[0].as_ref().to_vec(),
        )
    };
    let mut io: BoxIo = Box::new(tls);
    finish_raw_identity(&mut io, p, o).await;
    let secure = raw_read_line(&mut io).await;
    Session {
        io,
        plaintext: None,
        secure,
        version,
        seen_cert,
    }
}

// ---------------------------------------------------------------------------
// Local side helpers
// ---------------------------------------------------------------------------

fn test_config() -> TransportConfig {
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
        pairing: PairingConfig {
            request_timeout: Duration::from_secs(5),
            response_timeout: Duration::from_secs(5),
            ..PairingConfig::default()
        },
        ..TransportConfig::default()
    }
}

fn endpoint_with(store: Arc<dyn TrustStore>, config: TransportConfig) -> Arc<Endpoint> {
    let creds = Arc::new(DeviceCredentials::generate().unwrap());
    Arc::new(
        Endpoint::new(
            creds,
            "Desk",
            DeviceType::Desktop,
            vec!["kdeconnect.ping".into()],
            vec!["kdeconnect.ping".into()],
            store,
            config,
        )
        .unwrap(),
    )
}

fn endpoint(store: Arc<dyn TrustStore>) -> Arc<Endpoint> {
    endpoint_with(store, test_config())
}

fn id_of(p: &RawPeer) -> DeviceId {
    DeviceId::parse(&p.id).unwrap()
}

#[derive(Clone, Default)]
struct Auto {
    approve: bool,
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<Vec<ApprovalRequest>>>,
}

impl Auto {
    fn yes() -> Self {
        Self {
            approve: true,
            ..Self::default()
        }
    }
    fn no() -> Self {
        Self {
            approve: false,
            ..Self::default()
        }
    }
}

impl Approver for Auto {
    async fn approve(&mut self, request: ApprovalRequest) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(request);
        self.approve
    }
}

/// Approval that waits for the test to release it.
struct Gate(Option<oneshot::Receiver<bool>>);

impl Approver for Gate {
    async fn approve(&mut self, _request: ApprovalRequest) -> bool {
        self.0.take().unwrap().await.unwrap_or(false)
    }
}

fn pending(h: Handshake) -> PendingLink {
    match h {
        Handshake::Pending(p) => p,
        Handshake::Trusted(_) => panic!("expected a pending link, got a trusted one"),
    }
}

fn trusted(h: Handshake) -> TrustedLink {
    match h {
        Handshake::Trusted(t) => t,
        Handshake::Pending(_) => panic!("expected a trusted link, got a pending one"),
    }
}

async fn listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    (l, a)
}

fn peer_record(p: &RawPeer) -> TrustedPeer {
    TrustedPeer {
        device_id: id_of(p),
        name: "Raw Phone".into(),
        device_type: "phone".into(),
        protocol_version: 8,
        cert: CertInfo::parse(p.cert.as_ref()).unwrap(),
        paired_at_unix: 1,
    }
}

/// Stock-style behaviour while we dial: accept our request after the user "taps accept".
async fn raw_accepts_our_request(s: &mut Session) {
    let req = raw_read_line(&mut s.io).await.expect("pair request");
    assert_eq!(req["type"], "kdeconnect.pair");
    assert_eq!(req["body"]["pair"], true);
    assert!(req["body"]["timestamp"].is_i64());
    raw_send(&mut s.io, "kdeconnect.pair", json!({"pair": true})).await;
}

// ---------------------------------------------------------------------------
// Role inversion, identities, explicit pairing
// ---------------------------------------------------------------------------

async fn initiator_role_round_trip(v: V, expect_version: &str) {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('p');
    let (l, addr) = listener().await;

    let approver = Auto::yes();
    let (mut a2, peer_id) = (approver.clone(), id_of(&phone));
    let local = tokio::spawn({
        let ep = ep.clone();
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            let info = link.info().clone();
            let trusted = link.pair(PairMode::Request, &mut a2).await.unwrap();
            (info, trusted)
        }
    });

    // The phone side is the TCP acceptor and therefore the TLS *client*. If the crate
    // took the TLS client role as well, this handshake could never complete.
    let mut s = within(raw_as_acceptor(&l, &phone, v, &Opts::default())).await;
    let plain = s.plaintext.take().unwrap();
    assert_eq!(plain["type"], "kdeconnect.identity");
    assert_eq!(
        plain["body"]["deviceId"],
        ep.local_identity().device_id.as_str()
    );
    assert_eq!(plain["body"]["targetDeviceId"], phone.id);
    assert_eq!(plain["body"]["targetProtocolVersion"], 8);
    let secure = s.secure.take().unwrap();
    assert_eq!(
        secure["body"]["deviceId"],
        ep.local_identity().device_id.as_str()
    );
    assert_eq!(
        secure["body"]["incomingCapabilities"],
        json!(["kdeconnect.ping"])
    );
    assert_eq!(
        s.seen_cert,
        ep.credentials().cert().der(),
        "the server side must present its device certificate"
    );
    assert!(s.version.contains(expect_version), "{}", s.version);

    // Not paired yet: nothing is stored until both the phone and the local user agree.
    raw_accepts_our_request(&mut s).await;
    let (info, trusted_link) = within(local).await.unwrap();
    assert_eq!(info.role, Role::Initiator);
    assert_eq!(info.peer.device_id.as_str(), phone.id);
    assert_eq!(info.peer_cert.der(), phone.cert.as_ref());
    let stored = store
        .get(&id_of(&phone))
        .unwrap()
        .expect("pairing persisted");
    assert_eq!(stored.cert.der(), phone.cert.as_ref());
    assert_eq!(
        approver.calls.load(Ordering::SeqCst),
        1,
        "the local user was asked exactly once"
    );
    drop(trusted_link);
}

#[tokio::test]
async fn initiator_is_tls_server_tls12() {
    within(initiator_role_round_trip(V::Tls12, "TLSv1_2")).await;
}

#[tokio::test]
async fn initiator_is_tls_server_tls13_when_peer_offers_it() {
    within(initiator_role_round_trip(V::Both, "TLSv1_3")).await;
}

async fn acceptor_role_round_trip(v: V, expect_version: &str) {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('q');
    let (l, addr) = listener().await;

    let approver = Auto::yes();
    let local = tokio::spawn({
        let (ep, mut ap) = (ep.clone(), approver.clone());
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            let link = pending(ep.accept(tcp).await.unwrap());
            let info = link.info().clone();
            let t = link
                .pair(PairMode::AwaitPeerRequest, &mut ap)
                .await
                .unwrap();
            (info, t)
        }
    });

    // The phone dialled us: it is the TLS *server* and we are the TLS client.
    let mut s = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        v,
        &Opts::default(),
    ))
    .await;
    let secure = s.secure.take().unwrap();
    assert_eq!(
        secure["body"]["deviceId"],
        ep.local_identity().device_id.as_str()
    );
    assert_eq!(
        s.seen_cert,
        ep.credentials().cert().der(),
        "the TLS client must present its device certificate"
    );
    assert!(s.version.contains(expect_version));

    // Phone asks twice (duplicate); we must answer once.
    let ts = bridge_kde::cert::unix_now();
    raw_send(
        &mut s.io,
        "kdeconnect.pair",
        json!({"pair": true, "timestamp": ts}),
    )
    .await;
    raw_send(
        &mut s.io,
        "kdeconnect.pair",
        json!({"pair": true, "timestamp": ts}),
    )
    .await;
    let accept = raw_read_line(&mut s.io).await.expect("accept");
    assert_eq!(
        accept["body"],
        json!({"pair": true}),
        "accept carries no timestamp"
    );
    assert!(
        raw_try_read_line(&mut s.io, Duration::from_millis(300))
            .await
            .is_none(),
        "exactly one accept"
    );

    let (info, _t) = within(local).await.unwrap();
    assert_eq!(info.role, Role::Acceptor);
    let req = approver.seen.lock().unwrap()[0].clone();
    assert_eq!(req.direction, Direction::Incoming);
    assert_eq!(
        req.peer_cert_sha256,
        CertInfo::parse(phone.cert.as_ref())
            .unwrap()
            .fingerprint_hex()
    );
    let spki_phone = CertInfo::parse(phone.cert.as_ref()).unwrap();
    assert_eq!(
        req.verification_key,
        verification_key(
            ep.credentials().cert().spki_der(),
            spki_phone.spki_der(),
            ts
        )
    );
    assert!(store.get(&id_of(&phone)).unwrap().is_some());
}

#[tokio::test]
async fn acceptor_is_tls_client_tls12_with_android_style_string_version() {
    within(acceptor_role_round_trip(V::Tls12, "TLSv1_2")).await;
}

#[tokio::test]
async fn acceptor_is_tls_client_tls13() {
    within(acceptor_role_round_trip(V::Both, "TLSv1_3")).await;
}

#[tokio::test]
async fn pairing_requires_local_approval_even_after_peer_accepts() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('g');
    let (l, addr) = listener().await;
    let (tx, rx) = oneshot::channel();

    let peer_id = id_of(&phone);
    let local = tokio::spawn({
        let ep = ep.clone();
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            link.pair(PairMode::Request, &mut Gate(Some(rx))).await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    raw_accepts_our_request(&mut s).await;
    // The phone has accepted, the local user has not decided yet.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        store.get(&id_of(&phone)).unwrap().is_none(),
        "no trust before local approval"
    );
    assert!(!local.is_finished());
    tx.send(true).unwrap();
    within(local).await.unwrap().unwrap();
    assert!(store.get(&id_of(&phone)).unwrap().is_some());
}

#[tokio::test]
async fn local_rejection_stores_nothing_and_notifies_peer() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('r');
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let local = tokio::spawn({
        let ep = ep.clone();
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            link.pair(PairMode::Request, &mut Auto::no()).await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    raw_accepts_our_request(&mut s).await;
    let r = within(local).await.unwrap();
    assert!(
        matches!(r, Err(KdeError::Pairing(PairingFailure::RejectedLocally))),
        "{r:?}",
        r = r.as_ref().err()
    );
    let cancel = raw_read_line(&mut s.io).await.expect("cancel notice");
    assert_eq!(cancel["body"], json!({"pair": false}));
    assert!(store.list().unwrap().is_empty());
}

#[tokio::test]
async fn crossed_request_does_not_pair() {
    let store = Arc::new(MemoryTrustStore::new());
    let mut cfg = test_config();
    cfg.pairing.request_timeout = Duration::from_millis(700);
    let ep = endpoint_with(store.clone(), cfg);
    let phone = raw_peer('c');
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let ap = Auto::yes();
    let local = tokio::spawn({
        let (ep, mut ap) = (ep.clone(), ap.clone());
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            link.pair(PairMode::Request, &mut ap).await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let _ours = raw_read_line(&mut s.io).await.unwrap();
    // The phone's own simultaneous request (carries a timestamp): stock apps would read it as an accept.
    raw_send(
        &mut s.io,
        "kdeconnect.pair",
        json!({"pair": true, "timestamp": bridge_kde::cert::unix_now()}),
    )
    .await;
    let r = within(local).await.unwrap();
    assert!(matches!(r, Err(KdeError::Pairing(PairingFailure::Timeout))));
    assert!(store.list().unwrap().is_empty());
    let notice = raw_read_line(&mut s.io).await.expect("timeout notice");
    assert_eq!(notice["body"], json!({"pair": false}));
}

#[tokio::test]
async fn pairing_times_out_and_late_approval_is_useless() {
    let store = Arc::new(MemoryTrustStore::new());
    let mut cfg = test_config();
    cfg.pairing.request_timeout = Duration::from_millis(500);
    let ep = endpoint_with(store.clone(), cfg);
    let phone = raw_peer('t');
    let (l, addr) = listener().await;
    let (tx, rx) = oneshot::channel();
    let peer_id = id_of(&phone);
    let local = tokio::spawn({
        let ep = ep.clone();
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            link.pair(PairMode::Request, &mut Gate(Some(rx))).await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let _ = raw_read_line(&mut s.io).await.unwrap();
    let r = within(local).await.unwrap();
    assert!(matches!(r, Err(KdeError::Pairing(PairingFailure::Timeout))));
    let _ = tx.send(true); // the user finally clicks accept
    raw_send(&mut s.io, "kdeconnect.pair", json!({"pair": true})).await; // and the phone accepted too
    assert!(
        store.list().unwrap().is_empty(),
        "approval after the deadline must not create trust"
    );
}

#[tokio::test]
async fn link_drop_resets_pairing_and_new_certificate_gets_no_trust() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('d');
    let (l, addr) = listener().await;
    let (tx, rx) = oneshot::channel();
    let peer_id = id_of(&phone);
    let local = tokio::spawn({
        let (ep, peer_id) = (ep.clone(), peer_id.clone());
        async move {
            let link = pending(ep.connect(addr, &peer_id).await.unwrap());
            link.pair(PairMode::Request, &mut Gate(Some(rx))).await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let _ = raw_read_line(&mut s.io).await.unwrap();
    drop(s); // link dies while the local user is still looking at the prompt
    let r = within(local).await.unwrap();
    assert!(
        matches!(r, Err(KdeError::Pairing(PairingFailure::LinkClosed))),
        "{:?}",
        r.err()
    );
    let _ = tx.send(true);

    // Same claimed id, different certificate, reconnects: fresh pending state, nothing inherited.
    let (cert2, key2) = gen_cert(&phone.id);
    let phone2 = RawPeer {
        id: phone.id.clone(),
        cert: cert2,
        key: key2,
    };
    let ep2 = ep.clone();
    let dial = tokio::spawn(async move { ep2.connect(addr, &peer_id).await });
    let _s2 = within(raw_as_acceptor(&l, &phone2, V::Tls12, &Opts::default())).await;
    let _p = pending(within(dial).await.unwrap().unwrap());
    assert!(store.list().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// Trust, pinning, substitution
// ---------------------------------------------------------------------------

#[tokio::test]
async fn paired_peer_reconnects_as_trusted_in_both_roles_and_exchanges_features() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let store: Arc<dyn TrustStore> = Arc::new(FileTrustStore::open(&state).unwrap());
    let ep = endpoint(store.clone());
    let phone = raw_peer('w');
    let (l, addr) = listener().await;

    // Pair once.
    let peer_id = id_of(&phone);
    let t = tokio::spawn({
        let (ep, peer_id) = (ep.clone(), peer_id.clone());
        async move {
            pending(ep.connect(addr, &peer_id).await.unwrap())
                .pair(PairMode::Request, &mut Auto::yes())
                .await
        }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    raw_accepts_our_request(&mut s).await;
    within(t).await.unwrap().unwrap();
    drop(s);

    // The record survives a restart of the store.
    let store2: Arc<dyn TrustStore> = Arc::new(FileTrustStore::open(&state).unwrap());
    let ep = Arc::new(
        Endpoint::new(
            Arc::new(ep.credentials().clone()),
            "Desk",
            DeviceType::Desktop,
            vec![],
            vec![],
            store2.clone(),
            test_config(),
        )
        .unwrap(),
    );

    // We dial: trusted, feature packets flow.
    let d = tokio::spawn({
        let (ep, peer_id) = (ep.clone(), peer_id.clone());
        async move { trusted(ep.connect(addr, &peer_id).await.unwrap()) }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let mut link = within(d).await.unwrap();
    link.send(&Packet::new(
        "kdeconnect.ping",
        json!({"message": "hi"}).as_object().unwrap().clone(),
    ))
    .await
    .unwrap();
    let got = raw_read_line(&mut s.io).await.unwrap();
    assert_eq!(got["type"], "kdeconnect.ping");
    assert_eq!(got["body"]["message"], "hi");
    raw_send(&mut s.io, "kdeconnect.ping", json!({})).await;
    // Duplicate accept on a paired link is ignored, never an unpair.
    raw_send(&mut s.io, "kdeconnect.pair", json!({"pair": true})).await;
    raw_send(&mut s.io, "kdeconnect.ping", json!({"n": 2})).await;
    for expect in [json!({}), json!({"n": 2})] {
        match within(link.recv()).await.unwrap() {
            LinkEvent::Packet(p) => assert_eq!(Value::Object(p.body), expect),
            other => panic!("{other:?}"),
        }
    }
    // Reserved types cannot be forged by callers.
    assert!(
        link.send(&Packet::new("kdeconnect.pair", Default::default()))
            .await
            .is_err()
    );
    drop((s, link));

    // The phone dials: trusted too.
    let a = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            trusted(ep.accept(tcp).await.unwrap())
        }
    });
    let _s = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        V::Tls12,
        &Opts::default(),
    ))
    .await;
    assert_eq!(within(a).await.unwrap().info().role, Role::Acceptor);
}

#[tokio::test]
async fn pinned_peer_with_substituted_certificate_is_refused_in_both_roles() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('s');
    store.insert(peer_record(&phone)).unwrap();
    // Attacker: same claimed id and a matching CN, but its own certificate.
    let (bad_cert, bad_key) = gen_cert(&phone.id);
    let attacker = RawPeer {
        id: phone.id.clone(),
        cert: bad_cert,
        key: bad_key,
    };

    // Role 1: we dial, attacker is the TCP acceptor.
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let dial = tokio::spawn({
        let ep = ep.clone();
        async move { ep.connect(addr, &peer_id).await.map(|_| ()) }
    });
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        raw_as_acceptor_lenient(&l, &attacker),
    )
    .await;
    let err = within(dial).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::PinMismatch { .. }), "{err}");

    // Role 2: attacker dials us and claims the paired id.
    let (l2, addr2) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l2.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let _ = tokio::time::timeout(
        Duration::from_secs(5),
        raw_as_initiator_lenient(addr2, &attacker, ep.local_identity().device_id.as_str()),
    )
    .await;
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::PinMismatch { .. }), "{err}");

    // The record is untouched and the pin is still the original certificate.
    let rec = store.get(&id_of(&phone)).unwrap().unwrap();
    assert_eq!(rec.cert.der(), phone.cert.as_ref());
}

/// Attacker sessions where the handshake is expected to fail; errors are ignored.
async fn raw_as_acceptor_lenient(l: &TcpListener, p: &RawPeer) {
    let (mut tcp, addr) = l.accept().await.unwrap();
    let _ = raw_read_line(&mut tcp).await;
    let name = ServerName::IpAddress(addr.ip().into());
    if let Ok(mut tls) = TlsConnector::from(raw_client_cfg(&p.cert, &p.key, V::Tls12))
        .connect(name, tcp)
        .await
    {
        let _ = raw_send_ok(&mut tls, p).await;
        let _ = raw_read_line(&mut tls).await;
    }
}

async fn raw_as_initiator_lenient(addr: SocketAddr, p: &RawPeer, target: &str) {
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    raw_send(
        &mut tcp,
        "kdeconnect.identity",
        identity_body(
            &p.id,
            "Raw Phone",
            json!({"targetDeviceId": target, "targetProtocolVersion": 8}),
        ),
    )
    .await;
    if let Ok(mut tls) = TlsAcceptor::from(raw_server_cfg(&p.cert, &p.key, V::Tls12))
        .accept(tcp)
        .await
    {
        let _ = raw_send_ok(&mut tls, p).await;
        let _ = raw_read_line(&mut tls).await;
    }
}

async fn raw_send_ok<W: AsyncWrite + Unpin>(w: &mut W, p: &RawPeer) -> std::io::Result<()> {
    let line = format!(
        "{}\n",
        json!({"id": 1, "type": "kdeconnect.identity", "body": identity_body(&p.id, "Evil", json!({}))})
    );
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

/// Presents `cert` signed by an unrelated key (rustls' builders refuse such a pair, so use a resolver).
#[derive(Debug)]
struct Mismatched(Arc<rustls::sign::CertifiedKey>);

impl Mismatched {
    fn new(cert: &CertificateDer<'static>, key: PrivatePkcs8KeyDer<'static>) -> Arc<Self> {
        let signer = prov()
            .key_provider
            .load_private_key(PrivateKeyDer::from(key))
            .unwrap();
        Arc::new(Self(Arc::new(rustls::sign::CertifiedKey::new(
            vec![cert.clone()],
            signer,
        ))))
    }
}

impl rustls::client::ResolvesClientCert for Mismatched {
    fn resolve(
        &self,
        _: &[&[u8]],
        _: &[SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
    fn has_certs(&self) -> bool {
        true
    }
}

impl rustls::server::ResolvesServerCert for Mismatched {
    fn resolve(
        &self,
        _: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

#[tokio::test]
async fn replayed_pinned_certificate_without_its_private_key_is_refused_in_both_roles() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('k');
    store.insert(peer_record(&phone)).unwrap();
    let (_, other_key) = gen_cert("irrelevant");

    // Role 1: we dial; the attacker (TCP acceptor, TLS client) presents the pinned certificate but signs with another key.
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let dial = tokio::spawn({
        let ep = ep.clone();
        async move { ep.connect(addr, &peer_id).await.map(|_| ()) }
    });
    let (mut tcp, a) = l.accept().await.unwrap();
    let _ = raw_read_line(&mut tcp).await;
    let cfg = ClientConfig::builder_with_provider(prov())
        .with_protocol_versions(&versions(V::Tls12))
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAll(prov())))
        .with_client_cert_resolver(Mismatched::new(&phone.cert, other_key.clone_key()));
    let _ = TlsConnector::from(Arc::new(cfg))
        .connect(ServerName::IpAddress(a.ip().into()), tcp)
        .await;
    let err = within(dial).await.unwrap().unwrap_err();
    assert!(
        matches!(err, KdeError::Tls(_)),
        "signature over the handshake must fail: {err}"
    );

    // Role 2: the attacker dials us (TLS server) with the same trick.
    let (l2, addr2) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l2.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let mut tcp = TcpStream::connect(addr2).await.unwrap();
    raw_send(&mut tcp, "kdeconnect.identity", identity_body(&phone.id, "Evil", json!({"targetDeviceId": ep.local_identity().device_id.as_str(), "targetProtocolVersion": 8}))).await;
    let scfg = ServerConfig::builder_with_provider(prov())
        .with_protocol_versions(&versions(V::Tls12))
        .unwrap()
        .with_client_cert_verifier(Arc::new(AcceptAll(prov())))
        .with_cert_resolver(Mismatched::new(&phone.cert, other_key));
    let _ = TlsAcceptor::from(Arc::new(scfg)).accept(tcp).await;
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(
        matches!(err, KdeError::Tls(_)),
        "signature over the handshake must fail: {err}"
    );
    assert!(store.get(&id_of(&phone)).unwrap().is_some());
}

#[tokio::test]
async fn certificate_cn_must_equal_claimed_device_id() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let (cert, key) = gen_cert(&"z".repeat(32));
    let liar = RawPeer {
        id: "y".repeat(32),
        cert,
        key,
    }; // claims y..., certificate says z...
    let (l, addr) = listener().await;
    let peer_id = id_of(&liar);
    let dial = tokio::spawn({
        let ep = ep.clone();
        async move { ep.connect(addr, &peer_id).await.map(|_| ()) }
    });
    let _ = tokio::time::timeout(Duration::from_secs(5), raw_as_acceptor_lenient(&l, &liar)).await;
    let err = within(dial).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::Certificate(_)), "{err}");
    assert!(store.list().unwrap().is_empty());
}

#[tokio::test]
async fn secure_identity_must_match_plaintext_claim() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('m');
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let opts = Opts {
        secure_id: Some("n".repeat(32)),
        ..Opts::default()
    };
    let _ = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        V::Tls12,
        &opts,
    ))
    .await;
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::IdentityMismatch(_)), "{err}");
}

// ---------------------------------------------------------------------------
// Malformed, oversize and slow peers
// ---------------------------------------------------------------------------

async fn plaintext_attack(raw: Vec<u8>) -> KdeError {
    let ep = endpoint(Arc::new(MemoryTrustStore::new()));
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ()).unwrap_err()
        }
    });
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    let _ = tcp.write_all(&raw).await;
    let err = within(acc).await.unwrap();
    // After the error the socket is closed: no TLS bytes, just EOF/reset.
    let mut b = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(2), tcp.read(&mut b))
        .await
        .expect("socket closed promptly");
    assert!(matches!(n, Ok(0) | Err(_)));
    err
}

fn line(body: Value) -> Vec<u8> {
    format!(
        "{}\n",
        json!({"id": 1, "type": "kdeconnect.identity", "body": body})
    )
    .into_bytes()
}

#[tokio::test]
async fn oversize_plaintext_identity_is_rejected() {
    let mut big = vec![b'x'; 8193];
    big.push(b'\n');
    assert!(matches!(
        plaintext_attack(big).await,
        KdeError::LineTooLong { .. }
    ));
    // No newline at all, and far more than the cap.
    let err = plaintext_attack(vec![b'a'; 1_000_000]).await;
    assert!(
        matches!(err, KdeError::LineTooLong { .. } | KdeError::Io(_)),
        "{err}"
    );
}

#[tokio::test]
async fn malformed_and_misaddressed_plaintext_identities_are_rejected() {
    let me = "a".repeat(32);
    let ok = |extra: Value| identity_body(&"b".repeat(32), "Phone", extra);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not json", b"hello\n".to_vec()),
        ("not an object", b"[1]\n".to_vec()),
        (
            "missing target id",
            line(ok(json!({"targetProtocolVersion": 8}))),
        ),
        (
            "missing target version",
            line(ok(json!({"targetDeviceId": me}))),
        ),
        (
            "other target",
            line(ok(
                json!({"targetDeviceId": "c".repeat(32), "targetProtocolVersion": 8}),
            )),
        ),
        (
            "bad version string",
            line(ok(
                json!({"targetDeviceId": me, "targetProtocolVersion": "abc"}),
            )),
        ),
        (
            "protocol 7 claimed",
            line(identity_body(
                &"b".repeat(32),
                "Phone",
                json!({"protocolVersion": 7, "targetDeviceId": me, "targetProtocolVersion": 8}),
            )),
        ),
        (
            "target protocol 7",
            line(ok(
                json!({"targetDeviceId": me, "targetProtocolVersion": 7}),
            )),
        ),
        (
            "short device id",
            line(identity_body(
                "short",
                "Phone",
                json!({"targetDeviceId": me, "targetProtocolVersion": 8}),
            )),
        ),
        (
            "path-like device id",
            line(identity_body(
                &format!("../{}", "b".repeat(32)),
                "Phone",
                json!({"targetDeviceId": me, "targetProtocolVersion": 8}),
            )),
        ),
        (
            "empty name",
            line(identity_body(
                &"b".repeat(32),
                "...",
                json!({"targetDeviceId": me, "targetProtocolVersion": 8}),
            )),
        ),
        (
            "wrong packet type",
            format!(
                "{}\n",
                json!({"id": 1, "type": "kdeconnect.pair", "body": {}})
            )
            .into_bytes(),
        ),
        (
            "deep nesting",
            format!(
                "{{\"id\":1,\"type\":\"kdeconnect.identity\",\"body\":{}0{}}}\n",
                "[".repeat(100),
                "]".repeat(100)
            )
            .into_bytes(),
        ),
    ];
    for (name, bytes) in cases {
        // The endpoint under test has its own random id; rewrite `me` to it for the cases that should reach target checks.
        let ep = endpoint(Arc::new(MemoryTrustStore::new()));
        let bytes = String::from_utf8(bytes)
            .unwrap()
            .replace(&me, ep.local_identity().device_id.as_str())
            .into_bytes();
        let (l, addr) = listener().await;
        let acc = tokio::spawn({
            let ep = ep.clone();
            async move {
                let (tcp, _) = l.accept().await.unwrap();
                ep.accept(tcp).await.map(|_| ())
            }
        });
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(&bytes).await.unwrap();
        let res = within(acc).await.unwrap();
        assert!(res.is_err(), "case {name:?} must be rejected");
    }
}

#[tokio::test]
async fn oversize_secure_identity_is_rejected() {
    let ep = endpoint(Arc::new(MemoryTrustStore::new()));
    let phone = raw_peer('o');
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let opts = Opts {
        secure_oversize: true,
        ..Opts::default()
    };
    let _ = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        V::Tls12,
        &opts,
    ))
    .await;
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::LineTooLong { .. }), "{err}");
}

#[tokio::test]
async fn oversize_post_handshake_packet_is_fatal_and_not_resynchronised() {
    let store = Arc::new(MemoryTrustStore::new());
    let mut cfg = test_config();
    cfg.limits.packet_line = 1024;
    let ep = endpoint_with(store.clone(), cfg);
    let phone = raw_peer('l');
    store.insert(peer_record(&phone)).unwrap();
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let d = tokio::spawn({
        let ep = ep.clone();
        async move { trusted(ep.connect(addr, &peer_id).await.unwrap()) }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let mut link = within(d).await.unwrap();
    // 4 KiB of junk then a valid-looking packet in the same stream.
    let mut payload = vec![b'x'; 4096];
    payload.push(b'\n');
    s.io.write_all(&payload).await.unwrap();
    raw_send(&mut s.io, "kdeconnect.ping", json!({})).await;
    assert!(matches!(
        within(link.recv()).await,
        Err(KdeError::LineTooLong { limit: 1024 })
    ));
    // The tail of the oversize line, and the valid packet behind it, are never parsed.
    assert!(within(link.recv()).await.is_err());
}

#[tokio::test]
async fn malformed_post_handshake_packets_are_fatal() {
    for bad in [
        &b"{\"id\":1,\"type\":\"kdeconnect.ping\"}\n"[..],
        b"garbage\n",
        b"{\"id\":1,\"type\":\"kdeconnect.pair\",\"body\":{\"pair\":\"yes\"}}\n",
    ] {
        let store = Arc::new(MemoryTrustStore::new());
        let ep = endpoint(store.clone());
        let phone = raw_peer('j');
        store.insert(peer_record(&phone)).unwrap();
        let (l, addr) = listener().await;
        let peer_id = id_of(&phone);
        let d = tokio::spawn({
            let ep = ep.clone();
            async move { trusted(ep.connect(addr, &peer_id).await.unwrap()) }
        });
        let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
        let mut link = within(d).await.unwrap();
        s.io.write_all(bad).await.unwrap();
        assert!(within(link.recv()).await.is_err());
    }
}

#[tokio::test]
async fn silent_peers_time_out() {
    let mut cfg = test_config();
    cfg.timeouts.plaintext_identity = Duration::from_millis(300);
    cfg.timeouts.tls_handshake = Duration::from_millis(300);
    cfg.timeouts.secure_identity = Duration::from_millis(300);
    let ep = endpoint_with(Arc::new(MemoryTrustStore::new()), cfg);

    // Connects and says nothing.
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let _silent = TcpStream::connect(addr).await.unwrap();
    let start = std::time::Instant::now();
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::Timeout(_)), "{err}");
    assert!(start.elapsed() < Duration::from_secs(2));

    // One byte at a time cannot extend the deadline.
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let mut slow = TcpStream::connect(addr).await.unwrap();
    let drip = tokio::spawn(async move {
        for _ in 0..30 {
            if slow.write_all(b"{").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::Timeout(_)), "{err}");
    drip.abort();

    // Valid plaintext identity, then no TLS.
    let me = ep.local_identity().device_id.to_string();
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ())
        }
    });
    let mut tcp = TcpStream::connect(addr).await.unwrap();
    tcp.write_all(&line(identity_body(
        &"b".repeat(32),
        "Phone",
        json!({"targetDeviceId": me, "targetProtocolVersion": "8"}),
    )))
    .await
    .unwrap();
    let err = within(acc).await.unwrap().unwrap_err();
    assert!(
        matches!(err, KdeError::Timeout(_) | KdeError::Tls(_)),
        "{err}"
    );
}

#[tokio::test]
async fn silent_secure_identity_times_out() {
    let mut cfg = test_config();
    cfg.timeouts.secure_identity = Duration::from_millis(300);
    let ep = endpoint_with(Arc::new(MemoryTrustStore::new()), cfg);
    let phone = raw_peer('v');
    let (l, addr) = listener().await;
    let dial = tokio::spawn({
        let (ep, id) = (ep.clone(), id_of(&phone));
        async move { ep.connect(addr, &id).await.map(|_| ()) }
    });
    // The raw side finishes TLS and writes its own identity but stays silent afterwards? It must
    // not write its identity: skip it and keep the socket open.
    let (mut tcp, a) = l.accept().await.unwrap();
    let _ = raw_read_line(&mut tcp).await;
    let tls = TlsConnector::from(raw_client_cfg(&phone.cert, &phone.key, V::Tls12))
        .connect(ServerName::IpAddress(a.ip().into()), tcp)
        .await
        .unwrap();
    let err = within(dial).await.unwrap().unwrap_err();
    assert!(matches!(err, KdeError::Timeout(_)), "{err}");
    drop(tls);
}

// ---------------------------------------------------------------------------
// No sensitive traffic before pairing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn feature_packets_before_pairing_are_dropped_and_never_delivered() {
    let store = Arc::new(MemoryTrustStore::new());
    let mut cfg = test_config();
    cfg.pairing.request_timeout = Duration::from_millis(800);
    let ep = endpoint_with(store.clone(), cfg);
    let phone = raw_peer('x');
    let (l, addr) = listener().await;
    let ap = Auto::yes();
    let local = tokio::spawn({
        let (ep, mut ap) = (ep.clone(), ap.clone());
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            let link = pending(ep.accept(tcp).await.unwrap());
            link.pair(PairMode::AwaitPeerRequest, &mut ap).await
        }
    });
    let mut s = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        V::Tls12,
        &Opts::default(),
    ))
    .await;
    for (kind, body) in [
        ("kdeconnect.clipboard", json!({"content": "secret"})),
        ("kdeconnect.sms.messages", json!({"messages": []})),
        ("kdeconnect.runcommand", json!({"requestCommandList": true})),
        ("kdeconnect.contacts.request_all_uids_timestamps", json!({})),
    ] {
        raw_send(&mut s.io, kind, body).await;
    }
    let r = within(local).await.unwrap();
    assert!(
        matches!(r, Err(KdeError::Pairing(PairingFailure::Timeout))),
        "{:?}",
        r.err()
    );
    assert_eq!(
        ap.calls.load(Ordering::SeqCst),
        0,
        "no pairing prompt was triggered by feature packets"
    );
    assert!(store.list().unwrap().is_empty());
    // We told the peer once that we are not paired; nothing was echoed back.
    let hint = raw_read_line(&mut s.io).await.expect("one not-paired hint");
    assert_eq!(hint["type"], "kdeconnect.pair");
    assert_eq!(hint["body"], json!({"pair": false}));
}

#[tokio::test]
async fn feature_packet_flood_before_pairing_drops_the_link() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('f');
    let (l, addr) = listener().await;
    let local = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            pending(ep.accept(tcp).await.unwrap())
                .pair(PairMode::AwaitPeerRequest, &mut Auto::yes())
                .await
        }
    });
    let mut s = within(raw_as_initiator(
        addr,
        &phone,
        ep.local_identity().device_id.as_str(),
        V::Tls12,
        &Opts::default(),
    ))
    .await;
    for _ in 0..40 {
        let _ =
            s.io.write_all(b"{\"id\":1,\"type\":\"kdeconnect.clipboard\",\"body\":{}}\n")
                .await;
    }
    let r = within(local).await.unwrap();
    assert!(
        matches!(
            r,
            Err(KdeError::Pairing(PairingFailure::TooManyUnexpectedPackets))
        ),
        "{:?}",
        r.err()
    );
}

// ---------------------------------------------------------------------------
// Unpair, limits, address policy
// ---------------------------------------------------------------------------

#[tokio::test]
async fn peer_unpair_deletes_trust_and_next_connection_is_untrusted() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('u');
    store.insert(peer_record(&phone)).unwrap();
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let d = tokio::spawn({
        let (ep, id) = (ep.clone(), peer_id.clone());
        async move { trusted(ep.connect(addr, &id).await.unwrap()) }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let mut link = within(d).await.unwrap();
    raw_send(&mut s.io, "kdeconnect.pair", json!({"pair": false})).await;
    assert!(matches!(
        within(link.recv()).await.unwrap(),
        LinkEvent::PeerUnpaired
    ));
    assert!(store.get(&peer_id).unwrap().is_none());
    drop((s, link));

    let d = tokio::spawn({
        let (ep, id) = (ep.clone(), peer_id.clone());
        async move { ep.connect(addr, &id).await.unwrap() }
    });
    let _s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    let _ = pending(within(d).await.unwrap());
}

#[tokio::test]
async fn local_unpair_removes_record_before_notifying() {
    let store = Arc::new(MemoryTrustStore::new());
    let ep = endpoint(store.clone());
    let phone = raw_peer('h');
    store.insert(peer_record(&phone)).unwrap();
    let (l, addr) = listener().await;
    let peer_id = id_of(&phone);
    let d = tokio::spawn({
        let (ep, id) = (ep.clone(), peer_id.clone());
        async move { trusted(ep.connect(addr, &id).await.unwrap()) }
    });
    let mut s = within(raw_as_acceptor(&l, &phone, V::Tls12, &Opts::default())).await;
    within(within(d).await.unwrap().unpair()).await.unwrap();
    assert!(store.get(&peer_id).unwrap().is_none());
    let n = raw_read_line(&mut s.io).await.unwrap();
    assert_eq!(n["body"], json!({"pair": false}));
}

#[tokio::test]
async fn loopback_is_refused_unless_allowed_and_unpaired_slots_are_capped() {
    // Default policy: loopback sources are rejected before any byte is read.
    let ep = endpoint_with(
        Arc::new(MemoryTrustStore::new()),
        TransportConfig::default(),
    );
    let (l, addr) = listener().await;
    let acc = tokio::spawn({
        let ep = ep.clone();
        async move {
            let (tcp, _) = l.accept().await.unwrap();
            ep.accept(tcp).await.map(|_| ()).unwrap_err()
        }
    });
    let _c = TcpStream::connect(addr).await.unwrap();
    assert!(matches!(
        within(acc).await.unwrap(),
        KdeError::AddressRejected(_)
    ));

    for ip in [
        "10.1.2.3",
        "192.168.0.9",
        "172.16.5.5",
        "169.254.1.1",
        "100.64.0.1",
        "fd00::1",
        "fe80::1",
    ] {
        AddressPolicy::default()
            .check(ip.parse().unwrap())
            .unwrap_or_else(|e| panic!("{ip}: {e}"));
    }
    for ip in [
        "8.8.8.8",
        "1.1.1.1",
        "100.128.0.1",
        "2001:db8::1",
        "127.0.0.1",
        "::1",
    ] {
        assert!(
            AddressPolicy::default().check(ip.parse().unwrap()).is_err(),
            "{ip}"
        );
    }

    // Slot cap: with one slot, a second pending peer is refused while the first is alive.
    let mut cfg = test_config();
    cfg.max_unpaired = 1;
    let ep = endpoint_with(Arc::new(MemoryTrustStore::new()), cfg);
    let (a, b) = (raw_peer('1'), raw_peer('2'));
    let (la, addr_a) = listener().await;
    let (lb, addr_b) = listener().await;
    let first = tokio::spawn({
        let (ep, id) = (ep.clone(), id_of(&a));
        async move { ep.connect(addr_a, &id).await }
    });
    let _sa = within(raw_as_acceptor(&la, &a, V::Tls12, &Opts::default())).await;
    let kept = within(first).await.unwrap().unwrap();
    let second = tokio::spawn({
        let (ep, id) = (ep.clone(), id_of(&b));
        async move { ep.connect(addr_b, &id).await.map(|_| ()) }
    });
    let _ = &lb;
    assert!(matches!(
        within(second).await.unwrap(),
        Err(KdeError::TooManyConnections)
    ));
    drop(kept);
}
