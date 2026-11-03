// SPDX-License-Identifier: GPL-3.0-or-later
//! Authenticated service state and explicit user decisions. Networking never runs in a GUI frame.
use crate::{
    state::{DiscoveryModel, Strip, Tone, display_text},
    theme::{self, Palette},
};
use bridge_core::ipc::{self, LinkPhase, Request, Response, Status, Token};
use eframe::egui::{self, RichText};
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};
use zeroize::Zeroize;

#[path = "pair_ui.rs"]
mod pair_ui;
pub use pair_ui::{NextStep, Problem, draw_qr, qr_modules};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    pub status: Option<Status>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub busy: bool,
    /// The QR code the service issued, while the QR window is open.
    pub pair_qr: Option<ipc::PairQr>,
    /// Latest "Test connection" answer.
    pub test: Option<bridge_core::pairing_help::ConnectionTest>,
    /// The last pairing/connect failure in plain words.
    pub problem: Option<Problem>,
}
impl ControlState {
    pub fn connected(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.links.iter().any(|l| l.phase == LinkPhase::Connected))
    }
    pub fn strip(&self, discovery: &DiscoveryModel) -> Strip {
        if self.connected() {
            Strip {
                label: "Phone connected".into(),
                tone: Tone::Info,
                hollow: false,
            }
        } else if self
            .status
            .as_ref()
            .is_some_and(|s| !s.challenges.is_empty())
        {
            Strip {
                label: "Compare the pairing code".into(),
                tone: Tone::Warning,
                hollow: false,
            }
        } else if self.status.as_ref().is_some_and(|s| {
            s.links
                .iter()
                .any(|l| matches!(l.phase, LinkPhase::Connecting | LinkPhase::Pairing))
        }) {
            Strip {
                label: "Connecting to phone…".into(),
                tone: Tone::Neutral,
                hollow: true,
            }
        } else {
            discovery.strip()
        }
    }
    pub fn blocked_line(&self, discovery: &DiscoveryModel, verb: &str) -> Option<String> {
        if self.connected() {
            Some(format!(
                "Can't {verb} yet: this feature isn't connected to the phone link."
            ))
        } else {
            discovery.blocked_line(verb)
        }
    }
}

#[derive(Clone)]
pub(crate) struct Config {
    state_dir: Option<std::path::PathBuf>,
    port: u16,
}
impl Config {
    pub(crate) fn from_environment() -> Self {
        Self {
            state_dir: ipc::default_state_dir(),
            port: ipc::control_port_from_env(),
        }
    }
    fn token(&self) -> Result<Token, String> {
        let dir = self
            .state_dir
            .as_ref()
            .ok_or("This computer's private app folder couldn't be found.")?;
        let mut bytes = bridge_kde::fsutil::read_private_file(&ipc::token_path(dir))
            .map_err(|_| "The phone-link settings couldn't be read securely.".to_owned())?
            .ok_or_else(|| {
                crate::i18n::tr_args(
                    "connection.no_token",
                    &[("hint", &crate::i18n::tr("doctor.short"))],
                )
            })?;
        let token =
            Token::parse(&bytes).map_err(|_| "The phone-link settings are unreadable.".to_owned());
        bytes.zeroize();
        token
    }
    fn fetch(&self) -> Result<Status, String> {
        let token = self.token()?;
        ipc::status(self.port, &token).map_err(service_error)
    }
    pub(crate) fn request(&self, request: &Request) -> Result<Response, String> {
        let token = self.token()?;
        ipc::call(self.port, &token, request).map_err(service_error)
    }
}
fn service_error(error: ipc::IpcError) -> String {
    match error {
        ipc::IpcError::Io(_) => crate::i18n::tr_args(
            "connection.unreachable",
            &[("hint", &crate::i18n::tr("doctor.short"))],
        ),
        ipc::IpcError::Unavailable { .. } => "The service can't protect its private settings. Phone linking is unavailable.".into(),
        ipc::IpcError::Unauthorized | ipc::IpcError::Protocol(_) => "The service couldn't be authenticated. Check that both apps use the same phone-link settings.".into(),
        ipc::IpcError::Service(body) => display_text(&body.message),
    }
}

