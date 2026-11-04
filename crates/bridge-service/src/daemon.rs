// SPDX-License-Identifier: GPL-3.0-or-later
//! Link lifecycle: dial or accept, TLS handshake, explicit pairing, trusted sessions.
//!
//! Invariants (each is exercised by `tests/control_api.rs`):
//!
//! * At most one live link per device id. Unauthenticated/unpaired connections never displace
//!   anything; a connection that matched the pinned certificate may replace an older link.
//! * Trust is only ever written by `bridge_kde`'s pairing after a local approval for the exact
//!   connection and certificate that was shown. Discovery data is used for addresses only and never
//!   reaches the trust store.
//! * A pairing challenge is bound to one link attempt, one connection id and one certificate
//!   fingerprint, expires, and can be decided once. It disappears with its connection.
//! * Only negotiated packet types are accepted from a paired phone, and today that is ping.
//! * Events and status contain connection facts only: never message, contact or clipboard content.

use crate::share::{
    Disposition as ShareDisposition, Notifier as ShareNotifier, SHARE_REQUEST, ShareJob,
    ShareManager, ShareOptions,
};
use crate::{
    SharedState,
    call_control::{CallHub, CallNotifier, HostCalls},
    calling::CallingProbe,
    clipboard::{self, ClipOut, ClipboardBackend, ClipboardService, LinkCaps, Next},
    gate::{self, Incoming, Outgoing},
    hotkey_bind::{self, HotkeyBinder},
    now_ms,
    private::{ConsentStore, random_id},
};
use bridge_core::ipc::{
    CallingStatus, ChallengeDirection, ClipboardDeviceStatus, ClipboardStatus, ConsentEntry,
    Endpoint, ErrorBody, ErrorCode, Event, EventKind, EventPage, Feature, LinkPhase, LinkRole,
    LinkStatus, PairChallenge, PairMode, PairedDevice, Request, Response, Status,
};
use bridge_kde as kde;
use serde_json::Map;
use std::{
    collections::{HashMap, VecDeque},
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    net::TcpStream,
    sync::{Semaphore, mpsc, oneshot},
    task::JoinHandle,
    time::{Instant, timeout, timeout_at},
};

#[path = "pairing_flow.rs"]
mod pairing_flow;

pub const MAX_LINKS: usize = 16;
/// Unpaired links one source address may hold at once (S2-1 in docs/reports/security-review-2.md).
pub const MAX_UNPAIRED_PER_IP: usize = 3;
/// File in the state directory holding the computer name the user chose.
pub const DEVICE_NAME_FILE: &str = "device-name";
pub const MAX_CHALLENGES: usize = 8;
const EVENT_CAP: usize = 256;
const EVENTS_PER_PAGE: usize = 128;
/// Unnegotiated packets tolerated on a trusted link before it is dropped.
const MAX_REJECTED_PACKETS: u64 = 64;
/// Smallest spacing of `PingReceived` events, so a chatty phone cannot flush the event log.
const PING_EVENT_SPACING_MS: u64 = 1000;
const CALLING_DEADLINE: Duration = Duration::from_secs(2);
/// Largest packet line on a trusted link. Clipboard items carry up to two 64 KiB text fields, which
/// JSON may expand; everything bigger travels as a payload.
/// Phone data (a conversation page, a batch of vCards) is far larger than a clipboard item. GeekBridge
/// phones keep every packet under 1 MiB; stock KDE Connect phones may send more, up to this bound.
const MAX_PACKET_LINE: usize = 2 * 1024 * 1024;

pub struct DaemonOptions {
    pub device_name: String,
    pub transport: kde::TransportConfig,
    /// Developer/test switch: allow loopback peers and any TCP port. Off in normal operation,
    /// where only private-network addresses and KDE Connect's 1716-1764 range may be dialled.
    pub dev_loopback: bool,
    /// How long an unpaired outbound link waits for the user to start pairing.
    pub unpaired_idle: Duration,
    pub calling: Arc<dyn CallingProbe>,
    /// OS clipboard for the clipboard bridge. `None` (the default, and what tests use) leaves it
    /// unavailable; `run` installs the detected backend.
    pub clipboard: Option<Arc<dyn ClipboardBackend>>,
    /// Where the clipboard settings (freshness window, hotkey, image limit) are stored.
    pub clipboard_settings: Option<std::path::PathBuf>,
    /// Registers the "paste from phone" hotkey system-wide where the platform allows it.
    pub hotkey_binder: Option<Arc<dyn HotkeyBinder>>,
    /// Update checker. The default never runs by itself; `run` installs a scheduled one.
    pub updates: Arc<bridge_update::Updater>,
    /// File / link / text sharing: save directory, limits and where per-device permissions persist.
    pub share: ShareOptions,
    /// Desktop notification for incoming file offers. `None` shows none (tests); `run` installs one.
    pub share_notifier: Option<Arc<dyn ShareNotifier>>,
    /// Bluetooth hands-free backend for answering calls from this computer. `None` (tests, and
    /// platforms without an adapter) leaves only the phone link.
    pub call_host: Mutex<Option<Box<dyn HostCalls>>>,
    /// Desktop notification with Answer/Decline for a ringing call. `None` shows none.
    pub call_notifier: Option<Arc<dyn CallNotifier>>,
    /// Desktop notification for a newly arrived text (sender only). `None` shows none.
    pub sms_notifier: Option<Arc<dyn crate::phone_data::SmsNotifier>>,
    /// Show message text in new-message notifications. Default off ("hide message content").
    pub sms_show_content: bool,
    /// Desktop banner (with Dismiss and the phone's buttons) for a mirrored phone notification. `None` shows
    /// none (tests); the notifications are still listed by `geekbridge-ctl notifications`.
    pub mirror_notifier: Option<Arc<dyn crate::mirror::MirrorNotifier>>,
    /// Where the user-chosen computer name is saved. A saved name wins over `device_name`.
    pub device_name_file: Option<std::path::PathBuf>,
}

impl DaemonOptions {
    pub fn new(device_name: &str, calling: Arc<dyn CallingProbe>) -> Self {
        let mut transport = kde::TransportConfig::default();
        // Ping and clipboard are integrated; anything larger than a clipboard item is a payload.
        transport.limits.packet_line = MAX_PACKET_LINE;
        Self {
            device_name: device_name.to_owned(),
            transport,
            dev_loopback: false,
            unpaired_idle: Duration::from_secs(120),
            calling,
            clipboard: None,
            clipboard_settings: None,
            hotkey_binder: None,
            updates: bridge_update::Updater::new(bridge_update::Config::github(env!(
                "CARGO_PKG_VERSION"
            )))
            .expect("built-in update configuration is valid"),
            share: ShareOptions::default(),
            share_notifier: None,
            call_host: Mutex::new(None),
            call_notifier: None,
            sms_notifier: None,
            sms_show_content: false,
            mirror_notifier: None,
            device_name_file: None,
        }
    }
}

enum LinkCmd {
    Pair(PairMode),
    Ping,
    Unpair(oneshot::Sender<Result<(), String>>),
    Share(ShareJob),
    /// Say goodbye on the live link; the answer is true once the packet was written.
    Goodbye(bridge_core::goodbye::Reason, oneshot::Sender<bool>),
}

struct LiveLink {
    attempt: u64,
    status: LinkStatus,
    negotiated_out: Vec<String>,
    cmd: mpsc::Sender<LinkCmd>,
    task: JoinHandle<()>,
    last_ping_event_ms: u64,
}

struct PendingChallenge {
    attempt: u64,
    view: PairChallenge,
    expires: Instant,
    /// Taken by the one decision that is allowed to settle it.
    decision: Option<oneshot::Sender<bool>>,
}

#[derive(Default)]
struct Core {
    links: HashMap<String, LiveLink>,
    challenges: HashMap<String, PendingChallenge>,
    events: VecDeque<Event>,
    next_seq: u64,
    next_attempt: u64,
}

impl Core {
    fn emit(&mut self, device: Option<&str>, kind: EventKind) {
        self.next_seq += 1;
        if self.events.len() == EVENT_CAP {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            seq: self.next_seq,
            at_unix_ms: now_ms(),
            device_id: device.map(str::to_owned),
            kind,
        });
        crate::tray::poke();
    }
}

enum Entry {
    Dial(SocketAddr),
    Ready(Box<kde::Handshake>),
}

pub struct Daemon {
    endpoint: kde::Endpoint,
    store: Arc<dyn kde::TrustStore>,
    consents: ConsentStore,
    discovery: SharedState,
    core: Mutex<Core>,
    options: DaemonOptions,
    calling_gate: Arc<Semaphore>,
    inbound: Mutex<Option<SocketAddr>>,
    /// One-time tokens behind "show a QR code" pairing.
    tokens: Mutex<kde::qrpair::TokenBook>,
    clipboard: Option<Arc<ClipboardService>>,
    /// The hotkey currently registered with the binder, if any.
    bound_hotkey: Mutex<Option<String>>,
    share: Arc<ShareManager>,
    calls: Arc<CallHub>,
    data: Arc<crate::phone_data::PhoneData>,
    /// Mirrored phone notifications (memory only, bounded) and the banner for them.
    mirror: crate::mirror::Mirror,
    mirror_notifier: Option<Arc<dyn crate::mirror::MirrorNotifier>>,
    /// Recent phone photos: a list and thumbnails, asked for, memory only (`photos`).
    photos: crate::photos::Photos,
    /// Log lines and state dumps from a debug-build phone (`phone_debug`).
    debug: Arc<crate::phone_debug::PhoneDebug>,
    /// Automatic redial bookkeeping for paired phones (see `reconnect`).
    reconnect: Mutex<crate::reconnect::Book>,
    /// Dial-back after a silent loss, for phones whose announcements do not reach us (see `reconnect::Fallback`).
    fallback: Mutex<crate::reconnect::Fallback>,
    /// When and where each paired phone last announced itself (see `why`).
    sightings: Mutex<crate::why::Sightings>,
}

fn err(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
    }
}

/// The loss reason a clean close from the peer produces.
fn reason_of_clean_close() -> String {
    reason(&kde::KdeError::UnexpectedEof)
}

/// Short, single-line, printable description of a failure for the event log.
fn reason(e: &kde::KdeError) -> String {
    e.to_string()
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(160)
        .collect()
}

/// Developer interop capture: with `--dev-loopback` and `GEEKBRIDGE_WIRE_TAP_FILE=PATH`, append every
/// packet line (after TLS, both directions) to PATH. Packets carry personal data, so this is never on
/// for a normal run; the file is created owner-only.
fn dev_wire_tap(endpoint: kde::Endpoint, dev_loopback: bool) -> kde::Endpoint {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("GEEKBRIDGE_WIRE_TAP_FILE").filter(|_| dev_loopback) else {
        return endpoint;
    };
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let Ok(file) = options.open(path) else {
        return endpoint;
    };
    let file = Mutex::new(file);
    endpoint.with_wire_tap(Arc::new(move |sent, line| {
        if let Ok(mut f) = file.lock() {
            let _ = writeln!(
                f,
                "{} {}",
                if sent { ">>" } else { "<<" },
                String::from_utf8_lossy(line)
            );
        }
    }))
}

fn parse_id(id: &str) -> Result<kde::DeviceId, ErrorBody> {
    kde::DeviceId::parse(id).map_err(|_| err(ErrorCode::BadRequest, "invalid device id"))
}

