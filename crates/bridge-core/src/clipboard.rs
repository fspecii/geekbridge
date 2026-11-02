// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard sync policy and reducer for the custom GeekBridge protocol.
//!
//! This module is pure: it never reads or writes an OS clipboard, never reads a clock, never does
//! I/O, and keeps no clipboard history. Platform adapters feed it observations and act on the
//! decisions it returns. Callers pass `now_ms` from a monotonic clock (not wall time), so clock
//! changes cannot extend an expiry. Text is held in memory only while a remote item is staged for
//! confirmation; other state is limited to short-lived fingerprints (see [`ClipboardSync`]).
//!
//! Stock KDE Connect has no item ids, sensitivity flag, TTL, or size limit. See
//! [`kde_connect_should_apply`] and `docs/clipboard-sync.md` for what is and is not preserved
//! when talking to a stock peer.
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fmt,
    hash::{BuildHasher, RandomState},
};

pub const CLIPBOARD_PROTOCOL_VERSION: u32 = 1;
/// Default text limit for new policies.
pub const DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024;
/// No policy may exceed this, whatever the caller asks for.
pub const HARD_MAX_TEXT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_TTL_MS: u64 = 60_000;
pub const HARD_MAX_TTL_MS: u64 = 60 * 60 * 1000;
/// Window in which a local change event matching text we just applied is treated as our own echo.
pub const DEFAULT_ECHO_WINDOW_MS: u64 = 5_000;
pub const MAX_ID_BYTES: usize = 64;
const SEEN_IDS_CAP: usize = 64;
const APPLIED_CAP: usize = 8;

// ---------------------------------------------------------------- capabilities

/// How the platform lets this process read the clipboard. Ordered from least to most permissive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadAccess {
    /// Reading is not possible at all.
    Unsupported,
    /// Only as part of an explicit user gesture (button, tile, paste target).
    UserGesture,
    /// Whenever one of our windows has focus (Android 10+ without special privilege, Wayland core).
    Foreground,
    /// At any time, from a background process (X11, Windows, macOS polling, privileged Wayland).
    Background,
}

/// How clipboard changes can be noticed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveMode {
    None,
    Polling,
    Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardCapabilities {
    pub read: ReadAccess,
    pub write: bool,
    pub observe: ObserveMode,
    /// The adapter can report a sensitive/concealed hint when one exists on the platform.
    pub reports_sensitivity: bool,
    /// The adapter can mark text it writes as sensitive (e.g. exclude it from clipboard history).
    pub can_mark_written_sensitive: bool,
}
impl ClipboardCapabilities {
    pub const fn none() -> Self {
        Self {
            read: ReadAccess::Unsupported,
            write: false,
            observe: ObserveMode::None,
            reports_sensitivity: false,
            can_mark_written_sensitive: false,
        }
    }
    /// Manual send needs some way to read; automatic send needs unattended reads and change detection.
    pub fn supports_manual_send(&self) -> bool {
        self.read >= ReadAccess::UserGesture
    }
    pub fn supports_automatic_send(&self) -> bool {
        self.read == ReadAccess::Background && self.observe != ObserveMode::None
    }
    /// Sending when the app gains focus (Android 10+): reads are allowed whenever one of our windows
    /// has focus, but nothing can be noticed in the background.
    pub fn supports_focus_send(&self) -> bool {
        self.read >= ReadAccess::Foreground
    }
}

// ---------------------------------------------------------------- policy

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    Off,
    /// Only when the user explicitly asks to send.
    Manual,
    /// Every observed change. Requires [`ClipboardCapabilities::supports_automatic_send`].
    Automatic,
    /// Whenever the app gains focus (and on explicit user sends): the best an Android app can do
    /// without background reads. Applies the same echo/unchanged suppression as `Automatic`.
    /// Requires [`ClipboardCapabilities::supports_focus_send`].
    OnFocus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveMode {
    Off,
    /// Stage one item; the local clipboard changes only after [`ClipboardSync::confirm_pending`].
    Confirm,
    /// Write accepted items straight to the local clipboard. Requires `write` capability.
    Apply,
}

/// Per-peer policy. Everything is off by default: enabling sync is an explicit opt-in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardPolicy {
    pub send: SendMode,
    pub receive: ReceiveMode,
    pub max_text_bytes: usize,
    /// TTL placed on outgoing items.
    pub ttl_ms: u64,
    /// Upper bound on the TTL we honour on incoming items (also bounds staging time).
    pub max_accepted_ttl_ms: u64,
    pub echo_window_ms: u64,
    /// Let an explicit user send go out even when content is flagged sensitive.
    pub allow_sensitive_manual_send: bool,
    /// Accept incoming items the peer flagged sensitive. They are written marked sensitive when possible.
    pub accept_sensitive_incoming: bool,
    /// In automatic mode, send text whose sensitivity the adapter could not determine.
    pub automatic_send_unknown_sensitivity: bool,
}
impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            send: SendMode::Off,
            receive: ReceiveMode::Off,
            max_text_bytes: DEFAULT_MAX_TEXT_BYTES,
            ttl_ms: DEFAULT_TTL_MS,
            max_accepted_ttl_ms: 5 * 60 * 1000,
            echo_window_ms: DEFAULT_ECHO_WINDOW_MS,
            allow_sensitive_manual_send: false,
            accept_sensitive_incoming: false,
            automatic_send_unknown_sensitivity: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    MaxTextBytes,
    Ttl,
    AutomaticSendUnsupported,
    ManualSendUnsupported,
    FocusSendUnsupported,
    ReceiveUnsupported,
}

