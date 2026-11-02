// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard on the phone transport. Rust owns the wire; Kotlin owns the Android clipboard and every
//! decision about *whether* to hand something over (unlocked? fresh? sensitive? enabled?).
//!
//! * **Computer -> phone** (stock `kdeconnect.clipboard`, or `geekbridge.clipboard.item` for HTML,
//!   URLs and images) becomes an *event* that Kotlin collects with a blocking `transport_clipboard_wait`.
//!   There are no polling loops: the call parks on a condition variable until something arrives, and
//!   Kotlin writes the clipboard (Android allows writes in the background).
//! * **Phone -> computer** only ever happens when Kotlin calls `transport_clipboard_send`: either as the
//!   answer to a `pull` event, or as an explicit user action. Nothing is sent by this module on its own.
//! * Text events are dropped from memory as soon as Kotlin acknowledges them (`since`). Images are
//!   spooled to a private cache directory, sniffed for PNG/JPEG magic and deleted by Kotlin after use
//!   (and on every start and stop). Nothing is logged.
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bridge_core::clipboard::DEFAULT_MAX_TEXT_BYTES;
use bridge_core::clipboard_wire::{
    self as wire, CLIPBOARD, CLIPBOARD_CONNECT, DEFAULT_MAX_IMAGE_BYTES, ITEM, Item, ItemKind,
    Limits, NONE, NoneReason, PULL, RemoteClip,
};
use bridge_kde::{CancelToken, Packet, PayloadContext, TrustedLink};
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;

use super::{Arc, Core, Ctl, Phase};
use crate::dispatch::{ApiError, err};

/// Events kept for Kotlin; older ones are dropped (a phone that stopped listening loses nothing it
/// would want: the clipboard keeps only the latest anyway).
const MAX_EVENTS: usize = 8;
const MAX_INBOUND_IMAGES: usize = 2;
const MAX_WAIT: Duration = Duration::from_secs(55);

/// Packet types the phone declares when clipboard is on. It never advertises stock
/// `kdeconnect.clipboard` as *outgoing* behaviour it performs by itself, only as a capability for
/// explicit sends to peers that do not speak the item protocol.
pub(in crate::transport) fn incoming_types() -> Vec<String> {
    [CLIPBOARD, CLIPBOARD_CONNECT, PULL, ITEM]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}
pub(in crate::transport) fn outgoing_types() -> Vec<String> {
    [CLIPBOARD, ITEM, NONE]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

fn wall_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

struct Event {
    seq: u64,
    body: Value,
}

struct State {
    enabled: bool,
    max_image: u64,
    started_ms: i64,
    seq: u64,
    events: VecDeque<Event>,
    stopped: bool,
    cache: Option<PathBuf>,
    next_file: u64,
    inbound_images: usize,
}

pub(in crate::transport) struct Hub {
    st: Mutex<State>,
    cv: Condvar,
}

impl Default for Hub {
    fn default() -> Self {
        Self {
            st: Mutex::new(State {
                enabled: false,
                max_image: DEFAULT_MAX_IMAGE_BYTES,
                started_ms: wall_ms(),
                seq: 0,
                events: VecDeque::new(),
                stopped: false,
                cache: None,
                next_file: 1,
                inbound_images: 0,
            }),
            cv: Condvar::new(),
        }
    }
}

fn clear_cache(dir: &std::path::Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().starts_with("in-") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Called once when the transport starts.
    pub(in crate::transport) fn init(&self, cache: PathBuf, enabled: bool, max_image: u64) {
        let _ = std::fs::create_dir_all(&cache);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700));
        }
        clear_cache(&cache);
        let mut g = self.lock();
        g.cache = Some(cache);
        g.enabled = enabled;
        g.max_image = wire::clamp_image_bytes(max_image);
        g.started_ms = wall_ms();
    }

    pub(in crate::transport) fn configure(&self, enabled: Option<bool>, max_image: Option<u64>) {
        let mut g = self.lock();
        if let Some(e) = enabled {
            g.enabled = e;
            if !e {
                g.events.clear();
            }
        }
        if let Some(m) = max_image {
            g.max_image = wire::clamp_image_bytes(m);
        }
    }

    pub(in crate::transport) fn shutdown(&self) {
        let mut g = self.lock();
        g.stopped = true;
        g.events.clear();
        if let Some(dir) = &g.cache {
            clear_cache(dir);
        }
        drop(g);
        self.cv.notify_all();
    }

    fn push(&self, mut body: Value) {
        let mut g = self.lock();
        if !g.enabled || g.stopped {
            return;
        }
        g.seq += 1;
        let seq = g.seq;
        if let Some(map) = body.as_object_mut() {
            map.insert("seq".to_owned(), json!(seq));
        }
        if g.events.len() == MAX_EVENTS
            && let Some(old) = g.events.pop_front()
            && let Some(path) = old.body.get("path").and_then(Value::as_str)
        {
            let _ = std::fs::remove_file(path);
        }
        g.events.push_back(Event { seq, body });
        drop(g);
        self.cv.notify_all();
    }

    /// Block until an event newer than `since` exists, the transport stops, or `timeout` passes.
    /// Events up to `since` are released first.
    pub(in crate::transport) fn wait(&self, since: u64, timeout: Duration) -> Value {
        let timeout = timeout.min(MAX_WAIT);
        let deadline = std::time::Instant::now() + timeout;
        let mut g = self.lock();
        g.events.retain(|e| e.seq > since);
        while g.events.is_empty() && !g.stopped {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            g = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        json!({
            "events": g.events.iter().map(|e| e.body.clone()).collect::<Vec<_>>(),
            "last_seq": g.seq,
            "stopped": g.stopped,
            "enabled": g.enabled,
        })
    }

    fn limits(&self) -> (Limits, bool, i64) {
        let g = self.lock();
        (
            Limits {
                max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
                max_image_bytes: g.max_image,
            },
            g.enabled,
            g.started_ms,
        )
    }

    fn take_image_slot(&self) -> Option<(PathBuf, u64)> {
        let mut g = self.lock();
        if g.inbound_images >= MAX_INBOUND_IMAGES {
            return None;
        }
        let dir = g.cache.clone()?;
        g.inbound_images += 1;
        let n = g.next_file;
        g.next_file += 1;
        Some((dir.join(format!("in-{n}.img")), g.max_image))
    }

    fn release_image_slot(&self) {
        let mut g = self.lock();
        g.inbound_images = g.inbound_images.saturating_sub(1);
    }
}