impl Daemon {
    pub fn new(
        credentials: Arc<kde::DeviceCredentials>,
        store: Arc<dyn kde::TrustStore>,
        consents: ConsentStore,
        discovery: SharedState,
        mut options: DaemonOptions,
    ) -> Result<Arc<Self>, kde::KdeError> {
        if options.dev_loopback {
            options.transport.address_policy.loopback = true;
            options.transport.payload.any_port = true;
        }
        let clipboard = options.clipboard.take().map(|backend| {
            let local = credentials.device_id().to_string();
            match options.clipboard_settings.take() {
                Some(path) => ClipboardService::with_settings_file(backend, &local, path),
                None => ClipboardService::new(backend, &local),
            }
        });
        let endpoint = kde::Endpoint::new(
            credentials,
            &options.device_name,
            kde::DeviceType::Desktop,
            gate::LOCAL_INCOMING
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            gate::LOCAL_OUTGOING
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            store.clone(),
            options.transport.clone(),
        )?;
        if let Some(saved) = options
            .device_name_file
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
        {
            // A damaged or hand-edited file just keeps the configured name.
            let _ = endpoint.set_device_name(saved.trim());
        }
        let endpoint = dev_wire_tap(endpoint, options.dev_loopback);
        let calls = CallHub::new();
        if let Some(n) = options.call_notifier.take() {
            calls.set_notifier(n);
        }
        if let Some(host) = options
            .call_host
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            calls.start_host(host);
        }
        let share = ShareManager::new(options.share.clone());
        if let Some(n) = options.share_notifier.take() {
            share.set_notifier(n);
        }
        let debug = Arc::new(crate::phone_debug::PhoneDebug::new(consents.dir()));
        let data = crate::phone_data::PhoneData::new();
        data.set_notify_content(options.sms_show_content);
        if let Some(n) = options.sms_notifier.take() {
            data.set_notifier(n);
        }
        let mirror = crate::mirror::Mirror::default();
        mirror.set_show_content(options.sms_show_content);
        let mirror_notifier = options.mirror_notifier.take();
        let daemon = Arc::new(Self {
            endpoint,
            store,
            consents,
            discovery,
            core: Mutex::new(Core::default()),
            options,
            calling_gate: Arc::new(Semaphore::new(1)),
            inbound: Mutex::new(None),
            tokens: Mutex::new(kde::qrpair::TokenBook::new()),
            clipboard,
            bound_hotkey: Mutex::new(None),
            share,
            calls,
            data,
            mirror,
            mirror_notifier,
            photos: crate::photos::Photos::default(),
            debug,
            reconnect: Mutex::new(crate::reconnect::Book::default()),
            fallback: Mutex::new(crate::reconnect::Fallback::default()),
            sightings: Mutex::new(crate::why::Sightings::default()),
        });
        let weak = Arc::downgrade(&daemon);
        daemon.share.set_event_sink(Arc::new(move |device, kind| {
            if let Some(d) = weak.upgrade() {
                d.core().emit(device, kind);
            }
        }));
        Ok(daemon)
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn consents(&self) -> &ConsentStore {
        &self.consents
    }

    // ------------------------------------------------------------ recent photos

