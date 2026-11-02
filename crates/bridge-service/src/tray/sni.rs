//! Linux tray: a StatusNotifierItem (`org.kde.StatusNotifierItem`) with a `com.canonical.dbusmenu`
//! menu, hand-written on the `zbus` the call backend already links. Waybar's `tray` module, KDE
//! Plasma, GNOME with the AppIndicator extension, XFCE and swaync all host this pair.
//!
//! Optional by construction: with no session bus, or no `org.kde.StatusNotifierWatcher` owner, the
//! item is simply never registered and nothing is shown. When a watcher appears later (waybar
//! started after us, or restarted) the `NameOwnerChanged` signal for that one name registers the
//! item again; there is no timer anywhere in this file.
use super::{
    icon::{self, Style},
    model::{IconState, ItemKind, MenuItem, TrayAction, TrayInput, action_for, menu},
};
use futures_lite::StreamExt;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use zbus::{
    Connection,
    object_server::SignalEmitter,
    zvariant::{Array, Dict, ObjectPath, OwnedObjectPath, OwnedValue, StructureBuilder, Value},
};

pub const WATCHER: &str = "org.kde.StatusNotifierWatcher";
pub const WATCHER_PATH: &str = "/StatusNotifierWatcher";
pub const ITEM_PATH: &str = "/StatusNotifierItem";
pub const MENU_PATH: &str = "/MenuBar";

type Pixmap = (i32, i32, Vec<u8>);
type Props = HashMap<String, OwnedValue>;
type Layout = (i32, Props, Vec<OwnedValue>);

/// Everything the two interfaces answer from; replaced as a whole by [`Sni::update`].
struct Snapshot {
    input: TrayInput,
    state: IconState,
    menu: Vec<MenuItem>,
    revision: u32,
    pixmaps: Vec<Pixmap>,
}

impl Snapshot {
    fn new(input: &TrayInput, style: Style) -> Self {
        let state = input.icon_state();
        Self {
            input: input.clone(),
            state,
            menu: menu(input),
            revision: 1,
            pixmaps: pixmaps(state, style),
        }
    }
}

fn pixmaps(state: IconState, style: Style) -> Vec<Pixmap> {
    icon::SNI_SIZES
        .iter()
        .map(|&s| (s as i32, s as i32, icon::render(state, s, style).argb_be()))
        .collect()
}

type Shared = Arc<Mutex<Snapshot>>;
type OnAction = Arc<dyn Fn(TrayAction) + Send + Sync>;

fn lock(s: &Shared) -> MutexGuard<'_, Snapshot> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn ov<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
    v.into()
        .try_to_owned()
        .expect("plain values carry no file descriptors")
}

// ------------------------------------------------------------------ StatusNotifierItem

