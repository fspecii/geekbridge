// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in phone transport: KDE Connect protocol 8 over TCP/TLS using `bridge-kde`.
//!
//! Nothing here runs until a `transport_start` request. That request creates one small tokio
//! runtime owned by the [`Host`]; every connection, pairing and discovery task lives on it, so no
//! handshake or pairing ever blocks the calling (JNI/UI) thread. Request handlers only look at or
//! mutate bounded in-memory state and return at once; progress is read back with
//! `transport_status`. `transport_stop` (or dropping the host) rejects pending requests, closes
//! every socket and ends the runtime before returning, so a later start sees no old state.
//!
//! Schema, limits and the Kotlin integration checklist: `docs/android-transport.md`.
mod discovery;
mod engine;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Component, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bridge_kde::share::SHARE_REQUEST;
use bridge_kde::{
    AddressPolicy, DeviceCredentials, DeviceId, DeviceType, Endpoint, FileTrustStore, KdeError,
    Limits, PairMode, TransportConfig, TrustStore, identity::sanitize_device_name,
};
use serde_json::{Value, json};
use tokio::runtime::{Builder, Handle, Runtime};

use self::engine::{
    Core, Decision, Info, MAX_PACKET_LINE, MAX_UNPAIRED, PING, accept_loop, outbound,
};
use crate::dispatch::{ApiError, Fields, err};

pub(crate) const ACTIONS: [&str; 29] = [
    "pairing_parse_qr",
    "transport_start",
    "transport_status",
    "transport_wait",
    "transport_stop",
    "transport_connect",
    "transport_disconnect",
    "transport_pair_request",
    "transport_pair_decision",
    "transport_unpair",
    "transport_set_name",
    "transport_ping",
    "share_send_file",
    "share_send_text",
    "share_accept",
    "share_decline",
    "share_cancel",
    "share_dismiss",
    "transport_clipboard_config",
    "transport_clipboard_wait",
    "transport_clipboard_send",
    "transport_calls_config",
    "transport_calls_wait",
    "transport_calls_report",
    "transport_calls_result",
    "transport_data_config",
    "transport_data_wait",
    "transport_data_send",
    "transport_permits",
];

/// Longest packet line when phone data is enabled: 1 MiB body plus envelope and JSON escaping slack.
const DATA_PACKET_LINE: usize = 1024 * 1024 + 16 * 1024;
const DEFAULT_KDE_PORT: u16 = 1716;
/// Last port of the KDE Connect range 1716-1764.
const MAX_KDE_PORT: u16 = 1764;
const MAX_STATE_DIR_BYTES: usize = 1024;
const MAX_DEVICE_NAME_CHARS: usize = 128;
/// Longest string accepted for a code, fingerprint or device id argument.
const MAX_TOKEN_CHARS: usize = 64;
/// Subdirectory of `state_dir` that holds the identity and trust store. It is created `0700` by
/// `bridge-kde`, which refuses a directory readable by others; Android's `filesDir` itself may be
/// group-accessible, so the private directory is never `filesDir` directly.
const KDE_SUBDIR: &str = "kde";

// ------------------------------------------------------------------------------- host

enum Slot {
    Stopped,
    Starting,
    Running(Running),
    Stopping,
}

struct Running {
    core: Arc<Core>,
    rt: Runtime,
}

/// The transport lifecycle. One per [`Dispatcher`](crate::Dispatcher).
pub(crate) struct Host {
    slot: Mutex<Slot>,
    /// Pairing request ids, never reset, so an id from before a restart can never match again.
    requests: Arc<AtomicU64>,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            slot: Mutex::new(Slot::Stopped),
            requests: Arc::new(AtomicU64::new(1)),
        }
    }
}

/// Leaves `Stopped` behind unless [`Transition::finish`] ran, so a failure or panic during a
/// transition cannot wedge the host in `Starting`/`Stopping`.
struct Transition<'a> {
    host: &'a Host,
    armed: bool,
}

impl Transition<'_> {
    fn finish(mut self, next: Slot) {
        *self.host.lock() = next;
        self.armed = false;
    }
}

impl Drop for Transition<'_> {
    fn drop(&mut self) {
        if self.armed {
            *self.host.lock() = Slot::Stopped;
        }
    }
}

fn busy() -> ApiError {
    err("busy", "the transport is starting or stopping")
}

fn not_running() -> ApiError {
    err("not_running", "the transport is not running")
}

fn shutdown_runtime(rt: Runtime) {
    // `shutdown_timeout` blocks, which a runtime worker thread may not do.
    if Handle::try_current().is_ok() {
        rt.shutdown_background();
    } else {
        rt.shutdown_timeout(Duration::from_secs(2));
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let slot = self.slot.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Slot::Running(r) = std::mem::replace(slot, Slot::Stopped) {
            r.core.shutdown();
            shutdown_runtime(r.rt);
        }
    }
}