    fn photos_ready(&self, device_id: &str) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        if !self.consents().is_enabled(device_id, Feature::Photos) {
            return Err(err(
                ErrorCode::Unsupported,
                "recent photos are off for this phone",
            ));
        }
        if !self.data.caps(device_id).photos {
            return Err(err(
                ErrorCode::Unsupported,
                "this phone does not offer recent photos",
            ));
        }
        Ok(())
    }

    fn photos_view(&self, device_id: Option<&str>) -> bridge_core::photos_view::PhotosView {
        use bridge_core::photos_view::{PhotoDevice, PhotosView};
        let now = std::time::Instant::now();
        let connected: Vec<(String, String)> = self
            .core()
            .links
            .iter()
            .filter(|(id, l)| {
                l.status.phase == LinkPhase::Connected && device_id.is_none_or(|d| d == id.as_str())
            })
            .map(|(id, l)| (id.clone(), l.status.name.clone()))
            .collect();
        let mut view = PhotosView::default();
        for (id, name) in connected {
            if !self.data.caps(&id).photos {
                continue;
            }
            let (listed, partial) = self.photos.state(&id, now);
            let dev = PhotoDevice {
                device_id: id.clone(),
                name,
                partial,
                listed,
            };
            if self.consents().is_enabled(&id, Feature::Photos) {
                view.items.extend(self.photos.items(&id, now));
                view.devices.push(dev);
            } else {
                view.offered.push(dev);
            }
        }
        view.items.sort_by_key(|i| std::cmp::Reverse(i.taken_ms));
        let shown: Vec<&str> = view.devices.iter().map(|d| d.device_id.as_str()).collect();
        view.fetches = self
            .share
            .fetches()
            .into_iter()
            .filter(|f| shown.contains(&f.device_id.as_str()))
            .collect();
        view
    }

    /// Ask a phone for its newest photos. Asking again within a few seconds is answered `Ack` without a new request.
    fn photos_refresh(&self, device_id: &str) -> Result<(), ErrorBody> {
        self.photos_ready(device_id)?;
        match self
            .photos
            .begin_refresh(device_id, std::time::Instant::now())
        {
            crate::photos::Refresh::TooSoon => return Ok(()),
            crate::photos::Refresh::Full => {
                return Err(err(
                    ErrorCode::TooMany,
                    "too many phones are listing photos",
                ));
            }
            crate::photos::Refresh::Go => {}
        }
        if self.data.send_stock(
            device_id,
            gate::PHOTOS_REQUEST,
            bridge_core::photos_link::list_request_body(bridge_core::photos_link::MAX_ITEMS),
        ) {
            Ok(())
        } else {
            Err(err(ErrorCode::NotConnected, "the phone is not connected"))
        }
    }

    fn photo_thumb(
        &self,
        device_id: &str,
        id: &str,
    ) -> Result<bridge_core::photos_view::PhotoThumbView, ErrorBody> {
        use base64::Engine as _;
        parse_id(device_id)?;
        if !self.consents().is_enabled(device_id, Feature::Photos) {
            return Err(err(
                ErrorCode::Unsupported,
                "recent photos are off for this phone",
            ));
        }
        let (width, height, jpeg) = self
            .photos
            .thumb(device_id, id)
            .ok_or_else(|| err(ErrorCode::NotFound, "no such thumbnail"))?;
        Ok(bridge_core::photos_view::PhotoThumbView {
            width,
            height,
            jpeg_base64: base64::engine::general_purpose::STANDARD.encode(jpeg),
        })
    }

    /// Ask the phone to send one listed photo in full over the share channel.
    fn photo_fetch(
        &self,
        device_id: &str,
        id: &str,
        mode: bridge_core::photos_view::FetchMode,
    ) -> Result<(), ErrorBody> {
        self.photos_ready(device_id)?;
        let photo = self
            .photos
            .photo(device_id, id)
            .ok_or_else(|| err(ErrorCode::NotFound, "no such photo; refresh the list"))?;
        let fetch = self.share.expect_photo(device_id, id, &photo.name, mode)?;
        if self.data.send_stock(
            device_id,
            gate::PHOTOS_REQUEST,
            bridge_core::photos_link::get_request_body(id),
        ) {
            Ok(())
        } else {
            self.share.cancel_expectation(fetch);
            Err(err(ErrorCode::NotConnected, "the phone is not connected"))
        }
    }

    /// Ask a connected phone for the notifications it is showing (consent already checked by the caller).
    fn ask_notifications(&self, device_id: &str) {
        if self.data.caps(device_id).notification_request {
            self.data.send_stock(
                device_id,
                gate::NOTIFICATION_REQUEST,
                bridge_core::notification_link::request_body(),
            );
        }
    }

    fn notifications_view(&self, device_id: Option<&str>) -> bridge_core::ipc::NotificationsView {
        let enabled: Vec<String> = {
            let c = self.consents();
            self.core()
                .links
                .keys()
                .filter(|id| c.is_enabled(id, Feature::Notifications))
                .cloned()
                .collect()
        };
        self.mirror
            .view(device_id, enabled, std::time::Instant::now())
    }

    /// Dismiss one mirrored notification: gone here, cancelled on the phone when it lets us.
    fn dismiss_notification(&self, device_id: &str, id: &str) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        if !self
            .consents()
            .is_enabled(device_id, Feature::Notifications)
        {
            return Err(err(
                ErrorCode::Unsupported,
                "notification mirroring is off for this phone",
            ));
        }
        let Some(n) = self.mirror.get(device_id, id, std::time::Instant::now()) else {
            return Err(err(ErrorCode::NotFound, "no such notification"));
        };
        self.mirror.remove(device_id, id);
        if n.clearable && self.data.caps(device_id).notification_request {
            self.data.send_stock(
                device_id,
                gate::NOTIFICATION_REQUEST,
                bridge_core::notification_link::dismiss_body(id),
            );
        }
        Ok(())
    }

    /// Press one of a mirrored notification's buttons on the phone.
    fn notification_action(
        &self,
        device_id: &str,
        id: &str,
        action: &str,
    ) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        if !self
            .consents()
            .is_enabled(device_id, Feature::Notifications)
        {
            return Err(err(
                ErrorCode::Unsupported,
                "notification mirroring is off for this phone",
            ));
        }
        let Some(n) = self.mirror.get(device_id, id, std::time::Instant::now()) else {
            return Err(err(ErrorCode::NotFound, "no such notification"));
        };
        if !n.actions.iter().any(|a| a == action) {
            return Err(err(
                ErrorCode::BadRequest,
                "the notification has no such button",
            ));
        }
        if !self.data.caps(device_id).notification_action
            || !self.data.send_stock(
                device_id,
                gate::NOTIFICATION_ACTION,
                bridge_core::notification_link::action_body(id, action),
            )
        {
            return Err(err(
                ErrorCode::Failed,
                "the phone cannot take button presses right now",
            ));
        }
        Ok(())
    }

    /// The phone-data hub (calendar alert ticker).
    pub fn phone_data(&self) -> &Arc<crate::phone_data::PhoneData> {
        &self.data
    }

    pub fn set_inbound_listener(&self, addr: SocketAddr) {
        *self.inbound.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
        // Phones that only ever heard from us by IP learn the real port from the secure identity.
        self.endpoint.set_listen_port(addr.port());
    }

    // ------------------------------------------------------------------ queries

    pub fn status(&self) -> Result<Status, ErrorBody> {
        let peers = self
            .store
            .list()
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        let consents = self.consents();
        let paired: Vec<PairedDevice> = peers
            .into_iter()
            .map(|p| PairedDevice {
                consents: Feature::ALL
                    .iter()
                    .map(|f| ConsentEntry {
                        feature: *f,
                        enabled: consents.is_enabled(p.device_id.as_str(), *f),
                        adapter_integrated: gate::adapter_integrated(*f),
                    })
                    .collect(),
                device_id: p.device_id.to_string(),
                name: p.name,
                device_type: p.device_type,
                cert_sha256: p.cert.fingerprint_hex(),
                paired_at_unix: p.paired_at_unix,
            })
            .collect();
        let now = Instant::now();
        let core = self.core();
        let mut links: Vec<LinkStatus> = core.links.values().map(|l| l.status.clone()).collect();
        links.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        let mut challenges: Vec<PairChallenge> = core
            .challenges
            .values()
            .filter(|c| c.decision.is_some() && c.expires > now)
            .map(|c| {
                let mut view = c.view.clone();
                view.expires_in_ms = c.expires.saturating_duration_since(now).as_millis() as u64;
                view
            })
            .collect();
        challenges.sort_by(|a, b| a.challenge_id.cmp(&b.challenge_id));
        let disconnected = self.why_disconnected(&paired, &core);
        let creds = self.endpoint.credentials();
        Ok(Status {
            ipc_version: bridge_core::ipc::IPC_VERSION,
            service_version: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: creds.device_id().to_string(),
            device_name: self.endpoint.device_name(),
            own_cert_sha256: creds.cert().fingerprint_hex(),
            paired,
            links,
            challenges,
            event_seq: core.next_seq,
            inbound_listener: *self.inbound.lock().unwrap_or_else(|e| e.into_inner()),
            reconnecting: {
                let mut v = self
                    .reconnect
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .trying();
                v.extend(self.lock_fallback().pending());
                v.sort();
                v.dedup();
                v
            },
            disconnected,
        })
    }

    /// The plain-words reason for every paired phone that has no connected link.
    fn why_disconnected(
        &self,
        paired: &[PairedDevice],
        core: &Core,
    ) -> Vec<bridge_core::why_disconnected::WhyDisconnected> {
        use bridge_core::why_disconnected::{Facts, explain};
        const FRESH_MS: u64 = 20_000;
        let waiting: Vec<&PairedDevice> = paired
            .iter()
            .filter(|p| {
                !core
                    .links
                    .get(&p.device_id)
                    .is_some_and(|l| l.status.phase == LinkPhase::Connected)
            })
            .collect();
        if waiting.is_empty() {
            return vec![];
        }
        let now = now_ms();
        let ifaces = crate::lan_select::system_interfaces();
        let online = crate::why::computer_online(&ifaces);
        let listening = self
            .inbound
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        let book = self.reconnect.lock().unwrap_or_else(|e| e.into_inner());
        let sightings = self.sightings.lock().unwrap_or_else(|e| e.into_inner());
        waiting
            .into_iter()
            .map(|p| {
                let id = p.device_id.as_str();
                let (tries, paused) = book.progress(id);
                let seen = sightings.get(id);
                let age_ms = seen.map(|(_, at)| now.saturating_sub(at));
                explain(
                    id,
                    &Facts {
                        computer_online: online,
                        listening,
                        paused,
                        announcing: age_ms.is_some_and(|a| a < FRESH_MS),
                        tries,
                        max_tries: crate::reconnect::MAX_TRIES,
                        last_seen_ago_s: age_ms.map(|a| a / 1000),
                        on_our_network: seen
                            .and_then(|(ip, _)| crate::why::on_our_network(&ifaces, ip)),
                        last_end: crate::why::last_end(&core.events, id),
                    },
                )
            })
            .collect()
    }

    pub fn events(&self, after: u64) -> EventPage {
        let core = self.core();
        let newest = core.next_seq;
        if after > newest {
            // The client last spoke to an earlier run of the service.
            return EventPage {
                events: vec![],
                next_after: newest,
                gap: true,
            };
        }
        let oldest = core.events.front().map_or(newest + 1, |e| e.seq);
        let events: Vec<Event> = core
            .events
            .iter()
            .filter(|e| e.seq > after)
            .take(EVENTS_PER_PAGE)
            .cloned()
            .collect();
        EventPage {
            next_after: events.last().map_or(after, |e| e.seq),
            events,
            gap: after + 1 < oldest,
        }
    }

    // ------------------------------------------------------------------ dispatch

    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        let result = match request {
            Request::Status => self.status().map(|s| Response::Status(Box::new(s))),
            Request::Events { after } => Ok(Response::Events(self.events(after))),
            Request::Connect {
                device_id,
                endpoint,
            } => self
                .connect(&device_id, endpoint)
                .await
                .map(|()| Response::Ack),
            Request::Pair { device_id, mode } => {
                self.pair(&device_id, mode).map(|()| Response::Ack)
            }
            Request::Approve { challenge_id } => {
                self.decide(&challenge_id, true).map(|()| Response::Ack)
            }
            Request::Deny { challenge_id } => {
                self.decide(&challenge_id, false).map(|()| Response::Ack)
            }
            Request::PairQr => self.pair_qr().map(|q| Response::PairQr(Box::new(q))),
            Request::PairQrCancel => {
                self.pair_qr_cancel();
                Ok(Response::Ack)
            }
            Request::TestConnection {
                device_id,
                endpoint,
            } => self
                .test_connection(device_id.as_deref(), endpoint)
                .await
                .map(|t| Response::ConnectionTest(Box::new(t))),
            Request::SetDeviceName { name } => self.set_device_name(&name).map(|()| Response::Ack),
            Request::Disconnect { device_id } => {
                self.disconnect(&device_id).await.map(|()| Response::Ack)
            }
            Request::Unpair { device_id } => self.unpair(&device_id).await.map(|()| Response::Ack),
            Request::SetConsent {
                device_id,
                feature,
                enabled,
            } => {
                // File lock + fsync inside: off the async workers.
                let d = self.clone();
                tokio::task::spawn_blocking(move || d.set_consent(&device_id, feature, enabled))
                    .await
                    .unwrap_or_else(|_| Err(err(ErrorCode::Failed, "the change was interrupted")))
                    .map(|()| Response::Ack)
            }
            Request::Ping { device_id } => self.ping(&device_id).map(|()| Response::Ack),
            Request::PhoneDebugState { device_id } => self
                .phone_debug_state(&device_id)
                .await
                .map(|text| Response::PhoneDebugState { text }),
            Request::Notifications { device_id } => Ok(Response::Notifications(Box::new(
                self.notifications_view(device_id.as_deref()),
            ))),
            Request::Photos { device_id } => Ok(Response::Photos(Box::new(
                self.photos_view(device_id.as_deref()),
            ))),
            Request::PhotosRefresh { device_id } => {
                self.photos_refresh(&device_id).map(|()| Response::Ack)
            }
            Request::PhotoThumb { device_id, id } => self
                .photo_thumb(&device_id, &id)
                .map(|t| Response::PhotoThumb(Box::new(t))),
            Request::PhotoFetch {
                device_id,
                id,
                mode,
            } => self
                .photo_fetch(&device_id, &id, mode)
                .map(|()| Response::Ack),
            Request::DismissNotification { device_id, id } => self
                .dismiss_notification(&device_id, &id)
                .map(|()| Response::Ack),
            Request::NotificationAction {
                device_id,
                id,
                action,
            } => self
                .notification_action(&device_id, &id, &action)
                .map(|()| Response::Ack),
            Request::CallingStatus => self.calling_status().await.map(Response::CallingStatus),
            Request::Calls => Ok(Response::Calls(Box::new(self.calls.status()))),
            Request::CallAction { target, action } => {
                let hub = self.calls.clone();
                blocking(move || hub.act(&target, action)).await
            }
            Request::CallDial { source, number } => {
                let hub = self.calls.clone();
                blocking(move || hub.dial(&source, &number)).await
            }
            Request::CallMute { source, muted } => {
                let hub = self.calls.clone();
                blocking(move || hub.mute(&source, muted)).await
            }
            Request::CallAudio { source } => {
                let hub = self.calls.clone();
                blocking(move || hub.connect_audio(&source)).await
            }
            Request::ClipboardStatus => self
                .clipboard_status()
                .map(|s| Response::ClipboardStatus(Box::new(s))),
            Request::ClipboardPull { device_id } => self
                .clipboard_pull(device_id.as_deref())
                .map(|()| Response::Ack),
            Request::ClipboardPush { device_id } => self
                .clipboard_push(device_id.as_deref())
                .await
                .map(|()| Response::Ack),
            Request::ClipboardSettings {
                window_ms,
                hotkey,
                max_image_bytes,
            } => self
                .clipboard_settings(window_ms, hotkey.as_deref(), max_image_bytes)
                .map(|()| Response::Ack),
            Request::ShareStatus => self.share_status().map(|s| Response::Share(Box::new(s))),
            Request::ShareSendFile { device_id, path } => self
                .share_send(&device_id, ShareSend::File(path))
                .await
                .map(|transfer_id| Response::TransferStarted { transfer_id }),
            Request::ShareSendText {
                device_id,
                text_kind,
                text,
            } => self
                .share_send(&device_id, ShareSend::Text(text_kind, text))
                .await
                .map(|_| Response::Ack),
            Request::ShareDecide {
                transfer_id,
                accept,
            } => self
                .share
                .decide(transfer_id, accept)
                .map(|()| Response::Ack),
            Request::ShareCancel { transfer_id } => {
                self.share.cancel(transfer_id).map(|()| Response::Ack)
            }
            Request::ShareDismiss { transfer_id } => {
                self.share.dismiss(transfer_id).map(|()| Response::Ack)
            }
            Request::ShareSettings {
                device_id,
                receive,
                auto_accept,
            } => {
                // File lock + fsync inside: off the async workers.
                let d = self.clone();
                tokio::task::spawn_blocking(move || {
                    d.share_settings(&device_id, receive, auto_accept)
                })
                .await
                .unwrap_or_else(|_| Err(err(ErrorCode::Failed, "the change was interrupted")))
                .map(|()| Response::Ack)
            }
            Request::AgendaView {
                query,
                day,
                utc_offset_minutes,
                days,
                people_limit,
            } => self
                .data
                .agenda_view_range(&query, &day, utc_offset_minutes, days, people_limit)
                .map(|v| Response::Agenda(Box::new(v))),
            Request::CalendarEdit {
                device_id,
                op_id,
                action,
                calendar_id,
                event_id,
                draft,
            } => self
                .data
                .calendar_edit(
                    &device_id,
                    &op_id,
                    action,
                    calendar_id.as_deref(),
                    event_id.as_deref(),
                    draft.as_deref(),
                )
                .map(|()| Response::Ack),
            Request::CalendarEditRetry { device_id, op_id } => self
                .data
                .calendar_edit_retry(&device_id, &op_id)
                .map(|()| Response::Ack),
            Request::AgendaRefresh { device_id } => self
                .data
                .refresh_agenda(device_id.as_deref())
                .map(|()| Response::Ack),
            Request::MessagesView {
                device_id,
                thread_id,
                query,
                message_limit,
            } => self
                .data
                .messages_view_window(
                    device_id.as_deref(),
                    thread_id,
                    query.as_deref(),
                    message_limit,
                )
                .map(|v| Response::Messages(Box::new(v))),
            Request::MessagesRefresh {
                device_id,
                thread_id,
            } => self
                .data
                .refresh_messages(&device_id, thread_id)
                .map(|()| Response::Ack),
            Request::MessagesSend {
                device_id,
                recipient,
                text,
                submission_id,
                sub_id,
            } => self
                .data
                .send_text(&device_id, &recipient, &text, &submission_id, sub_id)
                .map(|send_id| Response::MessageQueued { send_id }),
            Request::MessagesMarkRead {
                device_id,
                thread_id,
            } => self
                .data
                .mark_read(&device_id, thread_id)
                .map(|()| Response::Ack),
            Request::MessagesResend {
                device_id,
                send_id,
                submission_id,
            } => self
                .data
                .resend(&device_id, send_id, &submission_id)
                .map(|send_id| Response::MessageQueued { send_id }),
            Request::UpdateStatus => Ok(self.update_response()),
            Request::UpdateCheck => {
                self.options.updates.start_check(true);
                Ok(self.update_response())
            }
            Request::UpdateDownload => self
                .options
                .updates
                .start_download()
                .map(|()| self.update_response())
                .map_err(|e| err(ErrorCode::Failed, e.to_string())),
            Request::UpdateSkip { version } => self
                .options
                .updates
                .skip(&version)
                .map(|()| self.update_response())
                .map_err(|_| err(ErrorCode::BadRequest, "not a version number")),
            Request::UpdateSetEnabled { enabled } => {
                self.options.updates.set_enabled(enabled);
                Ok(self.update_response())
            }
        };
        result.unwrap_or_else(Response::Error)
    }

    // ------------------------------------------------------------------ sharing

    fn share_status(&self) -> Result<bridge_core::share::ShareStatus, ErrorBody> {
        let paired = self
            .store
            .list()
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?
            .into_iter()
            .map(|p| p.device_id.to_string())
            .collect::<Vec<_>>();
        Ok(self.share.status(&paired))
    }

    fn share_settings(
        &self,
        device_id: &str,
        receive: Option<bool>,
        auto_accept: Option<bool>,
    ) -> Result<(), ErrorBody> {
        let id = parse_id(device_id)?;
        match self.store.get(&id) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(err(ErrorCode::NotFound, "device is not paired")),
            Err(e) => return Err(err(ErrorCode::Failed, reason(&e))),
        }
        self.share.set_settings(device_id, receive, auto_accept)
    }

    /// Queue a file or text for a connected, paired device that accepts shares.
    async fn share_send(&self, device_id: &str, what: ShareSend) -> Result<u64, ErrorBody> {
        parse_id(device_id)?;
        let (cmd, name) = {
            let core = self.core();
            let link = core
                .links
                .get(device_id)
                .filter(|l| l.status.phase == LinkPhase::Connected)
                .ok_or_else(|| {
                    err(
                        ErrorCode::NotConnected,
                        "the device is not connected and paired",
                    )
                })?;
            if !link.negotiated_out.iter().any(|k| k == SHARE_REQUEST) {
                return Err(err(
                    ErrorCode::Unsupported,
                    "the device does not accept shared files",
                ));
            }
            (link.cmd.clone(), link.status.name.clone())
        };
        let (id, job) = match what {
            ShareSend::File(path) => {
                let id = self.share.begin_send_file(device_id, &name, &path)?;
                (
                    id,
                    ShareJob::File {
                        transfer_id: id,
                        path: path.into(),
                    },
                )
            }
            ShareSend::Text(kind, text) => {
                ShareManager::validate_text(kind, &text)?;
                (0, ShareJob::Text { kind, text })
            }
        };
        if cmd.try_send(LinkCmd::Share(job)).is_err() {
            if id != 0 {
                self.share.abandon(id);
            }
            return Err(err(ErrorCode::Busy, "the link is busy or closing"));
        }
        Ok(id)
    }

    fn update_response(&self) -> Response {
        Response::Update(Box::new(self.options.updates.status()))
    }

    // ------------------------------------------------------------------ commands

    fn check_endpoint(&self, ip: IpAddr, port: u16) -> Result<(), ErrorBody> {
        self.endpoint
            .config()
            .address_policy
            .check(ip)
            .map_err(|_| {
                err(
                    ErrorCode::InvalidEndpoint,
                    "address is not on a permitted local network",
                )
            })?;
        if !self.options.dev_loopback && !(1716..=1764).contains(&port) {
            return Err(err(
                ErrorCode::InvalidEndpoint,
                "port is outside the KDE Connect range 1716-1764",
            ));
        }
        if port == 0 {
            return Err(err(ErrorCode::InvalidEndpoint, "port 0 is not valid"));
        }
        Ok(())
    }

    async fn connect(
        self: &Arc<Self>,
        device_id: &str,
        endpoint: Option<Endpoint>,
    ) -> Result<(), ErrorBody> {
        let target = parse_id(device_id)?;
        if target == *self.endpoint.credentials().device_id() {
            return Err(err(
                ErrorCode::BadRequest,
                "refusing to connect to ourselves",
            ));
        }
        let (ip, port) = match endpoint {
            Some(e) => (e.address, e.port),
            None => {
                let discovery = self.discovery.read().await;
                let seen = discovery
                    .devices
                    .iter()
                    .find(|d| d.id == device_id)
                    .ok_or_else(|| {
                        err(
                            ErrorCode::NotFound,
                            "device has not been discovered; pass an endpoint",
                        )
                    })?;
                (seen.address, seen.tcp_port)
            }
        };
        self.check_endpoint(ip, port)?;
        if matches!(self.store.get(&target), Ok(Some(_))) {
            self.lock_fallback().learn(device_id, ip, port);
        }
        let addr = SocketAddr::new(ip, port);
        let mut core = self.core();
        if core.links.contains_key(device_id) {
            return Err(err(
                ErrorCode::Busy,
                "a link for this device already exists",
            ));
        }
        if core.links.len() >= MAX_LINKS {
            return Err(err(ErrorCode::TooMany, "too many concurrent links"));
        }
        let paired_name = self.store.get(&target).ok().flatten().map(|p| p.name);
        let status = LinkStatus {
            device_id: device_id.to_owned(),
            name: paired_name.unwrap_or_default(),
            phase: LinkPhase::Connecting,
            role: Some(LinkRole::Initiator),
            connection_id: None,
            peer_ip: Some(ip),
            peer_cert_sha256: None,
            negotiated: vec![],
            since_unix_ms: now_ms(),
            pings_received: 0,
            last_ping_unix_ms: None,
            rejected_packets: 0,
            battery: None,
        };
        self.install(&mut core, status, vec![], Entry::Dial(addr));
        core.emit(Some(device_id), EventKind::Connecting);
        Ok(())
    }

    /// Register a link and start its task. Must be called with the core lock held so the task's
    /// first state update waits until the entry exists.
    fn install(
        self: &Arc<Self>,
        core: &mut Core,
        status: LinkStatus,
        negotiated_out: Vec<String>,
        entry: Entry,
    ) {
        core.next_attempt += 1;
        let attempt = core.next_attempt;
        let (tx, rx) = mpsc::channel(8);
        let id = status.device_id.clone();
        let task = tokio::spawn(run_session(self.clone(), id.clone(), attempt, entry, rx));
        core.links.insert(
            id,
            LiveLink {
                attempt,
                status,
                negotiated_out,
                cmd: tx,
                task,
                last_ping_event_ms: 0,
            },
        );
    }

    /// Rename this computer for the connections made from now on, and save the name.
    pub fn set_device_name(&self, raw: &str) -> Result<(), ErrorBody> {
        let name = kde::identity::validate_device_name(raw)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        if let Some(path) = &self.options.device_name_file {
            kde::fsutil::atomic_write_private(path, name.as_bytes())
                .map_err(|_| err(ErrorCode::Failed, "the name could not be saved"))?;
        }
        self.endpoint
            .set_device_name(&name)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        Ok(())
    }

    fn pair(&self, device_id: &str, mode: PairMode) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        let mut core = self.core();
        let link = core
            .links
            .get_mut(device_id)
            .ok_or_else(|| err(ErrorCode::NotFound, "no live link for this device"))?;
        if link.status.phase != LinkPhase::Unpaired {
            return Err(err(
                ErrorCode::NotUnpaired,
                "the link is not waiting to be paired",
            ));
        }
        link.cmd
            .try_send(LinkCmd::Pair(mode))
            .map_err(|_| err(ErrorCode::NotConnected, "the link is closing"))?;
        link.status.phase = LinkPhase::Pairing;
        Ok(())
    }

    /// Settle a challenge. Every binding is re-checked at decision time.
    fn decide(&self, challenge_id: &str, approve: bool) -> Result<(), ErrorBody> {
        let mut core = self.core();
        let core = &mut *core;
        let challenge = core
            .challenges
            .get_mut(challenge_id)
            .ok_or_else(|| err(ErrorCode::NotFound, "no such pairing challenge"))?;
        if challenge.expires <= Instant::now() {
            return Err(err(
                ErrorCode::ChallengeExpired,
                "the pairing challenge has expired",
            ));
        }
        let bound = core.links.get(&challenge.view.device_id).is_some_and(|l| {
            l.attempt == challenge.attempt
                && l.status.phase == LinkPhase::Pairing
                && l.status.connection_id == Some(challenge.view.connection_id)
                && l.status.peer_cert_sha256.as_deref()
                    == Some(challenge.view.peer_cert_sha256.as_str())
        });
        if !bound {
            return Err(err(
                ErrorCode::ChallengeStale,
                "the connection or certificate behind this challenge has changed",
            ));
        }
        let decision = challenge.decision.take().ok_or_else(|| {
            err(
                ErrorCode::ChallengeStale,
                "the challenge was already decided",
            )
        })?;
        decision
            .send(approve)
            .map_err(|_| err(ErrorCode::ChallengeStale, "the pairing attempt has ended"))
    }

    /// Remove the live link and wait until its task is gone, so nothing it was doing can still
    /// touch the trust store afterwards. False if there was no link.
    async fn take_down(&self, device_id: &str) -> bool {
        let Some(link) = self.core().links.remove(device_id) else {
            return false;
        };
        link.task.abort();
        // Aborted above, so this only waits for the cancellation to land.
        let _ = link.task.await;
        self.core()
            .challenges
            .retain(|_, c| c.attempt != link.attempt);
        true
    }

    /// One pass of the automatic redial: every paired phone that announced itself recently, has no link and whose
    /// back-off allows it is dialed at the address it announced from. Trust comes from the pairing store only.
    pub async fn reconnect_tick(self: &Arc<Self>) {
        const FRESH_MS: u64 = 20_000;
        let now_unix = now_ms();
        let heard: Vec<(String, IpAddr, u64)> = self
            .discovery
            .read()
            .await
            .devices
            .iter()
            .map(|d| (d.id.clone(), d.address, d.last_seen_unix_ms))
            .collect();
        {
            let mut s = self.sightings.lock().unwrap_or_else(|e| e.into_inner());
            for (id, ip, at) in heard {
                s.record(&id, ip, at);
            }
        }
        let seen: Vec<(String, IpAddr, u16)> = self
            .discovery
            .read()
            .await
            .devices
            .iter()
            .filter(|d| now_unix.saturating_sub(d.last_seen_unix_ms) < FRESH_MS)
            .map(|d| (d.id.clone(), d.address, d.tcp_port))
            .collect();
        // A phone counts as gone (its redial history and a manual-disconnect pause are dropped) only once it has
        // been silent for as long as discovery keeps it. Using the 20 s dial freshness here dropped the history in
        // every gap between two of the phone's 30 s announcements: a manual disconnect lasted one announcement and
        // the back-off restarted with each one.
        let recent: Vec<String> = self
            .discovery
            .read()
            .await
            .devices
            .iter()
            .filter(|d| now_unix.saturating_sub(d.last_seen_unix_ms) < crate::DEVICE_TTL_MS)
            .map(|d| d.id.clone())
            .collect();
        let ids: Vec<&str> = recent.iter().map(String::as_str).collect();
        let now = Instant::now();
        self.reconnect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keep_only(&ids);
        for (id, ip, port) in &seen {
            let Ok(target) = parse_id(id) else { continue };
            if !matches!(self.store.get(&target), Ok(Some(_))) {
                continue;
            }
            {
                // Announcing: the normal redial below takes over from a dial-back.
                let mut fb = self.lock_fallback();
                fb.learn(id, *ip, *port);
                fb.settled(id);
            }
            let (linked, connected) = {
                let core = self.core();
                let link = core.links.get(id);
                (
                    link.is_some(),
                    link.is_some_and(|l| l.status.phase == LinkPhase::Connected),
                )
            };
            let go = {
                let mut book = self.reconnect.lock().unwrap_or_else(|e| e.into_inner());
                if connected {
                    book.settled(id);
                    false
                } else {
                    !linked && book.due(id, (*ip, *port), now)
                }
            };
            if !go {
                continue;
            }
            let _ = self
                .connect(
                    id,
                    Some(Endpoint {
                        address: *ip,
                        port: *port,
                    }),
                )
                .await;
        }
        self.dial_back_tick(now).await;
    }

    /// Dial back paired phones whose link was lost silently and that are not announcing: a sleeping phone's timers
    /// stand still (no redial, no announcement), and an incoming connection is what wakes it.
    async fn dial_back_tick(self: &Arc<Self>, now: Instant) {
        let pending = self.lock_fallback().pending();
        for id in pending {
            let paired = parse_id(&id).is_ok_and(|t| matches!(self.store.get(&t), Ok(Some(_))));
            let (linked, connected) = {
                let core = self.core();
                let link = core.links.get(&id);
                (
                    link.is_some(),
                    link.is_some_and(|l| l.status.phase == LinkPhase::Connected),
                )
            };
            let paused = self
                .reconnect
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .progress(&id)
                .1;
            if !paired || connected || paused {
                self.lock_fallback().settled(&id);
                continue;
            }
            if linked {
                continue; // a dial or an inbound handshake is in progress
            }
            let due = self.lock_fallback().due(&id, now);
            let Some((address, port)) = due else {
                continue;
            };
            let _ = self.connect(&id, Some(Endpoint { address, port })).await;
        }
    }

    fn lock_fallback(&self) -> std::sync::MutexGuard<'_, crate::reconnect::Fallback> {
        self.fallback.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Tell a connected GeekBridge phone why the link is about to close. False when there was nobody to tell (no
    /// live link, a stock peer that does not declare the type) or the write failed; the caller closes anyway.
    async fn say_goodbye(&self, device_id: &str, reason: bridge_core::goodbye::Reason) -> bool {
        let cmd = {
            let core = self.core();
            core.links
                .get(device_id)
                .filter(|l| {
                    l.status.phase == LinkPhase::Connected
                        && l.negotiated_out.iter().any(|k| k == gate::GOODBYE)
                })
                .map(|l| l.cmd.clone())
        };
        let Some(cmd) = cmd else { return false };
        let (tx, rx) = oneshot::channel();
        if cmd.send(LinkCmd::Goodbye(reason, tx)).await.is_err() {
            return false;
        }
        let sent = matches!(timeout(Duration::from_secs(2), rx).await, Ok(Ok(true)));
        if sent {
            // Let the packet leave before the socket is torn down (closing with unread input resets it).
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        sent
    }

    /// The service is stopping: tell every connected GeekBridge phone, so it redials with its normal back-off
    /// instead of treating the silence as a lost network. Bounded; never blocks the shutdown for long.
    pub async fn goodbye_all(&self) {
        let ids: Vec<String> = self.core().links.keys().cloned().collect();
        let _ = timeout(Duration::from_secs(3), async {
            for id in &ids {
                self.say_goodbye(id, bridge_core::goodbye::Reason::Shutdown)
                    .await;
            }
        })
        .await;
    }

    async fn disconnect(&self, device_id: &str) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        let told = self
            .say_goodbye(device_id, bridge_core::goodbye::Reason::UserDisconnect)
            .await;
        let had_link = self.take_down(device_id).await;
        self.lock_fallback().settled(device_id);
        if !had_link {
            return Err(err(ErrorCode::NotFound, "no live link for this device"));
        }
        {
            let now = Instant::now();
            let mut book = self.reconnect.lock().unwrap_or_else(|e| e.into_inner());
            if told {
                // The phone stays away for this long; we match it instead of waiting for it to go quiet.
                book.pause_for(device_id, now, bridge_core::goodbye::SUPPRESS_FOR);
            } else {
                book.pause(device_id, now);
            }
        }
        self.core().emit(Some(device_id), EventKind::Disconnected);
        Ok(())
    }

    async fn unpair(self: &Arc<Self>, device_id: &str) -> Result<(), ErrorBody> {
        let id = parse_id(device_id)?;
        self.lock_fallback().forget(device_id);
        let connected = {
            let core = self.core();
            core.links
                .get(device_id)
                .filter(|l| l.status.phase == LinkPhase::Connected)
                .map(|l| l.cmd.clone())
        };
        if let Some(cmd) = connected {
            // The trusted session owns the link: it deletes the record, then tells the phone.
            let (tx, rx) = oneshot::channel();
            if cmd.send(LinkCmd::Unpair(tx)).await.is_ok() {
                match timeout(Duration::from_secs(2), rx).await {
                    Ok(Ok(Ok(()))) => {
                        self.forget_consent_off_worker(device_id).await?;
                        return Ok(());
                    }
                    Ok(Ok(Err(e))) => return Err(err(ErrorCode::Failed, e)),
                    _ => {}
                }
            }
        }
        // No trusted session (or it vanished): stop whatever is attached, then forget locally.
        self.take_down(device_id).await;
        // File lock + fsync: off the async workers.
        let removed = {
            let d = self.clone();
            tokio::task::spawn_blocking(move || d.store.remove(&id))
                .await
                .map_err(|_| err(ErrorCode::Failed, "the change was interrupted"))?
                .map_err(|e| err(ErrorCode::Failed, reason(&e)))?
        };
        self.forget_consent_off_worker(device_id).await?;
        if !removed {
            return Err(err(ErrorCode::NotFound, "device is not paired"));
        }
        self.core().emit(Some(device_id), EventKind::Unpaired);
        Ok(())
    }

    /// `forget_consent` takes file locks and fsyncs (share settings, consent file): never on an async worker.
    async fn forget_consent_off_worker(self: &Arc<Self>, device_id: &str) -> Result<(), ErrorBody> {
        let (d, id) = (self.clone(), device_id.to_owned());
        tokio::task::spawn_blocking(move || d.forget_consent(&id))
            .await
            .unwrap_or_else(|_| Err(err(ErrorCode::Failed, "the change was interrupted")))
    }

    fn forget_consent(&self, device_id: &str) -> Result<(), ErrorBody> {
        self.mirror.clear(device_id);
        self.photos.clear(device_id);
        self.share.forget_device(device_id);
        self.data.forget(device_id);
        if let Some(clip) = &self.clipboard {
            clip.forget(device_id);
            self.sync_hotkey();
        }
        // A debug-build phone's log is phone-derived data: it goes with the pairing.
        let logs = self.debug.erase(device_id);
        self.consents()
            .forget(device_id)
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        logs.map_err(|e| err(ErrorCode::Failed, e.to_string()))
    }

    fn set_consent(
        &self,
        device_id: &str,
        feature: Feature,
        enabled: bool,
    ) -> Result<(), ErrorBody> {
        let id = parse_id(device_id)?;
        match self.store.get(&id) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(err(ErrorCode::NotFound, "device is not paired")),
            Err(e) => return Err(err(ErrorCode::Failed, reason(&e))),
        }
        self.consents()
            .set(device_id, feature, enabled)
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        if feature == Feature::Clipboard
            && let Some(clip) = &self.clipboard
        {
            clip.set_enabled(device_id, enabled);
            self.sync_hotkey();
        }
        if feature == Feature::Telephony {
            self.calls.set_enabled(device_id, enabled);
        }
        self.data.set_enabled(device_id, feature, enabled);
        if feature == Feature::Notifications {
            if enabled {
                self.ask_notifications(device_id);
            } else {
                // Withdrawn: everything held is erased at once.
                self.mirror.clear(device_id);
            }
        }
        if feature == Feature::Debug && !enabled {
            // Withdrawn: the log kept for this phone is erased at once.
            self.debug
                .erase(device_id)
                .map_err(|e| err(ErrorCode::Failed, e.to_string()))?;
        }
        if feature == Feature::Photos && !enabled {
            // Withdrawn: the list, the thumbnails and any photo still being waited for are gone at once.
            self.photos.clear(device_id);
            self.share.drop_expectations(device_id);
        }
        if feature == Feature::Battery {
            if enabled {
                self.data.request_battery(device_id);
            } else if let Some(link) = self.core().links.get_mut(device_id) {
                // Withdrawn: what was shown is erased at once.
                link.status.battery = None;
            }
        }
        self.core().emit(
            Some(device_id),
            EventKind::ConsentChanged { feature, enabled },
        );
        Ok(())
    }

    /// Ask a debug-build phone for its redacted state dump and wait for the answer.
    async fn phone_debug_state(&self, device_id: &str) -> Result<String, ErrorBody> {
        parse_id(device_id)?;
        match self.store.get(&parse_id(device_id)?) {
            Ok(Some(_)) => {}
            Ok(None) => return Err(err(ErrorCode::NotFound, "device is not paired")),
            Err(e) => return Err(err(ErrorCode::Failed, reason(&e))),
        }
        let connected = self
            .core()
            .links
            .get(device_id)
            .is_some_and(|l| l.status.phase == LinkPhase::Connected);
        let consent = self.consents().is_enabled(device_id, Feature::Debug);
        let (request_id, answer) = self
            .debug
            .request_state(device_id, consent, connected)
            .map_err(|e| {
                let code = match e {
                    crate::phone_debug::AskError::NotConnected => ErrorCode::NotConnected,
                    _ => ErrorCode::Failed,
                };
                err(code, e.message())
            })?;
        let waited = tokio::time::timeout(crate::phone_debug::STATE_TIMEOUT, answer).await;
        match waited {
            Ok(Ok(text)) => Ok(text),
            _ => {
                self.debug.cancel_wait(device_id, &request_id);
                Err(err(
                    ErrorCode::Failed,
                    crate::phone_debug::AskError::Timeout.message(),
                ))
            }
        }
    }

    fn ping(&self, device_id: &str) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        let core = self.core();
        let link = core
            .links
            .get(device_id)
            .filter(|l| l.status.phase == LinkPhase::Connected)
            .ok_or_else(|| {
                err(
                    ErrorCode::NotConnected,
                    "the device is not connected and paired",
                )
            })?;
        let consents = self.consents();
        match gate::check_outgoing(gate::PING, &link.negotiated_out, |f| {
            consents.is_enabled(device_id, f)
        }) {
            Outgoing::Allowed => {}
            Outgoing::NotNegotiated => {
                return Err(err(
                    ErrorCode::Unsupported,
                    "the phone does not accept ping",
                ));
            }
            Outgoing::ConsentRequired(_) | Outgoing::NotIntegrated(_) => {
                return Err(err(ErrorCode::Unsupported, "ping is not available"));
            }
        }
        link.cmd
            .try_send(LinkCmd::Ping)
            .map_err(|_| err(ErrorCode::Busy, "the link is busy or closing"))
    }

    // ------------------------------------------------------------------ clipboard

    fn clipboard_status(&self) -> Result<ClipboardStatus, ErrorBody> {
        let peers = self
            .store
            .list()
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        let connected: std::collections::HashSet<String> = self
            .core()
            .links
            .iter()
            .filter(|(_, l)| l.status.phase == LinkPhase::Connected)
            .map(|(id, _)| id.clone())
            .collect();
        let consents = self.consents();
        let devices = peers
            .into_iter()
            .map(|p| {
                let id = p.device_id.to_string();
                let enabled = consents.is_enabled(&id, Feature::Clipboard);
                let live = connected.contains(&id);
                match &self.clipboard {
                    Some(clip) => clip.device_status(&id, &p.name, enabled, live),
                    None => ClipboardDeviceStatus {
                        device_id: id,
                        name: p.name,
                        enabled,
                        connected: live,
                        negotiated: false,
                        sent: 0,
                        received: 0,
                        skipped: 0,
                        last_sent_unix_ms: None,
                        last_received_unix_ms: None,
                        last_skip: None,
                    },
                }
            })
            .collect();
        Ok(match &self.clipboard {
            Some(clip) => clip.status(devices),
            None => clipboard::unavailable_status(devices),
        })
    }

    fn clipboard_service(&self) -> Result<&Arc<ClipboardService>, ErrorBody> {
        self.clipboard.as_ref().ok_or_else(|| {
            err(
                ErrorCode::Unsupported,
                "no clipboard backend is available on this computer",
            )
        })
    }

    fn clipboard_error(code: &'static str) -> ErrorBody {
        let kind = match code {
            "no_enabled_phone_connected" | "device_not_enabled_or_connected" => {
                ErrorCode::NotConnected
            }
            "too_many_requests" | "link_busy" => ErrorCode::Busy,
            _ => ErrorCode::Unsupported,
        };
        let text = match code {
            "no_enabled_phone_connected" | "device_not_enabled_or_connected" => {
                "no connected phone has Clipboard switched on. Turn on Clipboard sync in the phone app \
                 (Home > Settings) and allow Clipboard for it on this computer \
                 (`geekbridge-ctl consent DEVICE_ID clipboard on`), then try again"
            }
            "phone_app_cannot_be_asked" => {
                "this phone's GeekBridge app is too old to be asked for its clipboard: update the app on the phone"
            }
            "link_busy" => "the link to the phone is busy: wait a moment and try again",
            "too_many_requests" => {
                "too many clipboard requests are waiting: wait for the phone to answer, then try again"
            }
            other => other,
        };
        err(kind, text)
    }

    fn clipboard_pull(&self, device_id: Option<&str>) -> Result<(), ErrorBody> {
        if let Some(id) = device_id {
            parse_id(id)?;
        }
        self.clipboard_service()?
            .pull(device_id)
            .map(|_| ())
            .map_err(Self::clipboard_error)
    }

    async fn clipboard_push(&self, device_id: Option<&str>) -> Result<(), ErrorBody> {
        if let Some(id) = device_id {
            parse_id(id)?;
        }
        self.clipboard_service()?
            .push_now(device_id)
            .await
            .map(|_| ())
            .map_err(Self::clipboard_error)
    }

    fn clipboard_settings(
        &self,
        window_ms: Option<u64>,
        hotkey: Option<&str>,
        max_image_bytes: Option<u64>,
    ) -> Result<(), ErrorBody> {
        self.clipboard_service()?
            .update_settings(window_ms, hotkey, max_image_bytes)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        self.sync_hotkey();
        Ok(())
    }

    /// Keep the system-wide hotkey in step with the settings and with whether any phone is opted
    /// in: bound only while it is useful, and re-bound when the user changes the combination.
    fn sync_hotkey(&self) {
        let (Some(clip), Some(binder)) = (&self.clipboard, &self.options.hotkey_binder) else {
            return;
        };
        let wanted = clip
            .any_enabled()
            .then(|| clip.settings().hotkey)
            .and_then(|h| bridge_core::hotkey::Hotkey::parse(&h).ok());
        let mut bound = self.bound_hotkey.lock().unwrap_or_else(|e| e.into_inner());
        let wanted_text = wanted.as_ref().map(|h| h.normalized());
        if *bound == wanted_text {
            return;
        }
        if let Some(old) = bound
            .take()
            .and_then(|h| bridge_core::hotkey::Hotkey::parse(&h).ok())
        {
            binder.unbind(&old);
        }
        if let Some(h) = wanted {
            let command = hotkey_bind::paste_command(hotkey_bind::sibling_ctl().as_deref());
            if binder.bind(&h, &command) {
                *bound = Some(h.normalized());
            }
        }
        clip.set_binding_state(if bound.is_some() {
            "bound"
        } else {
            "not_bound"
        });
    }

    async fn calling_status(&self) -> Result<CallingStatus, ErrorBody> {
        let permit = self
            .calling_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| err(ErrorCode::Busy, "a calling probe is already running"))?;
        let probe = self.options.calling.clone();
        let work = tokio::task::spawn_blocking(move || {
            let status = probe.probe();
            drop(permit);
            status
        });
        match timeout(CALLING_DEADLINE, work).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(_)) => Err(err(ErrorCode::Failed, "calling probe failed")),
            Err(_) => Err(err(ErrorCode::Failed, "calling probe timed out")),
        }
    }

    // ------------------------------------------------------------------ inbound

    /// Handshake one accepted TCP connection (the address policy runs before any byte is read).
    /// Returns when the handshake is over; the session itself continues in its own task.
    pub async fn adopt_inbound(self: &Arc<Self>, stream: TcpStream) {
        if let Ok(handshake) = self.endpoint.accept(stream).await {
            self.install_ready(handshake);
        }
    }

    fn install_ready(self: &Arc<Self>, handshake: kde::Handshake) {
        let info = handshake.info().clone();
        let id = info.peer.device_id.to_string();
        let trusted = matches!(handshake, kde::Handshake::Trusted(_));
        let mut core = self.core();
        // Strangers must not starve the table (docs/reports/security-review-2.md S2-1): one address holds
        // only a few unpaired links, and a paired phone takes the place of the oldest stranger.
        let mut evicted = None;
        match core.links.get(&id) {
            Some(_) if !trusted => return,
            None if !trusted && unpaired_from(&core, info.peer_ip) >= MAX_UNPAIRED_PER_IP => {
                return;
            }
            None if core.links.len() >= MAX_LINKS => {
                let victim = trusted.then(|| oldest_unpaired(&core)).flatten();
                let Some(victim) = victim else { return };
                evicted = core.links.remove(&victim);
                if let Some(old) = &evicted {
                    core.challenges.retain(|_, c| c.attempt != old.attempt);
                }
            }
            _ => {}
        }
        if trusted
            && core.links.get(&id).is_some_and(|held| {
                crate::collision::keep_held(
                    &self.endpoint.local_identity().device_id,
                    &info.peer,
                    &held.status,
                    info.role,
                    now_ms(),
                )
            })
        {
            // Both ends dialed at once and the smaller id's connection is the one both keep: turn this one away.
            return;
        }
        let displaced = core.links.remove(&id);
        if let Some(old) = &displaced {
            core.challenges.retain(|_, c| c.attempt != old.attempt);
            core.emit(
                Some(&id),
                EventKind::LinkLost {
                    reason: "replaced by a newer authenticated connection".into(),
                },
            );
        }
        let (status, out) = describe(&info, trusted);
        self.install(&mut core, status, out, Entry::Ready(Box::new(handshake)));
        drop(core);
        // A phone that dialed in tells us where it listens: that is where a dial-back goes.
        if let (true, Some(port)) = (trusted, info.peer.geekbridge_port) {
            self.lock_fallback().learn(&id, info.peer_ip, port);
        }
        // Aborted only after the lock is released: dropping the task runs guards that take it.
        for old in displaced.into_iter().chain(evicted) {
            old.task.abort();
        }
    }

    // ------------------------------------------------------------------ task helpers

    /// Apply `f` if the link entry still belongs to `attempt`.
    fn update(
        &self,
        id: &str,
        attempt: u64,
        f: impl FnOnce(&mut LiveLink, &mut VecDeque<Event>),
    ) -> bool {
        let mut core = self.core();
        match core.links.get_mut(id) {
            Some(link) if link.attempt == attempt => {
                let mut sink = VecDeque::new();
                f(link, &mut sink);
                for event in sink {
                    core.emit(event.device_id.as_deref(), event.kind);
                }
                true
            }
            _ => false,
        }
    }

    /// End the link (if it is still ours), record why.
    fn finish(&self, id: &str, attempt: u64, kind: EventKind) {
        let mut core = self.core();
        if core.links.get(id).is_some_and(|l| l.attempt == attempt) {
            let removed = core.links.remove(id);
            core.challenges.retain(|_, c| c.attempt != attempt);
            // A trusted link that ended without a clean close (reset, timeout): dial the phone back if it does not
            // come back by itself. A clean close is the phone's own doing (its restart, its user's disconnect).
            let silent = removed.is_some_and(|l| l.status.phase == LinkPhase::Connected)
                && matches!(&kind, EventKind::LinkLost { reason } if *reason != reason_of_clean_close());
            core.emit(Some(id), kind);
            drop(core);
            if silent {
                self.lock_fallback().lost(id, Instant::now());
            }
        }
    }

    fn event(&self, id: &str, kind: EventKind) {
        self.core().emit(Some(id), kind);
    }
}

