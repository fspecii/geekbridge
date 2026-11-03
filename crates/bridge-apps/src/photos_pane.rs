// SPDX-License-Identifier: GPL-3.0-or-later
//! The "Phone photos" tab of the Share window: a grid of the newest photos from a phone, small previews only.
//! Design and privacy: `docs/design/recent-photos.md`.
//!
//! * A client of the control API like the rest of the window. Nothing is requested from a phone until the user
//!   allowed photos for it (an explicit button, off by default) and opened this tab.
//! * Previews are decoded on the worker thread (never on a frame), at most [`MAX_EDGE`] pixels on a side, and the
//!   decoded pictures and the textures made from them are bounded by the 24 photos the list can hold.
//! * A full picture is fetched only by **Save** (kept in the save folder) or **Copy to clipboard** (a scratch file the
//!   service empties soon after; this window copies it with the system's clipboard tool).
use crate::{
    i18n::{tr, tr_args},
    share_app,
    theme::{self, ButtonKind},
};
use bridge_core::{
    ipc::{self, Feature, Request, Response},
    photos_link::MAX_ITEMS,
    photos_view::{FetchInfo, FetchMode, FetchState, PhotoItem, PhotosView},
};
use eframe::egui::{self, Color32, RichText, Sense, TextureHandle, TextureOptions, vec2};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Set to `offered` to show the "allow photos" state without a service (screenshots).
pub const FIXTURE_ENV: &str = "GEEKBRIDGE_PHOTOS_FIXTURE";
/// Longest edge of a preview the window will decode.
pub const MAX_EDGE: u16 = 256;
/// Previews fetched from the service per worker cycle, so the window fills in without a burst.
const THUMBS_PER_CYCLE: usize = 4;
const CELL: f32 = 104.0;
const GAP: f32 = 8.0;
const MAX_HANDLED: usize = 64;

// ---------------------------------------------------------------------- decoding

/// A decoded preview, ready to become a texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub width: usize,
    pub height: usize,
    /// `width * height * 4` bytes, straight alpha (always opaque).
    pub rgba: Vec<u8>,
}

/// Why a preview was not decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NotJpeg,
    TooLarge,
    Damaged,
    Unsupported,
}

/// Decode a preview JPEG. Refuses anything that is not a small JPEG: the size is read from the header before any
/// pixel is decoded.
pub fn decode_thumb(jpeg: &[u8]) -> Result<Decoded, DecodeError> {
    use jpeg_decoder::PixelFormat;
    let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(jpeg));
    d.read_info().map_err(|_| DecodeError::NotJpeg)?;
    let info = d.info().ok_or(DecodeError::NotJpeg)?;
    if info.width == 0 || info.height == 0 || info.width > MAX_EDGE || info.height > MAX_EDGE {
        return Err(DecodeError::TooLarge);
    }
    let pixels = d.decode().map_err(|_| DecodeError::Damaged)?;
    let (w, h) = (usize::from(info.width), usize::from(info.height));
    let rgba = match info.pixel_format {
        PixelFormat::RGB24 if pixels.len() == w * h * 3 => pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        PixelFormat::L8 if pixels.len() == w * h => {
            pixels.iter().flat_map(|&g| [g, g, g, 255]).collect()
        }
        _ => return Err(DecodeError::Unsupported),
    };
    Ok(Decoded {
        width: w,
        height: h,
        rgba,
    })
}

// ---------------------------------------------------------------------- clipboard