impl Host {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn running(&self) -> Result<(Arc<Core>, Handle), ApiError> {
        match &*self.lock() {
            Slot::Running(r) => Ok((r.core.clone(), r.rt.handle().clone())),
            Slot::Stopped => Err(not_running()),
            Slot::Starting | Slot::Stopping => Err(busy()),
        }
    }

    fn start(&self, args: StartArgs) -> Result<Value, ApiError> {
        let guard = {
            let mut slot = self.lock();
            match &*slot {
                Slot::Stopped => *slot = Slot::Starting,
                Slot::Running(_) => {
                    return Err(err("already_running", "the transport is already running"));
                }
                Slot::Starting | Slot::Stopping => return Err(busy()),
            }
            Transition {
                host: self,
                armed: true,
            }
        };
        let (running, result) = build(args, self.requests.clone())?;
        guard.finish(Slot::Running(running));
        Ok(result)
    }

    fn stop(&self) -> Result<Value, ApiError> {
        let running = {
            let mut slot = self.lock();
            match std::mem::replace(&mut *slot, Slot::Stopping) {
                Slot::Running(r) => r,
                Slot::Stopped => {
                    *slot = Slot::Stopped;
                    return Ok(json!({ "state": "stopped", "was_running": false }));
                }
                other => {
                    *slot = other;
                    return Err(busy());
                }
            }
        };
        let guard = Transition {
            host: self,
            armed: true,
        };
        running.core.shutdown();
        shutdown_runtime(running.rt);
        guard.finish(Slot::Stopped);
        Ok(json!({ "state": "stopped", "was_running": true }))
    }

    fn status(&self, since: u64) -> Result<Value, ApiError> {
        let core = match &*self.lock() {
            Slot::Running(r) => r.core.clone(),
            Slot::Stopped => return Ok(json!({ "state": "stopped" })),
            Slot::Starting => return Ok(json!({ "state": "starting" })),
            Slot::Stopping => return Ok(json!({ "state": "stopping" })),
        };
        Ok(core.snapshot(since))
    }

    fn connect(&self, host: &str, port: u64, device: &str) -> Result<Value, ApiError> {
        let (core, handle) = self.running()?;
        // Literal addresses only: no name resolution (and so no network traffic) happens here.
        let ip: IpAddr = host
            .parse()
            .map_err(|_| err("invalid_endpoint", "host must be an IP address literal"))?;
        let port = u16::try_from(port)
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| err("invalid_endpoint", "port is out of range"))?;
        // `Endpoint::connect` has no address policy of its own (it only guards inbound), so an
        // explicit dial is held to the same rule: LAN addresses, loopback only on opt-in.
        core.policy.check(ip).map_err(|_| {
            err(
                "address_rejected",
                "the address is not permitted by the address policy",
            )
        })?;
        let target = parse_device_id(device)?;
        if target == *core.local_id() {
            return Err(err("invalid_device_id", "cannot connect to this device"));
        }
        let attempt = core.begin_attempt(&target)?;
        handle.spawn(outbound(
            core.clone(),
            attempt,
            SocketAddr::new(ip, port),
            target.clone(),
        ));
        Ok(json!({ "accepted": true, "device_id": target.as_str() }))
    }
}

/// Duplicate a descriptor Kotlin owns (`ParcelFileDescriptor.getFd()`); the original stays Kotlin's.
#[cfg(unix)]
#[allow(unsafe_code)]
fn dup_fd(fd: u64) -> Result<std::fs::File, ApiError> {
    use std::os::fd::{BorrowedFd, RawFd};
    let bad = || err("invalid_fd", "the file descriptor is not usable");
    let raw = RawFd::try_from(fd).map_err(|_| bad())?;
    // SAFETY: the caller keeps the descriptor open for the duration of this call (documented in
    // docs/android-contract.md); we only duplicate it immediately, and `dup` on a closed
    // descriptor fails with EBADF instead of touching anything.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    borrowed
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .map_err(|_| bad())
}

#[cfg(not(unix))]
fn dup_fd(_fd: u64) -> Result<std::fs::File, ApiError> {
    Err(err(
        "unsupported_platform",
        "file descriptors are not supported here",
    ))
}

fn parse_device_id(s: &str) -> Result<DeviceId, ApiError> {
    DeviceId::parse(s).map_err(|_| err("invalid_device_id", "device id is not acceptable"))
}

// ------------------------------------------------------------------------------- arguments