fn event(id: &str, kind: EventKind) -> Event {
    Event {
        seq: 0,
        at_unix_ms: 0,
        device_id: Some(id.to_owned()),
        kind,
    }
}

fn is_unpaired(l: &LiveLink) -> bool {
    matches!(l.status.phase, LinkPhase::Unpaired | LinkPhase::Pairing)
}

/// Links in the table that are waiting to be paired and came from `ip`.
fn unpaired_from(core: &Core, ip: IpAddr) -> usize {
    core.links
        .values()
        .filter(|l| is_unpaired(l) && l.status.peer_ip == Some(ip))
        .count()
}

/// The longest-waiting unpaired link: what a paired phone may displace when the table is full.
fn oldest_unpaired(core: &Core) -> Option<String> {
    core.links
        .iter()
        .filter(|(_, l)| is_unpaired(l))
        .min_by_key(|(_, l)| l.attempt)
        .map(|(id, _)| id.clone())
}

fn describe(info: &kde::LinkInfo, trusted: bool) -> (LinkStatus, Vec<String>) {
    let status = LinkStatus {
        device_id: info.peer.device_id.to_string(),
        name: info.peer.device_name.clone(),
        phase: if trusted {
            LinkPhase::Connected
        } else {
            LinkPhase::Unpaired
        },
        role: Some(match info.role {
            kde::Role::Initiator => LinkRole::Initiator,
            kde::Role::Acceptor => LinkRole::Acceptor,
        }),
        connection_id: Some(info.connection_id.0),
        peer_ip: Some(info.peer_ip),
        peer_cert_sha256: Some(info.peer_cert.fingerprint_hex()),
        negotiated: if trusted {
            gate::negotiated_incoming(&info.peer)
        } else {
            vec![]
        },
        since_unix_ms: now_ms(),
        pings_received: 0,
        last_ping_unix_ms: None,
        rejected_packets: 0,
        battery: None,
    };
    (
        status,
        if trusted {
            gate::negotiated_outgoing(&info.peer)
        } else {
            vec![]
        },
    )
}