/// The picture type the clipboard tools are told about; `None` for what the window cannot copy (HEIC and so on).
pub fn mime_for(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// How a program is run to put a picture on the clipboard: its arguments, whether the file is its standard input,
/// and environment it needs (the file name travels in the environment, never inside script text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyCommand {
    pub program: &'static str,
    pub args: Vec<String>,
    pub file_on_stdin: bool,
    pub env: Vec<(&'static str, String)>,
}

/// Clipboard tools to try in order for `path` of type `mime`.
pub fn copy_commands(path: &Path, mime: &str) -> Vec<CopyCommand> {
    let p = path.display().to_string();
    if cfg!(target_os = "windows") {
        vec![CopyCommand {
            program: "powershell", // i18n-ok: a program
            args: vec![
                "-NoProfile".into(), // i18n-ok: an option
                "-Command".into(),   // i18n-ok: an option
                "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; [System.Windows.Forms.Clipboard]::SetImage([System.Drawing.Image]::FromFile($env:GEEKBRIDGE_PHOTO))" // i18n-ok: a script
                    .into(),
            ],
            file_on_stdin: false,
            env: vec![("GEEKBRIDGE_PHOTO", p)], // i18n-ok: a variable
        }]
    } else if cfg!(target_os = "macos") {
        let class = match mime {
            "image/jpeg" => "JPEG picture", // i18n-ok: an AppleScript type
            "image/png" => "«class PNGf»",  // i18n-ok: an AppleScript type
            "image/gif" => "GIF picture",   // i18n-ok: an AppleScript type
            _ => return Vec::new(),
        };
        vec![CopyCommand {
            program: "osascript", // i18n-ok: a program
            args: vec![
                "-e".into(),
                "on run argv".into(), // i18n-ok: a script
                "-e".into(),
                format!("set the clipboard to (read (POSIX file (item 1 of argv)) as {class})"), // i18n-ok: a script
                "-e".into(),
                "end run".into(), // i18n-ok: a script
                p,
            ],
            file_on_stdin: false,
            env: Vec::new(),
        }]
    } else {
        vec![
            CopyCommand {
                program: "wl-copy",                           // i18n-ok: a program
                args: vec!["--type".into(), mime.to_owned()], // i18n-ok: an option
                file_on_stdin: true,
                env: Vec::new(),
            },
            CopyCommand {
                program: "xclip", // i18n-ok: a program
                args: vec![
                    "-selection".into(), // i18n-ok: an option
                    "clipboard".into(),  // i18n-ok: an option
                    "-t".into(),
                    mime.to_owned(),
                    "-i".into(),
                    p,
                ],
                file_on_stdin: false,
                env: Vec::new(),
            },
        ]
    }
}

/// Puts the picture at `path` on the clipboard. `Err` names the tools that were tried.
pub fn copy_picture(path: &Path, mime: &str) -> Result<(), String> {
    let commands = copy_commands(path, mime);
    for c in &commands {
        let mut cmd = Command::new(c.program);
        cmd.args(&c.args)
            .envs(c.env.iter().map(|(k, v)| (*k, v)))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if c.file_on_stdin {
            let Ok(file) = std::fs::File::open(path) else {
                return Err(commands[0].program.to_owned());
            };
            cmd.stdin(Stdio::from(file));
        } else {
            cmd.stdin(Stdio::null());
        }
        // The Linux tools stay in the background to serve the clipboard, so only start them.
        let Ok(mut child) = crate::home_app::no_window(&mut cmd).spawn() else {
            continue;
        };
        std::thread::sleep(Duration::from_millis(150));
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => continue,
            Ok(None) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            _ => return Ok(()),
        }
    }
    Err(commands
        .iter()
        .map(|c| c.program)
        .collect::<Vec<_>>()
        .join(", "))
}

// ---------------------------------------------------------------------- worker

type Key = (String, String);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyOutcome {
    Copied,
    NoTool(String),
}

