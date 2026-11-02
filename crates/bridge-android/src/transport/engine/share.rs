// SPDX-License-Identifier: GPL-3.0-or-later
//! KDE Connect `share` plugin on the phone: transfer book, JSON views and the tasks that move bytes.
//!
//! Files never pass through JSON or memory: Kotlin hands over duplicated file descriptors (a content
//! provider stream for sending, a pending MediaStore row for receiving) and the bytes are copied in
//! 64 KiB chunks through `bridge-kde`'s pinned payload sockets. Nothing is written until the user (or
//! their per-device auto-accept setting, enforced in Kotlin) calls `share_accept`.
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bridge_kde::share::{self, FileOffer, ShareContent};
use bridge_kde::{CancelToken, Packet, PayloadContext, PayloadError};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::time::Instant;

use super::{Arc, Core, Ctl, Phase, mpsc};
use crate::dispatch::{ApiError, err};

pub(in crate::transport) const MAX_TRANSFERS: usize = 32;
pub(in crate::transport) const MAX_ACTIVE: usize = 3;
const MAX_OFFERED: usize = 8;
const OFFER_TTL: Duration = Duration::from_secs(300);
const KEEP_FINISHED: Duration = Duration::from_secs(600);
pub(in crate::transport) const MAX_FILE_BYTES: u64 = share::DEFAULT_MAX_FILE_BYTES;
/// Received text is shown in status snapshots, so it is kept small.
const MAX_RECEIVED_TEXT: usize = 16 * 1024;
const MAX_DISPLAY_NAME_CHARS: usize = 255;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Dir {
    Incoming,
    Outgoing,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    File,
    Text,
    Url,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Offered,
    Waiting,
    Active,
    Done,
    Failed,
    Cancelled,
    Declined,
}

impl St {
    fn is_live(self) -> bool {
        matches!(self, St::Offered | St::Waiting | St::Active)
    }
}

struct Transfer {
    id: u64,
    connection_id: u64,
    device_id: String,
    peer_name: String,
    dir: Dir,
    kind: Kind,
    name: String,
    size: Option<u64>,
    bytes: Arc<AtomicU64>,
    state: St,
    code: Option<&'static str>,
    text: Option<String>,
    offer: Option<(FileOffer, PayloadContext)>,
    cancel: CancelToken,
    created: Instant,
    finished: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Book {
    items: Vec<Transfer>,
}

impl Book {
    fn active(&self) -> usize {
        self.items
            .iter()
            .filter(|t| matches!(t.state, St::Waiting | St::Active))
            .count()
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Transfer> {
        self.items.iter_mut().find(|t| t.id == id)
    }

    /// Expire stale entries and make room for one more; false if nothing can be evicted.
    fn make_room(&mut self) -> bool {
        let now = Instant::now();
        for t in &mut self.items {
            if t.state == St::Offered && now.duration_since(t.created) > OFFER_TTL {
                t.state = St::Failed;
                t.code = Some("expired");
                t.offer = None;
                t.finished = Some(now);
            }
        }
        self.items.retain(|t| {
            t.finished
                .is_none_or(|f| now.duration_since(f) <= KEEP_FINISHED)
        });
        while self.items.len() >= MAX_TRANSFERS {
            match self.items.iter().position(|t| !t.state.is_live()) {
                Some(i) => {
                    self.items.remove(i);
                }
                None => return false,
            }
        }
        true
    }

    /// Record how a transfer task ended. A transfer that is no longer live keeps its state.
    fn finish(&mut self, id: u64, result: Result<(), &'static str>) {
        if let Some(t) = self.get_mut(id)
            && t.state.is_live()
        {
            match result {
                Ok(()) => {
                    if let Some(n) = t.size {
                        t.bytes.store(n, Ordering::Relaxed);
                    }
                    t.state = St::Done;
                }
                Err(code) => {
                    t.state = if code == "cancelled" {
                        St::Cancelled
                    } else {
                        St::Failed
                    };
                    t.code = Some(code);
                }
            }
            t.finished = Some(Instant::now());
        }
    }

    pub(super) fn cancel_all(&mut self) {
        for t in &self.items {
            t.cancel.cancel();
        }
        self.items.clear();
    }
}

fn code_of(e: &PayloadError) -> &'static str {
    e.code()
}

pub(in crate::transport) struct SendJob {
    pub transfer_id: u64,
    pub file: File,
    pub name: String,
    pub size: u64,
    pub last_modified_ms: Option<i64>,
}

pub(in crate::transport) struct TextJob {
    pub kind: &'static str,
    pub text: String,
}

fn unknown_transfer() -> ApiError {
    err("unknown_transfer", "no such transfer")
}

impl Core {
    fn trusted_ctl(
        &self,
        connection_id: u64,
    ) -> Result<(mpsc::Sender<Ctl>, String, String), ApiError> {
        let st = self.lock();
        if !st.open {
            return Err(err("not_running", "the transport is not running"));
        }
        let c = st
            .conns
            .get(&connection_id)
            .ok_or_else(|| err("unknown_connection", "no such connection"))?;
        if c.phase != Phase::Trusted {
            return Err(err("not_trusted", "the peer is not trusted"));
        }
        Ok((c.ctl.clone(), c.peer_id.to_string(), c.peer_name.clone()))
    }

