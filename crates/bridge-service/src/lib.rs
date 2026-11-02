//! GeekBridge helper service.
//!
//! * Public, read-only discovery API ([`serve_api`]): untrusted announcement metadata only.
//! * Authenticated control API ([`control::serve_control`]): pairing and link lifecycle for
//!   explicitly chosen phones, backed by `bridge-kde`. See `docs/service-integration.md`.
pub mod autostart;
pub mod calendar_edit;
pub mod call_cli;
pub mod call_control;
pub mod calling;
pub mod clipboard;
pub mod config;
pub mod control;
pub mod daemon;
pub mod desktop_notify;
pub mod diagnose;
pub mod discovery;
pub mod gate;
pub mod hotkey_bind;
pub mod hotkey_cli;
pub mod hotkey_help;
pub mod hypr_config;
pub mod macos_host;
pub mod notify_text;
pub mod os_notify;
pub mod phone_data;
pub mod private;
pub mod session_env;
pub mod share;
pub mod tray;
pub mod waybar;
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

static OWN_DEVICE_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Ignore announcements from this device id (our own, heard back on the broadcast port).
pub fn ignore_own_announcements(device_id: &str) {
    let _ = OWN_DEVICE_ID.set(device_id.to_owned());
}

/// Announcements one source address may send per window; a phone announces every few seconds.
const ANNOUNCE_PER_WINDOW: u32 = 30;
const ANNOUNCE_WINDOW_MS: u64 = 10_000;
/// Different device ids one source address may announce within the list's lifetime. A phone has one id.
const IDS_PER_SOURCE: usize = 4;
const MAX_SOURCES: usize = 256;

/// Per-source limits on the unauthenticated discovery listing, so one LAN host cannot fill the device list with
/// invented phones or keep the daemon busy parsing. Nothing here is trusted; this only bounds the noise.
#[derive(Default)]
pub struct SourceLimiter {
    sources: std::collections::HashMap<std::net::IpAddr, SourceUse>,
}

struct SourceUse {
    window_start: u64,
    count: u32,
    ids: Vec<(String, u64)>,
}

impl SourceLimiter {
    fn entry(&mut self, ip: std::net::IpAddr, now: u64) -> Option<&mut SourceUse> {
        if !self.sources.contains_key(&ip) {
            if self.sources.len() >= MAX_SOURCES {
                // Forget sources whose window and ids have expired; if none did, drop the newcomer.
                self.sources.retain(|_, u| {
                    now.saturating_sub(u.window_start) < ANNOUNCE_WINDOW_MS
                        || u.ids
                            .iter()
                            .any(|(_, t)| now.saturating_sub(*t) < DEVICE_TTL_MS)
                });
                if self.sources.len() >= MAX_SOURCES {
                    return None;
                }
            }
            self.sources.insert(
                ip,
                SourceUse {
                    window_start: now,
                    count: 0,
                    ids: Vec::new(),
                },
            );
        }
        self.sources.get_mut(&ip)
    }

    /// May this source send another announcement right now?
    pub fn allow_packet(&mut self, ip: std::net::IpAddr, now: u64) -> bool {
        let Some(u) = self.entry(ip, now) else {
            return false;
        };
        if now.saturating_sub(u.window_start) >= ANNOUNCE_WINDOW_MS {
            u.window_start = now;
            u.count = 0;
        }
        u.count += 1;
        u.count <= ANNOUNCE_PER_WINDOW
    }

    /// May this source list this device id? A source is allowed [`IDS_PER_SOURCE`] ids at a time.
    pub fn allow_id(&mut self, ip: std::net::IpAddr, id: &str, now: u64) -> bool {
        let Some(u) = self.entry(ip, now) else {
            return false;
        };
        u.ids
            .retain(|(_, t)| now.saturating_sub(*t) < DEVICE_TTL_MS);
        if let Some(known) = u.ids.iter_mut().find(|(i, _)| i == id) {
            known.1 = now;
            return true;
        }
        if u.ids.len() >= IDS_PER_SOURCE {
            return false;
        }
        u.ids.push((id.to_owned(), now));
        true
    }
}

