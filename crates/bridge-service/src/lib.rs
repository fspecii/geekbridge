//! GeekBridge helper service.
//!
//! * Public, read-only discovery API ([`serve_api`]): untrusted announcement metadata only.
//! * Authenticated control API ([`control::serve_control`]): pairing and link lifecycle for
//!   explicitly chosen phones, backed by `bridge-kde`. See `docs/service-integration.md`.
pub mod call_cli;
pub mod call_control;
pub mod calling;
pub mod clipboard;
pub mod config;
pub mod control;
pub mod daemon;
pub mod discovery;
pub mod gate;
pub mod hotkey_bind;
pub mod macos_host;
pub mod private;
pub mod share;
pub mod windows_host;
use bridge_core::{MAX_API_BYTES, MAX_DEVICES, Snapshot, api_address};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{RwLock, Semaphore},
    time,
};

pub type SharedState = Arc<RwLock<Snapshot>>;
pub const DEVICE_TTL_MS: u64 = 90_000;
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn record_device(state: &mut Snapshot, device: bridge_core::Device) {
    if let Some(existing) = state.devices.iter_mut().find(|d| d.id == device.id) {
        *existing = device;
    } else if state.devices.len() < MAX_DEVICES {
        state.devices.push(device);
    }
    state
        .devices
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
}

/// Passive only: do not advertise a TCP endpoint before TLS/pairing is implemented.
pub async fn listen_discovery(socket: UdpSocket, state: SharedState) -> io::Result<()> {
    let mut bytes = vec![0; discovery::MAX_IDENTITY_BYTES + 1];
    loop {
        let (size, source) = socket.recv_from(&mut bytes).await?;
        if let Ok(device) = discovery::parse_identity(&bytes[..size], source.ip(), now_ms()) {
            record_device(&mut *state.write().await, device);
        }
    }
}

pub async fn expire_devices(state: SharedState) {
    let mut tick = time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let now = now_ms();
        state
            .write()
            .await
            .devices
            .retain(|d| is_fresh(d.last_seen_unix_ms, now));
    }
}

fn is_fresh(last_seen: u64, now: u64) -> bool {
    now.checked_sub(last_seen)
        .is_some_and(|age| age < DEVICE_TTL_MS)
}

// Keep the API available even if announcements exhaust the response budget.
// Only discovery metadata is exposed; no phone content belongs in this endpoint.
fn snapshot_body(snapshot: &Snapshot) -> io::Result<Vec<u8>> {
    let mut visible = snapshot.clone();
    let now = now_ms();
    visible
        .devices
        .retain(|d| is_fresh(d.last_seen_unix_ms, now));
    // Preserve all device identities instead of hiding established devices by name order.
    // Capability lists in UDP announcements are untrusted hints, not negotiated permissions.
    if serde_json::to_vec(&visible)?.len() + 256 > MAX_API_BYTES {
        visible.capabilities_truncated = true;
        for device in &mut visible.devices {
            device.incoming_capabilities.truncate(4);
            device.outgoing_capabilities.truncate(4);
        }
    }
    loop {
        let body = serde_json::to_vec(&visible)?;
        if body.len() + 256 <= MAX_API_BYTES {
            return Ok(body);
        }
        if visible.devices.pop().is_none() {
            return Err(io::Error::other("snapshot metadata too large"));
        }
    }
}

async fn serve_client(mut stream: TcpStream, state: SharedState) -> io::Result<()> {
    // Bound the entire request, including slow fragmented headers.
    let request = time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        loop {
            let byte = stream.read_u8().await?;
            bytes.push(byte);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
            if bytes.len() >= 1024 {
                return Err(io::Error::other("request too large"));
            }
        }
        Ok::<_, io::Error>(bytes)
    })
    .await??;
    let allowed = request == b"GET /v1/discovery HTTP/1.0\r\n\r\n";
    let (status, body) = if allowed {
        ("200 OK", snapshot_body(&*state.read().await)?)
    } else {
        ("404 Not Found", b"{}".to_vec())
    };
    if body.len() + 256 > MAX_API_BYTES {
        return Err(io::Error::other("snapshot too large"));
    }
    let header = format!(
        "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    time::timeout(Duration::from_secs(2), async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&body).await?;
        stream.shutdown().await
    })
    .await?
}

pub async fn serve_api(listener: TcpListener, state: SharedState) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(16));
    loop {
        let permit = permits
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let (stream, _) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = serve_client(stream, state).await;
        });
    }
}