    fn shares(&self) -> std::sync::MutexGuard<'_, Book> {
        self.shares
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(in crate::transport) fn share_send_file(
        &self,
        connection_id: u64,
        file: File,
        name: &str,
        size: u64,
        last_modified_ms: Option<i64>,
    ) -> Result<Value, ApiError> {
        if size == 0 {
            return Err(err("empty", "empty files cannot be sent"));
        }
        if size > MAX_FILE_BYTES {
            return Err(err(
                "too_large",
                "the file is larger than the transfer limit",
            ));
        }
        let (ctl, device_id, peer_name) = self.trusted_ctl(connection_id)?;
        let name = share::sanitize_filename(name);
        let id = self.requests.fetch_add(1, Ordering::Relaxed);
        {
            let mut book = self.shares();
            if !book.make_room() || book.active() >= MAX_ACTIVE {
                return Err(err("busy", "too many transfers"));
            }
            book.items.push(Transfer {
                id,
                connection_id,
                device_id,
                peer_name,
                dir: Dir::Outgoing,
                kind: Kind::File,
                name: name.clone(),
                size: Some(size),
                bytes: Arc::new(AtomicU64::new(0)),
                state: St::Waiting,
                code: None,
                text: None,
                offer: None,
                cancel: CancelToken::new(),
                created: Instant::now(),
                finished: None,
            });
        }
        let job = SendJob {
            transfer_id: id,
            file,
            name,
            size,
            last_modified_ms,
        };
        if ctl.try_send(Ctl::SendFile(Box::new(job))).is_err() {
            self.shares().items.retain(|t| t.id != id);
            return Err(err("busy", "the connection is busy"));
        }
        Ok(json!({ "transfer_id": id }))
    }

    pub(in crate::transport) fn share_send_text(
        &self,
        connection_id: u64,
        kind: &str,
        text: String,
    ) -> Result<Value, ApiError> {
        let (kind, max) = match kind {
            "text" => ("text", share::MAX_TEXT_BYTES),
            "url" => ("url", share::MAX_URL_BYTES),
            _ => return Err(err("invalid_field", "a field has the wrong type or value")),
        };
        if text.is_empty() || text.len() > max {
            return Err(err("invalid_field", "a field has the wrong type or value"));
        }
        let (ctl, _, _) = self.trusted_ctl(connection_id)?;
        ctl.try_send(Ctl::SendText(TextJob { kind, text }))
            .map_err(|_| err("busy", "the connection is busy"))?;
        Ok(json!({ "queued": true }))
    }

    pub(in crate::transport) fn share_accept(
        self: &Arc<Self>,
        handle: &Handle,
        transfer_id: u64,
        file: File,
    ) -> Result<Value, ApiError> {
        let (offer, ctx, bytes, cancel) = {
            let mut book = self.shares();
            book.make_room();
            let active = book.active();
            let t = book.get_mut(transfer_id).ok_or_else(unknown_transfer)?;
            if t.dir != Dir::Incoming || t.state != St::Offered || t.kind != Kind::File {
                return Err(err(
                    "wrong_state",
                    "the transfer is not waiting for an answer",
                ));
            }
            if active >= MAX_ACTIVE {
                return Err(err("busy", "too many transfers"));
            }
            let Some((offer, ctx)) = t.offer.take() else {
                return Err(err(
                    "wrong_state",
                    "the transfer is not waiting for an answer",
                ));
            };
            t.state = St::Active;
            (offer, ctx, t.bytes.clone(), t.cancel.clone())
        };
        let core = self.clone();
        handle.spawn(async move {
            let mut out = tokio::fs::File::from_std(file);
            let size = offer.size;
            let counter = bytes.clone();
            let result = ctx
                .receive(
                    offer.port,
                    size,
                    MAX_FILE_BYTES,
                    &mut out,
                    &mut |n| counter.store(n, Ordering::Relaxed),
                    &cancel,
                )
                .await;
            drop(out);
            core.finish_transfer(transfer_id, result.map(|_| ()).map_err(|e| code_of(&e)));
        });
        Ok(json!({ "accepted": true }))
    }