struct Item {
    shared: Shared,
    on_action: OnAction,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "geekbridge"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "GeekBridge"
    }
    #[zbus(property)]
    fn status(&self) -> String {
        lock(&self.shared).input.sni_status().to_owned()
    }
    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        lock(&self.shared).pixmaps.clone()
    }
    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        lock(&self.shared).pixmaps.clone()
    }
    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        let (title, body) = lock(&self.shared).input.tooltip();
        (String::new(), Vec::new(), title, body)
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(MENU_PATH).into()
    }

    /// A left click opens the app; the menu is on the right click (the host draws it).
    fn activate(&self, _x: i32, _y: i32) {
        (self.on_action)(TrayAction::Open);
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {
        (self.on_action)(TrayAction::PasteFromPhone);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_attention_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

// ------------------------------------------------------------------ dbusmenu

struct Menu {
    shared: Shared,
    on_action: OnAction,
}

fn item_props(item: &MenuItem) -> Props {
    let mut p = Props::new();
    match item.kind {
        ItemKind::Separator => {
            p.insert("type".into(), ov("separator"));
        }
        _ => {
            // `_` marks a mnemonic in dbusmenu labels; a phone name with one must keep it.
            p.insert("label".into(), ov(item.label.replace('_', "__")));
            p.insert("enabled".into(), ov(item.enabled));
        }
    }
    p.insert("visible".into(), ov(item.visible));
    p
}

fn wanted(props: Props, names: &[String]) -> Props {
    if names.is_empty() {
        props
    } else {
        props
            .into_iter()
            .filter(|(k, _)| names.contains(k))
            .collect()
    }
}

fn child_value(item: &MenuItem, names: &[String]) -> OwnedValue {
    let dict: Dict = wanted(item_props(item), names)
        .into_iter()
        .map(|(k, v)| (k, Value::from(v)))
        .collect::<HashMap<_, _>>()
        .into();
    let children: Array = Array::from(Vec::<Value>::new());
    let structure = StructureBuilder::new()
        .add_field(item.id)
        .append_field(Value::from(dict))
        .append_field(Value::from(children))
        .build()
        .expect("three fields");
    Value::Structure(structure)
        .try_to_owned()
        .expect("no file descriptors")
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let snap = lock(&self.shared);
        let mut root_props = Props::new();
        root_props.insert("children-display".into(), ov("submenu"));
        let layout = match parent_id {
            0 => {
                let kids = if recursion_depth == 0 {
                    Vec::new()
                } else {
                    snap.menu
                        .iter()
                        .map(|i| child_value(i, &property_names))
                        .collect()
                };
                (0, wanted(root_props, &property_names), kids)
            }
            id => {
                let item =
                    snap.menu.iter().find(|i| i.id == id).ok_or_else(|| {
                        zbus::fdo::Error::InvalidArgs(format!("no menu item {id}"))
                    })?;
                (id, wanted(item_props(item), &property_names), Vec::new())
            }
        };
        Ok((snap.revision, layout))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Props)> {
        let snap = lock(&self.shared);
        snap.menu
            .iter()
            .filter(|i| ids.is_empty() || ids.contains(&i.id))
            .map(|i| (i.id, wanted(item_props(i), &property_names)))
            .collect()
    }

    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        let snap = lock(&self.shared);
        snap.menu
            .iter()
            .find(|i| i.id == id)
            .and_then(|i| item_props(i).remove(name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on {id}")))
    }

    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id != "clicked" {
            return;
        }
        let action = action_for(&lock(&self.shared).input, id);
        if let Some(action) = action {
            (self.on_action)(action);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let snap = lock(&self.shared);
        let mut unknown = Vec::new();
        let mut actions = Vec::new();
        for (id, kind, _, _) in events {
            if !snap.menu.iter().any(|i| i.id == id) {
                unknown.push(id);
            } else if kind == "clicked"
                && let Some(a) = action_for(&snap.input, id)
            {
                actions.push(a);
            }
        }
        drop(snap);
        for a in actions {
            (self.on_action)(a);
        }
        unknown
    }

    /// The menu is already current (every change is signalled), so nothing needs refreshing.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }
    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated_props: Vec<(i32, Props)>,
        removed_props: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

// ------------------------------------------------------------------ the running item

pub struct Sni {
    conn: Connection,
    shared: Shared,
    style: Style,
    pub bus_name: String,
}

#[derive(Debug)]
pub enum StartError {
    NoBus(String),
}

impl Sni {
    /// Connect to the session bus (or to `address`, which tests point at a private bus), publish
    /// the two objects, and register with a watcher if one exists now or appears later.
    pub async fn start(
        address: Option<&str>,
        style: Style,
        initial: &TrayInput,
        on_action: OnAction,
    ) -> Result<Self, StartError> {
        let shared: Shared = Arc::new(Mutex::new(Snapshot::new(initial, style)));
        let bus_name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        // Connect ourselves: zbus would do it on the `blocking` crate's thread pool, whose first
        // thread then wakes twice a second for the rest of the process's life.
        let stream = connect(address).map_err(StartError::NoBus)?;
        let builder = zbus::connection::Builder::async_io_unix_stream(stream);
        let conn = builder
            .name(bus_name.as_str())
            .and_then(|b| {
                b.serve_at(
                    ITEM_PATH,
                    Item {
                        shared: shared.clone(),
                        on_action: on_action.clone(),
                    },
                )
            })
            .and_then(|b| {
                b.serve_at(
                    MENU_PATH,
                    Menu {
                        shared: shared.clone(),
                        on_action,
                    },
                )
            })
            .map_err(|e| StartError::NoBus(e.to_string()))?
            .build()
            .await
            .map_err(|e| StartError::NoBus(e.to_string()))?;
        tokio::spawn(follow_watcher(conn.clone(), bus_name.clone()));
        Ok(Self {
            conn,
            shared,
            style,
            bus_name,
        })
    }

    /// Show `input`. Emits only the signals whose content really changed.
    pub async fn update(&self, input: &TrayInput) {
        let (icon_changed, tip_changed, status_changed, menu_revision) = {
            let mut s = lock(&self.shared);
            let state = input.icon_state();
            let icon_changed = state != s.state;
            let tip_changed = input.tooltip() != s.input.tooltip();
            let status_changed = input.sni_status() != s.input.sni_status();
            let new_menu = menu(input);
            let menu_changed = new_menu != s.menu;
            if icon_changed {
                s.pixmaps = pixmaps(state, self.style);
            }
            s.state = state;
            s.input = input.clone();
            s.menu = new_menu;
            if menu_changed {
                s.revision += 1;
            }
            (
                icon_changed,
                tip_changed,
                status_changed,
                menu_changed.then_some(s.revision),
            )
        };
        let server = self.conn.object_server();
        if let Ok(item) = server.interface::<_, Item>(ITEM_PATH).await {
            let e = item.signal_emitter();
            if icon_changed {
                let _ = Item::new_icon(e).await;
                let _ = Item::new_attention_icon(e).await;
            }
            if tip_changed {
                let _ = Item::new_tool_tip(e).await;
            }
            if status_changed {
                let status = input.sni_status();
                let _ = Item::new_status(e, status).await;
            }
        }
        if let Some(revision) = menu_revision
            && let Ok(m) = server.interface::<_, Menu>(MENU_PATH).await
        {
            let _ = Menu::layout_updated(m.signal_emitter(), revision, 0).await;
        }
    }
}

/// The session bus socket: `address` (a `DBUS_SESSION_BUS_ADDRESS` value, as tests pass), else the
/// environment's, else `$XDG_RUNTIME_DIR/bus`. Only `unix:path=` and `unix:abstract=` entries are
/// understood; anything else means "no usable bus", which is a silent no-tray.
fn connect(address: Option<&str>) -> Result<std::os::unix::net::UnixStream, String> {
    use std::os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr, UnixStream},
    };
    let from_env = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    let list = match address.map(str::to_owned).or(from_env) {
        Some(list) if !list.is_empty() => list,
        _ => {
            let dir = std::env::var("XDG_RUNTIME_DIR")
                .map_err(|_| "no session bus address".to_owned())?;
            format!("unix:path={dir}/bus")
        }
    };
    let mut last = "no usable bus address".to_owned();
    for entry in list.split(';') {
        let Some(rest) = entry.strip_prefix("unix:") else {
            continue;
        };
        let value = |key: &str| {
            rest.split(',')
                .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
                .map(unescape)
        };
        let connected = if let Some(path) = value("path") {
            UnixStream::connect(path)
        } else if let Some(name) = value("abstract") {
            SocketAddr::from_abstract_name(name.as_bytes())
                .and_then(|a| UnixStream::connect_addr(&a))
        } else {
            continue;
        };
        match connected {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

/// D-Bus address values percent-escape everything but a few characters.
fn unescape(v: &str) -> String {
    let bytes = v.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: &[u8]| {
            std::str::from_utf8(b)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        };
        match (bytes[i], bytes.get(i + 1..i + 3).and_then(hex)) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Register with the watcher now if it has an owner, and again every time it gets a new owner
/// (the bar restarted). Waits on the bus's `NameOwnerChanged` for that single name.
async fn follow_watcher(conn: Connection, bus_name: String) {
    let Ok(dbus) = zbus::fdo::DBusProxy::new(&conn).await else {
        return;
    };
    let Ok(mut changes) = dbus
        .receive_name_owner_changed_with_args(&[(0, WATCHER)])
        .await
    else {
        return;
    };
    let watcher = zbus::names::BusName::try_from(WATCHER).expect("valid name");
    if dbus.name_has_owner(watcher).await.unwrap_or(false) {
        register(&conn, &bus_name).await;
    }
    while let Some(change) = changes.next().await {
        if change.args().is_ok_and(|a| a.new_owner().is_some()) {
            register(&conn, &bus_name).await;
        }
    }
}

/// `RegisterStatusNotifierItem`, retried twice because a watcher publishes its name a moment
/// before its object is ready. A failure is silent: no host, no icon.
async fn register(conn: &Connection, bus_name: &str) {
    for attempt in 0..3 {
        let sent = conn
            .call_method(
                Some(WATCHER),
                WATCHER_PATH,
                Some(WATCHER),
                "RegisterStatusNotifierItem",
                &(bus_name,),
            )
            .await;
        if sent.is_ok() {
            return;
        }
        if attempt < 2 {
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::unescape;

    #[test]
    fn address_values_are_unescaped() {
        assert_eq!(unescape("/run/user/1000/bus"), "/run/user/1000/bus");
        assert_eq!(unescape("/tmp/a%20b%2Fc"), "/tmp/a b/c");
        assert_eq!(unescape("100%"), "100%");
    }
}