/// Open private state and build the control endpoint. A failure is not fatal for the process: the
/// public discovery API keeps working and the control API reports why it is unavailable.
pub fn prepare_control(
    state_dir: &std::path::Path,
    discovery: SharedState,
    options: daemon::DaemonOptions,
) -> control::Control {
    match private::PrivateState::open(state_dir) {
        Ok(state) => {
            let private::PrivateState {
                credentials,
                store,
                consents,
                token,
                ..
            } = state;
            match daemon::Daemon::new(credentials, store, consents, discovery, options) {
                Ok(daemon) => control::Control::Ready { daemon, token },
                Err(err) => {
                    eprintln!("GeekBridge service: cannot start link manager: {err}");
                    control::Control::Unavailable(private::Unavailable {
                        code: "identity_unusable",
                        message: "the local device identity is not usable",
                    })
                }
            }
        }
        Err((why, detail)) => {
            // The detail may name paths; it goes to the operator's terminal, not to clients.
            eprintln!(
                "GeekBridge service: control API unavailable ({}): {detail}",
                why.code
            );
            control::Control::Unavailable(why)
        }
    }
}

/// Accept phones that dial us. Off unless configured; the address policy of the transport still
/// applies before a byte is read.
pub async fn serve_inbound(listener: TcpListener, daemon: Arc<daemon::Daemon>) -> io::Result<()> {
    let permits = Arc::new(Semaphore::new(16));
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) if err.raw_os_error().is_some() => {
                time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(err) => return Err(err),
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let _permit = permit;
            // Holds the permit for the handshake only; the session continues without it.
            daemon.adopt_inbound(stream).await;
        });
    }
}