    pub(in crate::transport) fn share_decline(&self, transfer_id: u64) -> Result<Value, ApiError> {
        let mut book = self.shares();
        let t = book.get_mut(transfer_id).ok_or_else(unknown_transfer)?;
        if t.dir != Dir::Incoming || t.state != St::Offered {
            return Err(err(
                "wrong_state",
                "the transfer is not waiting for an answer",
            ));
        }
        t.state = St::Declined;
        t.offer = None;
        t.finished = Some(Instant::now());
        Ok(json!({ "declined": true }))
    }

    pub(in crate::transport) fn share_cancel(&self, transfer_id: u64) -> Result<Value, ApiError> {
        let mut book = self.shares();
        let t = book.get_mut(transfer_id).ok_or_else(unknown_transfer)?;
        if !t.state.is_live() {
            return Err(err("wrong_state", "the transfer already finished"));
        }
        t.cancel.cancel();
        t.state = St::Cancelled;
        t.offer = None;
        t.finished = Some(Instant::now());
        Ok(json!({ "cancelled": true }))
    }

    pub(in crate::transport) fn share_dismiss(&self, transfer_id: u64) -> Result<Value, ApiError> {
        let mut book = self.shares();
        let i = book
            .items
            .iter()
            .position(|t| t.id == transfer_id)
            .ok_or_else(unknown_transfer)?;
        if book.items.get(i).is_some_and(|t| t.state.is_live()) {
            return Err(err("wrong_state", "the transfer is still in progress"));
        }
        book.items.remove(i);
        Ok(json!({ "dismissed": true }))
    }

    /// Record how a transfer task ended. A cancelled entry keeps its state.
    pub(super) fn finish_transfer(&self, id: u64, result: Result<(), &'static str>) {
        self.shares().finish(id, result);
    }

    /// The connection ended with `ctl` still queued: it will never run, so a queued file must not
    /// stay `Waiting` (it would hold a transfer slot and keep the link wanted forever).
    pub(super) fn drop_queued(&self, ctl: Ctl) {
        if let Ctl::SendFile(job) = ctl {
            self.finish_transfer(job.transfer_id, Err("link_lost"));
        }
    }

    /// A share packet from a trusted peer. Returns false if it was not acted on.
    pub(super) fn incoming_share(
        &self,
        connection_id: u64,
        ctx: PayloadContext,
        packet: &Packet,
    ) -> bool {
        let content = match share::parse_share(packet, MAX_FILE_BYTES) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let (kind, name, size, text, offer) = match content {
            ShareContent::Ignored => return true,
            ShareContent::File(o) => {
                let name = o
                    .name
                    .chars()
                    .take(MAX_DISPLAY_NAME_CHARS)
                    .collect::<String>();
                (Kind::File, name, o.size, None, Some(o))
            }
            ShareContent::Text(t) if t.len() <= MAX_RECEIVED_TEXT => {
                (Kind::Text, String::new(), None, Some(t), None)
            }
            ShareContent::Url(u) if u.len() <= MAX_RECEIVED_TEXT => {
                (Kind::Url, String::new(), None, Some(u), None)
            }
            _ => return false,
        };
        let (device_id, peer_name) = {
            let st = self.lock();
            match st.conns.get(&connection_id) {
                Some(c) if c.phase == Phase::Trusted => {
                    (c.peer_id.to_string(), c.peer_name.clone())
                }
                _ => return false,
            }
        };
        let id = self.requests.fetch_add(1, Ordering::Relaxed);
        let mut book = self.shares();
        if !book.make_room() {
            return false;
        }
        let pending = book.items.iter().filter(|t| t.state == St::Offered).count();
        if kind == Kind::File && pending >= MAX_OFFERED {
            return false;
        }
        let is_text = text.is_some();
        book.items.push(Transfer {
            id,
            connection_id,
            device_id,
            peer_name,
            dir: Dir::Incoming,
            kind,
            name,
            size,
            bytes: Arc::new(AtomicU64::new(0)),
            state: if is_text { St::Done } else { St::Offered },
            code: None,
            text,
            offer: offer.map(|o| (o, ctx)),
            cancel: CancelToken::new(),
            created: Instant::now(),
            finished: is_text.then(Instant::now),
        });
        true
    }