struct StartArgs {
    state_dir: PathBuf,
    device_name: String,
    /// `None` when `listen.enabled` is false.
    listen: Option<(IpAddr, u16)>,
    /// The port was not chosen by the app: when 1716 is taken (stock KDE Connect on the same phone)
    /// the next free port up to 1764 is used and announced instead.
    listen_walk: bool,
    discovery: DiscoveryArgs,
    allow_loopback: bool,
    /// `Some((enabled, max_image_bytes))` when the app wants clipboard packets negotiated.
    clipboard: Option<(bool, u64)>,
    /// `Some(enabled)` when the app wants call-control packets negotiated.
    calls: Option<bool>,
    /// Phone data features (contacts, calendar, SMS) to negotiate; all off when absent.
    data: engine::DataFlags,
    /// Which paired computers may use calls / contacts / calendar / SMS; `None` leaves everything open.
    permits: Option<engine::PermitMap>,
}

struct DiscoveryArgs {
    listen: bool,
    announce: bool,
    port: u16,
    announce_to: Option<SocketAddr>,
}

fn invalid_field() -> ApiError {
    err("invalid_field", "a field has the wrong type or value")
}

fn port_field(v: u64) -> Result<u16, ApiError> {
    u16::try_from(v).map_err(|_| invalid_field())
}

fn parse_start(mut f: Fields) -> Result<StartArgs, ApiError> {
    let state_dir = parse_state_dir(f.take_string("state_dir")?)?;
    let device_name = f.take_string("device_name")?;
    if device_name.chars().count() > MAX_DEVICE_NAME_CHARS
        || sanitize_device_name(&device_name).is_none()
    {
        return Err(err("invalid_device_name", "device name is not acceptable"));
    }

    let mut listen_walk = true;
    let listen = match f.take_opt_object("listen")? {
        None => Some((IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_KDE_PORT)),
        Some(mut o) => {
            let enabled = o.take_opt_bool("enabled")?.unwrap_or(true);
            let host = o.take_opt_string("host")?;
            let port = o.take_opt_u64("port")?;
            o.finish()?;
            if !enabled {
                if host.is_some() || port.is_some() {
                    return Err(invalid_field());
                }
                None
            } else {
                let ip = match host {
                    None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                    Some(h) => h.parse().map_err(|_| invalid_field())?,
                };
                listen_walk = port.is_none();
                Some((ip, port.map_or(Ok(DEFAULT_KDE_PORT), port_field)?))
            }
        }
    };

    let discovery = match f.take_opt_object("discovery")? {
        None => DiscoveryArgs {
            listen: false,
            announce: false,
            port: DEFAULT_KDE_PORT,
            announce_to: None,
        },
        Some(mut o) => {
            let listen = o.take_opt_bool("listen")?.unwrap_or(false);
            let announce = o.take_opt_bool("announce")?.unwrap_or(false);
            let port = o.take_opt_u64("port")?;
            let announce_to = o.take_opt_string("announce_to")?;
            o.finish()?;
            let port = port.map_or(Ok(DEFAULT_KDE_PORT), port_field)?;
            let announce_to = match announce_to {
                None => None,
                Some(_) if !announce => return Err(invalid_field()),
                Some(s) => Some(s.parse::<SocketAddr>().map_err(|_| invalid_field())?),
            };
            DiscoveryArgs {
                listen,
                announce,
                port,
                announce_to,
            }
        }
    };
    let allow_loopback = f.take_opt_bool("allow_loopback")?.unwrap_or(false);
    let clipboard = match f.take_opt_object("clipboard")? {
        None => None,
        Some(mut o) => {
            let enabled = o.take_opt_bool("enabled")?.unwrap_or(true);
            let max = o
                .take_opt_u64("max_image_bytes")?
                .unwrap_or(bridge_core::clipboard_wire::DEFAULT_MAX_IMAGE_BYTES);
            o.finish()?;
            Some((enabled, max))
        }
    };
    let calls = match f.take_opt_object("calls")? {
        None => None,
        Some(mut o) => {
            let enabled = o.take_opt_bool("enabled")?.unwrap_or(false);
            o.finish()?;
            Some(enabled)
        }
    };
    let data = match f.take_opt_object("data")? {
        None => engine::DataFlags::default(),
        Some(mut o) => parse_data_flags(&mut o)?,
    };
    let permits = f
        .take_opt_object("permits")?
        .map(parse_permits)
        .transpose()?;
    f.finish()?;

    if discovery.announce && listen.is_none() {
        return Err(err(
            "discovery_requires_listener",
            "announcing needs the TCP listener",
        ));
    }
    if !allow_loopback && listen.is_some_and(|(ip, _)| ip.is_loopback()) {
        return Err(err("loopback_not_allowed", "loopback needs allow_loopback"));
    }
    Ok(StartArgs {
        state_dir,
        device_name,
        listen,
        listen_walk,
        discovery,
        allow_loopback,
        clipboard,
        calls,
        data,
        permits,
    })
}

