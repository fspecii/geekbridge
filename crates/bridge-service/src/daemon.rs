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
/// Unpaired links one source address may hold at once (S2-1 in docs/security-review-2.md).
pub const MAX_UNPAIRED_PER_IP: usize = 3;
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
        }
    }
}

enum LinkCmd {
    Pair(PairMode),
    Ping,
    Unpair(oneshot::Sender<Result<(), String>>),
    Share(ShareJob),
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
    }
}

enum Entry {
    Dial(SocketAddr),
    Ready(Box<kde::Handshake>),
}

pub struct Daemon {
    endpoint: kde::Endpoint,
    store: Arc<dyn kde::TrustStore>,
    consents: Mutex<ConsentStore>,
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
}

fn err(code: ErrorCode, message: impl Into<String>) -> ErrorBody {
    ErrorBody {
        code,
        message: message.into(),
    }
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
        let data = crate::phone_data::PhoneData::new();
        data.set_notify_content(options.sms_show_content);
        if let Some(n) = options.sms_notifier.take() {
            data.set_notifier(n);
        }
        let daemon = Arc::new(Self {
            endpoint,
            store,
            consents: Mutex::new(consents),
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

    fn consents(&self) -> MutexGuard<'_, ConsentStore> {
        self.consents.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The phone-data hub (calendar alert ticker).
    pub fn phone_data(&self) -> &Arc<crate::phone_data::PhoneData> {
        &self.data
    }

    pub fn set_inbound_listener(&self, addr: SocketAddr) {
        *self.inbound.lock().unwrap_or_else(|e| e.into_inner()) = Some(addr);
    }

    // ------------------------------------------------------------------ queries

    pub fn status(&self) -> Result<Status, ErrorBody> {
        let peers = self
            .store
            .list()
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        let consents = self.consents();
        let paired = peers
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
        drop(consents);
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
        let creds = self.endpoint.credentials();
        Ok(Status {
            ipc_version: bridge_core::ipc::IPC_VERSION,
            service_version: env!("CARGO_PKG_VERSION").to_owned(),
            device_id: creds.device_id().to_string(),
            device_name: self.endpoint.local_identity().device_name.clone(),
            own_cert_sha256: creds.cert().fingerprint_hex(),
            paired,
            links,
            challenges,
            event_seq: core.next_seq,
            inbound_listener: *self.inbound.lock().unwrap_or_else(|e| e.into_inner()),
        })
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
            Request::Disconnect { device_id } => {
                self.disconnect(&device_id).await.map(|()| Response::Ack)
            }
            Request::Unpair { device_id } => self.unpair(&device_id).await.map(|()| Response::Ack),
            Request::SetConsent {
                device_id,
                feature,
                enabled,
            } => self
                .set_consent(&device_id, feature, enabled)
                .map(|()| Response::Ack),
            Request::Ping { device_id } => self.ping(&device_id).map(|()| Response::Ack),
            Request::CallingStatus => self.calling_status().await.map(Response::CallingStatus),
            Request::Calls => Ok(Response::Calls(Box::new(self.calls.status()))),
            Request::CallAction { target, action } => {
                let hub = self.calls.clone();
                blocking(move || hub.act(&target, action)).await
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
            } => self
                .share_settings(&device_id, receive, auto_accept)
                .map(|()| Response::Ack),
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
            } => self
                .data
                .messages_view(device_id.as_deref(), thread_id, query.as_deref())
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

    async fn disconnect(&self, device_id: &str) -> Result<(), ErrorBody> {
        parse_id(device_id)?;
        if !self.take_down(device_id).await {
            return Err(err(ErrorCode::NotFound, "no live link for this device"));
        }
        self.core().emit(Some(device_id), EventKind::Disconnected);
        Ok(())
    }

    async fn unpair(&self, device_id: &str) -> Result<(), ErrorBody> {
        let id = parse_id(device_id)?;
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
                        self.forget_consent(device_id)?;
                        return Ok(());
                    }
                    Ok(Ok(Err(e))) => return Err(err(ErrorCode::Failed, e)),
                    _ => {}
                }
            }
        }
        // No trusted session (or it vanished): stop whatever is attached, then forget locally.
        self.take_down(device_id).await;
        let removed = self
            .store
            .remove(&id)
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))?;
        self.forget_consent(device_id)?;
        if !removed {
            return Err(err(ErrorCode::NotFound, "device is not paired"));
        }
        self.core().emit(Some(device_id), EventKind::Unpaired);
        Ok(())
    }

    fn forget_consent(&self, device_id: &str) -> Result<(), ErrorBody> {
        self.share.forget_device(device_id);
        self.data.forget(device_id);
        if let Some(clip) = &self.clipboard {
            clip.forget(device_id);
            self.sync_hotkey();
        }
        self.consents()
            .forget(device_id)
            .map_err(|e| err(ErrorCode::Failed, reason(&e)))
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
        self.core().emit(
            Some(device_id),
            EventKind::ConsentChanged { feature, enabled },
        );
        Ok(())
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
        // Strangers must not starve the table (docs/security-review-2.md S2-1): one address holds
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
            core.links.remove(id);
            core.challenges.retain(|_, c| c.attempt != attempt);
            core.emit(Some(id), kind);
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
    match handshake {
        kde::Handshake::Trusted(link) => trusted_loop(&d, &id, attempt, link, &mut rx).await,
        kde::Handshake::Pending(pending) => {
            let mode = if inbound {
                // Waiting for the phone to ask sends nothing; the user still has to approve.
                d.update(&id, attempt, |l, _| l.status.phase = LinkPhase::Pairing);
                kde::PairMode::AwaitPeerRequest
            } else {
                let deadline = Instant::now() + d.options.unpaired_idle;
                loop {
                    match timeout_at(deadline, rx.recv()).await {
                        Ok(Some(LinkCmd::Pair(PairMode::Request))) => break kde::PairMode::Request,
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
                    if ours {
                        trusted_loop(&d, &id, attempt, link, &mut rx).await;
                    }
                }
                Err(e) => d.finish(&id, attempt, EventKind::PairFailed { reason: reason(&e) }),
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
) {
    let negotiated_in = gate::negotiated_incoming(&link.info().peer);
    let share_name = link.info().peer.device_name.clone();
    let (mut clip_rx, _clip_guard) = clipboard_link(d, id, &link);
    let (mut call_rx, _call_guard) = calls_link(d, id, &link);
    let (mut data_rx, _data_guard) = data_link(d, id, &link);
    let loss = loop {
        tokio::select! {
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
                    if !on_packet(d, id, attempt, &packet, &negotiated_in) {
                        break "too many packets outside the negotiated set".to_owned();
                    }
                }
                Ok(kde::LinkEvent::PeerUnpaired) => {
                    // bridge-kde already deleted the trust record.
                    let _ = d.forget_consent(id);
                    return d.finish(id, attempt, EventKind::PeerUnpaired);
                }
                Ok(kde::LinkEvent::PeerRequestedRePair { .. }) => d.event(id, EventKind::PeerRequestedRePair),
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
                    bridge_core::calls_link::request_body(&request.request_id, request.action),
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
            command = rx.recv() => match command {
                Some(LinkCmd::Ping) => {
                    let ping = kde::Packet::new(gate::PING, Map::new());
                    if let Err(e) = link.send(&ping).await {
                        break reason(&e);
                    }
                }
                Some(LinkCmd::Unpair(ack)) => {
                    let result = link.unpair().await.map_err(|e| reason(&e));
                    let ok = result.is_ok();
                    let _ = ack.send(result);
                    if ok {
                        d.finish(id, attempt, EventKind::Unpaired);
                    }
                    return;
                }
                Some(LinkCmd::Share(job)) => {
                    if !d.share.run_job(&mut link, job).await {
                        break "the link failed while announcing a share".to_owned();
                    }
                }
                Some(LinkCmd::Pair(_)) => {}
                None => return,
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
    drop(c);
    let edit = c_edit(d, id);
    let (rx, generation) = d.data.link_up(id, &peer.device_name, caps, consents);
    d.data.set_enabled(id, Feature::CalendarEdit, edit.0);
    d.data.set_enabled(id, Feature::CalendarAlerts, edit.1);
    (
        rx,
        DataLinkGuard(Some((d.clone(), id.to_owned(), generation))),
    )
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
        if let Some((d, id, generation)) = self.0.take() {
            d.data.link_down(&id, generation);
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
        Incoming::Clipboard | Incoming::Calls | Incoming::PhoneData | Incoming::Reject => {
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