// ---------------------------------------------------------------------- session task

async fn run_session(
    d: Arc<Daemon>,
    id: String,
    attempt: u64,
    entry: Entry,
    mut rx: mpsc::Receiver<LinkCmd>,
) {
    let handshake = match entry {
        Entry::Ready(h) => *h,
        Entry::Dial(addr) => {
            let target = match kde::DeviceId::parse(&id) {
                Ok(t) => t,
                Err(_) => {
                    return d.finish(
                        &id,
                        attempt,
                        EventKind::ConnectFailed {
                            reason: "invalid id".into(),
                        },
                    );
                }
            };
            match d.endpoint.connect(addr, &target).await {
                Ok(h) => h,
                Err(e) => {
                    return d.finish(
                        &id,
                        attempt,
                        EventKind::ConnectFailed { reason: reason(&e) },
                    );
                }
            }
        }
    };
    let info = handshake.info().clone();
    let inbound = info.role == kde::Role::Acceptor;
    let trusted = matches!(handshake, kde::Handshake::Trusted(_));
    let (described, out) = describe(&info, trusted);
    let still_ours = d.update(&id, attempt, |link, events| {
        link.status = LinkStatus {
            since_unix_ms: link.status.since_unix_ms,
            ..described
        };
        link.negotiated_out = out;
        events.push_back(event(
            &id,
            if trusted {
                EventKind::Connected
            } else {
                EventKind::NeedsPairing {
                    peer_cert_sha256: info.peer_cert.fingerprint_hex(),
                }
            },
        ));
    });
    if !still_ours {
        return;
    }
    let mut handshake = handshake;
    // A trusted link whose peer forgot us becomes a pending one in place (`re_pairing`).
    let mut re_pairing = false;
    loop {
        match handshake {
            kde::Handshake::Trusted(link) => {
                let Some(pending) = trusted_loop(&d, &id, attempt, link, &mut rx).await else {
                    return;
                };
                let fingerprint = pending.info().peer_cert.fingerprint_hex();
                let ours = d.update(&id, attempt, |l, events| {
                    l.status.phase = LinkPhase::Pairing;
                    l.negotiated_out = vec![];
                    events.push_back(event(
                        &id,
                        EventKind::NeedsPairing {
                            peer_cert_sha256: fingerprint,
                        },
                    ));
                });
                if !ours {
                    return;
                }
                re_pairing = true;
                handshake = kde::Handshake::Pending(pending);
            }
            kde::Handshake::Pending(pending) => {
                let mode = if inbound || re_pairing {
                    // Waiting for the phone to ask sends nothing; the user still has to approve.
                    d.update(&id, attempt, |l, _| l.status.phase = LinkPhase::Pairing);
                    kde::PairMode::AwaitPeerRequest
                } else {
                    let deadline = Instant::now() + d.options.unpaired_idle;
                    loop {
                        match timeout_at(deadline, rx.recv()).await {
                            Ok(Some(LinkCmd::Pair(PairMode::Request))) => {
                                break kde::PairMode::Request;
                            }
                            Ok(Some(LinkCmd::Pair(PairMode::Await))) => {
                                break kde::PairMode::AwaitPeerRequest;
                            }
                            Ok(Some(_)) => continue,
                            Ok(None) => return,
                            Err(_) => {
                                return d.finish(
                                    &id,
                                    attempt,
                                    EventKind::LinkLost {
                                        reason: "unpaired link idle".into(),
                                    },
                                );
                            }
                        }
                    }
                };
                let mut approver = ChallengeApprover {
                    daemon: d.clone(),
                    device_id: id.clone(),
                    attempt,
                    connection_id: info.connection_id.0,
                    peer_cert_sha256: info.peer_cert.fingerprint_hex(),
                };
                match pending.pair(mode, &mut approver).await {
                    Ok(link) => {
                        let (described, out) = describe(link.info(), true);
                        let ours = d.update(&id, attempt, |l, events| {
                            l.status = LinkStatus {
                                since_unix_ms: now_ms(),
                                ..described
                            };
                            l.negotiated_out = out;
                            events.push_back(event(&id, EventKind::Paired));
                            events.push_back(event(&id, EventKind::Connected));
                        });
                        if !ours {
                            return;
                        }
                        handshake = kde::Handshake::Trusted(link);
                    }
                    Err(e) => {
                        return d.finish(
                            &id,
                            attempt,
                            EventKind::PairFailed { reason: reason(&e) },
                        );
                    }
                }
            }
        }
    }
}