/// `{"sms": ["<device id>", ...], ...}`: the five known features only, at most 16 valid device ids each.
fn parse_permits(mut o: Fields) -> Result<engine::PermitMap, ApiError> {
    let mut map = engine::PermitMap::new();
    for feature in engine::PERMIT_FEATURES {
        let Some(v) = o.take_opt_array(feature)? else {
            continue;
        };
        if v.len() > 16 {
            return Err(invalid_field());
        }
        let mut ids = std::collections::HashSet::new();
        for item in v {
            let s = item.as_str().ok_or_else(invalid_field)?;
            ids.insert(parse_device_id(s)?.to_string());
        }
        map.insert(feature.to_owned(), ids);
    }
    o.finish()?;
    Ok(map)
}

fn parse_data_flags(o: &mut Fields) -> Result<engine::DataFlags, ApiError> {
    let flags = engine::DataFlags {
        contacts: o.take_opt_bool("contacts")?.unwrap_or(false),
        calendar: o.take_opt_bool("calendar")?.unwrap_or(false),
        calendar_edit: o.take_opt_bool("calendar_edit")?.unwrap_or(false),
        sms: o.take_opt_bool("sms")?.unwrap_or(false),
        debug: o.take_opt_bool("debug")?.unwrap_or(false),
    };
    Ok(flags)
}

fn parse_state_dir(s: String) -> Result<PathBuf, ApiError> {
    let bad = || err("invalid_state_dir", "state_dir is not acceptable");
    if s.is_empty() || s.len() > MAX_STATE_DIR_BYTES || s.contains('\0') {
        return Err(bad());
    }
    let path = PathBuf::from(s);
    if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
        return Err(bad());
    }
    // Follows symlinks on purpose: `/data/user/0` is one on Android.
    match std::fs::metadata(&path) {
        Ok(m) if m.is_dir() => Ok(path),
        _ => Err(bad()),
    }
}

/// Parse the text of a scanned (or pasted) pairing QR code. Pure: nothing is dialled or stored.
fn parse_qr(text: &str) -> Result<Value, ApiError> {
    use bridge_kde::qrpair::{PayloadError, QrPayload};
    let p = QrPayload::parse(text).map_err(|e| {
        let code = match e {
            PayloadError::NotGeekBridge => "qr_not_geekbridge",
            PayloadError::Version => "qr_version",
            PayloadError::Malformed => "qr_malformed",
        };
        err(code, e.message())
    })?;
    Ok(json!({
        "device_id": p.device_id,
        "name": p.name,
        "cert_sha256": p.cert_sha256,
        "token": p.token,
        "addresses": p.addresses.iter().map(|a| json!({ "host": a.ip().to_string(), "port": a.port() })).collect::<Vec<_>>(),
    }))
}

fn token(f: &mut Fields, key: &'static str) -> Result<Option<String>, ApiError> {
    match f.take_opt_string(key)? {
        Some(s) if s.chars().count() > MAX_TOKEN_CHARS => Err(invalid_field()),
        other => Ok(other),
    }
}