// ---------------------------------------------------------------------------- outgoing

/// What Kotlin asked the connection to send.
pub(in crate::transport) enum Out {
    None {
        request_id: String,
        reason: NoneReason,
    },
    Text {
        kind: ItemKind,
        text: String,
        html: Option<String>,
        request_id: Option<String>,
        age_ms: u64,
    },
    Image {
        file: File,
        mime: &'static str,
        size: u64,
        request_id: Option<String>,
        age_ms: u64,
    },
}

impl Out {
    /// A copy for another connection; images cannot be duplicated (one descriptor, one reader).
    fn duplicate(&self) -> Option<Out> {
        match self {
            Out::Image { .. } => None,
            Out::None { request_id, reason } => Some(Out::None {
                request_id: request_id.clone(),
                reason: *reason,
            }),
            Out::Text {
                kind,
                text,
                html,
                request_id,
                age_ms,
            } => Some(Out::Text {
                kind: *kind,
                text: text.clone(),
                html: html.clone(),
                request_id: request_id.clone(),
                age_ms: *age_ms,
            }),
        }
    }
}

/// Fields of `transport_clipboard_send`, already type-checked by the caller.
pub(in crate::transport) struct SendArgs {
    pub connection_id: Option<u64>,
    pub request_id: Option<String>,
    pub none: Option<String>,
    pub kind: Option<String>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub image: Option<File>,
    pub age_ms: u64,
}

fn invalid() -> ApiError {
    err("invalid_field", "a field has the wrong type or value")
}

impl Core {
    pub(in crate::transport) fn clip(&self) -> &Hub {
        &self.clip
    }