/// Listens for phones' announcements. Hearing one only lists the phone; it never dials or trusts.
/// This socket never sends; this computer's own announcement is `Daemon::announce_loop`.
pub async fn listen_discovery(socket: UdpSocket, state: SharedState) -> io::Result<()> {
    let mut bytes = vec![0; discovery::MAX_IDENTITY_BYTES + 1];
    let mut limiter = SourceLimiter::default();
    loop {
        let (size, source) = socket.recv_from(&mut bytes).await?;
        if !limiter.allow_packet(source.ip(), now_ms()) {
            continue;
        }
        if let Ok(device) = discovery::parse_identity(&bytes[..size], source.ip(), now_ms()) {
            // Our own announcement comes back on the shared broadcast port.
            if OWN_DEVICE_ID.get().is_some_and(|own| *own == device.id) {
                continue;
            }
            if !limiter.allow_id(source.ip(), &device.id, now_ms()) {
                continue;
            }
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
    #[cfg(unix)]
    return Ok(async move {
        tokio::select! {
            result = ctrl_c => result,
            _ = term.recv() => Ok(()),
        }
    });
    #[cfg(not(unix))]
    Ok(ctrl_c)
}

/// The update checker for a real run: settings persist beside the other private state, and the
/// background schedule (check shortly after start, then about daily) runs unless the operator set
/// `GEEKBRIDGE_NO_UPDATE_CHECK`. Manual checks from the UI or `geekbridge-ctl` work either way.
fn update_service(state_dir: &std::path::Path) -> Arc<bridge_update::Updater> {
    let mut cfg = bridge_update::Config::github(env!("CARGO_PKG_VERSION"));
    cfg.state_file = Some(state_dir.join("update-state.json"));
    // Test hook (scripts/e2e/update-install.sh): a mock release server. Only a loopback `http://` URL is
    // honoured, and the updater itself never contacts another http host.
    if let Ok(base) = std::env::var("GEEKBRIDGE_UPDATE_BASE_URL")
        && (base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:"))
    {
        cfg.base_url = base;
    }
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

/// Bind the inbound listener. The built-in default walks 1716..=1764 when its port is taken; an
/// explicit address is bound as given.
async fn bind_inbound(addr: SocketAddr, walk: bool) -> io::Result<TcpListener> {
    let mut last = None;
    let ports = if walk {
        addr.port()..=1764
    } else {
        addr.port()..=addr.port()
    };
    for port in ports {
        match TcpListener::bind(SocketAddr::new(addr.ip(), port)).await {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no port to listen on")))
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
    options.device_name_file = Some(config.state_dir.join(daemon::DEVICE_NAME_FILE));
    options.clipboard = clipboard::detect_backend();
    options.clipboard_settings = Some(config.state_dir.join(clipboard::SETTINGS_FILE));
    options.hotkey_binder = hotkey_bind::detect();
    options.updates = update_service(&config.state_dir);
    options.share.settings_path = Some(config.state_dir.join(share::SETTINGS_FILE));
    #[cfg(target_os = "linux")]
    {
        options.share_notifier = Some(Arc::new(share::NotifySend));
        options.call_notifier = Some(Arc::new(call_control::NotifySendCalls));
        options.sms_notifier = Some(Arc::new(phone_data::NotifySendSms));
        // Message text in notifications is opt-in: `touch <state dir>/sms-notify-content` (or set
        // GEEKBRIDGE_SMS_NOTIFY_CONTENT=show). Without it a notification names the sender only.
        options.sms_show_content = std::env::var("GEEKBRIDGE_SMS_NOTIFY_CONTENT")
            .is_ok_and(|v| v == "show")
            || config.state_dir.join("sms-notify-content").exists();
        desktop_notify::spawn_update_watcher(options.updates.clone());
        if config.calling_probe {
            let session = bridge_calls::Session::new(bridge_calls::default_backend());
            options.call_host =
                std::sync::Mutex::new(Some(Box::new(call_control::linux::LinuxHfp::new(session))));
        }
    }
    #[cfg(any(windows, target_os = "macos"))]
    {
        let banners = Arc::new(os_notify::BannerNotifier);
        options.share_notifier = Some(banners.clone());
        options.call_notifier = Some(banners.clone());
        options.sms_notifier = Some(banners);
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
    let mut announce = None;
    let alerts = match &*control {
        control::Control::Ready { daemon, .. } => {
            let data = daemon.phone_data().clone();
            Some(tokio::spawn(async move {
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
                loop {
                    tick.tick().await;
                    let data = data.clone();
                    let _ = tokio::task::spawn_blocking(move || data.tick_alerts()).await;
                }
            }))
        }
        control::Control::Unavailable(_) => None,
    };
    let inbound = match (&*control, config.inbound) {
        (control::Control::Ready { daemon, .. }, Some(addr)) => {
            match bind_inbound(addr, config.inbound_auto).await {
                Ok(listener) => {
                    daemon.set_inbound_listener(listener.local_addr()?);
                    ignore_own_announcements(
                        &daemon.status().map(|s| s.device_id).unwrap_or_default(),
                    );
                    if config.announce {
                        announce = Some(tokio::spawn(daemon.clone().announce_loop(discovery_port)));
                    }
                    Some(tokio::spawn(serve_inbound(listener, daemon.clone())))
                }
                Err(err) if config.inbound_auto => {
                    // The default listener is a convenience: another program on 1716-1764 (usually
                    // KDE Connect) must not stop the service from starting.
                    eprintln!("GeekBridge service: not accepting phone connections: {err}");
                    None
                }
                Err(err) => return Err(err),
            }
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
    let tray = match &*control {
        control::Control::Ready { daemon, .. }
            if config.tray && tray::enabled(&config.state_dir) =>
        {
            Some(tokio::spawn(tray::run(
                daemon.clone(),
                config.state_dir.clone(),
            )))
        }
        _ => None,
    };
    let result = tokio::select! {
        result = serve_api(api, state) => result,
        result = control::serve_control(control_listener, control) => result,
        result = shutdown => result,
        () = tray::quit_requested() => Ok(()),
    };
    if let Some(tray) = tray {
        tray.abort();
    }
    discovery.abort();
    expiry.abort();
    if let Some(alerts) = alerts {
        alerts.abort();
    }
    if let Some(announce) = announce {
        announce.abort();
    }
    if let Some(inbound) = inbound {
        inbound.abort();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_source_cannot_flood_the_discovery_list() {
        let a: std::net::IpAddr = "192.168.1.10".parse().unwrap();
        let b: std::net::IpAddr = "192.168.1.11".parse().unwrap();
        let mut l = SourceLimiter::default();
        // Packet rate: 30 per window, then refused until the window passes; another source is unaffected.
        assert!((0..ANNOUNCE_PER_WINDOW).all(|_| l.allow_packet(a, 1_000)));
        assert!(!l.allow_packet(a, 2_000));
        assert!(l.allow_packet(b, 2_000));
        assert!(l.allow_packet(a, 1_000 + ANNOUNCE_WINDOW_MS));
        // Invented ids: four per source, a known one keeps working, a fifth is refused, and they expire.
        for i in 0..IDS_PER_SOURCE {
            assert!(l.allow_id(a, &format!("id{i}"), 5_000));
        }
        assert!(!l.allow_id(a, "id-extra", 5_000));
        assert!(l.allow_id(a, "id0", 6_000));
        assert!(l.allow_id(b, "id-extra", 6_000));
        assert!(l.allow_id(a, "id-extra", 6_000 + DEVICE_TTL_MS + 1));
    }

    #[test]
    fn the_limiter_remembers_a_bounded_number_of_sources() {
        let mut l = SourceLimiter::default();
        for i in 0..MAX_SOURCES as u32 {
            assert!(l.allow_packet(
                std::net::IpAddr::from([10, 0, (i / 256) as u8, (i % 256) as u8]),
                1_000
            ));
        }
        // Table full of live sources: a newcomer is refused rather than evicting one.
        assert!(!l.allow_packet("10.9.9.9".parse().unwrap(), 1_500));
        // Once they expire the newcomer fits.
        assert!(l.allow_packet(
            "10.9.9.9".parse().unwrap(),
            1_000 + DEVICE_TTL_MS + ANNOUNCE_WINDOW_MS
        ));
    }
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