pub(crate) fn handle(host: &Host, action: &str, mut f: Fields) -> Result<Value, ApiError> {
    match action {
        "transport_start" => host.start(parse_start(f)?),
        "transport_status" => {
            let since = f.take_opt_u64("since")?.unwrap_or(0);
            f.finish()?;
            host.status(since)
        }
        "transport_wait" => {
            let since = f.take_opt_u64("since")?.unwrap_or(0);
            let timeout_ms = f.take_opt_u64("timeout_ms")?.unwrap_or(30_000);
            f.finish()?;
            // The slot lock is released before blocking: other requests keep working meanwhile.
            let (core, _) = host.running()?;
            Ok(core.wait_events(since, Duration::from_millis(timeout_ms)))
        }
        "transport_stop" => {
            f.finish()?;
            host.stop()
        }
        "transport_connect" => {
            let address = f.take_string("host")?;
            let port = f.take_u64("port")?;
            let device = f.take_string("device_id")?;
            f.finish()?;
            host.connect(&address, port, &device)
        }
        "transport_disconnect" | "transport_ping" => {
            let id = f.take_u64("connection_id")?;
            f.finish()?;
            let (core, _) = host.running()?;
            if action == "transport_ping" {
                core.ping(id)
            } else {
                core.disconnect(id)
            }
        }
        "transport_pair_request" => {
            let id = f.take_u64("connection_id")?;
            let mode = match f.take_opt_string("mode")?.as_deref() {
                None | Some("request") => PairMode::Request,
                Some("await") => PairMode::AwaitPeerRequest,
                Some(_) => return Err(invalid_field()),
            };
            let qr_token = token(&mut f, "qr_token")?;
            let cert = token(&mut f, "qr_cert_sha256")?;
            f.finish()?;
            let qr = match (qr_token, cert) {
                (Some(token), Some(cert_sha256)) => Some(engine::QrPin { token, cert_sha256 }),
                (None, None) => None,
                _ => return Err(err("missing_field", "a required field is missing")),
            };
            host.running()?.0.pair_request(id, mode, qr)
        }
        "pairing_parse_qr" => {
            let text = f.take_string("text")?;
            f.finish()?;
            parse_qr(&text)
        }
        "transport_pair_decision" => {
            let request_id = f.take_u64("request_id")?;
            let connection_id = f.take_u64("connection_id")?;
            let approve = f.take_bool("approve")?;
            let verification_code = token(&mut f, "verification_code")?;
            let peer_cert_sha256 = token(&mut f, "peer_cert_sha256")?;
            f.finish()?;
            // Approving must name what was shown: rejecting needs nothing but the request.
            if approve && (verification_code.is_none() || peer_cert_sha256.is_none()) {
                return Err(err("missing_field", "a required field is missing"));
            }
            host.running()?.0.decide(Decision {
                request_id,
                connection_id,
                approve,
                verification_code,
                peer_cert_sha256,
            })
        }
        "share_send_file" => {
            let connection_id = f.take_u64("connection_id")?;
            let fd = f.take_u64("fd")?;
            let name = f.take_string("name")?;
            let size = f.take_u64("size")?;
            let last_modified_ms = f.take_opt_u64("last_modified_ms")?;
            f.finish()?;
            if name.is_empty() || name.chars().count() > 255 {
                return Err(invalid_field());
            }
            let (core, _) = host.running()?;
            // Duplicated last, so a rejected request never holds a descriptor.
            let file = dup_fd(fd)?;
            core.share_send_file(
                connection_id,
                file,
                &name,
                size,
                last_modified_ms.and_then(|m| i64::try_from(m).ok()),
            )
        }
        "share_send_text" => {
            let connection_id = f.take_u64("connection_id")?;
            let kind = f.take_string("kind")?;
            let text = f.take_string("text")?;
            f.finish()?;
            host.running()?
                .0
                .share_send_text(connection_id, &kind, text)
        }
        "share_accept" => {
            let transfer_id = f.take_u64("transfer_id")?;
            let fd = f.take_u64("fd")?;
            f.finish()?;
            let (core, handle) = host.running()?;
            let file = dup_fd(fd)?;
            core.share_accept(&handle, transfer_id, file)
        }
        "share_decline" | "share_cancel" | "share_dismiss" => {
            let transfer_id = f.take_u64("transfer_id")?;
            f.finish()?;
            let (core, _) = host.running()?;
            match action {
                "share_decline" => core.share_decline(transfer_id),
                "share_cancel" => core.share_cancel(transfer_id),
                _ => core.share_dismiss(transfer_id),
            }
        }
        "transport_clipboard_config" => {
            let enabled = f.take_opt_bool("enabled")?;
            let max = f.take_opt_u64("max_image_bytes")?;
            f.finish()?;
            let (core, _) = host.running()?;
            core.clip().configure(enabled, max);
            Ok(json!({ "ok": true }))
        }
        "transport_clipboard_wait" => {
            let since = f.take_opt_u64("since")?.unwrap_or(0);
            let timeout_ms = f.take_opt_u64("timeout_ms")?.unwrap_or(30_000);
            f.finish()?;
            // The slot lock is released before blocking: other requests keep working meanwhile.
            let (core, _) = host.running()?;
            Ok(core.clip().wait(since, Duration::from_millis(timeout_ms)))
        }
        "transport_clipboard_send" => {
            let connection_id = f.take_opt_u64("connection_id")?;
            let request_id = token(&mut f, "request_id")?;
            let none = token(&mut f, "none")?;
            let kind = token(&mut f, "kind")?;
            let text = f.take_opt_string("text")?;
            let html = f.take_opt_string("html")?;
            let age_ms = f.take_opt_u64("age_ms")?.unwrap_or(0);
            let fd = f.take_opt_u64("image_fd")?;
            f.finish()?;
            let (core, _) = host.running()?;
            let image = fd.map(dup_fd).transpose()?;
            core.clipboard_send(engine::ClipboardSend {
                connection_id,
                request_id,
                none,
                kind,
                text,
                html,
                image,
                age_ms,
            })
        }
        "transport_calls_config" => {
            let enabled = f.take_bool("enabled")?;
            f.finish()?;
            let (core, _) = host.running()?;
            core.calls().configure(enabled);
            Ok(json!({ "ok": true }))
        }
        "transport_calls_wait" => {
            let since = f.take_opt_u64("since")?.unwrap_or(0);
            let timeout_ms = f.take_opt_u64("timeout_ms")?.unwrap_or(30_000);
            f.finish()?;
            let (core, _) = host.running()?;
            Ok(core.calls().wait(since, Duration::from_millis(timeout_ms)))
        }
        "transport_calls_report" => {
            let state = f.take_string("state")?;
            let number = f.take_opt_string("number")?;
            let name = f.take_opt_string("name")?;
            let can_answer = f.take_opt_bool("can_answer")?.unwrap_or(false);
            let can_end = f.take_opt_bool("can_end")?.unwrap_or(false);
            f.finish()?;
            let (core, _) = host.running()?;
            core.calls_report(engine::CallsReport {
                state,
                number,
                name,
                can_answer,
                can_end,
            })
        }
        "transport_calls_result" => {
            let connection_id = f.take_u64("connection_id")?;
            let request_id = f.take_string("request_id")?;
            let failure = token(&mut f, "failure")?;
            f.finish()?;
            let (core, _) = host.running()?;
            core.calls_result(connection_id, &request_id, failure.as_deref())
        }
        "transport_data_config" => {
            let (core, _) = host.running()?;
            let current = core.data().flags();
            let flags = engine::DataFlags {
                contacts: f.take_opt_bool("contacts")?.unwrap_or(current.contacts),
                calendar: f.take_opt_bool("calendar")?.unwrap_or(current.calendar),
                calendar_edit: f
                    .take_opt_bool("calendar_edit")?
                    .unwrap_or(current.calendar_edit),
                sms: f.take_opt_bool("sms")?.unwrap_or(current.sms),
                debug: f.take_opt_bool("debug")?.unwrap_or(current.debug),
            };
            f.finish()?;
            core.data().set_flags(flags);
            Ok(json!({ "ok": true }))
        }
        "transport_data_wait" => {
            let since = f.take_opt_u64("since")?.unwrap_or(0);
            let timeout_ms = f.take_opt_u64("timeout_ms")?.unwrap_or(25_000);
            f.finish()?;
            let (core, _) = host.running()?;
            Ok(core.data().wait(since, Duration::from_millis(timeout_ms)))
        }
        "transport_data_send" => {
            let kind = f.take_string("kind")?;
            let body = f.take_object("body")?;
            let connection_id = f.take_opt_u64("connection_id")?;
            f.finish()?;
            let (core, _) = host.running()?;
            core.data_send(&kind, body, connection_id)
        }
        "transport_permits" => {
            let map = parse_permits(
                f.take_opt_object("permits")?
                    .ok_or_else(|| err("missing_field", "a required field is missing"))?,
            )?;
            f.finish()?;
            let (core, _) = host.running()?;
            core.set_permits(map);
            Ok(json!({ "ok": true }))
        }
        "transport_set_name" => {
            let name = f.take_string("device_name")?;
            f.finish()?;
            host.running()?.0.set_device_name(&name)
        }
        "transport_unpair" => {
            let device = f.take_string("device_id")?;
            f.finish()?;
            let device = parse_device_id(&device)?;
            host.running()?.0.unpair(&device)
        }
        _ => Err(err("unknown_action", "unknown action")),
    }
}