/// Everything the window shows, produced by the worker thread.
#[derive(Clone, Default)]
pub struct Model {
    pub photos: Option<PhotosView>,
    pub thumbs: HashMap<Key, Arc<Decoded>>,
    pub copies: HashMap<u64, CopyOutcome>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

impl Model {
    fn same_as(&self, other: &Model) -> bool {
        self.photos == other.photos
            && self.notice == other.notice
            && self.error == other.error
            && self.copies == other.copies
            && self.thumbs.len() == other.thumbs.len()
            && self
                .thumbs
                .iter()
                .all(|(k, v)| other.thumbs.get(k).is_some_and(|o| Arc::ptr_eq(v, o)))
    }
}

pub(crate) enum Cmd {
    Refresh(String),
    Allow(String, bool),
    Fetch {
        device_id: String,
        id: String,
        mode: FetchMode,
    },
}

struct Worker {
    tx: mpsc::SyncSender<Cmd>,
    latest: Arc<Mutex<Option<Model>>>,
}

fn offered_fixture() -> PhotosView {
    PhotosView {
        offered: vec![bridge_core::photos_view::PhotoDevice {
            device_id: "fixture".into(),
            name: "Pixel (simulated)".into(), // i18n-ok: screenshot fixture
            partial: false,
            listed: false,
        }],
        ..PhotosView::default()
    }
}

impl Worker {
    fn spawn(wake: impl Fn() + Send + 'static, active: Arc<AtomicBool>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Cmd>(16);
        let latest = Arc::new(Mutex::new(None));
        let out = latest.clone();
        let fixture = std::env::var(FIXTURE_ENV).is_ok_and(|v| v == "offered");
        std::thread::Builder::new()
            .name("geekbridge-photos".into())
            .spawn(move || {
                let port = ipc::control_port_from_env();
                let mut model = Model::default();
                let mut published: Option<Model> = None;
                let mut handled: HashSet<u64> = HashSet::new();
                loop {
                    if fixture {
                        model.photos = Some(offered_fixture());
                    } else {
                        match share_app::token() {
                            Err(e) => {
                                model.photos = None;
                                model.error = Some(e);
                            }
                            Ok(token) => {
                                cycle(
                                    port,
                                    &token,
                                    &mut model,
                                    &mut handled,
                                    active.load(Ordering::Relaxed),
                                );
                            }
                        }
                    }
                    if published.as_ref().is_none_or(|p| !p.same_as(&model)) {
                        published = Some(model.clone());
                        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(model.clone());
                        wake();
                    }
                    let wait = if active.load(Ordering::Relaxed) {
                        500
                    } else {
                        2000
                    };
                    match rx.recv_timeout(crate::pace::scaled(Duration::from_millis(wait))) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(cmd) if fixture => {
                            let _ = cmd;
                        }
                        Ok(cmd) => {
                            let request = match cmd {
                                Cmd::Refresh(device_id) => Request::PhotosRefresh { device_id },
                                Cmd::Allow(device_id, enabled) => Request::SetConsent {
                                    device_id,
                                    feature: Feature::Photos,
                                    enabled,
                                },
                                Cmd::Fetch {
                                    device_id,
                                    id,
                                    mode,
                                } => Request::PhotoFetch {
                                    device_id,
                                    id,
                                    mode,
                                },
                            };
                            model.notice =
                                match share_app::token().map(|t| ipc::call(port, &t, &request)) {
                                    Ok(Ok(Response::Error(b))) => Some(b.message),
                                    Ok(Ok(_)) => None,
                                    Ok(Err(e)) => Some(share_app::describe(e)),
                                    Err(e) => Some(e),
                                };
                        }
                    }
                }
            })
            .expect("spawn photos worker");
        Self { tx, latest }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.try_send(cmd);
    }

    fn latest(&self) -> Option<Model> {
        self.latest.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// One poll: the view, then (while the tab is open) a few missing previews and any finished copy.
fn cycle(
    port: u16,
    token: &ipc::Token,
    model: &mut Model,
    handled: &mut HashSet<u64>,
    active: bool,
) {
    match ipc::call(port, token, &Request::Photos { device_id: None }) {
        Ok(Response::Photos(v)) => {
            model.photos = Some(*v);
            model.error = None;
        }
        Ok(_) => {}
        Err(e) => {
            model.photos = None;
            model.error = Some(share_app::describe(e));
            return;
        }
    }
    let Some(view) = model.photos.clone() else {
        return;
    };
    let wanted: HashSet<Key> = view
        .items
        .iter()
        .map(|i| (i.device_id.clone(), i.id.clone()))
        .collect();
    model.thumbs.retain(|k, _| wanted.contains(k));
    if !active {
        return;
    }
    let missing: Vec<&PhotoItem> = view
        .items
        .iter()
        .filter(|i| {
            i.has_thumb
                && !model
                    .thumbs
                    .contains_key(&(i.device_id.clone(), i.id.clone()))
        })
        .take(THUMBS_PER_CYCLE)
        .collect();
    for item in missing {
        let request = Request::PhotoThumb {
            device_id: item.device_id.clone(),
            id: item.id.clone(),
        };
        if let Ok(Response::PhotoThumb(t)) = ipc::call(port, token, &request) {
            use base64::Engine as _;
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&t.jpeg_base64)
                .ok()
                .and_then(|bytes| decode_thumb(&bytes).ok());
            if let Some(d) = decoded {
                model
                    .thumbs
                    .insert((item.device_id.clone(), item.id.clone()), Arc::new(d));
            }
        }
    }
    for f in &view.fetches {
        if f.mode == FetchMode::Copy
            && f.state == FetchState::Done
            && let Some(path) = &f.path
            && handled.insert(f.fetch_id)
        {
            let outcome = match mime_for(&f.name).map(|m| copy_picture(Path::new(path), m)) {
                Some(Ok(())) => CopyOutcome::Copied,
                Some(Err(tools)) => CopyOutcome::NoTool(tools),
                None => CopyOutcome::NoTool(String::new()),
            };
            model.copies.insert(f.fetch_id, outcome);
            if handled.len() > MAX_HANDLED {
                handled.clear();
            }
        }
    }
}

// ---------------------------------------------------------------------- pane

pub struct PhotosPane {
    worker: Worker,
    pub model: Model,
    active: Arc<AtomicBool>,
    selected: Option<Key>,
    textures: HashMap<Key, TextureHandle>,
    /// Phones this tab already asked for a list since they were allowed.
    asked: HashSet<String>,
}

/// What a click did (for tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Allow(String, bool),
    Refresh(String),
    Fetch(String, String, FetchMode),
    Select(String, String),
}