    /// Stop everything belonging to a device that was just unpaired.
    pub(super) fn cancel_transfers_of(&self, device_id: &str) {
        let mut book = self.shares();
        let now = Instant::now();
        for t in book
            .items
            .iter_mut()
            .filter(|t| t.device_id == device_id && t.state.is_live())
        {
            t.cancel.cancel();
            t.state = St::Cancelled;
            t.offer = None;
            t.finished = Some(now);
        }
    }

    pub(super) fn transfers_json(&self) -> Vec<Value> {
        let mut book = self.shares();
        book.make_room();
        book.items
            .iter()
            .map(|t| {
                let bytes = t.bytes.load(Ordering::Relaxed);
                let state = match t.state {
                    St::Offered => "offered",
                    St::Waiting if bytes > 0 => "active",
                    St::Waiting => "waiting",
                    St::Active => "active",
                    St::Done => "done",
                    St::Failed => "failed",
                    St::Cancelled => "cancelled",
                    St::Declined => "declined",
                };
                let mut o = json!({
                    "transfer_id": t.id,
                    "connection_id": t.connection_id,
                    "device_id": t.device_id,
                    "peer_name": t.peer_name,
                    "direction": if t.dir == Dir::Incoming { "incoming" } else { "outgoing" },
                    "kind": match t.kind { Kind::File => "file", Kind::Text => "text", Kind::Url => "url" },
                    "size": t.size,
                    "bytes": bytes,
                    "state": state,
                });
                if let Some(map) = o.as_object_mut() {
                    if t.kind == Kind::File {
                        map.insert("name".into(), t.name.clone().into());
                    }
                    if let Some(c) = t.code {
                        map.insert("code".into(), c.into());
                    }
                    if let Some(text) = &t.text {
                        map.insert("text".into(), text.clone().into());
                    }
                }
                o
            })
            .collect()
    }
}

/// Run one outgoing file on the trusted link's task: bind, announce, then serve in the background.
/// Returns false if the link itself failed (the caller closes it).
pub(super) async fn start_send(
    core: &Arc<Core>,
    link: &mut bridge_kde::TrustedLink,
    job: SendJob,
) -> bool {
    let ctx = link.payload_context();
    let (bytes, cancel) = {
        let mut book = core.shares();
        match book.get_mut(job.transfer_id) {
            Some(t) if t.state.is_live() => (t.bytes.clone(), t.cancel.clone()),
            _ => return true, // cancelled before it started
        }
    };
    let listener = match ctx.listen().await {
        Ok(l) => l,
        Err(e) => {
            core.finish_transfer(job.transfer_id, Err(code_of(&e)));
            return true;
        }
    };
    let packet = share::file_packet(&job.name, job.size, job.last_modified_ms, listener.port());
    if link.send_with_payload(&packet).await.is_err() {
        core.finish_transfer(job.transfer_id, Err("io_error"));
        return false;
    }
    let core = core.clone();
    tokio::spawn(async move {
        let file = tokio::fs::File::from_std(job.file);
        let result = listener
            .send(
                file,
                job.size,
                &mut |n| bytes.store(n, Ordering::Relaxed),
                &cancel,
            )
            .await;
        core.finish_transfer(job.transfer_id, result.map(|_| ()).map_err(|e| code_of(&e)));
    });
    true
}

pub(super) async fn send_text(link: &mut bridge_kde::TrustedLink, job: TextJob) -> bool {
    let packet = if job.kind == "url" {
        share::url_packet(&job.text)
    } else {
        share::text_packet(&job.text)
    };
    link.send(&packet).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting(id: u64) -> Transfer {
        Transfer {
            id,
            connection_id: 1,
            device_id: "d".into(),
            peer_name: "pc".into(),
            dir: Dir::Outgoing,
            kind: Kind::File,
            name: "a.bin".into(),
            size: Some(10),
            bytes: Arc::new(AtomicU64::new(0)),
            state: St::Waiting,
            code: None,
            text: None,
            offer: None,
            cancel: CancelToken::new(),
            created: Instant::now(),
            finished: None,
        }
    }

    #[test]
    fn a_queued_file_failed_with_its_connection_frees_its_slot() {
        let mut book = Book::default();
        for id in 1..=MAX_ACTIVE as u64 {
            book.items.push(waiting(id));
        }
        assert_eq!(book.active(), MAX_ACTIVE);
        book.finish(1, Err("link_lost"));
        assert_eq!(book.active(), MAX_ACTIVE - 1);
        let t = book.get_mut(1).unwrap();
        assert_eq!(t.state, St::Failed);
        assert_eq!(t.code, Some("link_lost"));
        // A transfer that already ended is left alone.
        book.finish(1, Ok(()));
        assert_eq!(book.get_mut(1).unwrap().state, St::Failed);
    }
}