// ------------------------------------------------------------------------------- start

fn state_error(e: KdeError) -> ApiError {
    match e {
        KdeError::InsecurePermissions { .. } => err(
            "insecure_state",
            "the state directory is not private to this app",
        ),
        KdeError::Certificate(_) => err("identity_unusable", "the stored identity is unusable"),
        KdeError::StoreCorrupt { .. } => err("state_corrupt", "the trust store is corrupt"),
        KdeError::StoreNewer { .. } => err(
            "state_newer",
            "the saved state was written by a newer app version and is left untouched",
        ),
        KdeError::UnsupportedPlatform(_) => err(
            "unsupported_platform",
            "private storage is not supported here",
        ),
        _ => err("state_unavailable", "the state directory could not be used"),
    }
}

fn bind_udp(addr: SocketAddr, broadcast: bool) -> std::io::Result<std::net::UdpSocket> {
    // SO_REUSEADDR, like stock KDE Connect: when both apps listen on UDP 1716 the kernel hands
    // each broadcast to both, so neither app silences the other's discovery.
    let socket = socket2::Socket::new(
        socket2::Domain::for_address(addr),
        socket2::Type::DGRAM,
        None,
    )?;
    socket.set_reuse_address(true)?;
    socket.bind(&addr.into())?;
    let socket: std::net::UdpSocket = socket.into();
    socket.set_nonblocking(true)?;
    if broadcast {
        socket.set_broadcast(true)?;
    }
    Ok(socket)
}