#[derive(Clone, Debug)]
enum Command {
    Refresh,
    Request(Request),
}
struct Worker {
    commands: mpsc::SyncSender<Command>,
    latest: Arc<Mutex<Option<ControlState>>>,
    stop: Arc<AtomicBool>,
}
impl Worker {
    fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let config = Config::from_environment();
        Self::spawn_with(
            move || config.fetch(),
            {
                let config = Config::from_environment();
                move |request| config.request(request)
            },
            wake,
        )
    }
    fn spawn_with(
        mut fetch: impl FnMut() -> Result<Status, String> + Send + 'static,
        mut request: impl FnMut(&Request) -> Result<Response, String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let (commands, rx) = mpsc::sync_channel(8);
        let latest = Arc::new(Mutex::new(None));
        let output = latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = stop.clone();
        std::thread::Builder::new()
            .name("geekbridge-link-control".into())
            .spawn(move || {
                let mut state = ControlState::default();
                let mut previous = None;
                // Events older than the first status are history, not news.
                let mut seen_seq: Option<u64> = None;
                let publish = |state: &ControlState, previous: &mut Option<ControlState>| {
                    if previous.as_ref() != Some(state) {
                        *previous = Some(state.clone());
                        *output.lock().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
                        wake();
                    }
                };
                while !quitting.load(Ordering::Acquire) {
                    match fetch() {
                        Ok(status) => {
                            state.status = Some(status);
                            state.error = None;
                        }
                        Err(error) => {
                            state.status = None;
                            state.error = Some(error);
                        }
                    }
                    state.busy = false;
                    if let Some(status) = &state.status {
                        let after = *seen_seq.get_or_insert(status.event_seq);
                        if status.event_seq > after {
                            if let Ok(Response::Events(page)) = request(&Request::Events { after })
                            {
                                absorb_events(&mut state, &page.events);
                                seen_seq = Some(page.next_after.max(after));
                            } else {
                                seen_seq = Some(status.event_seq);
                            }
                        }
                    }
                    publish(&state, &mut previous);
                    match rx.recv_timeout(crate::pace::scaled(Duration::from_secs(1))) {
                        Ok(Command::Request(command)) if !quitting.load(Ordering::Acquire) => {
                            state.busy = true;
                            publish(&state, &mut previous);
                            match &command {
                                Request::TestConnection { .. } => state.test = None,
                                Request::PairQr | Request::Connect { .. } => state.problem = None,
                                _ => {}
                            }
                            let result = request(&command);
                            apply_result(&mut state, &command, result);
                        }
                        Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("spawn authenticated service worker");
        Self {
            commands,
            latest,
            stop,
        }
    }
    fn send(&self, command: Command) -> bool {
        self.commands.try_send(command).is_ok()
    }
    fn latest(&self) -> Option<ControlState> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
/// Fold the answer to an explicit request into what the UI shows.
fn apply_result(state: &mut ControlState, command: &Request, result: Result<Response, String>) {
    match (command, result) {
        (Request::PairQr, Ok(Response::PairQr(qr))) => {
            state.pair_qr = Some(*qr);
            state.notice = None;
        }
        (Request::PairQrCancel, Ok(_)) => {
            state.pair_qr = None;
            state.notice = None;
        }
        (Request::TestConnection { .. }, Ok(Response::ConnectionTest(t))) => {
            state.test = Some(*t);
            state.notice = None;
        }
        (_, Ok(_)) => state.notice = Some(request_notice(command).into()),
        (_, Err(error)) => {
            state.notice = Some(error);
        }
    }
}

/// Turn service events into a plain-language problem line and clear it when pairing succeeds.
fn absorb_events(state: &mut ControlState, events: &[ipc::Event]) {
    for event in events {
        match &event.kind {
            ipc::EventKind::PairFailed { reason }
            | ipc::EventKind::QrRefused { reason }
            | ipc::EventKind::ConnectFailed { reason } => {
                state.problem = Some(Problem::from_reason(reason));
            }
            ipc::EventKind::Paired => state.problem = None,
            _ => {}
        }
    }
}

fn request_notice(request: &Request) -> &'static str {
    match request {
        Request::Connect { .. } => "Connection requested. Waiting for the phone.",
        Request::Pair { .. } => "Pairing requested. Compare the code on both devices.",
        Request::Approve { .. } => "Approval sent. Waiting for pairing to finish.",
        Request::Deny { .. } => "Pairing declined.",
        Request::Disconnect { .. } => "Disconnect requested.",
        Request::Unpair { .. } => "Forget-phone request accepted.",
        Request::SetDeviceName { .. } => "Name saved. Phones show it from their next connection.",
        Request::Ping { .. } => "Connection check sent.",
        _ => "Request accepted.",
    }
}

pub fn manual_endpoint(id: &str, address: &str) -> Result<Request, &'static str> {
    bridge_kde::DeviceId::parse(id.trim())
        .map_err(|_| "Enter the device ID shown on the phone.")?;
    let endpoint: std::net::SocketAddr = address
        .trim()
        .parse()
        .map_err(|_| "Enter an IP address and port, such as 192.168.1.20:1716.")?;
    if endpoint.port() == 0 || endpoint.ip().is_unspecified() || endpoint.ip().is_multicast() {
        return Err("Choose a specific phone address and a nonzero port.");
    }
    Ok(Request::Connect {
        device_id: id.trim().into(),
        endpoint: Some(ipc::Endpoint {
            address: endpoint.ip(),
            port: endpoint.port(),
        }),
    })
}
/// Why a typed computer name cannot be saved, in words for the person typing it.
fn name_problem(draft: &str) -> Option<&'static str> {
    use bridge_kde::identity::validate_device_name;
    match validate_device_name(draft) {
        Ok(_) => None,
        Err(_) if draft.chars().any(char::is_control) => Some("Use letters, numbers and spaces."),
        Err(_) if draft.trim().is_empty() => Some("Enter a name."),
        Err(_) => Some("Use letters, numbers and spaces, up to 32 characters."),
    }
}
fn code_matches(entered: &str, expected: &str) -> bool {
    expected.len() == 8
        && expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        && entered.trim().len() == 8
        && entered.trim().eq_ignore_ascii_case(expected)
}

pub struct Connections {
    worker: Worker,
    pub state: ControlState,
    manual_id: String,
    manual_address: String,
    active_challenge: Option<String>,
    entered_code: String,
    forget: Option<(String, String)>,
    pair: pair_ui::PairUi,
    /// What the user is typing in "This computer's name"; `None` while it shows the saved name.
    name_draft: Option<String>,
}
impl Connections {
    pub fn new(ctx: egui::Context) -> Self {
        Self {
            worker: Worker::spawn(move || ctx.request_repaint()),
            state: ControlState::default(),
            manual_id: String::new(),
            manual_address: String::new(),
            active_challenge: None,
            entered_code: String::new(),
            forget: None,
            pair: Default::default(),
            name_draft: None,
        }
    }
    pub fn update(&mut self) {
        if let Some(state) = self.worker.latest() {
            self.state = state;
        }
    }
    pub fn refresh(&self) {
        self.worker.send(Command::Refresh);
    }
    fn submit(&mut self, request: Request) {
        if self.worker.send(Command::Request(request)) {
            self.state.busy = true;
        } else {
            self.state.notice = Some("Another request is in progress. Try again shortly.".into());
        }
    }
    /// Settings row: rename this computer. Phones show the name from their next connection.
    pub fn name_row(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let Some(saved) = self.state.status.as_ref().map(|s| s.device_name.clone()) else {
            ui.label(theme::caption(pal, "Waiting for the background service…"));
            return;
        };
        let busy = self.state.busy;
        let draft = self.name_draft.get_or_insert_with(|| saved.clone());
        ui.label(theme::overline(pal, "This computer's name"));
        let mut save = false;
        ui.horizontal_wrapped(|ui| {
            crate::a11y::named(
                ui.add(
                    egui::TextEdit::singleline(draft)
                        .char_limit(bridge_kde::identity::MAX_USER_NAME_CHARS)
                        .desired_width(220.0),
                ),
                "Your computer name",
            );
            let problem = name_problem(draft);
            let changed = draft.trim() != saved;
            save = theme::pill_button(
                ui,
                pal,
                "Save name",
                theme::ButtonKind::Plain,
                changed && problem.is_none() && !busy,
            )
            .clicked();
            if let Some(problem) = problem.filter(|_| changed) {
                ui.label(theme::caption(pal, problem));
            }
        });
        ui.add(
            egui::Label::new(theme::caption(
                pal,
                "Shown on your phone. A phone that is already connected picks up the new name the next time it connects.",
            ))
            .wrap(),
        );
        if save {
            let name = draft.clone();
            self.name_draft = None;
            self.submit(Request::SetDeviceName { name });
        }
    }
    pub fn details(&mut self, ui: &mut egui::Ui, pal: &Palette, discovery: &DiscoveryModel) {
        ui.label(theme::overline(pal, "Phone linking"));
        if let Some(error) = &self.state.error {
            ui.add(egui::Label::new(theme::secondary(pal, error)).wrap());
            if ui.button("Check phone linking").clicked() {
                self.refresh();
            }
            return;
        }
        let Some(status) = self.state.status.clone() else {
            ui.label(theme::caption(pal, "Checking phone-link settings…"));
            return;
        };
        ui.label(theme::caption(
            pal,
            "Pairing by name (not by QR code): pair only after the same code shows on this computer and your phone.",
        ));
        if let Some(notice) = &self.state.notice {
            ui.add(egui::Label::new(theme::secondary(pal, notice)).wrap());
        }
        let enabled = !self.state.busy;
        let mut seen = BTreeSet::new();
        for link in &status.links {
            seen.insert(link.device_id.clone());
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(display_text(&link.name), 13.0));
                ui.label(theme::caption(
                    pal,
                    match link.phase {
                        LinkPhase::Connecting => "Connecting…",
                        LinkPhase::Unpaired => "Not paired",
                        LinkPhase::Pairing => "Pairing…",
                        LinkPhase::Connected => "Connected securely",
                    },
                ));
                if link.phase == LinkPhase::Unpaired
                    && ui.add_enabled(enabled, egui::Button::new("Pair")).clicked()
                {
                    self.submit(Request::Pair {
                        device_id: link.device_id.clone(),
                        mode: ipc::PairMode::Request,
                    });
                }
                if link.phase == LinkPhase::Connected
                    && ui
                        .add_enabled(enabled, egui::Button::new("Check connection"))
                        .clicked()
                {
                    self.submit(Request::Ping {
                        device_id: link.device_id.clone(),
                    });
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Disconnect"))
                    .clicked()
                {
                    self.submit(Request::Disconnect {
                        device_id: link.device_id.clone(),
                    });
                }
            });
        }
        for phone in &status.paired {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(display_text(&phone.name), 13.0));
                if !seen.contains(&phone.device_id) {
                    ui.label(theme::caption(pal, "Paired · offline"));
                    if ui
                        .add_enabled(enabled, egui::Button::new("Connect"))
                        .clicked()
                    {
                        self.submit(Request::Connect {
                            device_id: phone.device_id.clone(),
                            endpoint: None,
                        });
                    }
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Forget…"))
                    .clicked()
                {
                    self.forget = Some((phone.device_id.clone(), display_text(&phone.name)));
                }
            });
            seen.insert(phone.device_id.clone());
        }
        for phone in discovery
            .devices()
            .iter()
            .filter(|phone| !seen.contains(&phone.id))
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(&phone.name, 13.0));
                ui.label(theme::caption(pal, "Nearby"));
                if ui.add_enabled(enabled, egui::Button::new("Pair")).clicked() {
                    self.one_tap_pair(&phone.id);
                }
            });
        }
        egui::CollapsingHeader::new("Connect by address").show(ui, |ui| {
            ui.label(theme::caption(
                pal,
                "Use the address and device ID shown on the phone.",
            ));
            crate::a11y::named(
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_address)
                        .hint_text(crate::theme::hint(ui, "Phone IP:port"))
                        .char_limit(80),
                ),
                "Phone IP address and port",
            );
            crate::a11y::named(
                ui.add(
                    egui::TextEdit::singleline(&mut self.manual_id)
                        .hint_text(crate::theme::hint(ui, "Device ID"))
                        .char_limit(38),
                ),
                "Phone device ID",
            );
            if ui
                .add_enabled(enabled, egui::Button::new("Connect to this phone"))
                .clicked()
            {
                match manual_endpoint(&self.manual_id, &self.manual_address) {
                    Ok(request) => self.submit(request),
                    Err(message) => self.state.notice = Some(message.into()),
                }
            }
        });
    }
    pub fn dialogs(&mut self, ctx: &egui::Context, pal: &Palette) {
        self.pair_tick(ctx);
        self.qr_window(ctx, pal);
        let challenge = self
            .state
            .status
            .as_ref()
            .and_then(|s| s.challenges.first())
            .cloned();
        let next = challenge.as_ref().map(|c| c.challenge_id.clone());
        if self.active_challenge != next {
            self.active_challenge = next;
            self.entered_code.clear();
        }
        if let Some(challenge) = challenge {
            egui::Window::new("Pair phone")
                .id(egui::Id::new("pair-phone"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(theme::semibold(display_text(&challenge.device_name), 17.0));
                    ui.label("Your phone and this computer should show the same code:");
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(&challenge.verification_key)
                                .monospace()
                                .strong()
                                .size(46.0),
                        );
                    });
                    ui.add(
                        egui::Label::new(theme::caption(
                            pal,
                            "Type the code your phone shows to confirm. If they differ, choose Decline: someone else may be on your network.",
                        ))
                        .wrap(),
                    );
                    crate::a11y::named(ui.add(
                        egui::TextEdit::singleline(&mut self.entered_code)
                            .hint_text(crate::theme::hint(ui, "Code from phone"))
                            .char_limit(8),
                    ), "Pairing code from the phone");
                    ui.horizontal(|ui| {
                        let enabled = !self.state.busy && challenge.expires_in_ms > 0;
                        if ui
                            .add_enabled(enabled, egui::Button::new("Decline"))
                            .clicked()
                        {
                            self.submit(Request::Deny {
                                challenge_id: challenge.challenge_id.clone(),
                            });
                        }
                        if ui
                            .add_enabled(
                                enabled
                                    && code_matches(
                                        &self.entered_code,
                                        &challenge.verification_key,
                                    ),
                                egui::Button::new("Approve pairing"),
                            )
                            .clicked()
                        {
                            self.submit(Request::Approve {
                                challenge_id: challenge.challenge_id.clone(),
                            });
                        }
                    });
                    if let Some(notice) = &self.state.notice {
                        ui.add(egui::Label::new(theme::caption(pal, notice)).wrap());
                    }
                });
        }
        if let Some((id, name)) = self.forget.clone() {
            // Escape is the safe answer: keep the pairing.
            if theme::escape_pressed(ctx) {
                self.forget = None;
            }
            egui::Window::new("Forget phone?")
                .id(egui::Id::new("forget-phone"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "You'll need to pair with {name} again to reconnect."
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.forget = None;
                        }
                        if ui
                            .add_enabled(!self.state.busy, egui::Button::new("Forget phone"))
                            .clicked()
                        {
                            self.submit(Request::Unpair { device_id: id });
                            self.forget = None;
                        }
                    });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_computer_names_are_checked_before_they_are_sent() {
        assert_eq!(name_problem("Study PC"), None);
        assert!(name_problem("").is_some());
        assert!(name_problem("   ").is_some());
        assert!(name_problem("a\u{1b}b").is_some());
        assert!(name_problem(&"x".repeat(33)).is_some());
    }

    fn status() -> Status {
        Status {
            ipc_version: 1,
            service_version: "test".into(),
            device_id: "x".repeat(32),
            device_name: "test".into(),
            own_cert_sha256: "00".repeat(32),
            paired: vec![],
            links: vec![],
            challenges: vec![],
            event_seq: 0,
            inbound_listener: None,
            reconnecting: vec![],
            disconnected: vec![],
        }
    }
    #[test]
    fn approval_needs_the_complete_matching_code() {
        assert!(code_matches("abcd1234", "ABCD1234"));
        assert!(!code_matches("", "ABCD1234"));
        assert!(!code_matches("ABCD123", "ABCD1234"));
        assert!(!code_matches("ABCD1235", "ABCD1234"));
        assert!(!code_matches("ZZZZZZZZ", "ZZZZZZZZ"));
    }
    #[test]
    fn manual_connection_requires_a_valid_id_and_specific_endpoint() {
        let id = "a".repeat(32);
        assert!(manual_endpoint(&id, "192.168.1.10:1716").is_ok());
        assert!(manual_endpoint("../bad", "192.168.1.10:1716").is_err());
        assert!(manual_endpoint(&id, "0.0.0.0:1716").is_err());
        assert!(manual_endpoint(&id, "224.0.0.1:1716").is_err());
        assert!(manual_endpoint(&id, "192.168.1.10:0").is_err());
    }
    #[test]
    fn worker_runs_explicit_requests_off_the_calling_thread() {
        let caller = std::thread::current().id();
        let (sent, received) = mpsc::channel();
        let worker = Worker::spawn_with(
            || Ok(status()),
            move |_| {
                sent.send(std::thread::current().id()).unwrap();
                Ok(Response::Ack)
            },
            || {},
        );
        assert!(worker.send(Command::Request(Request::Ping {
            device_id: "x".repeat(32)
        })));
        assert_ne!(
            received.recv_timeout(Duration::from_secs(2)).unwrap(),
            caller
        );
    }
    #[test]
    fn authenticated_transport_never_enables_missing_feature_adapters() {
        let mut snapshot = status();
        snapshot.links.push(ipc::LinkStatus {
            device_id: "x".repeat(32),
            name: "Test peer".into(),
            phase: LinkPhase::Connected,
            role: None,
            connection_id: Some(1),
            peer_ip: None,
            peer_cert_sha256: Some("00".repeat(32)),
            negotiated: vec!["kdeconnect.ping".into()],
            since_unix_ms: 0,
            pings_received: 0,
            last_ping_unix_ms: None,
            rejected_packets: 0,
            battery: None,
        });
        let state = ControlState {
            status: Some(snapshot),
            ..Default::default()
        };
        let discovery = DiscoveryModel::from_snapshot(
            bridge_core::AppKind::Messages,
            &bridge_core::Snapshot::empty(1716),
        );
        assert!(state.connected());
        assert_eq!(state.strip(&discovery).label, "Phone connected");
        assert!(
            state
                .blocked_line(&discovery, "send")
                .unwrap()
                .contains("feature")
        );
    }

    #[test]
    fn unavailable_service_never_claims_a_connected_phone() {
        let state = ControlState {
            error: Some("not running".into()),
            ..Default::default()
        };
        assert!(!state.connected());
        assert!(
            state
                .blocked_line(&DiscoveryModel::Checking, "send")
                .unwrap()
                .contains("Can't send")
        );
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod render_tests;
