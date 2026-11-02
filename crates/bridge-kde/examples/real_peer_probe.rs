//! Interop probe: one GeekBridge link to a REAL KDE Connect peer, driven line by line from stdin.
//!
//! Developer tool for `scripts/interop-kdeconnect.sh`. It records every packet line (as seen after
//! TLS) to a JSONL capture file, prints inbound events, and sends packets built by the same helper
//! functions the service uses. It approves a pairing only when the verification key equals the one
//! the other side reported (read from `--key-file`).
//!
//! stdin commands: `ping`, `clip TEXT`, `clipconnect TEXT`, `sharetext TEXT`, `shareurl URL`,
//! `sharefile PATH`, `raw JSON-LINE`, `ext` (geekbridge.* extension packets), `quit`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bridge_core::{clipboard_wire, phone_link};
use bridge_kde::share::{self, ShareContent};
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, CancelToken, DeviceCredentials, DeviceId, DeviceType,
    Endpoint, FileTrustStore, Handshake, LinkEvent, Packet, PairMode, TransportConfig, TrustedLink,
};
use clap::Parser;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    state_dir: PathBuf,
    /// JSONL capture of every packet line (`{"dir":"in|out","line":"..."}`).
    #[arg(long)]
    capture: PathBuf,
    /// Where received shared files are stored.
    #[arg(long)]
    downloads: PathBuf,
    /// File that will contain the peer's verification key for the current pairing.
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    peer_id: String,
    /// Dial the peer (we become TCP initiator / TLS server).
    #[arg(long, conflicts_with = "listen")]
    connect: Option<SocketAddr>,
    /// Wait for the peer to dial us (we become TCP acceptor / TLS client).
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// `request` (we ask) or `await` (the peer asks).
    #[arg(long, default_value = "request")]
    pair_mode: String,
    #[arg(long, default_value = "GeekBridge")]
    name: String,
    /// Offer TLS 1.2 only (what Android's stack negotiates).
    #[arg(long)]
    tls12: bool,
    /// Present a fresh certificate for this device id (impersonation test); never persisted.
    #[arg(long)]
    forge_id: Option<String>,
    /// Print the handshake outcome and stop, even when the peer is not trusted.
    #[arg(long)]
    inspect_only: bool,
}

struct KeyApprover(PathBuf);