/// Bind the TCP listener. A taken port moves on to the next one up to `last` (1764, the range the
/// protocol reserves, when the port was not chosen by the app); the port bound is the one announced.
fn bind_tcp(ip: IpAddr, port: u16, last: u16) -> std::io::Result<std::net::TcpListener> {
    let mut result = std::net::TcpListener::bind(SocketAddr::new(ip, port));
    for next in port.saturating_add(1)..=last {
        if result.is_ok() {
            break;
        }
        result = std::net::TcpListener::bind(SocketAddr::new(ip, next));
    }
    result
}

/// Where to announce: an explicit target, else the limited broadcast address.
fn announce_target(args: &DiscoveryArgs, policy: &AddressPolicy) -> Result<SocketAddr, ApiError> {
    let target = args
        .announce_to
        .unwrap_or(SocketAddr::new(IpAddr::V4(Ipv4Addr::BROADCAST), args.port));
    let broadcast = target.ip() == IpAddr::V4(Ipv4Addr::BROADCAST);
    if !broadcast && policy.check(target.ip()).is_err() {
        return Err(err(
            "address_rejected",
            "the announce target is not permitted by the address policy",
        ));
    }
    Ok(target)
}

/// Spool directory for received clipboard images, inside the app's private state directory.
const CLIPBOARD_SUBDIR: &str = "clipboard-cache";

/// Packet types declared in the identity packet.
fn capabilities(
    clipboard: bool,
    calls: bool,
    data: engine::DataFlags,
) -> (Vec<String>, Vec<String>) {
    let mut incoming = vec![PING.to_owned(), SHARE_REQUEST.to_owned()];
    let mut outgoing = incoming.clone();
    if clipboard {
        incoming.extend(engine::clipboard_incoming_types());
        outgoing.extend(engine::clipboard_outgoing_types());
    }
    if calls {
        incoming.extend(engine::calls_incoming_types());
        outgoing.extend(engine::calls_outgoing_types());
    }
    incoming.extend(engine::data_incoming_types(data));
    outgoing.extend(engine::data_outgoing_types(data));
    (incoming, outgoing)
}