    /// Validate and queue an outgoing clipboard item. Answers to a `pull` go to the connection that
    /// asked; an explicit send with no `connection_id` goes to every trusted connection.
    pub(in crate::transport) fn clipboard_send(&self, a: SendArgs) -> Result<Value, ApiError> {
        let (limits, enabled, _) = self.clip.limits();
        if !enabled {
            return Err(err(
                "clipboard_disabled",
                "clipboard sync is off in the app",
            ));
        }
        if let Some(r) = &a.request_id
            && (!wire::valid_request_id(r) || a.connection_id.is_none())
        {
            return Err(invalid());
        }
        let out: Out = if let Some(reason) = &a.none {
            let request_id = a.request_id.clone().ok_or_else(invalid)?;
            let reason = NoneReason::parse(reason).ok_or_else(invalid)?;
            Out::None { request_id, reason }
        } else {
            let kind = match a.kind.as_deref() {
                Some("text") => ItemKind::Text,
                Some("url") => ItemKind::Url,
                Some("html") => ItemKind::Html,
                Some("image") => ItemKind::Image,
                _ => return Err(invalid()),
            };
            if kind == ItemKind::Image {
                let mut file = a.image.ok_or_else(invalid)?;
                let size = file.metadata().map_err(|_| invalid())?.len();
                if size == 0 || size > wire::clamp_image_bytes(limits.max_image_bytes) {
                    return Err(err("too_large", "the image is larger than the limit"));
                }
                let mut magic = [0_u8; 8];
                file.seek(SeekFrom::Start(0)).map_err(|_| invalid())?;
                let n = file.read(&mut magic).map_err(|_| invalid())?;
                file.seek(SeekFrom::Start(0)).map_err(|_| invalid())?;
                // The bytes decide what this is, whatever Kotlin believes.
                let mime = magic
                    .get(..n)
                    .and_then(wire::sniff_image)
                    .ok_or_else(|| err("unsupported_type", "only PNG and JPEG images are sent"))?;
                Out::Image {
                    file,
                    mime,
                    size,
                    request_id: a.request_id.clone(),
                    age_ms: a.age_ms,
                }
            } else {
                let text = a.text.ok_or_else(invalid)?;
                // Same checks as a received item, so what leaves is what the desktop accepts.
                let probe = wire::text_item_body(
                    kind,
                    &text,
                    a.html.as_deref(),
                    a.request_id.as_deref(),
                    a.age_ms,
                );
                match wire::parse_item(&probe, &limits) {
                    Ok(_) => {}
                    Err(wire::WireError::TooLarge) => {
                        return Err(err("too_large", "the text is larger than the limit"));
                    }
                    Err(_) => return Err(invalid()),
                }
                Out::Text {
                    kind,
                    text,
                    html: a.html,
                    request_id: a.request_id.clone(),
                    age_ms: a.age_ms,
                }
            }
        };
        let targets: Vec<(u64, tokio::sync::mpsc::Sender<Ctl>)> = {
            let st = self.lock();
            match a.connection_id {
                Some(id) => {
                    let c = st
                        .conns
                        .get(&id)
                        .ok_or_else(|| err("unknown_connection", "no such connection"))?;
                    if c.phase != Phase::Trusted {
                        return Err(err("not_trusted", "the peer is not trusted"));
                    }
                    vec![(id, c.ctl.clone())]
                }
                None => st
                    .conns
                    .values()
                    .filter(|c| c.phase == Phase::Trusted)
                    .map(|c| (c.id, c.ctl.clone()))
                    .collect(),
            }
        };
        if targets.is_empty() {
            return Err(err("not_connected", "no computer is connected"));
        }
        // A descriptor serves one connection, so an image goes to the first target only.
        let mut queued = 0;
        let mut out = Some(out);
        let last = targets.len().saturating_sub(1);
        for (i, (_, ctl)) in targets.into_iter().enumerate() {
            let this = if i == last {
                out.take()
            } else {
                out.as_ref().and_then(Out::duplicate)
            };
            if let Some(o) = this
                && ctl.try_send(Ctl::Clip(Box::new(o))).is_ok()
            {
                queued += 1;
            }
        }
        if queued == 0 {
            return Err(err("busy", "the connection is busy"));
        }
        Ok(json!({ "queued": queued }))
    }