async fn trusted_loop(
    d: &Arc<Daemon>,
    id: &str,
    attempt: u64,
    mut link: kde::TrustedLink,
    rx: &mut mpsc::Receiver<LinkCmd>,
) -> Option<kde::PendingLink> {
    let negotiated_in = gate::negotiated_incoming(&link.info().peer);
    let share_name = link.info().peer.device_name.clone();
    let (mut clip_rx, _clip_guard) = clipboard_link(d, id, &link);
    let (mut call_rx, _call_guard) = calls_link(d, id, &link);
    let (mut data_rx, _data_guard) = data_link(d, id, &link);
    let (mut answer_rx, _answer_guard) = answer_link(d, id, &link);
    let debug_declared = debug_declared(&link);
    debug_default(d, id, debug_declared).await;
    let (mut debug_rx, _debug_guard) = debug_link(d, id, &link);
    // A ping every 20 s keeps access-point / NAT idle timers and the phone's Wi-Fi power saving from
    // dropping a quiet link. The phone judges our silence (it knows what its own sleep looks like); we
    // never cut a phone off for being quiet, kernel keepalive does that. Stock KDE Connect peers get none: they
    // show every ping as a notification.
    let pings = gate::negotiated_outgoing(&link.info().peer)
        .iter()
        .any(|k| k == gate::PING)
        && bridge_core::heartbeat::is_geekbridge_peer(
            &link.info().peer.incoming_capabilities,
            &link.info().peer.outgoing_capabilities,
        );
    let mut beat = bridge_core::heartbeat::Heartbeat::new(std::time::Instant::now());
    let mut tick = tokio::time::interval_at(
        tokio::time::Instant::now() + bridge_core::heartbeat::EVERY,
        bridge_core::heartbeat::EVERY,
    );
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let loss = loop {
        tokio::select! {
            _ = tick.tick(), if pings => {
                if beat.ping_due(std::time::Instant::now()) {
                    let ping = kde::Packet::new(gate::PING, Map::new());
                    if let Err(e) = link.send(&ping).await {
                        break reason(&e);
                    }
                    beat.sent(std::time::Instant::now());
                }
            },
            incoming = link.recv() => match incoming {
                Ok(kde::LinkEvent::Packet(packet)) => {
                    if ShareManager::is_share_packet(&packet.kind) {
                        // Batch bookkeeping from stock peers; nothing in it is acted on.
                        if packet.kind == kde::share::SHARE_REQUEST_UPDATE {
                            continue;
                        }
                        let refused = !negotiated_in.contains(&packet.kind)
                            || matches!(
                                d.share.on_packet(id, &share_name, link.payload_context(), &packet),
                                ShareDisposition::Rejected
                            );
                        if refused && !count_rejected(d, id, attempt) {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if route_clipboard(d, id, &packet, &negotiated_in, &link) {
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::Calls {
                        let clean = packet.payload_size.is_none() && packet.payload_transfer_info.is_none();
                        if (!clean || !d.calls.on_packet(id, &packet.kind, &packet.body))
                            && !count_rejected(d, id, attempt)
                        {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::PhoneData {
                        let clean = packet.payload_size.is_none() && packet.payload_transfer_info.is_none();
                        let verdict = if clean {
                            d.data.on_packet(id, &packet.kind, &packet.body)
                        } else {
                            crate::phone_data::Disposition::Invalid
                        };
                        if verdict == crate::phone_data::Disposition::Invalid
                            && !count_rejected(d, id, attempt)
                        {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::Battery {
                        let clean = packet.payload_size.is_none() && packet.payload_transfer_info.is_none();
                        if (!clean || !on_battery_packet(d, id, attempt, &packet))
                            && !count_rejected(d, id, attempt)
                        {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::Photos {
                        let clean = packet.payload_size.is_none() && packet.payload_transfer_info.is_none();
                        if (!clean || !on_photos_packet(d, id, &packet))
                            && !count_rejected(d, id, attempt)
                        {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::Notification {
                        // An icon payload is never fetched; the packet itself is judged on its body.
                        if !on_notification_packet(d, id, &packet) && !count_rejected(d, id, attempt) {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if gate::classify_incoming(&packet.kind, &negotiated_in) == Incoming::Debug {
                        let clean = packet.payload_size.is_none() && packet.payload_transfer_info.is_none();
                        let verdict = if clean {
                            on_debug_packet(d, id, &packet)
                        } else {
                            crate::phone_debug::Disposition::Invalid
                        };
                        if verdict == crate::phone_debug::Disposition::Invalid
                            && !count_rejected(d, id, attempt)
                        {
                            break "too many packets outside the negotiated set".to_owned();
                        }
                        continue;
                    }
                    if !on_packet(d, id, attempt, &packet, &negotiated_in) {
                        break "too many packets outside the negotiated set".to_owned();
                    }
                }
                Ok(kde::LinkEvent::PeerUnpaired) => {
                    // bridge-kde already deleted the trust record.
                    let _ = d.forget_consent_off_worker(id).await;
                    d.finish(id, attempt, EventKind::PeerUnpaired);
                    return None;
                }
                Ok(kde::LinkEvent::PeerRequestedRePair { .. }) => {
                    // The peer forgot us while our record outlived it. Its request is answered
                    // like any incoming one: the user approves a challenge on this connection.
                    d.event(id, EventKind::PeerRequestedRePair);
                    d.share.link_lost(id);
                    match link.into_re_pairing() {
                        Ok(pending) => return Some(pending),
                        Err(back) => link = back,
                    }
                }
                Err(e) => break reason(&e),
            },
            Some(out) = clip_rx.recv() => {
                if let Err(e) = send_clip(&mut link, out).await {
                    break reason(&e);
                }
            },
            Some(request) = call_rx.recv() => {
                let packet = kde::Packet::new(
                    gate::CALLS_REQUEST,
                    match &request.number {
                        Some(n) => bridge_core::calls_link::dial_request_body(
                            &request.request_id,
                            n.expose(),
                        ),
                        None => bridge_core::calls_link::request_body(
                            &request.request_id,
                            request.action,
                        ),
                    },
                );
                if let Err(e) = link.send(&packet).await {
                    break reason(&e);
                }
            },
            Some(out) = data_rx.recv() => {
                if let Err(e) = link.send(&kde::Packet::new(out.kind, out.body)).await {
                    break reason(&e);
                }
            },
            Some(answer) = answer_rx.recv() => {
                let packet = kde::Packet::new(
                    gate::SHARE_RESULT,
                    bridge_core::share_result::body(&answer),
                );
                if let Err(e) = link.send(&packet).await {
                    break reason(&e);
                }
            },
            Some(out) = debug_rx.recv() => {
                if let Err(e) = link.send(&kde::Packet::new(&out.kind, out.body)).await {
                    break reason(&e);
                }
            },
            command = rx.recv() => match command {
                Some(LinkCmd::Ping) => {
                    let ping = kde::Packet::new(gate::PING, Map::new());
                    if let Err(e) = link.send(&ping).await {
                        break reason(&e);
                    }
                }
                Some(LinkCmd::Goodbye(why, ack)) => {
                    let packet =
                        kde::Packet::new(gate::GOODBYE, bridge_core::goodbye::body(why));
                    match link.send(&packet).await {
                        Ok(()) => {
                            let _ = ack.send(true);
                        }
                        Err(e) => {
                            let _ = ack.send(false);
                            break reason(&e);
                        }
                    }
                }
                Some(LinkCmd::Unpair(ack)) => {
                    let result = link.unpair().await.map_err(|e| reason(&e));
                    let ok = result.is_ok();
                    let _ = ack.send(result);
                    if ok {
                        d.finish(id, attempt, EventKind::Unpaired);
                    }
                    return None;
                }
                Some(LinkCmd::Share(job)) => {
                    if !d.share.run_job(&mut link, job).await {
                        break "the link failed while announcing a share".to_owned();
                    }
                }
                Some(LinkCmd::Pair(_)) => {}
                None => return None,
            },
        }
    };
    // Jobs queued behind the failure will never run: fail them so they stop holding transfer slots.
    rx.close();
    while let Ok(cmd) = rx.try_recv() {
        if let LinkCmd::Share(job) = cmd {
            d.share.drop_job(job);
        }
    }
    d.share.link_lost(id);
    d.finish(id, attempt, EventKind::LinkLost { reason: loss });
    None
}

enum ShareSend {
    File(String),
    Text(bridge_core::share::ShareTextKind, String),
}

/// Count a refused share packet against the link's misbehaviour budget. False once it is spent.
fn count_rejected(d: &Daemon, id: &str, attempt: u64) -> bool {
    let mut keep = true;
    d.update(id, attempt, |link, events| {
        link.status.rejected_packets += 1;
        if link.status.rejected_packets == 1 {
            events.push_back(event(id, EventKind::PacketRejected));
        }
        keep = link.status.rejected_packets <= MAX_REJECTED_PACKETS;
    });
    keep
}

/// Tells the clipboard bridge about a new trusted link. The guard takes the link down again however
/// the session task ends.
fn clipboard_link(
    d: &Arc<Daemon>,
    id: &str,
    link: &kde::TrustedLink,
) -> (mpsc::Receiver<ClipOut>, ClipboardLinkGuard) {
    let Some(clip) = d.clipboard.clone() else {
        // A closed channel: that select branch is simply never ready.
        return (mpsc::channel(1).1, ClipboardLinkGuard(None));
    };
    let peer = &link.info().peer;
    let caps = LinkCaps::from_negotiated(
        &gate::negotiated_outgoing(peer),
        &gate::negotiated_incoming(peer),
    );
    let enabled = d.consents().is_enabled(id, Feature::Clipboard);
    let (rx, generation) = clip.link_up_generation(id, &peer.device_name, caps, enabled);
    d.sync_hotkey();
    (
        rx,
        ClipboardLinkGuard(Some((d.clone(), id.to_owned(), generation))),
    )
}

/// Registers a trusted link with the call hub. Requests are only sent if the phone declared it
/// accepts `geekbridge.calls.request`; otherwise the hub never offers a link control for it.
fn calls_link(
    d: &Arc<Daemon>,
    id: &str,
    link: &kde::TrustedLink,
) -> (
    mpsc::Receiver<crate::call_control::LinkRequest>,
    CallsLinkGuard,
) {
    let peer = &link.info().peer;
    let speaks = gate::negotiated_outgoing(peer)
        .iter()
        .any(|k| k == gate::CALLS_REQUEST)
        && gate::negotiated_incoming(peer)
            .iter()
            .any(|k| k == gate::CALLS_STATE);
    if !speaks {
        return (mpsc::channel(1).1, CallsLinkGuard(None));
    }
    let enabled = d.consents().is_enabled(id, Feature::Telephony);
    let (rx, generation) = d.calls.link_up_generation(id, &peer.device_name, enabled);
    (
        rx,
        CallsLinkGuard(Some((d.clone(), id.to_owned(), generation))),
    )
}

/// Registers a trusted link with the phone-data hub. Requests go out only for features the user
/// allowed for this device and that the phone declared.
fn data_link(
    d: &Arc<Daemon>,
    id: &str,
    link: &kde::TrustedLink,
) -> (mpsc::Receiver<crate::phone_data::Out>, DataLinkGuard) {
    let peer = &link.info().peer;
    let caps = crate::phone_data::Caps::from_negotiated(
        &gate::negotiated_outgoing(peer),
        &gate::negotiated_incoming(peer),
    );
    let c = d.consents();
    let consents = (
        c.is_enabled(id, Feature::Contacts),
        c.is_enabled(id, Feature::Calendar),
        c.is_enabled(id, Feature::Sms),
    );
    let edit = c_edit(d, id);
    let battery = d.consents().is_enabled(id, Feature::Battery);
    let (rx, generation) = d.data.link_up(id, &peer.device_name, caps, consents);
    if battery {
        d.data.request_battery(id);
    }
    if d.consents().is_enabled(id, Feature::Notifications) {
        d.ask_notifications(id);
    }
    d.data.set_enabled(id, Feature::CalendarEdit, edit.0);
    d.data.set_enabled(id, Feature::CalendarAlerts, edit.1);
    (
        rx,
        DataLinkGuard(Some((d.clone(), id.to_owned(), generation))),
    )
}

/// Registers a trusted link for the answers to the phone's shares. Only a phone that declared
/// `geekbridge.share.result` gets any; any other peer's queue stays empty and closed.
fn answer_link(
    d: &Arc<Daemon>,
    id: &str,
    link: &kde::TrustedLink,
) -> (
    mpsc::Receiver<bridge_core::share_result::ShareResult>,
    AnswerGuard,
) {
    let speaks = gate::negotiated_outgoing(&link.info().peer)
        .iter()
        .any(|k| k == gate::SHARE_RESULT);
    if !speaks {
        return (mpsc::channel(1).1, AnswerGuard(None));
    }
    let (rx, generation) = d.share.answers_up(id);
    (
        rx,
        AnswerGuard(Some((d.clone(), id.to_owned(), generation))),
    )
}

struct AnswerGuard(Option<(Arc<Daemon>, String, u64)>);
impl Drop for AnswerGuard {
    fn drop(&mut self) {
        if let Some((d, id, generation)) = self.0.take() {
            d.share.answers_down(&id, generation);
        }
    }
}

/// A phone that declares the debug packets gets the `debug` consent switched on once. The write
/// (file lock + fsync) runs off the async workers.
async fn debug_default(d: &Arc<Daemon>, id: &str, declared: bool) {
    if declared {
        let (daemon, device) = (d.clone(), id.to_owned());
        let switched =
            tokio::task::spawn_blocking(move || daemon.consents().default_debug_on(&device))
                .await
                .unwrap_or(Ok(false));
        if matches!(switched, Ok(true)) {
            d.core().emit(
                Some(id),
                EventKind::ConsentChanged {
                    feature: Feature::Debug,
                    enabled: true,
                },
            );
        }
    }
}

/// Does the phone declare any `geekbridge.debug.*` packet?
fn debug_declared(link: &kde::TrustedLink) -> bool {
    let peer = &link.info().peer;
    gate::negotiated_outgoing(peer)
        .iter()
        .any(|k| k == gate::DEBUG_STATE_REQUEST)
        || gate::negotiated_incoming(peer)
            .iter()
            .any(|k| k == gate::DEBUG_LOG)
}

/// Registers a trusted link with the debug hub. A phone that declares the debug packets (only a debug
/// build does) gets the `debug` consent switched on once; the user can switch it off for good.
fn debug_link(
    d: &Arc<Daemon>,
    id: &str,
    link: &kde::TrustedLink,
) -> (mpsc::Receiver<crate::phone_debug::Out>, DebugLinkGuard) {
    let peer = &link.info().peer;
    let accepts = gate::negotiated_outgoing(peer)
        .iter()
        .any(|k| k == gate::DEBUG_STATE_REQUEST);
    let (rx, generation) = d.debug.link_up(id, accepts);
    (
        rx,
        DebugLinkGuard(Some((d.clone(), id.to_owned(), generation))),
    )
}

struct DebugLinkGuard(Option<(Arc<Daemon>, String, u64)>);
impl Drop for DebugLinkGuard {
    fn drop(&mut self) {
        if let Some((d, id, generation)) = self.0.take() {
            d.debug.link_down(&id, generation);
        }
    }
}

/// One `geekbridge.debug.*` packet from a negotiated, trusted (so paired) phone.
fn on_debug_packet(d: &Daemon, id: &str, packet: &kde::Packet) -> crate::phone_debug::Disposition {
    let consent = d.consents().is_enabled(id, Feature::Debug);
    match packet.kind.as_str() {
        gate::DEBUG_LOG => d.debug.on_log(id, consent, &packet.body),
        gate::DEBUG_STATE => d.debug.on_state(id, consent, &packet.body),
        _ => crate::phone_debug::Disposition::Invalid,
    }
}

/// One `kdeconnect.battery` packet from a negotiated, trusted phone. False only when it is malformed.
/// Without the `battery` consent it is dropped unread (and counts as fine: stock phones send it unasked).
fn on_battery_packet(d: &Daemon, id: &str, attempt: u64, packet: &kde::Packet) -> bool {
    if !d.consents().is_enabled(id, Feature::Battery) {
        return true;
    }
    let Ok(reading) = bridge_core::battery_link::parse(&packet.body) else {
        return false;
    };
    if let bridge_core::battery_link::Reading::Level {
        percent,
        charging,
        low,
    } = reading
    {
        d.update(id, attempt, |link, _| {
            link.status.battery = Some(bridge_core::ipc::BatteryStatus {
                percent,
                charging,
                low,
            });
        });
    }
    true
}

/// One `geekbridge.photos.list` / `.thumb` packet from a negotiated, trusted phone. False only when it is
/// malformed. Without the `photos` consent, or when this computer did not ask for it, it is dropped unread.
/// Names and image bytes are never logged.
fn on_photos_packet(d: &Arc<Daemon>, id: &str, packet: &kde::Packet) -> bool {
    use bridge_core::photos_link as wire;
    if !d.consents().is_enabled(id, Feature::Photos) {
        return true;
    }
    let now = std::time::Instant::now();
    match packet.kind.as_str() {
        wire::LIST => {
            let Ok(list) = wire::parse_list(&packet.body) else {
                return false;
            };
            let (_, need) = d.photos.on_list(id, list, now);
            for photo in need {
                if !d
                    .data
                    .send_stock(id, gate::PHOTOS_REQUEST, wire::thumb_request_body(&photo))
                {
                    break;
                }
            }
            true
        }
        wire::THUMB => match wire::parse_thumb(&packet.body) {
            Ok(t) => {
                let _ = d.photos.on_thumb(id, t, now);
                true
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// One `kdeconnect.notification` packet from a negotiated, trusted phone. False only when it is malformed.
/// Without the `notifications` consent it is dropped unread (stock phones send it unasked). Titles and
/// texts are never logged.
fn on_notification_packet(d: &Arc<Daemon>, id: &str, packet: &kde::Packet) -> bool {
    if !d.consents().is_enabled(id, Feature::Notifications) {
        return true;
    }
    let event = match bridge_core::notification_link::parse(&packet.body) {
        Ok(e) => e,
        Err(_) => return false,
    };
    let now = std::time::Instant::now();
    match event {
        bridge_core::notification_link::Event::Cancel { id: nid } => {
            d.mirror.remove(id, &nid);
        }
        bridge_core::notification_link::Event::Post(n) => {
            let name = d
                .core()
                .links
                .get(id)
                .map(|l| l.status.name.clone())
                .unwrap_or_default();
            let loud = !n.silent;
            let only_once = n.only_once;
            let banner = d.mirror.banner(id, &name, &n);
            let nid = n.id.clone();
            let outcome = d.mirror.post(id, &name, n, now);
            let alert = loud
                && !(only_once && outcome == crate::mirror::Posted::Updated)
                && d.mirror.banner_allowed(id, now);
            if alert && let Some(notifier) = d.mirror_notifier.clone() {
                let (daemon, device) = (d.clone(), id.to_owned());
                notifier.show(
                    &banner,
                    Box::new(move |choice| {
                        // Best effort: a stale or withdrawn notification is simply refused.
                        let _ = match choice {
                            crate::mirror::Choice::Dismiss => {
                                daemon.dismiss_notification(&device, &nid)
                            }
                            crate::mirror::Choice::Action(a) => {
                                daemon.notification_action(&device, &nid, &a)
                            }
                        };
                    }),
                );
            }
        }
    }
    true
}

/// (calendar editing, calendar alerts) consents of one phone.
fn c_edit(d: &Arc<Daemon>, id: &str) -> (bool, bool) {
    let c = d.consents();
    (
        c.is_enabled(id, Feature::CalendarEdit),
        c.is_enabled(id, Feature::CalendarAlerts),
    )
}

struct DataLinkGuard(Option<(Arc<Daemon>, String, u64)>);
impl Drop for DataLinkGuard {
    fn drop(&mut self) {
        if let Some((d, id, generation)) = self.0.take()
            && d.data.link_down(&id, generation)
        {
            // What was mirrored belongs to that session: gone with the link.
            d.mirror.clear(&id);
            d.photos.clear(&id);
            d.share.drop_expectations(&id);
        }
    }
}

struct CallsLinkGuard(Option<(Arc<Daemon>, String, u64)>);
impl Drop for CallsLinkGuard {
    fn drop(&mut self) {
        if let Some((d, id, generation)) = self.0.take() {
            d.calls.link_down_generation(&id, generation);
        }
    }
}

/// Run blocking hub work off the async runtime and turn the result into a reply.
async fn blocking(
    f: impl FnOnce() -> Result<(), ErrorBody> + Send + 'static,
) -> Result<Response, ErrorBody> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(())) => Ok(Response::Ack),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(err(ErrorCode::Failed, "the call request was interrupted")),
    }
}

struct ClipboardLinkGuard(Option<(Arc<Daemon>, String, u64)>);
impl Drop for ClipboardLinkGuard {
    fn drop(&mut self) {
        if let Some((d, id, generation)) = self.0.take()
            && let Some(clip) = &d.clipboard
        {
            clip.link_down_generation(&id, generation);
            d.sync_hotkey();
        }
    }
}

/// Hands a clipboard packet to the clipboard bridge. True if the packet was a clipboard packet;
/// whether it does anything depends on the device's opt-in, which the bridge checks.
fn route_clipboard(
    d: &Arc<Daemon>,
    id: &str,
    packet: &kde::Packet,
    negotiated: &[String],
    link: &kde::TrustedLink,
) -> bool {
    if gate::classify_incoming(&packet.kind, negotiated) != Incoming::Clipboard {
        return false;
    }
    let has_payload = packet.payload_size.is_some() || packet.payload_transfer_info.is_some();
    if has_payload && packet.kind != gate::CLIPBOARD_ITEM {
        return false; // a payload on any other clipboard packet is misbehaviour
    }
    let Some(clip) = d.clipboard.clone() else {
        return true;
    };
    let port = packet
        .payload_transfer_info
        .as_ref()
        .and_then(|m| m.get("port"))
        .and_then(|p| p.as_u64())
        .and_then(|p| u16::try_from(p).ok());
    if let Next::ReceiveImage(ticket) = clip.on_packet(id, &packet.kind, &packet.body, port) {
        let ctx = link.payload_context();
        tokio::spawn(async move {
            let mut bytes: Vec<u8> = Vec::new();
            let cancel = kde::CancelToken::new();
            let mut progress = |_: u64| {};
            let transfer = ctx.receive(
                port.unwrap_or(0),
                Some(ticket.size),
                ticket.max,
                &mut bytes,
                &mut progress,
                &cancel,
            );
            let outcome = timeout(Duration::from_secs(120), transfer).await;
            match outcome {
                Ok(Ok(_)) => clip.image_received(&ticket, bytes),
                _ => clip.image_failed(&ticket, "failed"),
            }
        });
    }
    true
}

async fn send_clip(link: &mut kde::TrustedLink, out: ClipOut) -> Result<(), kde::KdeError> {
    match out {
        ClipOut::Packet { kind, body } => link.send(&kde::Packet::new(kind, body)).await,
        ClipOut::Image { body, bytes } => {
            // No free payload port: skip this image, the link itself is fine.
            let Ok(listener) = link.payload_context().listen().await else {
                return Ok(());
            };
            let mut packet = kde::Packet::new(gate::CLIPBOARD_ITEM, body);
            packet.payload_size = Some(bytes.len() as i64);
            let mut info = Map::new();
            info.insert("port".into(), serde_json::Value::from(listener.port()));
            packet.payload_transfer_info = Some(info);
            link.send_with_payload(&packet).await?;
            tokio::spawn(async move {
                let size = bytes.len() as u64;
                let cancel = kde::CancelToken::new();
                let mut progress = |_: u64| {};
                let _ = listener
                    .send(std::io::Cursor::new(bytes), size, &mut progress, &cancel)
                    .await;
            });
            Ok(())
        }
    }
}

/// Returns false when the link should be dropped for misbehaviour.
fn on_packet(
    d: &Daemon,
    id: &str,
    attempt: u64,
    packet: &kde::Packet,
    negotiated: &[String],
) -> bool {
    let verdict = if packet.payload_size.is_some() || packet.payload_transfer_info.is_some() {
        Incoming::Reject
    } else {
        gate::classify_incoming(&packet.kind, negotiated)
    };
    let mut keep = true;
    d.update(id, attempt, |link, events| match verdict {
        Incoming::Ping => {
            let now = now_ms();
            link.status.pings_received += 1;
            link.status.last_ping_unix_ms = Some(now);
            // The ping body is never read or stored.
            if now.saturating_sub(link.last_ping_event_ms) >= PING_EVENT_SPACING_MS {
                link.last_ping_event_ms = now;
                events.push_back(event(id, EventKind::PingReceived));
            }
        }
        // Clipboard packets are routed by `route_clipboard` before this function runs; one that
        // reaches here carried a payload it must not have, so it is treated as misbehaviour.
        Incoming::Clipboard
        | Incoming::Calls
        | Incoming::PhoneData
        | Incoming::Debug
        | Incoming::Battery
        | Incoming::Notification
        | Incoming::Photos
        | Incoming::Reject => {
            link.status.rejected_packets += 1;
            if link.status.rejected_packets == 1 {
                events.push_back(event(id, EventKind::PacketRejected));
            }
            keep = link.status.rejected_packets <= MAX_REJECTED_PACKETS;
        }
    });
    keep
}

// ---------------------------------------------------------------------- approvals

struct ChallengeApprover {
    daemon: Arc<Daemon>,
    device_id: String,
    attempt: u64,
    connection_id: u64,
    peer_cert_sha256: String,
}

struct ChallengeGuard {
    daemon: Arc<Daemon>,
    challenge_id: String,
}

impl Drop for ChallengeGuard {
    fn drop(&mut self) {
        self.daemon.core().challenges.remove(&self.challenge_id);
    }
}

impl kde::Approver for ChallengeApprover {
    async fn approve(&mut self, request: kde::ApprovalRequest) -> bool {
        // A phone that scanned our QR code presents its one-time token: a valid one is our approval
        // (the user put the code on this screen on purpose); anything else is refused outright.
        if let Some(token) = request.presented_token.as_deref()
            && request.direction == kde::Direction::Incoming
        {
            return self.daemon.redeem_qr(&self.device_id, token);
        }
        let challenge_id = random_id();
        let (tx, rx) = oneshot::channel();
        {
            let mut core = self.daemon.core();
            if core.challenges.len() >= MAX_CHALLENGES {
                return false;
            }
            core.challenges.insert(
                challenge_id.clone(),
                PendingChallenge {
                    attempt: self.attempt,
                    view: PairChallenge {
                        challenge_id: challenge_id.clone(),
                        device_id: self.device_id.clone(),
                        device_name: request.peer_name.clone(),
                        direction: match request.direction {
                            kde::Direction::Outgoing => ChallengeDirection::Outgoing,
                            kde::Direction::Incoming => ChallengeDirection::Incoming,
                        },
                        connection_id: self.connection_id,
                        peer_cert_sha256: self.peer_cert_sha256.clone(),
                        own_cert_sha256: request.own_cert_sha256.clone(),
                        verification_key: request.verification_key.clone(),
                        expires_in_ms: request.expires_in.as_millis() as u64,
                    },
                    expires: Instant::now() + request.expires_in,
                    decision: Some(tx),
                },
            );
            core.emit(
                Some(&self.device_id),
                EventKind::PairChallenge {
                    challenge_id: challenge_id.clone(),
                },
            );
        }
        // Removes the challenge when this future finishes or is dropped by the pairing driver.
        let _guard = ChallengeGuard {
            daemon: self.daemon.clone(),
            challenge_id,
        };
        rx.await.unwrap_or(false)
    }
}