fn build(args: StartArgs, requests: Arc<AtomicU64>) -> Result<(Running, Value), ApiError> {
    let clipboard_on = args.clipboard.is_some();
    let calls_on = args.calls.is_some();
    let policy = AddressPolicy {
        local_networks: true,
        loopback: args.allow_loopback,
    };
    let announce_to = if args.discovery.announce {
        Some(announce_target(&args.discovery, &policy)?)
    } else {
        None
    };

    let dir = args.state_dir.join(KDE_SUBDIR);
    let (creds, created) = DeviceCredentials::load_or_create(&dir).map_err(state_error)?;
    let store = Arc::new(FileTrustStore::open(&dir).map_err(state_error)?);
    let config = TransportConfig {
        address_policy: policy.clone(),
        max_unpaired: MAX_UNPAIRED,
        limits: Limits {
            packet_line: {
                let base = if args.clipboard.is_some() {
                    bridge_core::clipboard_wire::MAX_ITEM_LINE_BYTES
                } else {
                    MAX_PACKET_LINE
                };
                // The link applies one limit to both directions: phone data packets go out up to
                // 1 MiB, so the line limit must cover them (and incoming lines may be that long).
                if args.data == engine::DataFlags::default() {
                    base
                } else {
                    base.max(DATA_PACKET_LINE)
                }
            },
            ..Limits::default()
        },
        payload: bridge_kde::PayloadConfig {
            any_port: policy.loopback,
            ..bridge_kde::PayloadConfig::default()
        },
        ..TransportConfig::default()
    };
    let store_dyn: Arc<dyn TrustStore> = store.clone();
    let endpoint = Endpoint::new(
        Arc::new(creds),
        &args.device_name,
        DeviceType::Phone,
        capabilities(args.clipboard.is_some(), args.calls.is_some(), args.data).0,
        capabilities(args.clipboard.is_some(), args.calls.is_some(), args.data).1,
        store_dyn,
        config,
    )
    .map_err(|_| err("invalid_device_name", "device name is not acceptable"))?;

    // Bind with std on this thread (non-blocking, no I/O wait) so port conflicts are reported
    // synchronously, then hand the sockets to the runtime.
    let tcp = match args.listen {
        Some((ip, port)) => {
            let listener = bind_tcp(ip, port, if args.listen_walk { MAX_KDE_PORT } else { port })
                .and_then(|l| l.set_nonblocking(true).map(|()| l))
                .map_err(|_| err("listen_failed", "the listen address could not be bound"))?;
            Some(listener)
        }
        None => None,
    };
    let udp_listen = if args.discovery.listen {
        Some(bind_udp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), args.discovery.port),
            false,
        ))
    } else {
        None
    };
    let udp_announce = if announce_to.is_some() {
        Some(bind_udp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            true,
        ))
    } else {
        None
    };

    let rt = Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .thread_name("gb-transport")
        .enable_all()
        .build()
        .map_err(|_| err("runtime_failed", "the transport runtime could not start"))?;
    let enter = rt.enter();

    let listen_addr = tcp.as_ref().and_then(|l| l.local_addr().ok());
    let tcp = tcp
        .map(tokio::net::TcpListener::from_std)
        .transpose()
        .map_err(|_| err("listen_failed", "the listen address could not be bound"))?;

    let mut status = discovery::Status::default();
    let mut listen_socket = None;
    match udp_listen {
        Some(Ok(s)) => {
            status.listening_port = s.local_addr().ok().map(|a| a.port());
            listen_socket = tokio::net::UdpSocket::from_std(s).ok();
        }
        Some(Err(_)) => status.error = Some("discovery_bind_failed"),
        None => {}
    }
    let announce = match (udp_announce, announce_to, listen_addr) {
        (Some(Ok(s)), Some(target), Some(addr)) => tokio::net::UdpSocket::from_std(s)
            .ok()
            .map(|s| (s, target, addr.port())),
        (Some(Err(_)), ..) => {
            status.error = Some("discovery_bind_failed");
            None
        }
        _ => None,
    };
    status.announcing = announce.is_some();

    let info = Info {
        device_name: endpoint.local_identity().device_name.clone(),
        cert_sha256: endpoint.credentials().cert().fingerprint_hex(),
        listen: listen_addr,
        discovery: status,
    };
    let announce_port = announce.as_ref().map(|(_, _, port)| *port);
    let core = Arc::new(Core::new(endpoint, store, policy, info, requests));
    if let Some((enabled, max_image)) = args.clipboard {
        core.clip()
            .init(args.state_dir.join(CLIPBOARD_SUBDIR), enabled, max_image);
    }

    if let Some(enabled) = args.calls {
        core.calls().configure(enabled);
    }
    core.data().set_flags(args.data);
    if let Some(p) = args.permits {
        core.set_permits(p);
    }

    // The TCP listener is bound and listening before any task (or announcement) exists.
    if let Some(listener) = tcp {
        rt.spawn(accept_loop(core.clone(), listener));
    }
    if let Some(socket) = listen_socket {
        rt.spawn(discovery::listen_loop(core.clone(), socket));
    }
    if let (Some((socket, target, _)), Some(port)) = (announce, announce_port) {
        // Re-encoded every round, so a rename shows up in the next announcement.
        let announcing = core.clone();
        rt.spawn(discovery::announce_loop(socket, target, move || {
            discovery::encode_announcement(&announcing.endpoint.current_identity(), port)
        }));
    }

    // A restart (network change) must not leave the computer waiting for the next announcement.
    engine::redial_remembered(&core, rt.handle());

    let result = json!({
        "state": "running",
        "device_id": core.local_id().as_str(),
        "device_name": core.info.device_name,
        "cert_sha256": core.info.cert_sha256,
        "identity_created": created,
        "listen": core.info.listen.map(|a| json!({ "host": a.ip().to_string(), "port": a.port() })),
        "discovery": core.info.discovery.to_json(),
        "capabilities": { "incoming": capabilities(clipboard_on, calls_on, args.data).0, "outgoing": capabilities(clipboard_on, calls_on, args.data).1 },
    });
    drop(enter);
    Ok((Running { core, rt }, result))
}

#[cfg(test)]
mod coexist_tests {
    use super::*;

    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    #[test]
    fn busy_port_walks_to_the_next_free_one() {
        let first = std::net::TcpListener::bind((LO, 0)).unwrap();
        let base = first.local_addr().unwrap().port();
        let second = bind_tcp(LO, base, base.saturating_add(5)).unwrap();
        assert_ne!(second.local_addr().unwrap().port(), base);
        assert!(second.local_addr().unwrap().port() <= base.saturating_add(5));
    }

    #[test]
    fn fixed_port_is_never_walked() {
        let first = std::net::TcpListener::bind((LO, 0)).unwrap();
        let base = first.local_addr().unwrap().port();
        assert!(bind_tcp(LO, base, base).is_err());
    }

    #[test]
    fn two_discovery_sockets_can_share_the_port() {
        let a = bind_udp(SocketAddr::new(LO, 0), false).unwrap();
        let port = a.local_addr().unwrap().port();
        // A stock KDE Connect on the same phone binds with SO_REUSEADDR as well.
        let b = bind_udp(SocketAddr::new(LO, port), false).unwrap();
        assert_eq!(b.local_addr().unwrap().port(), port);
    }
}