impl ClipboardPolicy {
    pub fn validate(&self, caps: &ClipboardCapabilities) -> Result<(), PolicyError> {
        if self.max_text_bytes == 0 || self.max_text_bytes > HARD_MAX_TEXT_BYTES {
            return Err(PolicyError::MaxTextBytes);
        }
        if self.ttl_ms == 0
            || self.ttl_ms > HARD_MAX_TTL_MS
            || self.max_accepted_ttl_ms == 0
            || self.max_accepted_ttl_ms > HARD_MAX_TTL_MS
        {
            return Err(PolicyError::Ttl);
        }
        match self.send {
            SendMode::Automatic if !caps.supports_automatic_send() => {
                return Err(PolicyError::AutomaticSendUnsupported);
            }
            SendMode::Manual if !caps.supports_manual_send() => {
                return Err(PolicyError::ManualSendUnsupported);
            }
            SendMode::OnFocus if !caps.supports_focus_send() => {
                return Err(PolicyError::FocusSendUnsupported);
            }
            _ => {}
        }
        if self.receive != ReceiveMode::Off && !caps.write {
            return Err(PolicyError::ReceiveUnsupported);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- wire message

/// Only UTF-8 plain text exists today. An unknown value fails deserialization, so a newer peer
/// sending images or files is rejected rather than misinterpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardFormat {
    #[serde(rename = "text/plain;charset=utf-8")]
    TextUtf8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Manual,
    Automatic,
}

/// Custom-protocol clipboard item. `deny_unknown_fields` keeps the schema strict.
///
/// - `id`: unique per item, chosen by the sender (e.g. 128 random bits as hex); used for dedup/replay.
/// - `origin_device`: device that first captured the text. Must equal the authenticated session peer.
/// - `ttl_ms`: a duration, not an absolute time, so it is independent of the two devices' clocks.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClipboardMessage {
    pub version: u32,
    pub id: String,
    pub origin_device: String,
    pub format: ClipboardFormat,
    pub text: String,
    #[serde(default)]
    pub sensitive: bool,
    pub ttl_ms: u64,
    pub trigger: Trigger,
}
impl fmt::Debug for ClipboardMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardMessage")
            .field("id", &self.id)
            .field("origin_device", &self.origin_device)
            .field(
                "text",
                &format_args!("<redacted {} bytes>", self.text.len()),
            )
            .field("sensitive", &self.sensitive)
            .field("ttl_ms", &self.ttl_ms)
            .field("trigger", &self.trigger)
            .finish()
    }
}

/// Worst case JSON expansion is 6 bytes per text byte (`\u00XX`) plus the envelope.
pub fn max_wire_bytes(max_text_bytes: usize) -> usize {
    max_text_bytes
        .min(HARD_MAX_TEXT_BYTES)
        .saturating_mul(6)
        .saturating_add(1024)
}