/// Resolves on Ctrl-C and, on Unix, on SIGTERM (what systemd and launchd send on stop), so the
/// service always leaves through `run`'s cleanup. Handlers are installed before this returns, so a
/// signal that arrives right after cannot take the default (immediate termination) action.
fn shutdown_signal() -> io::Result<impl std::future::Future<Output = io::Result<()>>> {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let ctrl_c = tokio::signal::ctrl_c();
    Ok(async move {
        #[cfg(unix)]
        {
            tokio::select! {
                result = ctrl_c => result,
                _ = term.recv() => Ok(()),
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await
        }
    })
}

/// The update checker for a real run: settings persist beside the other private state, and the
/// background schedule (check shortly after start, then about daily) runs unless the operator set
/// `GEEKBRIDGE_NO_UPDATE_CHECK`. Manual checks from the UI or `geekbridge-ctl` work either way.
fn update_service(state_dir: &std::path::Path) -> Arc<bridge_update::Updater> {
    let mut cfg = bridge_update::Config::github(env!("CARGO_PKG_VERSION"));
    cfg.state_file = Some(state_dir.join("update-state.json"));
    match bridge_update::Updater::new(cfg) {
        Ok(updater) => {
            if std::env::var_os("GEEKBRIDGE_NO_UPDATE_CHECK").is_none() {
                updater.spawn_scheduler();
            }
            updater
        }
        Err(err) => {
            eprintln!("GeekBridge service: update checks unavailable: {err}");
            bridge_update::Updater::new(bridge_update::Config::github(env!("CARGO_PKG_VERSION")))
                .expect("built-in update configuration is valid")
        }
    }
}

pub async fn run(config: config::Config) -> io::Result<()> {
    let shutdown = shutdown_signal()?;
    let api = TcpListener::bind(api_address(config.api_port)).await?;
    let control_listener =
        TcpListener::bind(bridge_core::ipc::control_address(config.control_port)).await?;
    let state = Arc::new(RwLock::new(Snapshot::empty(config.discovery_port)));
    let discovery_port = config.discovery_port;
    let socket = UdpSocket::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        discovery_port,
    ))
    .await;
    let discovery_state = state.clone();
    let discovery = tokio::spawn(async move {
        let result = match socket {
            Ok(socket) => listen_discovery(socket, discovery_state.clone()).await,
            Err(err) => Err(err),
        };
        if let Err(err) = result {
            discovery_state.write().await.discovery_error = Some(format!(
                "Cannot listen on UDP {discovery_port}: {err}. Check whether KDE Connect is already running."
            ));
        }
    });
    let expiry = tokio::spawn(expire_devices(state.clone()));

    let calling: Arc<dyn calling::CallingProbe> = if config.calling_probe {
        Arc::new(calling::HostCallingProbe)
    } else {
        Arc::new(calling::DisabledCallingProbe)
    };
    let mut options = daemon::DaemonOptions::new(&config.device_name, calling);
    options.dev_loopback = config.dev_loopback;
    options.clipboard = clipboard::detect_backend();
    options.clipboard_settings = Some(config.state_dir.join(clipboard::SETTINGS_FILE));
    options.hotkey_binder = hotkey_bind::detect();
    options.updates = update_service(&config.state_dir);
    options.share.settings_path = Some(config.state_dir.join(share::SETTINGS_FILE));
    #[cfg(target_os = "linux")]
    {
        options.share_notifier = Some(Arc::new(share::NotifySend));
        options.call_notifier = Some(Arc::new(call_control::NotifySendCalls));
        if config.calling_probe {
            let session = bridge_calls::Session::new(bridge_calls::default_backend());
            options.call_host =
                std::sync::Mutex::new(Some(Box::new(call_control::linux::LinuxHfp::new(session))));
        }
    }
    #[cfg(windows)]
    {
        if config.calling_probe
            && let Ok(service) = bridge_calls_windows::spawn_default(Default::default())
            && let Ok(host) = windows_host::WindowsHost::new(service)
        {
            options.call_host = std::sync::Mutex::new(Some(Box::new(host)));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if config.calling_probe
            && let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            // The user's explicit phone choice; without it the helper is started but never connects.
            let phone = std::env::var("GEEKBRIDGE_MAC_PHONE").ok();
            let helper = dir.join("geekbridge-macos-calls");
            if let Ok(host) = macos_host::MacHelperHost::spawn(&helper, phone.as_deref()) {
                options.call_host = std::sync::Mutex::new(Some(Box::new(host)));
            }
        }
    }
    let control = Arc::new(prepare_control(&config.state_dir, state.clone(), options));
    let inbound = match (&*control, config.inbound) {
        (control::Control::Ready { daemon, .. }, Some(addr)) => {
            let listener = TcpListener::bind(addr).await?;
            daemon.set_inbound_listener(listener.local_addr()?);
            Some(tokio::spawn(serve_inbound(listener, daemon.clone())))
        }
        _ => None,
    };
    eprintln!(
        "GeekBridge service: discovery API {} (read-only), control API {} ({})",
        api.local_addr()?,
        control_listener.local_addr()?,
        match &*control {
            control::Control::Ready { .. } => "authenticated",
            control::Control::Unavailable(_) => "UNAVAILABLE: no private state",
        }
    );
    let result = tokio::select! {
        result = serve_api(api, state) => result,
        result = control::serve_control(control_listener, control) => result,
        result = shutdown => result,
    };
    discovery.abort();
    expiry.abort();
    if let Some(inbound) = inbound {
        inbound.abort();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_ends_the_service_cleanly() {
        let shutdown = shutdown_signal().unwrap();
        let status = std::process::Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        time::timeout(Duration::from_secs(5), shutdown)
            .await
            .expect("SIGTERM was not observed")
            .unwrap();
    }
    #[tokio::test]
    async fn api_rejects_commands_and_serves_snapshot() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let handle = tokio::spawn(serve_api(listener, state));
        let snapshot = tokio::task::spawn_blocking(move || bridge_core::fetch_snapshot(port))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.discovery_port, 1716);
        let mut client = TcpStream::connect(api_address(port)).await.unwrap();
        client
            .write_all(b"POST /v1/send HTTP/1.0\r\n\r\n")
            .await
            .unwrap();
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).await.unwrap();
        assert!(bytes.starts_with(b"HTTP/1.0 404"));
        handle.abort();
    }
    #[tokio::test]
    async fn udp_identity_reaches_apps_and_deduplicates() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let state = Arc::new(RwLock::new(Snapshot::empty(addr.port())));
        let task = tokio::spawn(listen_discovery(socket, state.clone()));
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let packet = br#"{"id":1,"type":"kdeconnect.identity","body":{"deviceId":"0123456789abcdef0123456789abcdef","deviceName":"Test phone","deviceType":"phone","tcpPort":1716,"protocolVersion":8}}"#;
        for _ in 0..2 {
            sender.send_to(packet, addr).await.unwrap();
        }
        time::timeout(Duration::from_secs(2), async {
            loop {
                if !state.read().await.devices.is_empty() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.read().await.devices.len(), 1);
        assert_eq!(
            state.read().await.devices[0].state,
            bridge_core::ConnectionState::Discovered
        );
        task.abort();
    }
}