    /// A clipboard-protocol packet from a trusted peer. Returns true if the packet is a clipboard
    /// packet (acted on or deliberately ignored), false if it is none of ours.
    pub(in crate::transport) fn incoming_clipboard(
        self: &Arc<Self>,
        connection_id: u64,
        ctx: PayloadContext,
        p: &Packet,
    ) -> bool {
        let known = matches!(p.kind.as_str(), CLIPBOARD | CLIPBOARD_CONNECT | PULL | ITEM);
        if !known {
            return false;
        }
        let (limits, enabled, started_ms) = self.clip.limits();
        let device = self
            .lock()
            .conns
            .get(&connection_id)
            .map(|c| c.peer_id.to_string());
        let Some(device) = device else {
            return true;
        };
        let has_payload = p.payload_size.is_some() || p.payload_transfer_info.is_some();
        if has_payload && p.kind != ITEM {
            return false;
        }
        if p.kind == PULL {
            let Ok((request_id, max_age_ms)) = wire::parse_pull(&p.body) else {
                return true;
            };
            if enabled {
                self.clip.push(json!({
                    "kind": "pull",
                    "connection_id": connection_id,
                    "device_id": device,
                    "request_id": request_id,
                    "max_age_ms": max_age_ms,
                }));
            } else if let Some(ctl) = self.ctl_of(connection_id) {
                let _ = ctl.try_send(Ctl::Clip(Box::new(Out::None {
                    request_id,
                    reason: NoneReason::Disabled,
                })));
            }
            return true;
        }
        if !enabled {
            return true;
        }
        match p.kind.as_str() {
            CLIPBOARD | CLIPBOARD_CONNECT => {
                match wire::parse(&p.kind, &p.body, limits.max_text_bytes) {
                    Ok(RemoteClip::Update { content }) => {
                        self.deliver_text(&device, wire::text_kind(&content), content, None)
                    }
                    Ok(RemoteClip::Connect {
                        content,
                        timestamp_ms,
                    }) if timestamp_ms != 0 && timestamp_ms >= started_ms => {
                        self.deliver_text(&device, wire::text_kind(&content), content, None);
                    }
                    _ => {}
                }
            }
            ITEM => {
                let Ok(parsed) = wire::parse_item(&p.body, &limits) else {
                    return true;
                };
                match parsed.item {
                    Item::Text { kind, text, html } => self.deliver_text(&device, kind, text, html),
                    Item::Image { mime, size } => {
                        let port = p
                            .payload_transfer_info
                            .as_ref()
                            .and_then(|m| m.get("port"))
                            .and_then(Value::as_u64)
                            .and_then(|v| u16::try_from(v).ok());
                        if let Some(port) = port {
                            self.receive_image(&device, ctx, port, mime, size);
                        }
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn deliver_text(&self, device: &str, kind: ItemKind, text: String, html: Option<String>) {
        self.clip.push(json!({
            "kind": "item",
            "item": kind.as_str(),
            "device_id": device,
            "text": text,
            "html": html,
        }));
    }

    fn ctl_of(&self, id: u64) -> Option<tokio::sync::mpsc::Sender<Ctl>> {
        self.lock().conns.get(&id).map(|c| c.ctl.clone())
    }

    fn receive_image(
        self: &Arc<Self>,
        device: &str,
        ctx: PayloadContext,
        port: u16,
        mime: &'static str,
        size: u64,
    ) {
        let Some((path, max)) = self.clip.take_image_slot() else {
            return;
        };
        let core = self.clone();
        let device = device.to_owned();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.clip.release_image_slot();
            return;
        };
        handle.spawn(async move {
            let result = async {
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600);
                }
                let file = opts.open(&path).map_err(|_| ())?;
                let mut file = tokio::fs::File::from_std(file);
                let cancel = CancelToken::new();
                let mut progress = |_: u64| {};
                tokio::time::timeout(
                    Duration::from_secs(120),
                    ctx.receive(port, Some(size), max, &mut file, &mut progress, &cancel),
                )
                .await
                .map_err(|_| ())?
                .map_err(|_| ())?;
                file.flush().await.map_err(|_| ())?;
                drop(file);
                // Only genuine PNG/JPEG data of the announced type is handed to Kotlin.
                let mut head = [0_u8; 8];
                let n = File::open(&path)
                    .and_then(|mut f| f.read(&mut head))
                    .map_err(|_| ())?;
                if head.get(..n).and_then(wire::sniff_image) != Some(mime) {
                    return Err(());
                }
                Ok(())
            }
            .await;
            core.clip.release_image_slot();
            match result {
                Ok(()) => core.clip.push(json!({
                    "kind": "item",
                    "item": "image",
                    "device_id": device,
                    "path": path.to_string_lossy(),
                    "mime": mime,
                    "size": size,
                })),
                Err(()) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        });
    }
}

/// Send one queued item on `link`. False means the link is broken and should close.
pub(super) async fn send(link: &mut TrustedLink, out: Out) -> bool {
    let accepts = |link: &TrustedLink, kind: &str| {
        link.info()
            .peer
            .incoming_capabilities
            .iter()
            .any(|c| c == kind)
    };
    let sent = match out {
        Out::None { request_id, reason } => {
            if !accepts(link, NONE) {
                return true;
            }
            link.send(&Packet::new(NONE, wire::none_body(&request_id, reason)))
                .await
        }
        Out::Text {
            kind,
            text,
            html,
            request_id,
            age_ms,
        } => {
            if accepts(link, ITEM) {
                let body = wire::text_item_body(
                    kind,
                    &text,
                    html.as_deref(),
                    request_id.as_deref(),
                    age_ms,
                );
                link.send(&Packet::new(ITEM, body)).await
            } else if accepts(link, CLIPBOARD) && request_id.is_none() {
                // A stock peer: plain text only, and only for an explicit send.
                link.send(&Packet::new(CLIPBOARD, wire::update_body(&text)))
                    .await
            } else {
                return true;
            }
        }
        Out::Image {
            file,
            mime,
            size,
            request_id,
            age_ms,
        } => {
            if !accepts(link, ITEM) {
                return true;
            }
            let Ok(listener) = link.payload_context().listen().await else {
                return true; // no free payload port: skip, the link is fine
            };
            let mut packet = Packet::new(
                ITEM,
                wire::image_item_body(mime, size, request_id.as_deref(), age_ms),
            );
            packet.payload_size = i64::try_from(size).ok();
            let mut info = Map::new();
            info.insert("port".into(), Value::from(listener.port()));
            packet.payload_transfer_info = Some(info);
            let r = link.send_with_payload(&packet).await;
            if r.is_ok() {
                tokio::spawn(async move {
                    let cancel = CancelToken::new();
                    let mut progress = |_: u64| {};
                    let _ = listener
                        .send(
                            tokio::fs::File::from_std(file),
                            size,
                            &mut progress,
                            &cancel,
                        )
                        .await;
                });
            }
            r
        }
    };
    sent.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_returns_events_and_releases_acknowledged_ones() {
        let hub = Hub::default();
        hub.configure(Some(true), None);
        hub.push(json!({"kind": "item", "text": "a"}));
        hub.push(json!({"kind": "item", "text": "b"}));
        let r = hub.wait(0, Duration::from_millis(10));
        assert_eq!(r["events"].as_array().unwrap().len(), 2);
        assert_eq!(r["last_seq"], 2);
        // Acknowledging seq 1 releases it; 2 remains until acknowledged.
        let r = hub.wait(1, Duration::from_millis(10));
        assert_eq!(r["events"].as_array().unwrap().len(), 1);
        let r = hub.wait(2, Duration::from_millis(10));
        assert!(r["events"].as_array().unwrap().is_empty());
    }

    #[test]
    fn wait_wakes_on_push_and_on_shutdown_without_polling() {
        let hub = std::sync::Arc::new(Hub::default());
        hub.configure(Some(true), None);
        let h = hub.clone();
        let t = std::thread::spawn(move || h.wait(0, Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        hub.push(json!({"kind": "pull"}));
        let r = t.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(r["events"].as_array().unwrap().len(), 1);
        let h = hub.clone();
        let t = std::thread::spawn(move || h.wait(1, Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(100));
        hub.shutdown();
        let r = t.join().unwrap();
        assert_eq!(r["stopped"], true);
    }

    #[test]
    fn disabled_hub_drops_everything_and_is_bounded() {
        let hub = Hub::default();
        hub.push(json!({"kind": "item"}));
        assert!(
            hub.wait(0, Duration::from_millis(5))["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        hub.configure(Some(true), None);
        for _ in 0..(MAX_EVENTS + 5) {
            hub.push(json!({"kind": "item"}));
        }
        assert_eq!(
            hub.wait(0, Duration::from_millis(5))["events"]
                .as_array()
                .unwrap()
                .len(),
            MAX_EVENTS
        );
        hub.configure(Some(false), None);
        assert!(
            hub.wait(0, Duration::from_millis(5))["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn image_cache_is_private_and_wiped_on_init_and_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("clip");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("in-9.img"), b"stale").unwrap();
        std::fs::write(cache.join("keep.txt"), b"other").unwrap();
        let hub = Hub::default();
        hub.init(cache.clone(), true, 1 << 20);
        assert!(!cache.join("in-9.img").exists());
        assert!(
            cache.join("keep.txt").exists(),
            "only our own spool files are touched"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        let (p, _) = hub.take_image_slot().unwrap();
        std::fs::write(&p, b"x").unwrap();
        hub.shutdown();
        assert!(!p.exists());
        // At most two images are spooled at once.
        let hub = Hub::default();
        hub.init(cache, true, 1 << 20);
        assert!(hub.take_image_slot().is_some());
        assert!(hub.take_image_slot().is_some());
        assert!(hub.take_image_slot().is_none());
        hub.release_image_slot();
        assert!(hub.take_image_slot().is_some());
    }
}
