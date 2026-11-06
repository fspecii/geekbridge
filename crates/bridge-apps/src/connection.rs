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

/// Phones already linked under the same visible name as `name` but a different device id: what a reinstalled
/// phone app looks like (a new id and certificate). Only ever a hint; nothing is merged or trusted from it.
fn same_name_entries<'a>(
    paired: &'a [ipc::PairedDevice],
    device_id: &str,
    name: &str,
) -> Vec<&'a ipc::PairedDevice> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return Vec::new();
    }
    paired
        .iter()
        .filter(|p| p.device_id != device_id && p.name.trim().to_lowercase() == key)
        .collect()
}

/// True when `phone` is an older entry with a newer one under the same name, and is not online itself.
fn looks_replaced(paired: &[ipc::PairedDevice], phone: &ipc::PairedDevice, online: bool) -> bool {
    !online
        && same_name_entries(paired, &phone.device_id, &phone.name)
            .iter()
            .any(|o| o.paired_at_unix > phone.paired_at_unix)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ControlState {
    pub status: Option<Status>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub busy: bool,
    /// The QR code the service issued, while the QR window is open.
    pub pair_qr: Option<ipc::PairQr>,
    /// A phone presented the code and the service accepted it: the phone is asking its owner to confirm.
    pub qr_scanned: bool,
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
                label: "Compare the code".into(),
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
    cached: Arc<Mutex<Option<Token>>>,
}
impl Config {
    pub(crate) fn from_environment() -> Self {
        Self {
            state_dir: ipc::default_state_dir(),
            port: ipc::control_port_from_env(),
            cached: Arc::default(),
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
    /// Run `op` with the cached token; on an authentication failure (the service may have
    /// regenerated it) re-read the file once and retry.
    fn with_token<T>(&self, op: impl Fn(&Token) -> Result<T, ipc::IpcError>) -> Result<T, String> {
        let cached = self.cached.lock().ok().and_then(|g| g.clone());
        if let Some(token) = cached {
            match op(&token) {
                Err(ipc::IpcError::Unauthorized | ipc::IpcError::Protocol(_)) => {}
                other => return other.map_err(service_error),
            }
        }
        let token = self.token()?;
        if let Ok(mut g) = self.cached.lock() {
            *g = Some(token.clone());
        }
        op(&token).map_err(service_error)
    }
    fn fetch(&self) -> Result<Status, String> {
        self.with_token(|t| ipc::status(self.port, t))
    }
    pub(crate) fn request(&self, request: &Request) -> Result<Response, String> {
        self.with_token(|t| ipc::call(self.port, t, request))
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
            state.qr_scanned = false;
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
                state.qr_scanned = false;
                state.problem = Some(Problem::from_reason(reason));
            }
            ipc::EventKind::QrAccepted => {
                state.qr_scanned = true;
                state.problem = None;
            }
            ipc::EventKind::Paired => {
                state.qr_scanned = false;
                state.problem = None;
            }
            _ => {}
        }
    }
}

fn request_notice(request: &Request) -> &'static str {
    match request {
        Request::Connect { .. } => "Connection requested. Waiting for the phone.",
        Request::Pair { .. } => "Link requested. Compare the code on both devices.",
        Request::Approve { .. } => "Approval sent. Waiting for linking to finish.",
        Request::Deny { .. } => "Link declined.",
        Request::Disconnect { .. } => "Disconnect requested.",
        Request::Unpair { .. } => "Phone unlinked.",
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
/// Gives a row's button the phone's name for a screen reader ("Unlink Pixel"): several rows share the
/// same visible words, and two linked phones can even share a name.
fn named_for(response: egui::Response, key: &str, phone: &str) -> egui::Response {
    let name = crate::i18n::tr_args(key, &[("name", &display_text(phone))]);
    crate::a11y::named(response, &name)
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
            if theme::pill_button(
                ui,
                pal,
                "Check phone linking",
                theme::ButtonKind::Plain,
                true,
            )
            .clicked()
            {
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
            "Linking by name (not by QR code): link only after the same code shows on this computer and your phone.",
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
                        LinkPhase::Unpaired => "Not linked",
                        LinkPhase::Pairing => "Linking…",
                        LinkPhase::Connected => "Connected securely",
                    },
                ));
                if link.phase == LinkPhase::Unpaired
                    && named_for(
                        ui.add_enabled(enabled, egui::Button::new("Link")),
                        "phones.link_named",
                        &link.name,
                    )
                    .clicked()
                {
                    self.submit(Request::Pair {
                        device_id: link.device_id.clone(),
                        mode: ipc::PairMode::Request,
                    });
                }
                if link.phase == LinkPhase::Connected
                    && named_for(
                        ui.add_enabled(enabled, egui::Button::new("Check connection")),
                        "phones.check_named",
                        &link.name,
                    )
                    .clicked()
                {
                    self.submit(Request::Ping {
                        device_id: link.device_id.clone(),
                    });
                }
                if named_for(
                    ui.add_enabled(enabled, egui::Button::new("Disconnect")),
                    "phones.disconnect_named",
                    &link.name,
                )
                .clicked()
                {
                    self.submit(Request::Disconnect {
                        device_id: link.device_id.clone(),
                    });
                }
            });
        }
        let online: BTreeSet<String> = seen.clone();
        for phone in &status.paired {
            let replaced = looks_replaced(&status.paired, phone, online.contains(&phone.device_id));
            ui.horizontal_wrapped(|ui| {
                ui.label(theme::semibold(display_text(&phone.name), 13.0));
                if !seen.contains(&phone.device_id) {
                    ui.label(theme::caption(pal, "Linked · offline"));
                    if named_for(
                        ui.add_enabled(enabled, egui::Button::new("Connect")),
                        "phones.connect_named",
                        &phone.name,
                    )
                    .clicked()
                    {
                        self.submit(Request::Connect {
                            device_id: phone.device_id.clone(),
                            endpoint: None,
                        });
                    }
                }
                if named_for(
                    ui.add_enabled(enabled, egui::Button::new("Unlink…")),
                    if replaced {
                        "phones.unlink_old_named"
                    } else {
                        "phones.unlink_named"
                    },
                    &phone.name,
                )
                .clicked()
                {
                    self.forget = Some((phone.device_id.clone(), display_text(&phone.name)));
                }
            });
            if replaced {
                ui.add(
                    egui::Label::new(theme::caption(
                        pal,
                        crate::i18n::tr("pair.reinstall.old_entry"),
                    ))
                    .wrap(),
                );
            }
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
                if named_for(
                    ui.add_enabled(enabled, egui::Button::new("Link")),
                    "phones.link_named",
                    &phone.name,
                )
                .clicked()
                {
                    self.one_tap_pair(&phone.id);
                }
            });
        }
        let by_address = egui::CollapsingHeader::new("Connect by address").show(ui, |ui| {
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
        theme::ring_when_focused(ui, pal, &by_address.header_response);
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
            let shown = egui::Window::new(crate::i18n::tr("pair.dialog.link_title"))
                .id(egui::Id::new("pair-phone"))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    // The title bar is a drag handle that Tab lands on without a name or a ring;
                    // the title is plain text instead and names the window.
                    ui.label(theme::semibold(crate::i18n::tr("pair.dialog.link_title"), 17.0));
                    ui.label(theme::semibold(display_text(&challenge.device_name), 15.0));
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
                    if let Some(status) = &self.state.status
                        && !same_name_entries(
                            &status.paired,
                            &challenge.device_id,
                            &challenge.device_name,
                        )
                        .is_empty()
                    {
                        ui.add(
                            egui::Label::new(theme::caption(
                                pal,
                                crate::i18n::tr("pair.reinstall.prompt"),
                            ))
                            .wrap(),
                        );
                    }
                    crate::a11y::named(ui.add(
                        egui::TextEdit::singleline(&mut self.entered_code)
                            .hint_text(crate::theme::hint(ui, "Code from phone"))
                            .char_limit(8),
                    ), "Code from the phone");
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
                                egui::Button::new("Approve link"),
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
            if let Some(shown) = shown {
                crate::a11y::describe(
                    ctx,
                    shown.response.id,
                    crate::i18n::tr("pair.dialog.link_title"),
                    None,
                );
            }
        }
        if let Some((id, name)) = self.forget.clone() {
            // Escape is the safe answer: keep the pairing.
            if theme::escape_pressed(ctx) {
                self.forget = None;
            }
            let shown = egui::Window::new(crate::i18n::tr("pair.dialog.unlink_title"))
                .id(egui::Id::new("forget-phone"))
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(theme::semibold(crate::i18n::tr("pair.dialog.unlink_title"), 17.0));
                    ui.label(format!(
                        "{name} will no longer be trusted by this computer. To use it again, link it with a new QR code."
                    ));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.forget = None;
                        }
                        if ui
                            .add_enabled(!self.state.busy, egui::Button::new("Unlink phone"))
                            .clicked()
                        {
                            self.submit(Request::Unpair { device_id: id });
                            self.forget = None;
                        }
                    });
                });
            if let Some(shown) = shown {
                crate::a11y::describe(
                    ctx,
                    shown.response.id,
                    crate::i18n::tr("pair.dialog.unlink_title"),
                    None,
                );
            }
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

    // ---- the token cache

    #[cfg(unix)]
    fn token_of(byte: u8) -> Token {
        Token::from_bytes([byte; 32])
    }

    /// A config whose state folder holds `file` (or nothing), with `cached` already in memory.
    #[cfg(unix)]
    fn config(dir: &tempfile::TempDir, file: Option<&Token>, cached: Option<Token>) -> Config {
        let state = dir.path().join("state");
        bridge_kde::fsutil::ensure_private_dir(&state).unwrap();
        if let Some(t) = file {
            bridge_kde::fsutil::atomic_write_private(
                &ipc::token_path(&state),
                t.to_file_contents().as_bytes(),
            )
            .unwrap();
        }
        Config {
            state_dir: Some(state),
            port: 1,
            cached: Arc::new(Mutex::new(cached)),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_cached_token_is_used_without_reading_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(&dir, None, Some(token_of(1))); // no file at all
        let seen = std::cell::RefCell::new(Vec::new());
        let out = c.with_token(|t| {
            seen.borrow_mut().push(t.clone());
            Ok(7)
        });
        assert_eq!(out, Ok(7));
        assert!(*seen.borrow() == [token_of(1)]);
    }

    #[cfg(unix)]
    #[test]
    fn the_first_call_reads_the_file_once_and_keeps_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(&dir, Some(&token_of(2)), None);
        let calls = std::cell::Cell::new(0);
        assert_eq!(
            c.with_token(|_| {
                calls.set(calls.get() + 1);
                Ok(())
            }),
            Ok(())
        );
        assert_eq!(calls.get(), 1);
        // Gone from disk, still usable from memory.
        std::fs::remove_file(ipc::token_path(&dir.path().join("state"))).unwrap();
        assert_eq!(c.with_token(|_| Ok(())), Ok(()));
        assert!(c.cached.lock().unwrap().as_ref() == Some(&token_of(2)));
    }

    #[cfg(unix)]
    #[test]
    fn an_unauthorized_or_protocol_answer_rereads_the_file_once_and_retries_once() {
        for stale_error in [
            || ipc::IpcError::Unauthorized,
            || ipc::IpcError::Protocol("bad hello".into()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            // The service regenerated its token: the file has the new one, memory the old.
            let c = config(&dir, Some(&token_of(9)), Some(token_of(1)));
            let tried = std::cell::RefCell::new(Vec::new());
            let out = c.with_token(|t| {
                tried.borrow_mut().push(t.clone());
                if *t == token_of(1) {
                    Err(stale_error())
                } else {
                    Ok("fine")
                }
            });
            assert_eq!(out, Ok("fine"));
            assert!(*tried.borrow() == [token_of(1), token_of(9)]);
            assert!(c.cached.lock().unwrap().as_ref() == Some(&token_of(9)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_token_that_stays_refused_is_tried_twice_at_most() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(&dir, Some(&token_of(9)), Some(token_of(1)));
        let calls = std::cell::Cell::new(0);
        let out: Result<(), String> = c.with_token(|_| {
            calls.set(calls.get() + 1);
            Err(ipc::IpcError::Unauthorized)
        });
        assert!(out.is_err());
        assert_eq!(
            calls.get(),
            2,
            "the cached token, then the re-read one: no loop"
        );
    }

    #[cfg(unix)]
    #[test]
    fn other_errors_neither_reread_nor_retry() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(&dir, Some(&token_of(9)), Some(token_of(1)));
        let calls = std::cell::Cell::new(0);
        let out: Result<(), String> = c.with_token(|_| {
            calls.set(calls.get() + 1);
            Err(ipc::IpcError::Io(std::io::Error::other("down")))
        });
        assert!(out.is_err());
        assert_eq!(calls.get(), 1);
        assert!(c.cached.lock().unwrap().as_ref() == Some(&token_of(1)));
    }

    #[test]
    fn without_a_state_folder_nothing_is_tried() {
        let c = Config {
            state_dir: None,
            port: 1,
            cached: Arc::default(),
        };
        let calls = std::cell::Cell::new(0);
        let out: Result<(), String> = c.with_token(|_| {
            calls.set(calls.get() + 1);
            Ok(())
        });
        assert!(out.is_err());
        assert_eq!(calls.get(), 0);
    }
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod render_tests;
