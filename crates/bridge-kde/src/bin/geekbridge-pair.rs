//! Developer tool for pairing GeekBridge with one explicitly chosen KDE Connect device.
//!
//! It never scans or broadcasts, talks only to the endpoint given on the command
//! line, and never approves a pairing by itself: you must type the verification
//! code that the *other* device displays. It sends no feature packets.

use std::io::{IsTerminal, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use bridge_kde::cert::identity_path;
use bridge_kde::{
    AddressPolicy, ApprovalRequest, Approver, DeviceCredentials, DeviceId, DeviceType, Direction,
    Endpoint, FileTrustStore, Handshake, KdeError, LinkInfo, PairMode, TransportConfig, TrustStore,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tokio::net::TcpListener;

#[derive(Parser)]
#[command(
    name = "geekbridge-pair",
    version,
    about = "Pair GeekBridge with one explicitly chosen KDE Connect device (developer tool)",
    long_about = "Pair GeekBridge with one explicitly chosen KDE Connect device (developer tool).\n\n\
        This tool does not scan the network, does not broadcast, and never auto-accepts. \
        You give it one endpoint (--connect HOST:PORT --device-id ID, or --listen ADDR --peer-ip IP), \
        it performs the KDE Connect protocol 8 TLS handshake, shows the peer certificate fingerprint, \
        and - for `pair` - asks you to type the verification code shown on the other device.\n\n\
        Interoperability with real devices is NOT established by the automated tests; \
        see `docs/architecture.md`."
)]
struct Cli {
    /// Directory for the device identity (identity.pem, mode 0600) and trust store.
    /// Default: $XDG_DATA_HOME/geekbridge/kde or ~/.local/share/geekbridge/kde.
    #[arg(long, global = true, value_name = "DIR")]
    state_dir: Option<PathBuf>,
    /// Device name announced to the peer.
    #[arg(long, global = true, default_value = "GeekBridge", value_name = "NAME")]
    name: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show this device's id and certificate fingerprint (creates the identity on first use).
    Identity,
    /// List paired devices.
    List,
    /// Handshake with a device and show its certificate; pairs nothing, stores nothing.
    Inspect(EndpointArgs),
    /// Pair with a device. Requires a TTY and typing the code shown on the other device.
    Pair(PairArgs),
    /// Forget a paired device; with --connect also tell the device (best effort).
    Unpair {
        #[arg(long, value_name = "ID")]
        device_id: String,
        /// Endpoint to notify. The handshake must pass the pinned certificate check.
        #[arg(long, value_name = "HOST:PORT")]
        connect: Option<SocketAddr>,
    },
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("how").required(true).multiple(false).args(["connect", "listen"])))]
struct EndpointArgs {
    /// Dial this endpoint (we become the TCP initiator and TLS server). Needs --device-id.
    #[arg(long, value_name = "HOST:PORT", requires = "device_id")]
    connect: Option<SocketAddr>,
    /// Device id of the peer being dialled (with --connect).
    #[arg(long, value_name = "ID")]
    device_id: Option<String>,
    /// Listen here for one inbound connection (we become the TCP acceptor and TLS client). Needs --peer-ip.
    #[arg(long, value_name = "ADDR:PORT", requires = "peer_ip")]
    listen: Option<SocketAddr>,
    /// Only connections from this address are accepted (with --listen).
    #[arg(long, value_name = "IP")]
    peer_ip: Option<IpAddr>,
    /// How long to wait for the inbound connection (with --listen).
    #[arg(long, default_value_t = 120, value_name = "SECS")]
    wait_secs: u64,
}

#[derive(Args)]
struct PairArgs {
    #[command(flatten)]
    endpoint: EndpointArgs,
    /// request: send a pair request. await: wait for the peer to send one.
    #[arg(long, value_enum, default_value_t = ModeArg::Request)]
    mode: ModeArg,
}

#[derive(Clone, Copy, ValueEnum)]
enum ModeArg {
    Request,
    Await,
}