impl ClipboardMessage {
    /// Parse with a size check before any allocation by the JSON parser.
    pub fn from_json_slice(bytes: &[u8], max_text_bytes: usize) -> Result<Self, MessageError> {
        if bytes.len() > max_wire_bytes(max_text_bytes) {
            return Err(MessageError::TooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| MessageError::Malformed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageError {
    TooLarge,
    Malformed,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_ID_BYTES
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ---------------------------------------------------------------- adapter input/output

/// How the adapter obtained the text. Compared against [`ClipboardCapabilities::read`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadContext {
    UserGesture,
    Foreground,
    Background,
}
impl ReadContext {
    fn required_access(self) -> ReadAccess {
        match self {
            Self::UserGesture => ReadAccess::UserGesture,
            Self::Foreground => ReadAccess::Foreground,
            Self::Background => ReadAccess::Background,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// The platform offers no hint, or the adapter cannot read it.
    Unknown,
    Normal,
    Sensitive,
}

/// Text the adapter just read from the local clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct LocalObservation {
    pub text: String,
    pub sensitivity: Sensitivity,
    pub context: ReadContext,
}
impl fmt::Debug for LocalObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalObservation")
            .field(
                "text",
                &format_args!("<redacted {} bytes>", self.text.len()),
            )
            .field("sensitivity", &self.sensitivity)
            .field("context", &self.context)
            .finish()
    }
}
impl LocalObservation {
    /// Validate raw clipboard bytes as UTF-8 within `max_bytes` before building a `String`.
    /// Non-UTF-8 and over-limit data are refused, never truncated or lossily converted.
    pub fn from_bytes(
        bytes: &[u8],
        max_bytes: usize,
        sensitivity: Sensitivity,
        context: ReadContext,
    ) -> Result<Self, TextError> {
        if bytes.len() > max_bytes.min(HARD_MAX_TEXT_BYTES) {
            return Err(TextError::TooLarge);
        }
        let text = std::str::from_utf8(bytes)
            .map_err(|_| TextError::NotUtf8)?
            .to_owned();
        Ok(Self {
            text,
            sensitivity,
            context,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextError {
    Empty,
    TooLarge,
    NotUtf8,
    ContainsNul,
}

fn check_text(text: &str, max: usize) -> Result<(), TextError> {
    if text.is_empty() {
        return Err(TextError::Empty);
    }
    if text.len() > max {
        return Err(TextError::TooLarge);
    }
    if text.contains('\0') {
        return Err(TextError::ContainsNul);
    }
    Ok(())
}

/// What to do about a local clipboard change or user send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendDecision {
    Send(ClipboardMessage),
    Skip(SkipReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    SendDisabled,
    /// Observation came from a change event but the policy is manual-only.
    AutomaticNotEnabled,
    /// Adapter claims a read the declared capabilities do not allow (e.g. a background read on Android 10+).
    ReadNotPermitted,
    EchoOfRemote,
    Unchanged,
    Sensitive,
    /// The caller-supplied item id is not 1-64 chars of `[A-Za-z0-9_-]`.
    InvalidId,
    Invalid(TextError),
}

/// Text for the adapter to write to the local clipboard.
#[derive(Clone, PartialEq, Eq)]
pub struct ApplyRequest {
    pub item_id: String,
    pub text: String,
    /// Set the platform's concealed/exclude-from-history hint if `can_mark_written_sensitive`.
    pub mark_sensitive: bool,
    pub expires_at_ms: u64,
}
impl fmt::Debug for ApplyRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplyRequest")
            .field("item_id", &self.item_id)
            .field(
                "text",
                &format_args!("<redacted {} bytes>", self.text.len()),
            )
            .field("mark_sensitive", &self.mark_sensitive)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteDecision {
    /// Write this to the clipboard now. The reducer has already recorded the echo fingerprint, so
    /// no acknowledgement call is needed.
    Apply(ApplyRequest),
    /// Item held for user confirmation; metadata only (the text stays inside the reducer).
    Staged {
        item_id: String,
        sensitive: bool,
        len: usize,
        expires_at_ms: u64,
    },
    Reject(RejectReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    ReceiveDisabled,
    WriteUnsupported,
    UnsupportedVersion,
    InvalidId,
    /// `origin_device` did not match the authenticated peer.
    OriginMismatch,
    /// The item claims to originate from this device: a loop.
    OwnOrigin,
    Duplicate,
    InvalidTtl,
    Sensitive,
    Invalid(TextError),
}

// ---------------------------------------------------------------- reducer

#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint(u64, usize);

struct Applied {
    fp: Fingerprint,
    until_ms: u64,
}
struct Pending {
    id: String,
    text: String,
    sensitive: bool,
    expires_at_ms: u64,
}

/// Per-peer state machine. One instance per paired device; a remote item applied here is
/// suppressed from being re-sent (to this or any other peer's reducer) by the echo fingerprint,
/// so GeekBridge never relays clipboard content between peers.
///
/// Retained state: a keyed 64-bit fingerprint plus length of up to 8 recently applied items, up to
/// 64 recent item ids, and at most one staged item. All expire; none are persisted. Fingerprints
/// use a per-instance random SipHash key, so they are meaningless outside this process.
pub struct ClipboardSync {
    local_device: String,
    peer_device: String,
    policy: ClipboardPolicy,
    caps: ClipboardCapabilities,
    hasher: RandomState,
    last_observed: Option<Fingerprint>,
    applied: VecDeque<Applied>,
    seen_ids: VecDeque<(String, u64)>,
    pending: Option<Pending>,
}

impl fmt::Debug for ClipboardSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClipboardSync")
            .field("peer_device", &self.peer_device)
            .field("policy", &self.policy)
            .field("pending", &self.pending.as_ref().map(|p| &p.id))
            .finish_non_exhaustive()
    }
}

impl ClipboardSync {
    pub fn new(
        local_device: impl Into<String>,
        peer_device: impl Into<String>,
        policy: ClipboardPolicy,
        caps: ClipboardCapabilities,
    ) -> Result<Self, PolicyError> {
        policy.validate(&caps)?;
        Ok(Self {
            local_device: local_device.into(),
            peer_device: peer_device.into(),
            policy,
            caps,
            hasher: RandomState::new(),
            last_observed: None,
            applied: VecDeque::new(),
            seen_ids: VecDeque::new(),
            pending: None,
        })
    }

    pub fn policy(&self) -> &ClipboardPolicy {
        &self.policy
    }

    /// Replace the policy. Staged content is dropped if receiving is no longer in `Confirm` mode.
    pub fn set_policy(&mut self, policy: ClipboardPolicy) -> Result<(), PolicyError> {
        policy.validate(&self.caps)?;
        if policy.receive != ReceiveMode::Confirm {
            self.pending = None;
        }
        self.policy = policy;
        Ok(())
    }

    fn fingerprint(&self, text: &str) -> Fingerprint {
        Fingerprint(self.hasher.hash_one(text), text.len())
    }

    /// Drop everything whose time has passed. Every public method calls this first.
    pub fn expire(&mut self, now_ms: u64) {
        self.applied.retain(|a| a.until_ms > now_ms);
        self.seen_ids.retain(|(_, until)| *until > now_ms);
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.expires_at_ms <= now_ms)
        {
            self.pending = None;
        }
    }

    /// The adapter observed a clipboard change by itself (no user gesture).
    pub fn local_changed(
        &mut self,
        obs: LocalObservation,
        now_ms: u64,
        new_id: impl FnOnce() -> String,
    ) -> SendDecision {
        self.expire(now_ms);
        match self.policy.send {
            SendMode::Off => return SendDecision::Skip(SkipReason::SendDisabled),
            SendMode::Manual | SendMode::OnFocus => {
                return SendDecision::Skip(SkipReason::AutomaticNotEnabled);
            }
            SendMode::Automatic => {}
        }
        self.prepare_send(obs, Trigger::Automatic, now_ms, new_id)
    }

    /// The app gained focus and the adapter read the clipboard (Android 10+). Needs
    /// [`SendMode::OnFocus`] and a `Foreground` observation; echo and unchanged text are suppressed, so refocusing
    /// the app does not resend the same item and never bounces a received item back.
    pub fn focus_gained(
        &mut self,
        obs: LocalObservation,
        now_ms: u64,
        new_id: impl FnOnce() -> String,
    ) -> SendDecision {
        self.expire(now_ms);
        match self.policy.send {
            SendMode::Off => return SendDecision::Skip(SkipReason::SendDisabled),
            SendMode::Manual => return SendDecision::Skip(SkipReason::AutomaticNotEnabled),
            SendMode::Automatic | SendMode::OnFocus => {}
        }
        self.prepare_send(obs, Trigger::Automatic, now_ms, new_id)
    }

    /// Record `text` as the current local clipboard without sending it, e.g. when sync is switched
    /// on or a link comes up, so content copied *before* the user opted in is never sent.
    pub fn set_baseline(&mut self, text: &str) {
        self.last_observed = Some(self.fingerprint(text));
    }

    /// The clipboard no longer holds text (emptied, or something else): the next text is a change
    /// even if it equals what was seen before.
    pub fn forget_observed(&mut self) {
        self.last_observed = None;
    }

    /// Another peer's reducer applied `text` to the local clipboard. Treat it as an echo here too,
    /// so one phone's clipboard is never relayed to another phone.
    pub fn note_applied_elsewhere(&mut self, text: &str, now_ms: u64) {
        self.expire(now_ms);
        let fp = self.fingerprint(text);
        if self.applied.len() == APPLIED_CAP {
            self.applied.pop_front();
        }
        self.applied.push_back(Applied {
            fp,
            until_ms: now_ms.saturating_add(self.policy.echo_window_ms),
        });
        self.last_observed = Some(fp);
    }

    /// The user explicitly asked to send the current clipboard.
    pub fn user_send(
        &mut self,
        obs: LocalObservation,
        now_ms: u64,
        new_id: impl FnOnce() -> String,
    ) -> SendDecision {
        self.expire(now_ms);
        if self.policy.send == SendMode::Off {
            return SendDecision::Skip(SkipReason::SendDisabled);
        }
        self.prepare_send(obs, Trigger::Manual, now_ms, new_id)
    }

    fn prepare_send(
        &mut self,
        obs: LocalObservation,
        trigger: Trigger,
        now_ms: u64,
        new_id: impl FnOnce() -> String,
    ) -> SendDecision {
        if self.caps.read < obs.context.required_access() {
            return SendDecision::Skip(SkipReason::ReadNotPermitted);
        }
        let allowed_context = match self.policy.send {
            SendMode::OnFocus => obs.context == ReadContext::Foreground,
            _ => obs.context == ReadContext::Background,
        };
        if trigger == Trigger::Automatic && !allowed_context {
            // Automatic mode relies on unattended reads; on-focus mode on reads while focused.
            // Anything else is manual only.
            return SendDecision::Skip(SkipReason::ReadNotPermitted);
        }
        if let Err(e) = check_text(&obs.text, self.policy.max_text_bytes) {
            return SendDecision::Skip(SkipReason::Invalid(e));
        }
        // Sensitivity is checked before the echo/unchanged bookkeeping so a blocked secret never
        // leaves a fingerprint behind and is re-evaluated every time it is observed.
        let allowed = match (obs.sensitivity, trigger) {
            (Sensitivity::Sensitive, Trigger::Automatic) => false,
            (Sensitivity::Sensitive, Trigger::Manual) => self.policy.allow_sensitive_manual_send,
            (Sensitivity::Unknown, Trigger::Automatic) => {
                self.policy.automatic_send_unknown_sensitivity
            }
            _ => true,
        };
        if !allowed {
            return SendDecision::Skip(SkipReason::Sensitive);
        }
        let fp = self.fingerprint(&obs.text);
        // Echo and unchanged checks apply to automatic sends only; an explicit user send is honoured.
        if trigger == Trigger::Automatic {
            if self.applied.iter().any(|a| a.fp == fp) {
                self.last_observed = Some(fp);
                return SendDecision::Skip(SkipReason::EchoOfRemote);
            }
            if self.last_observed == Some(fp) {
                return SendDecision::Skip(SkipReason::Unchanged);
            }
        }
        self.last_observed = Some(fp);
        let id = new_id();
        if !valid_id(&id) {
            return SendDecision::Skip(SkipReason::InvalidId);
        }
        // Remember our own id so a peer echoing it back is recognised as a duplicate.
        self.remember_id(
            id.clone(),
            now_ms.saturating_add(self.policy.max_accepted_ttl_ms),
        );
        SendDecision::Send(ClipboardMessage {
            version: CLIPBOARD_PROTOCOL_VERSION,
            id,
            origin_device: self.local_device.clone(),
            format: ClipboardFormat::TextUtf8,
            text: obs.text,
            sensitive: obs.sensitivity == Sensitivity::Sensitive,
            ttl_ms: self.policy.ttl_ms,
            trigger,
        })
    }

    fn remember_id(&mut self, id: String, until_ms: u64) {
        if self.seen_ids.len() == SEEN_IDS_CAP {
            self.seen_ids.pop_front();
        }
        self.seen_ids.push_back((id, until_ms));
    }

    /// A message arrived from the authenticated peer.
    pub fn remote_received(&mut self, msg: ClipboardMessage, now_ms: u64) -> RemoteDecision {
        use RejectReason::*;
        self.expire(now_ms);
        if self.policy.receive == ReceiveMode::Off {
            return RemoteDecision::Reject(ReceiveDisabled);
        }
        if !self.caps.write {
            return RemoteDecision::Reject(WriteUnsupported);
        }
        if msg.version != CLIPBOARD_PROTOCOL_VERSION {
            return RemoteDecision::Reject(UnsupportedVersion);
        }
        if !valid_id(&msg.id) || !valid_id(&msg.origin_device) {
            return RemoteDecision::Reject(InvalidId);
        }
        if msg.origin_device == self.local_device {
            return RemoteDecision::Reject(OwnOrigin);
        }
        if msg.origin_device != self.peer_device {
            return RemoteDecision::Reject(OriginMismatch);
        }
        if self.seen_ids.iter().any(|(id, _)| *id == msg.id) {
            return RemoteDecision::Reject(Duplicate);
        }
        if msg.ttl_ms == 0 {
            return RemoteDecision::Reject(InvalidTtl);
        }
        if let Err(e) = check_text(&msg.text, self.policy.max_text_bytes) {
            return RemoteDecision::Reject(Invalid(e));
        }
        if msg.sensitive && !self.policy.accept_sensitive_incoming {
            return RemoteDecision::Reject(Sensitive);
        }

        let ttl = msg.ttl_ms.min(self.policy.max_accepted_ttl_ms);
        let expires_at_ms = now_ms.saturating_add(ttl);
        self.remember_id(msg.id.clone(), expires_at_ms);

        match self.policy.receive {
            ReceiveMode::Apply => RemoteDecision::Apply(self.record_applied(
                msg.id,
                msg.text,
                msg.sensitive,
                now_ms,
                expires_at_ms,
            )),
            _ => {
                let staged = RemoteDecision::Staged {
                    item_id: msg.id.clone(),
                    sensitive: msg.sensitive,
                    len: msg.text.len(),
                    expires_at_ms,
                };
                // A newer item replaces the older staged one; there is no queue.
                self.pending = Some(Pending {
                    id: msg.id,
                    text: msg.text,
                    sensitive: msg.sensitive,
                    expires_at_ms,
                });
                staged
            }
        }
    }

    /// The user confirmed the staged item (`item_id` guards against confirming a replaced one).
    pub fn confirm_pending(&mut self, item_id: &str, now_ms: u64) -> RemoteDecision {
        self.expire(now_ms);
        if self.pending.as_ref().is_none_or(|p| p.id != item_id) {
            return RemoteDecision::Reject(RejectReason::Duplicate);
        }
        let p = self.pending.take().expect("checked above");
        RemoteDecision::Apply(self.record_applied(
            p.id,
            p.text,
            p.sensitive,
            now_ms,
            p.expires_at_ms,
        ))
    }

    pub fn discard_pending(&mut self) {
        self.pending = None;
    }
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    fn record_applied(
        &mut self,
        id: String,
        text: String,
        sensitive: bool,
        now_ms: u64,
        expires_at_ms: u64,
    ) -> ApplyRequest {
        let fp = self.fingerprint(&text);
        if self.applied.len() == APPLIED_CAP {
            self.applied.pop_front();
        }
        self.applied.push_back(Applied {
            fp,
            until_ms: now_ms.saturating_add(self.policy.echo_window_ms),
        });
        // The new local clipboard state is the baseline: do not report it as a fresh local change.
        self.last_observed = Some(fp);
        ApplyRequest {
            item_id: id,
            text,
            mark_sensitive: sensitive,
            expires_at_ms,
        }
    }

    /// Wipe all transient state, e.g. on disconnect, unpair, or screen lock.
    pub fn clear(&mut self) {
        self.last_observed = None;
        self.applied.clear();
        self.seen_ids.clear();
        self.pending = None;
    }
}

// ---------------------------------------------------------------- stock KDE Connect compatibility

/// Mirror of the stock `kdeconnect.clipboard.connect` rule (desktop `ClipboardPlugin::receivePacket`,
/// Android `ClipboardPlugin.onPacketReceived`): ignore when the packet timestamp is 0 (unknown) or
/// strictly older than the local last-update time; otherwise the content replaces the clipboard.
/// Both values are wall-clock epoch milliseconds from two different devices, so clock skew decides
/// the outcome, and equal timestamps are accepted. This is a compatibility check for the KDE
/// adapter only; GeekBridge's own protocol uses ids and TTLs instead.
pub fn kde_connect_should_apply(packet_timestamp_ms: i64, local_update_timestamp_ms: i64) -> bool {
    packet_timestamp_ms != 0 && packet_timestamp_ms >= local_update_timestamp_ms
}

/// Build a message from a stock `kdeconnect.clipboard` / `.connect` packet. Stock packets carry no
/// id, sensitivity, TTL, or size limit, so those are supplied locally (`sensitive` is always
/// `false`: stock peers do not say). The caller must have already applied
/// [`kde_connect_should_apply`] for `.connect` packets.
pub fn message_from_kde_packet(
    content: String,
    peer_device: &str,
    local_id: String,
    policy: &ClipboardPolicy,
) -> ClipboardMessage {
    ClipboardMessage {
        version: CLIPBOARD_PROTOCOL_VERSION,
        id: local_id,
        origin_device: peer_device.to_owned(),
        format: ClipboardFormat::TextUtf8,
        text: content,
        sensitive: false,
        ttl_ms: policy.ttl_ms,
        trigger: Trigger::Automatic,
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "desktop-1";
    const PEER: &str = "phone-1";

    fn bg_caps() -> ClipboardCapabilities {
        ClipboardCapabilities {
            read: ReadAccess::Background,
            write: true,
            observe: ObserveMode::Event,
            reports_sensitivity: true,
            can_mark_written_sensitive: true,
        }
    }
    fn android_caps() -> ClipboardCapabilities {
        ClipboardCapabilities {
            read: ReadAccess::Foreground,
            write: true,
            observe: ObserveMode::Event,
            reports_sensitivity: true,
            can_mark_written_sensitive: false,
        }
    }
    fn auto_policy() -> ClipboardPolicy {
        ClipboardPolicy {
            send: SendMode::Automatic,
            receive: ReceiveMode::Apply,
            ..Default::default()
        }
    }
    fn sync(policy: ClipboardPolicy, caps: ClipboardCapabilities) -> ClipboardSync {
        ClipboardSync::new(ME, PEER, policy, caps).unwrap()
    }
    fn obs(text: &str, s: Sensitivity, c: ReadContext) -> LocalObservation {
        LocalObservation {
            text: text.into(),
            sensitivity: s,
            context: c,
        }
    }
    fn bg(text: &str) -> LocalObservation {
        obs(text, Sensitivity::Normal, ReadContext::Background)
    }
    fn msg(id: &str, text: &str) -> ClipboardMessage {
        ClipboardMessage {
            version: 1,
            id: id.into(),
            origin_device: PEER.into(),
            format: ClipboardFormat::TextUtf8,
            text: text.into(),
            sensitive: false,
            ttl_ms: 10_000,
            trigger: Trigger::Automatic,
        }
    }
    fn id(s: &str) -> impl FnOnce() -> String {
        let s = s.to_owned();
        move || s
    }

    #[test]
    fn defaults_are_off_and_valid() {
        let p = ClipboardPolicy::default();
        assert_eq!((p.send, p.receive), (SendMode::Off, ReceiveMode::Off));
        assert!(p.validate(&ClipboardCapabilities::none()).is_ok());
        let mut s = sync(p, bg_caps());
        assert_eq!(
            s.local_changed(bg("x"), 0, id("a")),
            SendDecision::Skip(SkipReason::SendDisabled)
        );
        assert_eq!(
            s.remote_received(msg("m1", "x"), 0),
            RemoteDecision::Reject(RejectReason::ReceiveDisabled)
        );
    }

    #[test]
    fn echo_of_applied_remote_text_is_not_resent() {
        let mut s = sync(auto_policy(), bg_caps());
        assert!(matches!(
            s.remote_received(msg("m1", "hello"), 100),
            RemoteDecision::Apply(_)
        ));
        // Adapter reports the change it caused (possibly several times, as X11 does).
        for t in [150, 300, 4_000] {
            assert_eq!(
                s.local_changed(bg("hello"), t, id("n")),
                SendDecision::Skip(SkipReason::EchoOfRemote)
            );
        }
        // A different copy is sent.
        assert!(matches!(
            s.local_changed(bg("other"), 4_500, id("n1")),
            SendDecision::Send(_)
        ));
    }

    #[test]
    fn echo_window_expires_but_baseline_still_dedups_repeat() {
        let mut s = sync(auto_policy(), bg_caps());
        s.remote_received(msg("m1", "hello"), 0);
        // Same text observed repeatedly right after apply is still "unchanged" against the baseline.
        assert_eq!(
            s.local_changed(bg("hello"), 6_000, id("n")),
            SendDecision::Skip(SkipReason::Unchanged)
        );
        // After a different copy, copying "hello" again past the echo window is a genuine user copy.
        assert!(matches!(
            s.local_changed(bg("x"), 6_100, id("n1")),
            SendDecision::Send(_)
        ));
        assert!(matches!(
            s.local_changed(bg("hello"), 6_200, id("n2")),
            SendDecision::Send(_)
        ));
    }

    #[test]
    fn identical_consecutive_local_changes_sent_once() {
        let mut s = sync(auto_policy(), bg_caps());
        assert!(matches!(
            s.local_changed(bg("a"), 0, id("n1")),
            SendDecision::Send(_)
        ));
        assert_eq!(
            s.local_changed(bg("a"), 1, id("n2")),
            SendDecision::Skip(SkipReason::Unchanged)
        );
    }

    #[test]
    fn loop_ids_and_origins() {
        let mut s = sync(auto_policy(), bg_caps());
        let SendDecision::Send(out) = s.local_changed(bg("a"), 0, id("item-1")) else {
            panic!()
        };
        assert_eq!(out.origin_device, ME);
        // Peer bounces our item back: own origin, and the id is also remembered.
        assert_eq!(
            s.remote_received(out.clone(), 10),
            RemoteDecision::Reject(RejectReason::OwnOrigin)
        );
        let mut spoof = out;
        spoof.origin_device = PEER.into();
        assert_eq!(
            s.remote_received(spoof, 10),
            RemoteDecision::Reject(RejectReason::Duplicate)
        );
        // Replay of an accepted remote id.
        assert!(matches!(
            s.remote_received(msg("m1", "b"), 20),
            RemoteDecision::Apply(_)
        ));
        assert_eq!(
            s.remote_received(msg("m1", "b"), 30),
            RemoteDecision::Reject(RejectReason::Duplicate)
        );
        // A message claiming a third device as origin is not accepted from this session.
        let mut third = msg("m2", "c");
        third.origin_device = "tablet-9".into();
        assert_eq!(
            s.remote_received(third, 40),
            RemoteDecision::Reject(RejectReason::OriginMismatch)
        );
    }

    #[test]
    fn expired_id_memory_and_staged_item() {
        let p = ClipboardPolicy {
            receive: ReceiveMode::Confirm,
            ..Default::default()
        };
        let mut s = sync(p, bg_caps());
        let RemoteDecision::Staged {
            item_id,
            expires_at_ms,
            ..
        } = s.remote_received(msg("m1", "secret-ish"), 1_000)
        else {
            panic!()
        };
        assert_eq!(expires_at_ms, 11_000);
        assert!(s.has_pending());
        // Confirming after expiry fails and the text is gone.
        assert_eq!(
            s.confirm_pending(&item_id, 11_000),
            RemoteDecision::Reject(RejectReason::Duplicate)
        );
        assert!(!s.has_pending());
        // Once the id memory has expired, a replay is no longer recognised: ids only bound replay by TTL.
        assert!(matches!(
            s.remote_received(msg("m1", "again"), 20_000),
            RemoteDecision::Staged { .. }
        ));
    }

    #[test]
    fn remote_ttl_is_clamped_and_zero_rejected() {
        let p = ClipboardPolicy {
            receive: ReceiveMode::Apply,
            max_accepted_ttl_ms: 2_000,
            ..Default::default()
        };
        let mut s = sync(p, bg_caps());
        let mut m = msg("m1", "t");
        m.ttl_ms = u64::MAX;
        let RemoteDecision::Apply(a) = s.remote_received(m, 500) else {
            panic!()
        };
        assert_eq!(a.expires_at_ms, 2_500);
        let mut z = msg("m2", "t");
        z.ttl_ms = 0;
        assert_eq!(
            s.remote_received(z, 500),
            RemoteDecision::Reject(RejectReason::InvalidTtl)
        );
    }

    #[test]
    fn confirm_applies_only_matching_item_and_records_echo() {
        let p = ClipboardPolicy {
            send: SendMode::Automatic,
            receive: ReceiveMode::Confirm,
            ..Default::default()
        };
        let mut s = sync(p, bg_caps());
        s.remote_received(msg("m1", "one"), 0);
        s.remote_received(msg("m2", "two"), 1); // replaces m1
        assert_eq!(
            s.confirm_pending("m1", 2),
            RemoteDecision::Reject(RejectReason::Duplicate)
        );
        let RemoteDecision::Apply(a) = s.confirm_pending("m2", 3) else {
            panic!()
        };
        assert_eq!(a.text, "two");
        assert_eq!(
            s.local_changed(bg("two"), 4, id("n")),
            SendDecision::Skip(SkipReason::EchoOfRemote)
        );
    }

    #[test]
    fn sensitive_data_opt_out_on_send() {
        let mut s = sync(auto_policy(), bg_caps());
        let secret = obs("hunter2", Sensitivity::Sensitive, ReadContext::Background);
        assert_eq!(
            s.local_changed(secret.clone(), 0, id("n1")),
            SendDecision::Skip(SkipReason::Sensitive)
        );
        // Explicit manual send is also blocked unless separately opted in.
        let gesture = obs("hunter2", Sensitivity::Sensitive, ReadContext::UserGesture);
        assert_eq!(
            s.user_send(gesture.clone(), 1, id("n2")),
            SendDecision::Skip(SkipReason::Sensitive)
        );
        let mut p = auto_policy();
        p.allow_sensitive_manual_send = true;
        s.set_policy(p).unwrap();
        let SendDecision::Send(m) = s.user_send(gesture, 2, id("n3")) else {
            panic!()
        };
        assert!(m.sensitive);
        assert_eq!(m.trigger, Trigger::Manual);
        // Automatic still never sends sensitive content.
        assert_eq!(
            s.local_changed(secret, 3, id("n4")),
            SendDecision::Skip(SkipReason::Sensitive)
        );
    }

    #[test]
    fn unknown_sensitivity_can_be_blocked_in_automatic_mode() {
        let mut p = auto_policy();
        p.automatic_send_unknown_sensitivity = false;
        let mut s = sync(p, bg_caps());
        let unknown = obs("x", Sensitivity::Unknown, ReadContext::Background);
        assert_eq!(
            s.local_changed(unknown, 0, id("n")),
            SendDecision::Skip(SkipReason::Sensitive)
        );
    }

    #[test]
    fn sensitive_data_opt_out_on_receive() {
        let mut s = sync(auto_policy(), bg_caps());
        let mut m = msg("m1", "pw");
        m.sensitive = true;
        assert_eq!(
            s.remote_received(m.clone(), 0),
            RemoteDecision::Reject(RejectReason::Sensitive)
        );
        let mut p = auto_policy();
        p.accept_sensitive_incoming = true;
        s.set_policy(p).unwrap();
        let RemoteDecision::Apply(a) = s.remote_received(m, 1) else {
            panic!()
        };
        assert!(a.mark_sensitive);
    }

    #[test]
    fn oversized_and_invalid_text_rejected_both_ways() {
        let p = ClipboardPolicy {
            max_text_bytes: 8,
            ..auto_policy()
        };
        let mut s = sync(p, bg_caps());
        assert_eq!(
            s.local_changed(bg("123456789"), 0, id("n")),
            SendDecision::Skip(SkipReason::Invalid(TextError::TooLarge))
        );
        assert_eq!(
            s.local_changed(bg(""), 0, id("n")),
            SendDecision::Skip(SkipReason::Invalid(TextError::Empty))
        );
        assert_eq!(
            s.local_changed(bg("a\0b"), 0, id("n")),
            SendDecision::Skip(SkipReason::Invalid(TextError::ContainsNul))
        );
        // Exactly at the limit is fine; limit is in bytes, so multibyte text counts fully.
        assert!(matches!(
            s.local_changed(bg("12345678"), 1, id("n1")),
            SendDecision::Send(_)
        ));
        assert_eq!(
            s.local_changed(bg("ééééé"), 2, id("n2")),
            SendDecision::Skip(SkipReason::Invalid(TextError::TooLarge))
        );
        assert_eq!(
            s.remote_received(msg("m1", "123456789"), 3),
            RemoteDecision::Reject(RejectReason::Invalid(TextError::TooLarge))
        );
    }

    #[test]
    fn raw_bytes_are_bounded_and_must_be_utf8() {
        let ok =
            LocalObservation::from_bytes(b"hi", 8, Sensitivity::Unknown, ReadContext::Background);
        assert!(ok.is_ok());
        let big = LocalObservation::from_bytes(
            &[b'a'; 9],
            8,
            Sensitivity::Unknown,
            ReadContext::Background,
        );
        assert_eq!(big.unwrap_err(), TextError::TooLarge);
        let bad = LocalObservation::from_bytes(
            &[0xff, 0xfe],
            8,
            Sensitivity::Unknown,
            ReadContext::Background,
        );
        assert_eq!(bad.unwrap_err(), TextError::NotUtf8);
        // The hard cap applies even if a caller passes a larger limit.
        let huge = vec![b'a'; HARD_MAX_TEXT_BYTES + 1];
        let r = LocalObservation::from_bytes(
            &huge,
            usize::MAX,
            Sensitivity::Unknown,
            ReadContext::Background,
        );
        assert_eq!(r.unwrap_err(), TextError::TooLarge);
    }

    #[test]
    fn wire_parse_is_bounded_and_strict() {
        let good = br#"{"version":1,"id":"a","origin_device":"phone-1","format":"text/plain;charset=utf-8","text":"hi","ttl_ms":1000,"trigger":"manual"}"#;
        let m = ClipboardMessage::from_json_slice(good, 1024).unwrap();
        assert!(!m.sensitive);
        assert_eq!(
            ClipboardMessage::from_json_slice(&vec![b' '; max_wire_bytes(8) + 1], 8),
            Err(MessageError::TooLarge)
        );
        let unknown_field = br#"{"version":1,"id":"a","origin_device":"p","format":"text/plain;charset=utf-8","text":"hi","ttl_ms":1,"trigger":"manual","extra":1}"#;
        assert_eq!(
            ClipboardMessage::from_json_slice(unknown_field, 1024),
            Err(MessageError::Malformed)
        );
        let image = br#"{"version":1,"id":"a","origin_device":"p","format":"image/png","text":"","ttl_ms":1,"trigger":"manual"}"#;
        assert_eq!(
            ClipboardMessage::from_json_slice(image, 1024),
            Err(MessageError::Malformed)
        );
        let invalid_utf8 = b"{\"version\":1,\"id\":\"a\",\"text\":\"\xff\"}";
        assert_eq!(
            ClipboardMessage::from_json_slice(invalid_utf8, 1024),
            Err(MessageError::Malformed)
        );
    }

    #[test]
    fn unsupported_background_reads() {
        // Policy cannot enable automatic send where the platform forbids unattended reads.
        let p = ClipboardPolicy {
            send: SendMode::Automatic,
            ..Default::default()
        };
        assert_eq!(
            ClipboardSync::new(ME, PEER, p, android_caps()).unwrap_err(),
            PolicyError::AutomaticSendUnsupported
        );
        let polling_none = ClipboardCapabilities {
            observe: ObserveMode::None,
            ..bg_caps()
        };
        assert!(!polling_none.supports_automatic_send());
        // Manual works with foreground reads only.
        let manual = ClipboardPolicy {
            send: SendMode::Manual,
            ..Default::default()
        };
        let mut s = sync(manual.clone(), android_caps());
        assert!(matches!(
            s.user_send(
                obs("a", Sensitivity::Normal, ReadContext::Foreground),
                0,
                id("n")
            ),
            SendDecision::Send(_)
        ));
        // Change events are not honoured in manual mode.
        assert_eq!(
            s.local_changed(bg("b"), 1, id("n1")),
            SendDecision::Skip(SkipReason::AutomaticNotEnabled)
        );
        // An adapter claiming a background read the capabilities forbid is refused.
        assert_eq!(
            s.user_send(bg("c"), 2, id("n2")),
            SendDecision::Skip(SkipReason::ReadNotPermitted)
        );
        // No read capability at all: manual send is a policy error too.
        assert_eq!(
            ClipboardSync::new(ME, PEER, manual, ClipboardCapabilities::none()).unwrap_err(),
            PolicyError::ManualSendUnsupported
        );
    }

    #[test]
    fn automatic_send_requires_background_context_even_with_background_caps() {
        let mut s = sync(auto_policy(), bg_caps());
        let fg = obs("a", Sensitivity::Normal, ReadContext::Foreground);
        assert_eq!(
            s.local_changed(fg, 0, id("n")),
            SendDecision::Skip(SkipReason::ReadNotPermitted)
        );
    }

    #[test]
    fn receive_requires_write_capability() {
        let p = ClipboardPolicy {
            receive: ReceiveMode::Apply,
            ..Default::default()
        };
        let no_write = ClipboardCapabilities {
            write: false,
            ..bg_caps()
        };
        assert_eq!(
            ClipboardSync::new(ME, PEER, p, no_write).unwrap_err(),
            PolicyError::ReceiveUnsupported
        );
    }

    #[test]
    fn policy_bounds() {
        let caps = bg_caps();
        let bad = |f: fn(&mut ClipboardPolicy)| {
            let mut p = ClipboardPolicy::default();
            f(&mut p);
            p.validate(&caps)
        };
        assert_eq!(
            bad(|p| p.max_text_bytes = 0),
            Err(PolicyError::MaxTextBytes)
        );
        assert_eq!(
            bad(|p| p.max_text_bytes = HARD_MAX_TEXT_BYTES + 1),
            Err(PolicyError::MaxTextBytes)
        );
        assert_eq!(bad(|p| p.ttl_ms = 0), Err(PolicyError::Ttl));
        assert_eq!(
            bad(|p| p.max_accepted_ttl_ms = HARD_MAX_TTL_MS + 1),
            Err(PolicyError::Ttl)
        );
        // Serde defaults fill missing fields with the safe (off) policy.
        let p: ClipboardPolicy = serde_json::from_str(r#"{"send":"manual"}"#).unwrap();
        assert_eq!(p.receive, ReceiveMode::Off);
        assert!(!p.accept_sensitive_incoming);
    }

    #[test]
    fn invalid_ids_rejected() {
        let mut s = sync(auto_policy(), bg_caps());
        assert_eq!(
            s.remote_received(msg("bad id!", "x"), 0),
            RemoteDecision::Reject(RejectReason::InvalidId)
        );
        assert_eq!(
            s.remote_received(msg(&"a".repeat(MAX_ID_BYTES + 1), "x"), 0),
            RemoteDecision::Reject(RejectReason::InvalidId)
        );
        assert_eq!(
            s.local_changed(bg("x"), 0, id("bad id")),
            SendDecision::Skip(SkipReason::InvalidId)
        );
    }

    #[test]
    fn debug_output_never_contains_text() {
        let m = msg("m1", "TOPSECRET");
        let o = obs("TOPSECRET", Sensitivity::Normal, ReadContext::Background);
        let mut s = sync(
            ClipboardPolicy {
                receive: ReceiveMode::Confirm,
                ..Default::default()
            },
            bg_caps(),
        );
        s.remote_received(m.clone(), 0);
        let dump = format!("{m:?} {o:?} {s:?}");
        assert!(!dump.contains("TOPSECRET"));
    }

    #[test]
    fn clear_and_policy_downgrade_drop_staged_text() {
        let mut s = sync(
            ClipboardPolicy {
                receive: ReceiveMode::Confirm,
                ..Default::default()
            },
            bg_caps(),
        );
        s.remote_received(msg("m1", "x"), 0);
        s.set_policy(ClipboardPolicy::default()).unwrap();
        assert!(!s.has_pending());
        s.set_policy(ClipboardPolicy {
            receive: ReceiveMode::Confirm,
            ..Default::default()
        })
        .unwrap();
        s.remote_received(msg("m2", "x"), 1);
        s.clear();
        assert!(!s.has_pending());
    }

    #[test]
    fn kde_timestamp_rule_matches_stock_clients() {
        assert!(!kde_connect_should_apply(0, 0)); // 0 means unknown: ignored
        assert!(!kde_connect_should_apply(0, 5));
        assert!(!kde_connect_should_apply(99, 100)); // older than local: ignored
        assert!(kde_connect_should_apply(100, 100)); // equal: accepted (both sides may swap)
        assert!(kde_connect_should_apply(101, 100));
        // A phone whose clock is 1h behind loses every reconnect comparison; this is why the custom
        // protocol does not use cross-device timestamps.
        assert!(!kde_connect_should_apply(1_000_000, 1_000_000 + 3_600_000));
    }

    fn focus_policy() -> ClipboardPolicy {
        ClipboardPolicy {
            send: SendMode::OnFocus,
            receive: ReceiveMode::Apply,
            ..Default::default()
        }
    }
    fn fg(text: &str) -> LocalObservation {
        obs(text, Sensitivity::Normal, ReadContext::Foreground)
    }

    #[test]
    fn on_focus_mode_needs_foreground_reads_and_dedups() {
        // Valid on Android-like capabilities, where Automatic is not.
        let mut s = sync(focus_policy(), android_caps());
        assert!(matches!(
            s.focus_gained(fg("a"), 0, id("n1")),
            SendDecision::Send(m) if m.trigger == Trigger::Automatic
        ));
        // Refocusing with the same clipboard sends nothing.
        assert_eq!(
            s.focus_gained(fg("a"), 1, id("n2")),
            SendDecision::Skip(SkipReason::Unchanged)
        );
        assert!(matches!(
            s.focus_gained(fg("b"), 2, id("n3")),
            SendDecision::Send(_)
        ));
        // A background-context read is not what this mode relies on.
        assert_eq!(
            s.focus_gained(bg("c"), 3, id("n4")),
            SendDecision::Skip(SkipReason::ReadNotPermitted)
        );
        // Change events are not honoured; there are none on Android 10+.
        assert_eq!(
            s.local_changed(bg("c"), 4, id("n5")),
            SendDecision::Skip(SkipReason::AutomaticNotEnabled)
        );
        // No read capability at all: policy error.
        assert_eq!(
            ClipboardSync::new(ME, PEER, focus_policy(), ClipboardCapabilities::none())
                .unwrap_err(),
            PolicyError::FocusSendUnsupported
        );
    }

    #[test]
    fn on_focus_does_not_bounce_received_text_back() {
        let mut s = sync(focus_policy(), android_caps());
        assert!(matches!(
            s.remote_received(msg("m1", "from pc"), 0),
            RemoteDecision::Apply(_)
        ));
        assert_eq!(
            s.focus_gained(fg("from pc"), 10, id("n")),
            SendDecision::Skip(SkipReason::EchoOfRemote)
        );
        // An explicit user send is still honoured, even for identical text.
        assert!(matches!(
            s.user_send(
                obs("from pc", Sensitivity::Normal, ReadContext::UserGesture),
                20,
                id("n2")
            ),
            SendDecision::Send(_)
        ));
    }

    #[test]
    fn on_focus_never_sends_sensitive_text() {
        let mut s = sync(focus_policy(), android_caps());
        let secret = obs("pw", Sensitivity::Sensitive, ReadContext::Foreground);
        assert_eq!(
            s.focus_gained(secret, 0, id("n")),
            SendDecision::Skip(SkipReason::Sensitive)
        );
    }

    #[test]
    fn baseline_suppresses_pre_existing_content() {
        let mut s = sync(auto_policy(), bg_caps());
        s.set_baseline("already there");
        assert_eq!(
            s.local_changed(bg("already there"), 0, id("n")),
            SendDecision::Skip(SkipReason::Unchanged)
        );
        assert!(matches!(
            s.local_changed(bg("new"), 1, id("n1")),
            SendDecision::Send(_)
        ));
    }

    #[test]
    fn text_applied_for_another_peer_is_not_relayed() {
        let mut s = sync(auto_policy(), bg_caps());
        s.note_applied_elsewhere("from phone A", 0);
        assert_eq!(
            s.local_changed(bg("from phone A"), 1, id("n")),
            SendDecision::Skip(SkipReason::EchoOfRemote)
        );
        assert!(matches!(
            s.local_changed(bg("mine"), 2, id("n1")),
            SendDecision::Send(_)
        ));
    }

    #[test]
    fn kde_message_goes_through_the_same_checks() {
        let p = auto_policy();
        let mut s = sync(p.clone(), bg_caps());
        let m = message_from_kde_packet("from stock".into(), PEER, "kde-1".into(), &p);
        assert!(!m.sensitive);
        assert!(matches!(s.remote_received(m, 0), RemoteDecision::Apply(_)));
        assert_eq!(
            s.local_changed(bg("from stock"), 1, id("n")),
            SendDecision::Skip(SkipReason::EchoOfRemote)
        );
        let big =
            message_from_kde_packet("x".repeat(p.max_text_bytes + 1), PEER, "kde-2".into(), &p);
        assert_eq!(
            s.remote_received(big, 2),
            RemoteDecision::Reject(RejectReason::Invalid(TextError::TooLarge))
        );
    }
}