impl Approver for KeyApprover {
    async fn approve(&mut self, req: ApprovalRequest) -> bool {
        println!(
            "PAIR-PROMPT key={} direction={:?}",
            req.verification_key, req.direction
        );
        for _ in 0..200 {
            if let Ok(k) = std::fs::read_to_string(&self.0) {
                let k = k.trim();
                if !k.is_empty() {
                    let ok = k.eq_ignore_ascii_case(&req.verification_key);
                    println!(
                        "PAIR-KEYS ours={} peer={k} equal={ok}",
                        req.verification_key
                    );
                    return ok;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("PAIR-KEYS peer key never appeared");
        false
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run(Args::parse()).await {
        println!("FATAL {e}");
        std::process::exit(1);
    }
}

async fn run(a: Args) -> Result<(), String> {
    let e = |e: bridge_kde::KdeError| e.to_string();
    let creds = match &a.forge_id {
        Some(id) => {
            DeviceCredentials::generate_with_id(&DeviceId::parse(id).map_err(e)?).map_err(e)?
        }
        None => {
            DeviceCredentials::load_or_create(&a.state_dir)
                .map_err(e)?
                .0
        }
    };
    println!("LOCAL-ID {}", creds.device_id());
    let store = Arc::new(FileTrustStore::open(&a.state_dir).map_err(e)?);
    let cap = Arc::new(Mutex::new(
        std::fs::File::create(&a.capture).map_err(|x| x.to_string())?,
    ));
    let tap: bridge_kde::WireTap = Arc::new(move |out, line| {
        use std::io::Write;
        let rec = json!({"dir": if out {"out"} else {"in"}, "line": String::from_utf8_lossy(line)});
        let _ = writeln!(cap.lock().unwrap(), "{rec}");
    });
    // The same capability lists the desktop service declares (bridge-service gate.rs).
    let incoming: Vec<String> = [
        "kdeconnect.ping",
        "kdeconnect.clipboard",
        "kdeconnect.clipboard.connect",
        "geekbridge.clipboard.item",
        "geekbridge.clipboard.none",
        "kdeconnect.share.request",
        "geekbridge.calls.state",
        "geekbridge.calls.result",
        "kdeconnect.contacts.response_uids_timestamps",
        "kdeconnect.contacts.response_vcards",
        "geekbridge.calendar.sync",
        "geekbridge.calendar.write_result",
        "kdeconnect.sms.messages",
        "geekbridge.sms.status",
    ]
    .map(str::to_owned)
    .into();
    let outgoing: Vec<String> = [
        "kdeconnect.ping",
        "kdeconnect.clipboard",
        "kdeconnect.clipboard.connect",
        "geekbridge.clipboard.pull",
        "geekbridge.clipboard.item",
        "kdeconnect.share.request",
        "geekbridge.calls.request",
        "kdeconnect.contacts.request_all_uids_timestamps",
        "kdeconnect.contacts.request_vcards_by_uid",
        "geekbridge.calendar.request",
        "geekbridge.calendar.write",
        "kdeconnect.sms.request_conversations",
        "kdeconnect.sms.request_conversation",
        "kdeconnect.sms.request",
    ]
    .map(str::to_owned)
    .into();
    let config = TransportConfig {
        address_policy: AddressPolicy {
            local_networks: true,
            loopback: true,
        },
        tls_versions: if a.tls12 {
            bridge_kde::TlsVersions::Tls12Only
        } else {
            bridge_kde::TlsVersions::Tls12And13
        },
        ..TransportConfig::default()
    };
    let ep = Endpoint::new(
        Arc::new(creds),
        &a.name,
        DeviceType::Desktop,
        incoming,
        outgoing,
        store,
        config,
    )
    .map_err(e)?
    .with_wire_tap(tap);

    let peer = DeviceId::parse(&a.peer_id).map_err(e)?;
    let hs = if let Some(addr) = a.connect {
        println!("DIAL {addr}");
        ep.connect(addr, &peer).await.map_err(e)?
    } else {
        let bind = a.listen.ok_or("choose --connect or --listen")?;
        let l = TcpListener::bind(bind).await.map_err(|x| x.to_string())?;
        println!("LISTEN {}", l.local_addr().unwrap());
        let (tcp, from) = tokio::time::timeout(Duration::from_secs(60), l.accept())
            .await
            .map_err(|_| "no inbound connection")?
            .map_err(|x| x.to_string())?;
        println!("ACCEPT {from}");
        ep.accept(tcp).await.map_err(e)?
    };
    let i = hs.info();
    println!(
        "LINK role={:?} tls={} cipher={} peer_name={:?} peer_type={} proto={} cert_cn={} incoming={:?} outgoing={:?}",
        i.role,
        i.tls_version,
        i.cipher_suite,
        i.peer.device_name,
        i.peer.device_type.as_str(),
        i.peer.protocol_version,
        i.peer_cert.common_name(),
        i.peer.incoming_capabilities,
        i.peer.outgoing_capabilities
    );
    let mut link = match hs {
        Handshake::Trusted(l) => {
            println!("TRUSTED (pinned certificate matched)");
            l
        }
        Handshake::Pending(_) if a.inspect_only => {
            println!("UNTRUSTED handshake completed, not pairing (--inspect-only)");
            return Ok(());
        }
        Handshake::Pending(p) => {
            let mode = if a.pair_mode == "await" {
                PairMode::AwaitPeerRequest
            } else {
                PairMode::Request
            };
            println!("PENDING pairing mode={mode:?}");
            let l = p.pair(mode, &mut KeyApprover(a.key_file.clone())).await;
            match l {
                Ok(l) => {
                    println!("PAIRED");
                    l
                }
                Err(err) => return Err(format!("pairing failed: {err}")),
            }
        }
    };
    session(&mut link, &a).await
}

async fn session(link: &mut TrustedLink, a: &Args) -> Result<(), String> {
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    println!("READY");
    loop {
        tokio::select! {
            ev = link.recv() => match ev {
                Ok(LinkEvent::Packet(p)) => handle_inbound(link, a, p),
                Ok(LinkEvent::PeerUnpaired) => { println!("EVENT peer-unpaired"); return Ok(()); }
                Ok(LinkEvent::PeerRequestedRePair { timestamp }) => println!("EVENT re-pair-request ts={timestamp}"),
                Err(err) => { println!("CLOSED {err}"); return Ok(()); }
            },
            line = stdin.next_line() => {
                let Some(line) = line.map_err(|e| e.to_string())? else { return Ok(()) };
                let (cmd, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                let sent = match cmd {
                    "ping" => link.send(&Packet::new("kdeconnect.ping", Map::new())).await,
                    "clip" => link.send(&Packet::new(clipboard_wire::CLIPBOARD, clipboard_wire::update_body(rest))).await,
                    "clipconnect" => link.send(&Packet::new(clipboard_wire::CLIPBOARD_CONNECT, clipboard_wire::connect_body(rest, now_ms()))).await,
                    "sharetext" => link.send(&share::text_packet(rest)).await,
                    "shareurl" => link.send(&share::url_packet(rest)).await,
                    "sharefile" => match send_file(link, rest).await { Ok(()) => Ok(()), Err(m) => { println!("ERR sharefile {m}"); continue } },
                    "raw" => match Packet::from_line(rest.as_bytes()) { Ok(p) => link.send(&p).await, Err(e) => { println!("ERR raw {e}"); continue } },
                    "ext" => send_extensions(link).await,
                    "quit" => return Ok(()),
                    other => { println!("ERR unknown command {other}"); continue }
                };
                println!("SENT {cmd} ok={}", sent.is_ok());
                if let Err(e) = sent { println!("ERR {e}"); }
            }
        }
    }
}

async fn send_extensions(link: &mut TrustedLink) -> bridge_kde::Result<()> {
    // GeekBridge-only packets; a stock peer must ignore them without dropping the link.
    link.send(&Packet::new(
        phone_link::CALENDAR_REQUEST,
        phone_link::calendar_request_body("probe-req-1", None),
    ))
    .await?;
    let mut b = Map::new();
    b.insert("requestId".into(), Value::from("probe-req-2"));
    link.send(&Packet::new(phone_link::CALENDAR_WRITE, b))
        .await?;
    link.send(&Packet::new("geekbridge.calls.request", Map::new()))
        .await?;
    link.send(&Packet::new(
        clipboard_wire::PULL,
        clipboard_wire::pull_body("probe-req-3", 30_000),
    ))
    .await
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

async fn send_file(link: &mut TrustedLink, path: &str) -> Result<(), String> {
    let out = share::open_outgoing(std::path::Path::new(path), 1 << 30)
        .await
        .map_err(|e| e.to_string())?;
    let ctx = link.payload_context();
    let (packet, listener) = share::prepare_send(&ctx, &out)
        .await
        .map_err(|e| e.to_string())?;
    link.send_with_payload(&packet)
        .await
        .map_err(|e| e.to_string())?;
    let (file, size) = (out.file, out.size);
    tokio::spawn(async move {
        let cancel = CancelToken::new();
        let r = listener.send(file, size, &mut |_| {}, &cancel).await;
        println!("FILE-SENT bytes={:?}", r.map_err(|e| e.to_string()));
    });
    Ok(())
}

fn handle_inbound(link: &TrustedLink, a: &Args, p: Packet) {
    println!(
        "RECV type={} id={} payloadSize={:?} body={}",
        p.kind,
        p.id,
        p.payload_size,
        Value::Object(p.body.clone())
    );
    if p.kind.starts_with("kdeconnect.share.request") {
        match share::parse_share(&p, 1 << 30) {
            Ok(ShareContent::File(offer)) => {
                println!("SHARE-FILE name={:?} size={:?}", offer.name, offer.size);
                let ctx = link.payload_context();
                let dir = a.downloads.clone();
                tokio::spawn(async move {
                    let cancel = CancelToken::new();
                    let r = share::receive_file(&ctx, &offer, &dir, 1 << 30, &mut |_| {}, &cancel)
                        .await;
                    println!("FILE-RECEIVED {:?}", r.map_err(|e| e.to_string()));
                });
            }
            Ok(other) => println!("SHARE {other:?}"),
            Err(e) => println!("SHARE-ERR {e:?}"),
        }
    }
}