fn state_dir(cli: &Cli) -> Result<PathBuf, String> {
    if let Some(d) = &cli.state_dir {
        return Ok(d.clone());
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .ok_or("cannot determine a state directory; pass --state-dir")?;
    Ok(base.join("geekbridge").join("kde"))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = rt.block_on(run(cli));
    let _ = std::io::stdout().flush();
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    // A terminal read may still be blocked in a worker thread; do not wait for it.
    std::process::exit(code)
}

async fn run(cli: Cli) -> Result<(), String> {
    let dir = state_dir(&cli)?;
    let e = |e: KdeError| e.to_string();
    match &cli.command {
        Command::List => {
            let store = FileTrustStore::open(&dir).map_err(e)?;
            let peers = store.list().map_err(e)?;
            if peers.is_empty() {
                println!("no paired devices");
            }
            for p in peers {
                println!(
                    "{}  {:<32}  {}  sha256:{}",
                    p.device_id,
                    p.name,
                    p.device_type,
                    p.cert.fingerprint_hex()
                );
            }
            Ok(())
        }
        Command::Identity => {
            let (creds, created) = DeviceCredentials::load_or_create(&dir).map_err(e)?;
            println!("device id:          {}", creds.device_id());
            println!("certificate sha256: {}", creds.cert().fingerprint_hex());
            println!(
                "identity file:      {} (contains the private key; never share it)",
                identity_path(&dir).display()
            );
            if created {
                println!("(new identity created)");
            }
            Ok(())
        }
        Command::Unpair { device_id, connect } => {
            let id = DeviceId::parse(device_id).map_err(e)?;
            let (ep, store) = endpoint(&cli, &dir, false)?;
            if store.get(&id).map_err(e)?.is_none() {
                return Err(format!("{id} is not paired"));
            }
            if let Some(addr) = connect {
                match ep.connect(*addr, &id).await {
                    Ok(Handshake::Trusted(link)) => {
                        link.unpair().await.map_err(e)?;
                        println!("unpaired {id}; the device was notified");
                        return Ok(());
                    }
                    Ok(Handshake::Pending(_)) => {
                        eprintln!("warning: device did not present the pinned identity")
                    }
                    Err(err) => eprintln!("warning: could not reach the device ({err})"),
                }
            }
            store.remove(&id).map_err(e)?;
            println!(
                "unpaired {id} locally; the device was not notified and will only find out when it next sends us data"
            );
            Ok(())
        }
        Command::Inspect(args) => {
            let (ep, _) = endpoint(&cli, &dir, args.peer_ip.is_some_and(|ip| ip.is_loopback()))?;
            let hs = handshake(&ep, args).await?;
            let trusted = matches!(hs, Handshake::Trusted(_));
            print_link(hs.info(), trusted);
            println!("\ninspect only: nothing was paired, stored or sent besides our identity.");
            Ok(())
        }
        Command::Pair(args) => {
            if !std::io::stdin().is_terminal() {
                return Err(
                    "pairing needs an interactive terminal; refusing to run without one".into(),
                );
            }
            let (ep, _) = endpoint(
                &cli,
                &dir,
                args.endpoint.peer_ip.is_some_and(|ip| ip.is_loopback()),
            )?;
            match handshake(&ep, &args.endpoint).await? {
                Handshake::Trusted(link) => {
                    print_link(link.info(), true);
                    println!("\nalready paired with this exact certificate; nothing to do.");
                    Ok(())
                }
                Handshake::Pending(link) => {
                    print_link(link.info(), false);
                    let mode = match args.mode {
                        ModeArg::Request => PairMode::Request,
                        ModeArg::Await => PairMode::AwaitPeerRequest,
                    };
                    println!(
                        "\nunpaired peer: starting explicit pairing ({} s window)",
                        ep.config().pairing.request_timeout.as_secs()
                    );
                    let linked = link.pair(mode, &mut TerminalApprover).await.map_err(e)?;
                    println!(
                        "paired with {} ({}); trust record written",
                        linked.info().peer.device_name,
                        linked.info().peer.device_id
                    );
                    Ok(())
                }
            }
        }
    }
}

fn endpoint(
    cli: &Cli,
    dir: &Path,
    allow_loopback: bool,
) -> Result<(Endpoint, Arc<FileTrustStore>), String> {
    let e = |e: KdeError| e.to_string();
    let (creds, _) = DeviceCredentials::load_or_create(dir).map_err(e)?;
    let store = Arc::new(FileTrustStore::open(dir).map_err(e)?);
    let config = TransportConfig {
        address_policy: AddressPolicy {
            local_networks: true,
            loopback: allow_loopback,
        },
        ..TransportConfig::default()
    };
    // No capabilities are advertised: this tool carries no feature traffic.
    let ep = Endpoint::new(
        Arc::new(creds),
        &cli.name,
        DeviceType::Desktop,
        vec![],
        vec![],
        store.clone(),
        config,
    )
    .map_err(e)?;
    Ok((ep, store))
}

async fn handshake(ep: &Endpoint, args: &EndpointArgs) -> Result<Handshake, String> {
    let e = |e: KdeError| e.to_string();
    if let Some(addr) = args.connect {
        let id = args
            .device_id
            .as_deref()
            .ok_or("--connect needs --device-id")?;
        let id = DeviceId::parse(id).map_err(e)?;
        println!("dialling {addr} as TCP initiator (TLS server) for device {id}");
        return ep.connect(addr, &id).await.map_err(e);
    }
    let bind = args.listen.ok_or("choose --connect or --listen")?;
    let want = args.peer_ip.ok_or("--listen needs --peer-ip")?;
    let listener = TcpListener::bind(bind)
        .await
        .map_err(|x| format!("cannot listen on {bind}: {x}"))?;
    println!(
        "listening on {} for {want} (we become TLS client when it connects); waiting up to {} s",
        listener.local_addr().map_err(|x| x.to_string())?,
        args.wait_secs
    );
    let wait = Duration::from_secs(args.wait_secs);
    let accepted = tokio::time::timeout(wait, async {
        loop {
            let (tcp, from) = listener.accept().await.map_err(|x| x.to_string())?;
            if from.ip() == want {
                return Ok::<_, String>(tcp);
            }
            eprintln!(
                "ignored connection from {} (not the expected peer)",
                from.ip()
            );
        }
    })
    .await
    .map_err(|_| "no connection from the expected peer in time".to_string())??;
    ep.accept(accepted).await.map_err(e)
}

fn print_link(info: &LinkInfo, trusted: bool) {
    println!("peer device id:     {}", info.peer.device_id);
    println!("peer name:          {}", info.peer.device_name);
    println!("peer type:          {}", info.peer.device_type.as_str());
    println!("peer address:       {}", info.peer_ip);
    println!(
        "peer certificate:   sha256:{}",
        info.peer_cert.fingerprint_hex()
    );
    println!(
        "our role:           TCP {:?} / TLS {}",
        info.role,
        match info.role {
            bridge_kde::Role::Initiator => "server",
            bridge_kde::Role::Acceptor => "client",
        }
    );
    println!(
        "TLS:                {} {}",
        info.tls_version, info.cipher_suite
    );
    println!(
        "trust:              {}",
        if trusted {
            "paired (certificate matches the pinned one)"
        } else {
            "NOT paired (certificate is unverified)"
        }
    );
}

/// Approves only if the user types the verification code the other device displays.
struct TerminalApprover;

impl Approver for TerminalApprover {
    async fn approve(&mut self, r: ApprovalRequest) -> bool {
        let who = match r.direction {
            Direction::Outgoing => "we asked the device to pair",
            Direction::Incoming => "the device asked to pair with us",
        };
        eprintln!("\n== pairing: {who} ==");
        eprintln!("device:                {} ({})", r.peer_name, r.peer_id);
        eprintln!("peer certificate:      sha256:{}", r.peer_cert_sha256);
        eprintln!("our certificate:       sha256:{}", r.own_cert_sha256);
        eprintln!(
            "Look at the verification code on the OTHER device. It must be an 8-character code."
        );
        eprintln!(
            "Type it here to approve, or just press Enter to reject ({} s left):",
            r.expires_in.as_secs()
        );
        let expected = r.verification_key.clone();
        let typed = tokio::task::spawn_blocking(|| {
            let mut s = String::new();
            std::io::stdin().read_line(&mut s).map(|_| s)
        })
        .await;
        let Ok(Ok(typed)) = typed else { return false };
        let typed = typed.trim().to_uppercase();
        if typed.is_empty() {
            eprintln!("rejected.");
            false
        } else if typed == expected {
            eprintln!("code matches; approved.");
            true
        } else {
            eprintln!(
                "code does NOT match the one computed from the two certificates - possible interception. Rejecting."
            );
            false
        }
    }
}