impl PhotosPane {
    pub fn new(wake: impl Fn() + Send + 'static) -> Self {
        let active = Arc::new(AtomicBool::new(false));
        Self {
            worker: Worker::spawn(wake, active.clone()),
            model: Model::default(),
            active,
            selected: None,
            textures: HashMap::new(),
            asked: HashSet::new(),
        }
    }

    /// For tests: a pane whose commands go to `rx` and which never touches the service.
    #[cfg(test)]
    pub(crate) fn for_test(model: Model) -> (Self, mpsc::Receiver<Cmd>) {
        let (tx, rx) = mpsc::sync_channel(16);
        (
            Self {
                worker: Worker {
                    tx,
                    latest: Arc::new(Mutex::new(None)),
                },
                model,
                active: Arc::new(AtomicBool::new(false)),
                selected: None,
                textures: HashMap::new(),
                asked: HashSet::new(),
            },
            rx,
        )
    }

    pub fn poll(&mut self) {
        if let Some(m) = self.worker.latest() {
            self.model = m;
        }
    }

    /// A connected phone offers recent photos (allowed or not): the tab is worth showing.
    pub fn available(&self) -> bool {
        self.model
            .photos
            .as_ref()
            .is_some_and(|v| !v.devices.is_empty() || !v.offered.is_empty())
    }

    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Relaxed);
    }

    fn command(&self, a: &Action) {
        self.worker.send(match a.clone() {
            Action::Allow(d, on) => Cmd::Allow(d, on),
            Action::Refresh(d) => Cmd::Refresh(d),
            Action::Fetch(device_id, id, mode) => Cmd::Fetch {
                device_id,
                id,
                mode,
            },
            Action::Select(..) => return,
        });
    }

    /// The tab's content. Returns the actions the user took this frame.
    pub fn show(&mut self, ui: &mut egui::Ui, pal: &theme::Palette) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(e) = &self.model.error {
            ui.add(egui::Label::new(RichText::new(e).color(pal.err)).wrap());
            ui.add_space(8.0);
        }
        let Some(view) = self.model.photos.clone() else {
            ui.label(theme::caption(pal, tr("photos.loading")));
            return actions;
        };
        // Phones that were allowed again after being stopped are asked again.
        self.asked
            .retain(|d| view.devices.iter().any(|x| &x.device_id == d));
        if view.devices.is_empty() && view.offered.is_empty() {
            ui.add(egui::Label::new(theme::secondary(pal, tr("photos.none"))).wrap());
        }
        for d in &view.offered {
            theme::group(ui, pal, |ui| {
                let phone = &d.name;
                ui.add(
                    egui::Label::new(theme::semibold(
                        tr_args("photos.allow.title", &[("phone", phone)]),
                        14.0,
                    ))
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(theme::caption(
                        pal,
                        tr_args("photos.allow.body", &[("phone", phone), ("n", &MAX_ITEMS)]),
                    ))
                    .wrap(),
                );
                ui.add_space(4.0);
                if theme::pill_button(
                    ui,
                    pal,
                    &tr_args("photos.allow.button", &[("phone", phone)]),
                    ButtonKind::Primary,
                    true,
                )
                .clicked()
                {
                    actions.push(Action::Allow(d.device_id.clone(), true));
                }
            });
            ui.add_space(8.0);
        }
        for d in &view.devices {
            if self.asked.insert(d.device_id.clone()) {
                actions.push(Action::Refresh(d.device_id.clone()));
            }
            self.device_section(ui, pal, &view, d, &mut actions);
        }
        // Screenshot aid: open with the newest photo selected (`GEEKBRIDGE_PHOTOS_SELECT_FIRST=1`).
        if self.selected.is_none()
            && std::env::var_os("GEEKBRIDGE_PHOTOS_SELECT_FIRST").is_some() // i18n-ok: a variable
            && let Some(first) = view.items.first()
        {
            self.selected = Some((first.device_id.clone(), first.id.clone()));
        }
        let live: HashSet<Key> = self.model.thumbs.keys().cloned().collect();
        self.textures.retain(|k, _| live.contains(k));
        for a in &actions {
            self.command(a);
        }
        actions
    }

    fn texture_for(&mut self, ctx: &egui::Context, key: &Key) -> Option<TextureHandle> {
        if let Some(t) = self.textures.get(key) {
            return Some(t.clone());
        }
        let d = self.model.thumbs.get(key)?;
        let image = egui::ColorImage::from_rgba_unmultiplied([d.width, d.height], &d.rgba);
        let handle = ctx.load_texture(
            format!("photo-{}-{}", key.0, key.1),
            image,
            TextureOptions::LINEAR,
        );
        self.textures.insert(key.clone(), handle.clone());
        Some(handle)
    }

    fn device_section(
        &mut self,
        ui: &mut egui::Ui,
        pal: &theme::Palette,
        view: &PhotosView,
        dev: &bridge_core::photos_view::PhotoDevice,
        actions: &mut Vec<Action>,
    ) {
        let items: Vec<&PhotoItem> = view
            .items
            .iter()
            .filter(|i| i.device_id == dev.device_id)
            .collect();
        ui.horizontal(|ui| {
            ui.label(theme::overline(pal, tr("photos.title")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::pill_button(ui, pal, tr("photos.refresh"), ButtonKind::Plain, true)
                    .clicked()
                {
                    actions.push(Action::Refresh(dev.device_id.clone()));
                }
            });
        });
        if dev.partial {
            ui.add(
                egui::Label::new(theme::caption(
                    pal,
                    tr_args("photos.partial", &[("phone", &dev.name)]),
                ))
                .wrap(),
            );
        }
        ui.add_space(4.0);
        if !dev.listed {
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    tr_args("photos.refreshing", &[("phone", &dev.name)]),
                ))
                .wrap(),
            );
        } else if items.is_empty() {
            ui.add(
                egui::Label::new(theme::secondary(
                    pal,
                    tr_args("photos.empty", &[("phone", &dev.name)]),
                ))
                .wrap(),
            );
        }
        let cols = (((ui.available_width() + GAP) / (CELL + GAP)).floor() as usize).max(2);
        for row in items.chunks(cols) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for item in row {
                    self.cell(ui, pal, item, actions);
                }
            });
            ui.add_space(GAP - 4.0);
        }
        if let Some(sel) = self.selected.clone().filter(|k| k.0 == dev.device_id)
            && let Some(item) = items.iter().find(|i| i.id == sel.1)
        {
            ui.add_space(6.0);
            self.detail(ui, pal, view, dev, item, actions);
        }
        ui.add_space(8.0);
        ui.add(egui::Label::new(theme::caption(pal, tr("photos.privacy"))).wrap());
        ui.add_space(4.0);
        if theme::pill_button(ui, pal, tr("photos.stop"), ButtonKind::Plain, true).clicked() {
            actions.push(Action::Allow(dev.device_id.clone(), false));
        }
    }

    fn cell(
        &mut self,
        ui: &mut egui::Ui,
        pal: &theme::Palette,
        item: &PhotoItem,
        actions: &mut Vec<Action>,
    ) {
        let key = (item.device_id.clone(), item.id.clone());
        let (rect, response) = ui.allocate_exact_size(vec2(CELL, CELL), Sense::click());
        let selected = self.selected.as_ref() == Some(&key);
        ui.painter().rect_filled(rect, 8.0, pal.surface);
        let tex = self.texture_for(ui.ctx(), &key);
        if let Some(tex) = &tex {
            let size = tex.size_vec2();
            let scale = ((CELL - 8.0) / size.x).min((CELL - 8.0) / size.y);
            let fitted = egui::Rect::from_center_size(rect.center(), size * scale);
            ui.painter().image(
                tex.id(),
                fitted,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        if selected {
            ui.painter().rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(2.0_f32, pal.accent),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter().rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.0_f32, pal.separator),
                egui::StrokeKind::Inside,
            );
        }
        theme::ring_when_focused(ui, pal, &response);
        let when = crate::localtime::stamp_local(item.taken_ms as i64);
        let label = tr_args(
            if tex.is_some() {
                "photos.cell"
            } else {
                "photos.cell.loading"
            },
            &[("name", &item.name), ("when", &when)],
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &label)
        });
        if response.clicked() {
            self.selected = Some(key);
            actions.push(Action::Select(item.device_id.clone(), item.id.clone()));
        }
    }

    fn detail(
        &mut self,
        ui: &mut egui::Ui,
        pal: &theme::Palette,
        view: &PhotosView,
        dev: &bridge_core::photos_view::PhotoDevice,
        item: &PhotoItem,
        actions: &mut Vec<Action>,
    ) {
        let fetch: Option<&FetchInfo> = view
            .fetches
            .iter()
            .rev()
            .find(|f| f.device_id == item.device_id && f.photo_id == item.id);
        let busy =
            fetch.is_some_and(|f| matches!(f.state, FetchState::Waiting | FetchState::Receiving));
        theme::group(ui, pal, |ui| {
            ui.add(egui::Label::new(theme::semibold(&item.name, 14.0)).truncate());
            ui.label(theme::caption(
                pal,
                tr_args(
                    "photos.detail",
                    &[
                        ("size", &share_app::human_size(item.size)),
                        ("width", &item.width),
                        ("height", &item.height),
                        ("when", &crate::localtime::stamp_local(item.taken_ms as i64)),
                    ],
                ),
            ));
            let can_copy = mime_for(&item.name).is_some();
            ui.horizontal(|ui| {
                if theme::pill_button(ui, pal, tr("photos.save"), ButtonKind::Primary, !busy)
                    .clicked()
                {
                    actions.push(Action::Fetch(
                        item.device_id.clone(),
                        item.id.clone(),
                        FetchMode::Save,
                    ));
                }
                if theme::pill_button(
                    ui,
                    pal,
                    tr("photos.copy"),
                    ButtonKind::Plain,
                    !busy && can_copy,
                )
                .clicked()
                {
                    actions.push(Action::Fetch(
                        item.device_id.clone(),
                        item.id.clone(),
                        FetchMode::Copy,
                    ));
                }
            });
            if !can_copy {
                ui.add(egui::Label::new(theme::caption(pal, tr("photos.copy.unsupported"))).wrap());
            }
            if let Some(f) = fetch {
                self.fetch_line(ui, pal, dev, f);
            }
        });
    }

    fn fetch_line(
        &self,
        ui: &mut egui::Ui,
        pal: &theme::Palette,
        dev: &bridge_core::photos_view::PhotoDevice,
        f: &FetchInfo,
    ) {
        let line = match (f.state, f.mode) {
            (FetchState::Waiting, _) => tr_args("photos.fetch.waiting", &[("phone", &dev.name)]),
            (FetchState::Receiving, _) => tr("photos.fetch.receiving").to_owned(),
            (FetchState::Done, FetchMode::Save) => tr_args(
                "photos.fetch.saved",
                &[("path", &f.path.clone().unwrap_or_default())],
            ),
            (FetchState::Done, FetchMode::Copy) => match self.model.copies.get(&f.fetch_id) {
                Some(CopyOutcome::Copied) => tr("photos.fetch.copied").to_owned(),
                Some(CopyOutcome::NoTool(tool)) => {
                    tr_args("photos.fetch.copy_failed", &[("tool", tool)])
                }
                None => tr("photos.fetch.receiving").to_owned(),
            },
            (FetchState::Failed, _) => tr_args(
                "photos.fetch.failed",
                &[(
                    "reason",
                    &match f.code.as_deref() {
                        Some("expired") => tr("photos.fetch.reason.expired"),
                        _ => tr("photos.fetch.reason.other"),
                    },
                )],
            ),
        };
        let label = ui.add(egui::Label::new(theme::caption(pal, &line)).wrap());
        crate::a11y::live_region(ui.ctx(), label.id, crate::a11y::Live::Polite);
        if f.state == FetchState::Done
            && f.mode == FetchMode::Save
            && let Some(path) = &f.path
            && theme::pill_button(ui, pal, tr("photos.fetch.show"), ButtonKind::Plain, true)
                .clicked()
        {
            share_app::open_in_system(Path::new(path), true);
        }
    }
}

#[cfg(test)]
#[path = "photos_pane_tests.rs"]
mod tests;
