// SPDX-License-Identifier: GPL-3.0-or-later
//! Answering, declining and ending phone calls from this computer.
//!
//! The hub merges every place that can see or control a call into the one view the apps show
//! ([`bridge_core::calls_view`]):
//!
//! * **Host Bluetooth calling** ([`HostCalls`]): this computer as the HFP hands-free unit. On Linux
//!   that is `bridge-calls` over PipeWire's telephony service. Windows and macOS adapters plug in
//!   through the same trait. This is the path that carries call audio.
//! * **The GeekBridge link** (`geekbridge.calls.*`, [`bridge_core::calls_link`]): the paired phone
//!   reports ringing/idle and executes answer/decline/end through Android's `TelecomManager`.
//!   Signalling only; audio stays on the phone unless the HFP link above carries it.
//!
//! Nothing here is offered to the user unless it passed the same pre-flight that execution uses, and
//! every control request is checked against the *current* view first, so a stale click cannot hit a
//! different call. Link packets are honoured only for devices the user enabled ("Telephony" in the
//! device permissions). Phone numbers and names live in memory only and are never logged.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use bridge_core::call_replies::MAX_REPLIES;
#[cfg(target_os = "linux")]
use bridge_core::calls::Support;
use bridge_core::calls::{CallCommand, CallState, Sensitive};
use bridge_core::calls_link::{self as link, LinkAction, LinkCallReport, LinkFailure};
use bridge_core::calls_view::{
    self as view, ActionTarget, CallsView, ControlPath, SourceSnapshot, callback_number,
};
use bridge_core::ipc::{CallAction, CallsStatus, DeclineReplies, ErrorBody, ErrorCode};
use tokio::sync::mpsc;

const LINK_PREFIX: &str = "link:";
/// Requests to a phone that stay unanswered this long are forgotten (the phone has no reply path
/// if it was killed in between).
const PENDING_TTL: Duration = Duration::from_secs(20);
const MAX_PENDING: usize = 4;
/// Keypad tones the service accepts per [`TONE_WINDOW`]: a person typing a PIN fast stays well under it, a script
/// pressing keys in a loop does not. The phone applies its own limit as well.
pub const MAX_TONES: usize = 20;
pub const TONE_WINDOW: Duration = Duration::from_secs(5);
const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const HOST_POLL: Duration = Duration::from_millis(250);

/// The hub's clocks. [`Timings::default`] is what the service uses; tests shorten them.
#[derive(Clone, Copy, Debug)]
pub struct Timings {
    /// How often the watchdog runs (stale link calls, the card process, link-flap memory).
    pub tick: Duration,
    /// A phone that reported a ring and said nothing since is asked for its state after this
    /// long. Android sends changes only, so silence is normal; the ask is cheap and the answer
    /// either confirms the ring or ends it.
    pub ring_quiet: Duration,
    /// The same for a call that is going on (calls are long; this only catches a phone that stopped
    /// answering on a link that still looks up).
    pub live_quiet: Duration,
    /// How long an ask may stay unanswered.
    pub probe_grace: Duration,
    /// Unanswered asks after which the phone's call is cleared here (it cannot be confirmed, and a
    /// ghost call would keep the card and "in a call" up forever).
    pub probes: u32,
    /// A ring that disappears because the link dropped and comes back after the reconnect within
    /// this long is the same ring: no second banner, the same card.
    pub flap_memory: Duration,
    /// How long the card process may outlive the last ring ([`crate::call_overlay::LINGER`]).
    pub overlay_linger: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            tick: Duration::from_millis(250),
            ring_quiet: Duration::from_secs(20),
            live_quiet: Duration::from_secs(120),
            probe_grace: Duration::from_secs(5),
            probes: 2,
            flap_memory: Duration::from_secs(60),
            overlay_linger: crate::call_overlay::LINGER,
        }
    }
}

/// Shown when the watchdog cleared a call the phone stopped confirming.
pub const STALE_CALL_NOTICE: &str =
    "The phone stopped confirming the call, so it is no longer shown here. Check the phone.";

/// One word (or the refusal) for a result in the call trace. Messages are fixed text, no numbers.
fn outcome(r: &Result<(), ErrorBody>) -> String {
    match r {
        Ok(()) => "ok".into(),
        Err(e) => format!("refused ({:?}): {}", e.code, e.message),
    }
}

fn err(code: ErrorCode, message: &str) -> ErrorBody {
    ErrorBody {
        code,
        message: message.to_owned(),
    }
}

// ------------------------------------------------------------------ host backends

/// A computer-side backend that talks Bluetooth hands-free to phones. Runs on its own thread.
pub trait HostCalls: Send + 'static {
    /// Short name for diagnostics ("Bluetooth hands-free (Linux)").
    fn label(&self) -> String;
    /// Wait up to `wait` for changes, then return the current snapshot of every phone.
    fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String>;
    /// Execute a command on the phone behind `source`. `Ok` means accepted for dispatch.
    fn execute(&mut self, source: &str, command: &CallCommand) -> Result<(), String>;
    /// Lines about the call audio (where it plays, what is wrong and how to fix it). Read after
    /// every poll.
    fn notes(&self) -> Vec<String> {
        Vec::new()
    }
    /// Names of the phones whose GeekBridge link is up right now (before every poll). The Linux
    /// host reconnects a set-up phone's hands-free profile when its link comes up.
    fn set_link_phones(&mut self, names: Vec<String>) {
        let _ = names;
    }
}

struct HostCommand {
    source: String,
    command: CallCommand,
    reply: std_mpsc::SyncSender<Result<(), String>>,
    /// When the caller stops waiting. A command still queued after this is dropped, not run late (a keypad tone
    /// must never reach a different call than the one it was pressed in).
    deadline: Instant,
}

/// Run `cmd` through `run` unless its deadline has passed; either way the caller is answered.
fn run_host_command(
    cmd: HostCommand,
    now: Instant,
    run: impl FnOnce(&str, &CallCommand) -> Result<(), String>,
) {
    let result = if now >= cmd.deadline {
        Err("the command waited too long and was dropped".to_owned())
    } else {
        run(&cmd.source, &cmd.command)
    };
    let _ = cmd.reply.send(result);
}

#[cfg(test)]
mod host_deadline_tests {
    use super::*;

    fn command(deadline: Instant) -> (HostCommand, std_mpsc::Receiver<Result<(), String>>) {
        let (reply, wait) = std_mpsc::sync_channel(1);
        let cmd = HostCommand {
            source: "hfp:x".into(),
            command: CallCommand::ConnectAudio,
            reply,
            deadline,
        };
        (cmd, wait)
    }

    #[test]
    fn an_expired_host_command_is_dropped_and_a_live_one_runs() {
        let now = Instant::now();
        let (cmd, wait) = command(now);
        let mut ran = false;
        run_host_command(cmd, now, |_, _| {
            ran = true;
            Ok(())
        });
        assert!(!ran, "an expired command must not run");
        assert!(wait.try_recv().unwrap().is_err());

        let (cmd, wait) = command(now + Duration::from_secs(8));
        run_host_command(cmd, now, |_, _| {
            ran = true;
            Ok(())
        });
        assert!(ran);
        assert_eq!(wait.try_recv().unwrap(), Ok(()));
    }
}

#[derive(Default)]
struct HostShared {
    snapshots: Vec<SourceSnapshot>,
    label: String,
    problem: Option<String>,
    notes: Vec<String>,
}

struct HostHandle {
    shared: Arc<Mutex<HostShared>>,
    commands: std_mpsc::Sender<HostCommand>,
    stop: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for HostHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------ notifications

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingChoice {
    Answer,
    Decline,
    /// Decline and send the saved text with this index ([`DeclineReplies::texts`]).
    DeclineWith(u8),
}

/// What the desktop shows when a call rings while no Calls window is open.
pub trait CallNotifier: Send + Sync {
    /// `can_answer`/`can_decline` say which buttons to offer. `reply` may be dropped unused.
    fn ringing(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    );
    /// Like [`Self::ringing`], with the short texts the banner may offer as "Decline with message"
    /// (empty when the action is not available for this call). A banner without such buttons
    /// ignores them.
    fn ringing_with_replies(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        replies: &[String],
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    ) {
        let _ = replies;
        self.ringing(caller, can_answer, can_decline, reply);
    }
    /// A ring ended with nobody answering. `number` is a plain, dialable number when the caller
    /// is known by one, so the banner may offer "Call back" and "Message". Default: say nothing.
    fn missed(&self, caller: &str, number: Option<&str>) {
        let _ = (caller, number);
    }
}

/// Best-effort freedesktop notification through `notify-send` with Answer/Decline actions
/// (libnotify >= 0.8). Without the tool or action support nothing is shown; the Calls window and
/// the phone itself still work.
pub struct NotifySendCalls;
impl NotifySendCalls {
    pub fn args(caller: &str, can_answer: bool, can_decline: bool) -> Vec<String> {
        Self::args_with_replies(caller, can_answer, can_decline, &[])
    }

    /// As [`Self::args`], plus one "Decline + text" button per saved reply (only when Decline is
    /// on offer). The texts are the person's own words, never a number, and are cleaned like the
    /// caller name; the action names are fixed (`reply0`..).
    pub fn args_with_replies(
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        replies: &[String],
    ) -> Vec<String> {
        let clean = crate::notify_text::plain(caller, 80);
        let mut args = vec![
            "--app-name=GeekBridge".to_owned(),
            "--icon=dev.geekbridge.Calls".to_owned(),
            "--urgency=critical".to_owned(),
            "--category=call.incoming".to_owned(),
            "--expire-time=30000".to_owned(),
        ];
        if can_answer || can_decline {
            args.push("--wait".into());
        }
        if can_answer {
            args.push("--action=answer=Answer".into());
        }
        if can_decline {
            args.push("--action=decline=Decline".into());
            for (i, text) in replies.iter().take(MAX_REPLIES).enumerate() {
                let text = crate::notify_text::plain(text, 60);
                args.push(format!("--action=reply{i}=Decline + text: {text}"));
            }
        }
        if can_answer || can_decline {
            // A click on the body (mako draws no buttons) opens the Calls window; it never answers.
            args.push("--action=default=Open".into());
        }
        // `--`: a caller-ID name that starts with a dash is text, not an option.
        args.push("--".into());
        args.push("Incoming call".into());
        args.push(clean);
        args
    }
}
impl NotifySendCalls {
    /// The missed-call banner: a plain notice with "Call back" and "Message" when the number is
    /// plain, and a click on the body opening the Calls window (Recents).
    pub fn missed_args(caller: &str, number: Option<&str>) -> Vec<String> {
        let clean = crate::notify_text::plain(caller, 80);
        let mut args = vec![
            "--app-name=GeekBridge".to_owned(),
            "--icon=dev.geekbridge.Calls".to_owned(),
            "--urgency=normal".to_owned(),
            "--category=call.missed".to_owned(),
            "--expire-time=60000".to_owned(),
            "--wait".to_owned(),
        ];
        if number.is_some() {
            args.push("--action=callback=Call back".into());
            args.push("--action=message=Message".into());
        }
        args.push("--action=default=Open".into());
        args.push("--".into());
        args.push("Missed call".into());
        args.push(clean);
        args
    }
}
impl CallNotifier for NotifySendCalls {
    fn missed(&self, caller: &str, number: Option<&str>) {
        let args = Self::missed_args(caller, number);
        let number = number.map(str::to_owned);
        let mut cmd = std::process::Command::new("notify-send");
        cmd.args(args);
        // Queued for the banner pool (bounded, reaped by one thread, spawned off this thread).
        crate::banner_pool::submit(
            &format!("missed:{caller}"),
            cmd,
            MISSED_BANNER_LIMIT,
            move |out| {
                // The number travels in the new window's environment, never on a command line.
                // "Call back" only fills the keypad: the Calls window still asks before anything
                // is dialed.
                match (out.trim(), number) {
                    ("callback", Some(n)) => crate::desktop_notify::open_app_with_env(
                        "geekbridge-calls",
                        "GEEKBRIDGE_DIAL",
                        &n,
                    ),
                    ("message", Some(n)) => crate::desktop_notify::open_app_with_env(
                        "geekbridge-messages",
                        "GEEKBRIDGE_MESSAGE_TO",
                        &n,
                    ),
                    ("default", _) => crate::desktop_notify::open_app("geekbridge-calls"),
                    _ => {}
                }
            },
        );
    }

    fn ringing(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    ) {
        self.ringing_with_replies(caller, can_answer, can_decline, &[], reply);
    }

    fn ringing_with_replies(
        &self,
        caller: &str,
        can_answer: bool,
        can_decline: bool,
        replies: &[String],
        reply: Box<dyn FnOnce(RingChoice) + Send>,
    ) {
        let replies: Vec<String> = replies.iter().take(MAX_REPLIES).cloned().collect();
        let args = Self::args_with_replies(caller, can_answer, can_decline, &replies);
        // The top-right card with Answer / Reject is started by the hub (call_overlay::Supervisor);
        // this banner stays as the additional signal and the fallback.
        let mut cmd = std::process::Command::new("notify-send");
        cmd.args(args);
        crate::banner_pool::submit(
            &format!("ring:{caller}"),
            cmd,
            RING_BANNER_LIMIT,
            move |out| match out.trim() {
                "answer" if can_answer => reply(RingChoice::Answer),
                "decline" if can_decline => reply(RingChoice::Decline),
                other if can_decline && other.starts_with("reply") => {
                    if let Some(i) = other["reply".len()..]
                        .parse::<u8>()
                        .ok()
                        .filter(|i| usize::from(*i) < replies.len())
                    {
                        reply(RingChoice::DeclineWith(i));
                    }
                }
                "default" => crate::desktop_notify::open_app("geekbridge-calls"),
                _ => {}
            },
        );
    }
}

/// A ring banner expires after 30 s on its own; a server that never ends it is cut off shortly after.
const RING_BANNER_LIMIT: std::time::Duration = std::time::Duration::from_secs(35);
/// The missed-call banner asks for 60 s.
const MISSED_BANNER_LIMIT: std::time::Duration = std::time::Duration::from_secs(65);

// ------------------------------------------------------------------ decline with a message

/// What "Decline with message" needs from the rest of the service: the saved texts and the
/// Messages path. The hub decides which call it is about; this decides whether a text may go.
pub trait ReplyTexts: Send + Sync {
    /// The saved texts, else the defaults (at most [`MAX_REPLIES`]).
    fn texts(&self) -> Vec<String>;
    /// Messages is allowed for `device_id` on both sides, the phone is connected and takes texts,
    /// and the send limit has room.
    fn can_send(&self, device_id: &str) -> bool;
    /// Send `text` to `number` through the phone `device_id`; `submission` makes a repeat harmless.
    fn send(
        &self,
        device_id: &str,
        number: &str,
        text: &str,
        submission: &str,
    ) -> Result<(), ErrorBody>;
}

/// The phone a link call row belongs to (`link:<device>|<call>` in its parts or key).
fn link_device(row: &view::CallRow) -> Option<&str> {
    let keys: Vec<&String> = if row.parts.is_empty() {
        vec![&row.key]
    } else {
        row.parts.iter().collect()
    };
    keys.into_iter().find_map(|k| {
        let rest = k.strip_prefix(LINK_PREFIX)?;
        Some(rest.split('|').next().unwrap_or(rest))
    })
}

// ------------------------------------------------------------------ the hub

/// A request for the phone, queued for the link task that owns the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRequest {
    pub request_id: String,
    pub action: LinkAction,
    /// The number of a `dial` request.
    pub number: Option<Sensitive>,
    /// The keypad character of a `tone` request. Never logged or traced: it may be a PIN.
    pub tone: Option<char>,
}

struct LinkPhone {
    /// Which `link_up` created this entry; see [`CallHub::link_down_generation`].
    generation: u64,
    name: String,
    enabled: bool,
    report: Option<LinkCallReport>,
    state: CallState,
    tx: mpsc::Sender<LinkRequest>,
    pending: Vec<(String, LinkAction, Instant)>,
    /// When the last state packet arrived (or the link came up).
    last_state: Instant,
    /// An unanswered "report" ask: when, and how many in a row.
    probe: Option<(Instant, u32)>,
}

/// The rings of a link that dropped, kept for [`Timings::flap_memory`].
struct Suspended {
    device: String,
    /// Part key -> ring number.
    rings: BTreeMap<String, u64>,
    /// Which of those rings had been announced.
    announced: BTreeSet<u64>,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    next_generation: u64,
    links: BTreeMap<String, LinkPhone>,
    notice: Option<String>,
    next_request: u64,
    /// Ring numbers: every source part (`source|call`) of every row that rings now, to the number
    /// of that ring. A ring keeps its number while sources join or leave its row, so a call is
    /// announced once however its sources arrive, and a banner click can tell its ring from a
    /// later one on the same phone (the link always calls its call `link-current`).
    rings: BTreeMap<String, u64>,
    next_ring: u64,
    /// Rings already announced.
    announced: BTreeSet<u64>,
    /// When a ring was first seen without a name or number (see [`UNNAMED_GRACE`]).
    unnamed_since: BTreeMap<u64, Instant>,
    suspended: Vec<Suspended>,
    /// What the call trace last recorded about the view (only changes are written).
    trace: crate::call_trace::ViewMemo,
    /// Calls that ended since the service started (memory only).
    history: crate::call_history::History,
    /// Rings already declined with a text, so a second click cannot send a second one.
    replied: BTreeSet<u64>,
    /// `notice` is the phone's "no such call" for an answer or decline that raced the phone (the
    /// user picked up by hand, the caller gave up); the next report that shows the call moved on
    /// explains it, so it is cleared then.
    race_notice: bool,
    /// Recent keypad tones accepted here (the service's own rate limit, whatever the UI does).
    tone_times: VecDeque<Instant>,
    /// Tones sent to the live call so far: which call (its row key) and how many. Only the count is ever written to
    /// the call trace.
    tone_count: Option<(String, u64)>,
    /// Rings held quiet because the desktop is in Do Not Disturb and the person asked for that. Decided once,
    /// when the ring is first seen, so one ring does not pop a card halfway through.
    quiet: BTreeMap<u64, bool>,
}

/// The phone often reports "ringing" a moment before the caller's number follows. A notification
/// cannot be updated afterwards, so a call without a caller is held back this long before it is
/// announced as "Unknown caller" (a private number never gets one).
const UNNAMED_GRACE: Duration = Duration::from_millis(1500);

pub struct CallHub {
    inner: Mutex<Inner>,
    host: Mutex<Option<HostHandle>>,
    notifier: Mutex<Option<Arc<dyn CallNotifier>>>,
    overlay: Mutex<Option<crate::call_overlay::Supervisor>>,
    /// Asked when a call rings: should the card and the banner stay away (Do Not Disturb, if respected)?
    quiet: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    /// Held for a whole [`Self::announce_new_calls`]: the host thread, packets, the watchdog and the
    /// grace re-check all call it, and a view built before another caller's update must not undo
    /// that update (a ring forgotten for a moment comes back as a new ring with a second banner).
    announcing: Mutex<()>,
    /// "Decline with message": the texts and the Messages path (see [`ReplyTexts`]).
    replies: Mutex<Option<Arc<dyn ReplyTexts>>>,
    /// Wakes the parked watchdog: it runs only while there is something to watch (a ring, a call
    /// on a link, a card, a dropped link's rings), so an idle service gains no wake-ups.
    wake: Arc<(Mutex<bool>, std::sync::Condvar)>,
    timings: Timings,
}

impl CallHub {
    pub fn new() -> Arc<Self> {
        Self::with_timings(Timings::default())
    }

    /// A hub with its own clocks, and its watchdog thread (ends with the hub).
    pub fn with_timings(timings: Timings) -> Arc<Self> {
        let hub = Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            host: Mutex::new(None),
            notifier: Mutex::new(None),
            overlay: Mutex::new(None),
            replies: Mutex::new(None),
            quiet: Mutex::new(None),
            announcing: Mutex::new(()),
            wake: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
            timings,
        });
        let weak = Arc::downgrade(&hub);
        let wake = hub.wake.clone();
        let _ = std::thread::Builder::new()
            .name("geekbridge-calls-watchdog".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(timings.tick);
                    let keep = match weak.upgrade() {
                        Some(hub) => hub.tick(),
                        None => return,
                    };
                    if keep {
                        continue;
                    }
                    // Nothing to watch: park until poked. The timeout only lets the thread notice
                    // that the hub is gone.
                    let (flag, cv) = &*wake;
                    let mut woken = flag.lock().unwrap_or_else(|e| e.into_inner());
                    while !*woken {
                        if weak.strong_count() == 0 {
                            return;
                        }
                        woken = cv
                            .wait_timeout(woken, Duration::from_secs(30))
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                    *woken = false;
                }
            });
        hub
    }

    /// Let the watchdog run (it parks while nothing needs watching).
    fn poke_watchdog(&self) {
        let (flag, cv) = &*self.wake;
        *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_one();
    }

    /// Start the incoming-call card through `launcher` whenever a call rings.
    pub fn set_overlay(&self, launcher: Arc<dyn crate::call_overlay::Launcher>) {
        *self.overlay.lock().unwrap_or_else(|e| e.into_inner()) = Some(
            crate::call_overlay::Supervisor::with_linger(launcher, self.timings.overlay_linger),
        );
    }

    /// Keep the card and the ringing banner away while `quiet` says so. The ring is still recorded in
    /// Recents and shown by the tray; a missed-call banner is left to the desktop's own rules.
    pub fn set_quiet(&self, quiet: Arc<dyn Fn() -> bool + Send + Sync>) {
        *self.quiet.lock().unwrap_or_else(|e| e.into_inner()) = Some(quiet);
    }

    /// Whether the incoming-call card process is up.
    pub fn overlay_running(&self) -> bool {
        self.overlay
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .is_some_and(|o| o.card_running())
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_replies(&self, replies: Arc<dyn ReplyTexts>) {
        *self.replies.lock().unwrap_or_else(|e| e.into_inner()) = Some(replies);
    }

    fn reply_source(&self) -> Option<Arc<dyn ReplyTexts>> {
        self.replies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The texts on offer for `row` (empty unless the call can be declined, came over the phone
    /// link with a plain number, and the Messages path would take a text now).
    fn replies_for(&self, row: &view::CallRow) -> Vec<String> {
        let Some(src) = self.reply_source() else {
            return Vec::new();
        };
        let ok = row.reject.is_some()
            && row
                .number
                .as_ref()
                .and_then(|n| callback_number(n.expose()))
                .is_some()
            && link_device(row).is_some_and(|d| src.can_send(d));
        if !ok {
            return Vec::new();
        }
        let mut t = src.texts();
        t.truncate(MAX_REPLIES);
        t
    }

    pub fn set_notifier(&self, notifier: Arc<dyn CallNotifier>) {
        *self.notifier.lock().unwrap_or_else(|e| e.into_inner()) = Some(notifier);
    }

    /// Start the host Bluetooth backend on its own thread. Replaces a previous one.
    pub fn start_host(self: &Arc<Self>, mut host: Box<dyn HostCalls>) {
        let shared = Arc::new(Mutex::new(HostShared {
            label: host.label(),
            ..HostShared::default()
        }));
        let (commands, command_rx) = std_mpsc::channel::<HostCommand>();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handle = HostHandle {
            shared: shared.clone(),
            commands,
            stop: stop.clone(),
        };
        let hub: Weak<CallHub> = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("geekbridge-calls-host".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    while let Ok(cmd) = command_rx.try_recv() {
                        run_host_command(cmd, Instant::now(), |s, c| host.execute(s, c));
                    }
                    if let Some(hub) = hub.upgrade() {
                        host.set_link_phones(hub.link_phone_names());
                    }
                    let polled = host.poll(HOST_POLL);
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        let before = (g.problem.clone(), g.snapshots.len());
                        match polled {
                            Ok(snaps) => {
                                g.snapshots = snaps;
                                g.problem = None;
                            }
                            Err(problem) => {
                                g.snapshots.clear();
                                g.problem = Some(problem);
                            }
                        }
                        if before != (g.problem.clone(), g.snapshots.len()) {
                            crate::call_trace::record(
                                "bluetooth",
                                &format!(
                                    "phones_for_calls={} problem={}",
                                    g.snapshots.len(),
                                    g.problem.as_deref().unwrap_or("none")
                                ),
                            );
                        }
                        g.notes = host.notes();
                    }
                    match hub.upgrade() {
                        Some(hub) => hub.announce_new_calls(),
                        None => return,
                    }
                    if shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .problem
                        .is_some()
                    {
                        // A broken backend is retried slowly, not in a tight loop.
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            });
        if spawned.is_ok() {
            *self.host.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        }
    }

    // -------------------------------------------------------------- link side

    /// A trusted link to `device_id` is up. The returned receiver yields requests to send to the
    /// phone; dropping it (link gone) is how the hub learns the link is unusable.
    pub fn link_up(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        enabled: bool,
    ) -> mpsc::Receiver<LinkRequest> {
        self.link_up_generation(device_id, name, enabled).0
    }

    /// [`Self::link_up`], also returning the generation for [`Self::link_down_generation`].
    pub fn link_up_generation(
        self: &Arc<Self>,
        device_id: &str,
        name: &str,
        enabled: bool,
    ) -> (mpsc::Receiver<LinkRequest>, u64) {
        let (tx, rx) = mpsc::channel(8);
        let mut inner = self.inner();
        inner.next_generation += 1;
        let generation = inner.next_generation;
        crate::call_trace::record(
            "link",
            &format!(
                "up phone={} calls_consent={enabled}",
                crate::call_trace::hash(device_id)
            ),
        );
        inner.links.insert(
            device_id.to_owned(),
            LinkPhone {
                generation,
                name: name.chars().filter(|c| !c.is_control()).take(80).collect(),
                enabled,
                report: None,
                state: CallState::default(),
                tx,
                pending: Vec::new(),
                last_state: Instant::now(),
                probe: None,
            },
        );
        (rx, generation)
    }

    pub fn link_down(self: &Arc<Self>, device_id: &str) {
        crate::call_trace::record(
            "link",
            &format!("down phone={}", crate::call_trace::hash(device_id)),
        );
        {
            let mut inner = self.inner();
            Self::suspend(&mut inner, device_id);
            inner.links.remove(device_id);
        }
        self.announce_new_calls();
    }

    /// Remember the rings `device_id`'s link was showing: if the link comes back and the phone
    /// still rings, that is the same ring (see [`Timings::flap_memory`]).
    fn suspend(inner: &mut Inner, device_id: &str) {
        let prefix = format!("{LINK_PREFIX}{device_id}|");
        let rings: BTreeMap<String, u64> = inner
            .rings
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        if rings.is_empty() {
            return;
        }
        let announced = rings
            .values()
            .filter(|r| inner.announced.contains(r))
            .copied()
            .collect();
        inner.suspended.retain(|s| s.device != device_id);
        inner.suspended.push(Suspended {
            device: device_id.to_owned(),
            rings,
            announced,
            at: Instant::now(),
        });
    }

    /// Remove `device_id` only if it is still the link `generation` created. A session displaced by
    /// a reconnect ends after its successor registered and must not take the successor down.
    pub fn link_down_generation(self: &Arc<Self>, device_id: &str, generation: u64) {
        {
            let mut inner = self.inner();
            if inner
                .links
                .get(device_id)
                .is_some_and(|p| p.generation != generation)
            {
                return;
            }
            crate::call_trace::record(
                "link",
                &format!("down phone={}", crate::call_trace::hash(device_id)),
            );
            Self::suspend(&mut inner, device_id);
            inner.links.remove(device_id);
        }
        self.announce_new_calls();
    }

    /// Per-device "Telephony" consent changed. Turning it off forgets what the phone reported.
    pub fn set_enabled(self: &Arc<Self>, device_id: &str, enabled: bool) {
        let mut guard = self.inner();
        let inner = &mut *guard;
        if !enabled {
            // Recents hold numbers and names: they go with the consent, link up or not.
            let source = format!("{LINK_PREFIX}{device_id}");
            inner.history.forget_source(&source);
        }
        let Some(phone) = inner.links.get_mut(device_id) else {
            return;
        };
        let was = std::mem::replace(&mut phone.enabled, enabled);
        if !enabled {
            phone.report = None;
            phone.state = CallState::default();
            phone.pending.clear();
        } else if !was {
            // The phone's state was dropped while the consent was off and it only sends changes: ask for it again.
            // No result is expected, so it is not a pending request; a phone that does not know it ignores it.
            inner.next_request += 1;
            let _ = phone.tx.try_send(LinkRequest {
                request_id: format!("s{}", inner.next_request),
                action: LinkAction::Report,
                number: None,
                tone: None,
            });
        }
    }

    /// The phone was unpaired: its Recents (numbers and names) are forgotten.
    pub fn forget_device(self: &Arc<Self>, device_id: &str) {
        let source = format!("{LINK_PREFIX}{device_id}");
        self.inner().history.forget_source(&source);
    }

    /// A `geekbridge.calls.*` packet arrived. False if it was malformed or unexpected (the caller
    /// counts that as misbehaviour); packets from a device without consent are dropped quietly.
    pub fn on_packet(
        self: &Arc<Self>,
        device_id: &str,
        kind: &str,
        body: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        let ok = {
            let mut guard = self.inner();
            let Inner {
                links,
                notice,
                suspended,
                race_notice,
                history,
                ..
            } = &mut *guard;
            let Some(phone) = links.get_mut(device_id) else {
                return true;
            };
            if !phone.enabled {
                return true;
            }
            match kind {
                link::STATE => match LinkCallReport::parse(body) {
                    Ok(report) => {
                        let seen = (report.state, phone.report.as_ref().map(|r| r.state));
                        if seen.1 != Some(seen.0) {
                            crate::call_trace::record(
                                "link",
                                &format!(
                                    "state phone={} {} -> {} can_answer={} can_end={} number={}",
                                    crate::call_trace::hash(device_id),
                                    seen.1.map_or("none", |s| s.as_str()),
                                    seen.0.as_str(),
                                    report.can_answer,
                                    report.can_end,
                                    report.number.as_ref().map_or("-".to_owned(), |n| {
                                        crate::call_trace::tail(n.expose())
                                    }),
                                ),
                            );
                        }
                        report.apply(&mut phone.state);
                        phone.last_state = Instant::now();
                        phone.probe = None;
                        if report.state != link::PhoneState::Idle {
                            self.poke_watchdog();
                        }
                        if report.state != link::PhoneState::Ringing {
                            // The phone's word on the ring that a dropped link left open.
                            suspended.retain(|s| s.device != device_id);
                        }
                        if notice.as_deref() == Some(STALE_CALL_NOTICE) {
                            // The phone confirms its calls again.
                            *notice = None;
                        }
                        if std::mem::take(race_notice) && report.state != link::PhoneState::Ringing
                        {
                            *notice = None;
                        }
                        phone.report = Some(report);
                        if phone.state.calls.is_empty() {
                            // A fresh call starts without the previous call's refusal.
                            *notice = None;
                        }
                        true
                    }
                    Err(_) => false,
                },
                link::RESULT => match link::parse_result(body) {
                    Ok((id, outcome)) => {
                        crate::call_trace::record(
                            "request",
                            &format!(
                                "phone replied to {id} on {}: {}",
                                crate::call_trace::hash(device_id),
                                match &outcome {
                                    Ok(()) => "ok".to_owned(),
                                    Err(reason) => format!("{reason:?}").to_lowercase(),
                                }
                            ),
                        );
                        let action = phone
                            .pending
                            .iter()
                            .find(|(p, _, _)| *p == id)
                            .map(|(_, a, _)| *a);
                        phone.pending.retain(|(p, _, _)| *p != id);
                        if let (Some(action), Err(reason)) = (action, outcome) {
                            if action == LinkAction::Reject {
                                // Queued is not declined: the ring that follows is a miss.
                                history.undeclined(&format!("{LINK_PREFIX}{device_id}"));
                            }
                            // "No such call" for what already happened (the phone was answered by
                            // hand a moment before the click, a decline raced the caller hanging
                            // up, a second click) is not a failure worth a notice.
                            let done = reason == LinkFailure::NoSuchCall
                                && match action {
                                    LinkAction::Answer => {
                                        phone.state.calls.values().any(|c| !c.phase.is_ringing())
                                    }
                                    LinkAction::Reject | LinkAction::Hangup => {
                                        phone.state.calls.is_empty()
                                            || phone
                                                .report
                                                .as_ref()
                                                .is_some_and(|r| r.state == link::PhoneState::Idle)
                                    }
                                    _ => false,
                                };
                            if !done {
                                *notice = Some(
                                    match (action, reason) {
                                        (LinkAction::Tone, LinkFailure::RateLimited) => {
                                            "The phone is limiting keypad tones; wait a moment."
                                        }
                                        (LinkAction::Tone, LinkFailure::NoSuchCall) => {
                                            "The call had changed, so the tone was not played."
                                        }
                                        (LinkAction::Tone, LinkFailure::PermissionMissing) => {
                                            "The phone cannot play keypad tones now."
                                        }
                                        (LinkAction::Tone, LinkFailure::Failed) => {
                                            "The phone could not play that tone."
                                        }
                                        _ => reason.message(),
                                    }
                                    .to_owned(),
                                );
                                *race_notice = reason == LinkFailure::NoSuchCall
                                    && matches!(action, LinkAction::Answer | LinkAction::Reject);
                            }
                        }
                        action.is_some()
                    }
                    Err(_) => false,
                },
                _ => false,
            }
        };
        self.announce_new_calls();
        ok
    }

    // -------------------------------------------------------------- view

    /// Every source's current snapshot (what the view is built from), without the status lines.
    fn snapshots(&self) -> Vec<SourceSnapshot> {
        let mut snaps = Vec::new();
        {
            let inner = self.inner();
            for (id, p) in &inner.links {
                if let (true, Some(report)) = (p.enabled, &p.report) {
                    snaps.push(SourceSnapshot {
                        key: format!("{LINK_PREFIX}{id}"),
                        label: p.name.clone(),
                        caps: report.capabilities(),
                        state: p.state.clone(),
                    });
                }
            }
        }
        if let Some(host) = self.host.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let g = host.shared.lock().unwrap_or_else(|e| e.into_inner());
            snaps.extend(g.snapshots.iter().cloned());
        }
        snaps
    }

    fn sources(&self) -> (Vec<SourceSnapshot>, Vec<String>) {
        let mut snaps = Vec::new();
        let mut lines = Vec::new();
        {
            let inner = self.inner();
            for (id, p) in &inner.links {
                if !p.enabled {
                    lines.push(format!(
                        "Phone link ({}): allow Calls for this phone in its permissions to see and answer calls from here.",
                        p.name
                    ));
                } else if let Some(report) = &p.report {
                    snaps.push(SourceSnapshot {
                        key: format!("{LINK_PREFIX}{id}"),
                        label: p.name.clone(),
                        caps: report.capabilities(),
                        state: p.state.clone(),
                    });
                    if !report.can_answer && !report.can_dial {
                        lines.push(format!(
                            "Phone link ({}): calls are visible, but the phone has not allowed GeekBridge to answer or place them.",
                            p.name
                        ));
                    } else if !report.can_dial {
                        lines.push(format!(
                            "Phone link ({}): ready to answer; to dial from here, allow GeekBridge to make phone calls in the phone app's setup.",
                            p.name
                        ));
                    } else {
                        lines.push(format!("Phone link ({}): ready.", p.name));
                    }
                } else {
                    lines.push(format!(
                        "Phone link ({}): no call state received. On the phone, turn on \"Share call status with my computer\" in Setup > Answer calls (or update the phone app).",
                        p.name
                    ));
                }
            }
        }
        if let Some(host) = self.host.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let g = host.shared.lock().unwrap_or_else(|e| e.into_inner());
            match &g.problem {
                Some(problem) => lines.push(format!("{}: {problem}", g.label)),
                None if g.snapshots.is_empty() => {
                    lines.push(format!("{}: no phone connected for calls.", g.label))
                }
                None => lines.push(format!("{}: ready.", g.label)),
            }
            lines.extend(g.notes.iter().cloned());
            snaps.extend(g.snapshots.iter().cloned());
        } else {
            lines.push("Bluetooth hands-free calling is not available on this computer.".into());
        }
        // Why the voice cannot come here (no adapter / not paired / ready) and the next step.
        #[cfg(all(target_os = "linux", not(test)))]
        if !snaps.iter().any(|s| s.key.starts_with("hfp:")) {
            lines.push(bridge_calls::readiness::detect().line());
        }
        (snaps, lines)
    }

    /// Why the voice cannot come to this computer, when control works anyway. Only on Linux, only
    /// while no hands-free phone is registered (the same facts as the `sources` line).
    fn audio_note(snaps: &[SourceSnapshot]) -> Option<String> {
        let _ = snaps;
        #[cfg(all(target_os = "linux", not(test)))]
        if !snaps.iter().any(|s| s.key.starts_with("hfp:")) {
            let r = bridge_calls::readiness::detect();
            if r != bridge_calls::readiness::Readiness::Ready {
                return Some(r.line());
            }
        }
        None
    }

    pub fn view(&self) -> CallsView {
        view::build(&self.snapshots())
    }

    pub fn status(&self) -> CallsStatus {
        let (snaps, sources) = self.sources();
        // A problem with a live call's audio (with its fix) wins over the general readiness note.
        let audio_problem = self
            .host_notes()
            .into_iter()
            .find(|l| !l.starts_with("Call audio:"));
        let mut view = view::build(&snaps);
        let texts = self.reply_source().map(|r| r.texts()).unwrap_or_default();
        let offered = view
            .ringing()
            .is_some_and(|r| !self.replies_for(r).is_empty());
        let inner = self.inner();
        let offered = offered
            && view.ringing().is_some_and(|r| {
                !r.parts
                    .iter()
                    .chain(std::iter::once(&r.key))
                    .filter_map(|p| inner.rings.get(p))
                    .any(|n| inner.replied.contains(n))
            });
        view.recent = inner.history.recent(Instant::now());
        CallsStatus {
            view,
            notice: inner.notice.clone(),
            sources,
            audio_note: audio_problem.or_else(|| Self::audio_note(&snaps)),
            wifi: None,
            replies: DeclineReplies { texts, offered },
        }
    }

    /// A call is live (not just ringing) on this phone, as its GeekBridge link reports it.
    pub fn phone_in_call(&self, device_id: &str) -> bool {
        self.inner()
            .links
            .get(device_id)
            .is_some_and(|p| p.enabled && p.state.calls.values().any(|c| !c.phase.is_ringing()))
    }

    /// Names of the phones whose GeekBridge link is up.
    pub fn link_phone_names(&self) -> Vec<String> {
        self.inner()
            .links
            .values()
            .map(|p| p.name.clone())
            .collect()
    }

    /// The host's call-audio lines (where a live call's audio plays and what is wrong).
    pub fn host_notes(&self) -> Vec<String> {
        self.host
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|h| {
                h.shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .notes
                    .clone()
            })
            .unwrap_or_default()
    }

    /// Send a live call's audio back to the phone. Only Bluetooth hands-free sources carry the
    /// audio here, so only they can.
    pub fn audio_to_phone(&self, source: &str) -> Result<(), ErrorBody> {
        let result = self.audio_to_phone_inner(source);
        crate::call_trace::record(
            "request",
            &format!(
                "audio to phone src={}: {}",
                crate::call_trace::hash(source),
                outcome(&result)
            ),
        );
        result
    }

    fn audio_to_phone_inner(&self, source: &str) -> Result<(), ErrorBody> {
        let live = self.view().rows.iter().any(|r| {
            r.hangup
                .as_ref()
                .is_some_and(|t| t.source == source && t.path == ControlPath::Hfp)
        });
        if !live {
            return Err(err(
                ErrorCode::Unsupported,
                "no call's audio from that phone is on this computer",
            ));
        }
        self.send_host(source, &CallCommand::DisconnectAudio)
    }

    // -------------------------------------------------------------- control

    /// Execute a user's decision. Blocking (host commands wait for the backend thread).
    pub fn act(
        self: &Arc<Self>,
        target: &ActionTarget,
        action: CallAction,
    ) -> Result<(), ErrorBody> {
        let trace_what = format!(
            "{action:?} call={} via={}",
            crate::call_trace::hash(&format!("{}|{}", target.source, target.call.as_str())),
            match target.path {
                ControlPath::Hfp => "bluetooth",
                ControlPath::Link => "phone-link",
            }
        )
        .to_lowercase();
        let result = self.act_inner(target, action);
        crate::call_trace::record("request", &format!("{trace_what}: {}", outcome(&result)));
        result
    }

    fn act_inner(
        self: &Arc<Self>,
        target: &ActionTarget,
        action: CallAction,
    ) -> Result<(), ErrorBody> {
        let view = self.view();
        let offered = view.rows.iter().any(|r| {
            let slot = match action {
                CallAction::Answer => &r.answer,
                CallAction::Reject => &r.reject,
                CallAction::Hangup => &r.hangup,
            };
            slot.as_ref() == Some(target)
        });
        if !offered {
            return Err(err(
                ErrorCode::NotFound,
                "that call changed; nothing was sent",
            ));
        }
        let call = target.call.clone();
        let command = match action {
            CallAction::Answer => CallCommand::Answer { call },
            CallAction::Reject => CallCommand::Reject { call },
            CallAction::Hangup => CallCommand::Hangup { call },
        };
        let result = match target.path {
            ControlPath::Link => {
                let wire = match action {
                    CallAction::Answer => LinkAction::Answer,
                    CallAction::Reject => LinkAction::Reject,
                    CallAction::Hangup => LinkAction::Hangup,
                };
                self.send_link(&target.source, wire, None, None)
            }
            ControlPath::Hfp => self.send_host(&target.source, &command),
        };
        let mut inner = self.inner();
        match &result {
            Ok(()) => inner.notice = None,
            Err(e) => inner.notice = Some(e.message.clone()),
        }
        if result.is_ok() && action == CallAction::Reject {
            // So the ring that follows is listed as declined, not missed (and gets no banner).
            for r in view
                .rows
                .iter()
                .filter(|r| r.reject.as_ref() == Some(target))
            {
                inner.history.declined(&if r.parts.is_empty() {
                    vec![r.key.clone()]
                } else {
                    r.parts.clone()
                });
            }
        }
        result
    }

    /// Decline the ringing call `target` offers and send it saved text number `index` through the
    /// phone's Messages path. Everything is checked before the call is touched, so a refusal
    /// leaves it ringing. The decline is the point: once it is done it stays done, and a text the
    /// phone then refuses is reported in the notice, not as a failed decline. A second request for
    /// the same ring does nothing.
    pub fn decline_with_reply(
        self: &Arc<Self>,
        target: &ActionTarget,
        index: usize,
    ) -> Result<(), ErrorBody> {
        self.decline_with_reply_expecting(target, index, None)
    }

    /// As [`Self::decline_with_reply`], but refuse when text `index` is no longer `expect` (a button
    /// was labelled with the list as it was when the call rang; the list may have been edited since).
    pub fn decline_with_reply_expecting(
        self: &Arc<Self>,
        target: &ActionTarget,
        index: usize,
        expect: Option<&str>,
    ) -> Result<(), ErrorBody> {
        let result = self.decline_with_reply_inner(target, index, expect);
        crate::call_trace::record(
            "request",
            &format!("decline-with-text reply={index}: {}", outcome(&result)),
        );
        result
    }

    fn decline_with_reply_inner(
        self: &Arc<Self>,
        target: &ActionTarget,
        index: usize,
        expect: Option<&str>,
    ) -> Result<(), ErrorBody> {
        let src = self.reply_source().ok_or_else(|| {
            err(
                ErrorCode::Unsupported,
                "declining with a message is not available",
            )
        })?;
        let view = self.view();
        let row = view
            .rows
            .iter()
            .find(|r| r.ringing() && r.reject.as_ref() == Some(target))
            .ok_or_else(|| err(ErrorCode::NotFound, "that call changed; nothing was sent"))?;
        let texts = self.replies_for(row);
        if texts.is_empty() {
            return Err(err(
                ErrorCode::Unsupported,
                "a text cannot be sent for this call; nothing was sent",
            ));
        }
        let text = texts
            .get(index)
            .cloned()
            .ok_or_else(|| err(ErrorCode::BadRequest, "no such reply"))?;
        if expect.is_some_and(|e| e != text) {
            return Err(err(
                ErrorCode::BadRequest,
                "the saved texts changed; nothing was sent",
            ));
        }
        let (Some(device), Some(number)) = (
            link_device(row).map(str::to_owned),
            row.number
                .as_ref()
                .and_then(|n| callback_number(n.expose())),
        ) else {
            return Err(err(
                ErrorCode::Unsupported,
                "this call has no number to text",
            ));
        };
        let ring = {
            let inner = self.inner();
            row.parts
                .iter()
                .chain(std::iter::once(&row.key))
                .find_map(|p| inner.rings.get(p).copied())
        };
        // Without a ring number a repeat could not be recognised and would send a second text.
        let ring =
            ring.ok_or_else(|| err(ErrorCode::NotFound, "that call changed; nothing was sent"))?;
        if !self.inner().replied.insert(ring) {
            return Ok(());
        }
        if let Err(e) = self.act(target, CallAction::Reject) {
            self.inner().replied.remove(&ring);
            return Err(e);
        }
        let submission = format!(
            "dr{}-{}",
            ring,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis())
        );
        let sent = src.send(&device, &number, &text, &submission);
        crate::call_trace::record(
            "reply",
            &format!(
                "text after decline: {}",
                match &sent {
                    Ok(()) => "handed to the phone".to_owned(),
                    Err(e) => format!("not sent ({:?})", e.code),
                }
            ),
        );
        if let Err(e) = sent {
            self.inner().notice = Some(format!(
                "Declined, but the text was not sent: {}",
                e.message
            ));
        }
        Ok(())
    }

    pub fn mute(&self, source: &str, muted: bool) -> Result<(), ErrorBody> {
        let result = self.mute_inner(source, muted);
        crate::call_trace::record("request", &format!("mute={muted}: {}", outcome(&result)));
        result
    }

    fn mute_inner(&self, source: &str, muted: bool) -> Result<(), ErrorBody> {
        let offered = self.view().mute.is_some_and(|m| m.source == source);
        if !offered {
            return Err(err(ErrorCode::Unsupported, "muting is not available now"));
        }
        self.send_host(source, &CallCommand::SetMicrophoneMute { muted })
    }

    /// Send one keypad tone (DTMF) into the live call. `source` must be the `tones` target the current view offers
    /// (a live, answered call and a source that takes tones); `tone` must be one of `0-9 * #`. Rate-limited here and
    /// again on the phone. The character is never logged or traced (it may be a PIN); the trace gets the number
    /// of tones sent to this call. Blocking.
    pub fn tone(&self, source: &str, tone: &Sensitive) -> Result<(), ErrorBody> {
        let result = self.tone_inner(source, tone);
        let count = self.inner().tone_count.as_ref().map_or(0, |(_, n)| *n);
        crate::call_trace::record(
            "request",
            &format!(
                "keypad tones sent={count} src={}: {}",
                crate::call_trace::hash(source),
                outcome(&result)
            ),
        );
        result
    }

    fn tone_inner(&self, source: &str, tone: &Sensitive) -> Result<(), ErrorBody> {
        let mut chars = tone.expose().chars();
        let c = match (chars.next(), chars.next()) {
            (Some(c), None) if link::valid_tone(c) => c,
            _ => {
                return Err(err(
                    ErrorCode::BadRequest,
                    "that is not a keypad tone (one of 0-9, * and #)",
                ));
            }
        };
        let view = self.view();
        let Some(offered) = view.tones.clone().filter(|t| t.source == source) else {
            return Err(err(
                ErrorCode::NotFound,
                "keypad tones are not available now; nothing was sent",
            ));
        };
        let key = view.live().map(|r| r.key.clone()).unwrap_or_default();
        {
            let mut inner = self.inner();
            let now = Instant::now();
            while inner
                .tone_times
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) >= TONE_WINDOW)
            {
                inner.tone_times.pop_front();
            }
            if inner.tone_times.len() >= MAX_TONES {
                return Err(err(ErrorCode::Busy, "too many keypad tones; wait a moment"));
            }
            inner.tone_times.push_back(now);
        }
        let result = match offered.path {
            ControlPath::Link => self.send_link(source, LinkAction::Tone, None, Some(c)),
            ControlPath::Hfp => self.send_host(
                source,
                &CallCommand::SendTones {
                    tones: tone.clone(),
                },
            ),
        };
        let mut inner = self.inner();
        if result.is_ok() {
            match &mut inner.tone_count {
                Some((k, n)) if *k == key => *n += 1,
                other => *other = Some((key, 1)),
            }
        }
        inner.notice = result.as_ref().err().map(|e| e.message.clone());
        result
    }

    pub fn connect_audio(&self, source: &str) -> Result<(), ErrorBody> {
        let result = self.connect_audio_inner(source);
        crate::call_trace::record(
            "request",
            &format!(
                "audio to computer src={}: {}",
                crate::call_trace::hash(source),
                outcome(&result)
            ),
        );
        result
    }

    fn connect_audio_inner(&self, source: &str) -> Result<(), ErrorBody> {
        let offered = self.view().audio.connect.as_deref() == Some(source);
        if !offered {
            return Err(err(
                ErrorCode::Unsupported,
                "moving the audio to this computer is not available now",
            ));
        }
        self.send_host(source, &CallCommand::ConnectAudio)
    }

    /// Place a call. `source` must be the dial target the current view offers (which already means:
    /// consent for Telephony is on, the phone reported it may place calls and no call is going on),
    /// and `number` must be plain digits. Blocking.
    pub fn dial(self: &Arc<Self>, source: &str, number: &Sensitive) -> Result<(), ErrorBody> {
        let result = self.dial_inner(source, number);
        crate::call_trace::record(
            "request",
            &format!(
                "dial number={} src={}: {}",
                crate::call_trace::tail(number.expose()),
                crate::call_trace::hash(source),
                outcome(&result)
            ),
        );
        result
    }

    fn dial_inner(self: &Arc<Self>, source: &str, number: &Sensitive) -> Result<(), ErrorBody> {
        if !link::valid_dial_number(number.expose()) {
            return Err(err(
                ErrorCode::BadRequest,
                "that is not a number this computer can dial (digits, optional leading +)",
            ));
        }
        let offered = self.view().dial.filter(|d| d.source == source);
        let Some(offered) = offered else {
            return Err(err(
                ErrorCode::NotFound,
                "dialing is not available right now; nothing was sent",
            ));
        };
        let result = match offered.path {
            ControlPath::Link => self.send_link(source, LinkAction::Dial, Some(number), None),
            ControlPath::Hfp => self.send_host(
                source,
                &CallCommand::Dial {
                    number: number.clone(),
                },
            ),
        };
        let mut inner = self.inner();
        match &result {
            Ok(()) => inner.notice = None,
            Err(e) => inner.notice = Some(e.message.clone()),
        }
        result
    }

    fn send_link(
        &self,
        source: &str,
        wire: LinkAction,
        dial: Option<&Sensitive>,
        tone: Option<char>,
    ) -> Result<(), ErrorBody> {
        let device_id = source
            .strip_prefix(LINK_PREFIX)
            .ok_or_else(|| err(ErrorCode::BadRequest, "unknown call source"))?;
        let mut inner = self.inner();
        inner.next_request += 1;
        let request_id = format!("c{}", inner.next_request);
        let phone = inner
            .links
            .get_mut(device_id)
            .ok_or_else(|| err(ErrorCode::NotConnected, "the phone is not connected"))?;
        if !phone.enabled {
            // Consent was turned off after the request passed the view check.
            return Err(err(ErrorCode::NotConnected, "the phone is not connected"));
        }
        phone
            .pending
            .retain(|(_, _, at)| at.elapsed() < PENDING_TTL);
        if !matches!(wire, LinkAction::Dial | LinkAction::Tone)
            && phone.pending.iter().any(|(_, a, _)| *a == wire)
        {
            // A second Answer (double click, the banner after the card) while the first is on its
            // way: the phone is already doing it, so nothing more is sent.
            return Ok(());
        }
        // Keypad taps come faster than answers and have their own bound (the rate limit); they must neither fill the
        // few slots Answer/Hang up/Dial need nor be refused by them.
        let tones = phone
            .pending
            .iter()
            .filter(|(_, a, _)| *a == LinkAction::Tone)
            .count();
        let full = if wire == LinkAction::Tone {
            tones >= MAX_TONES
        } else {
            phone.pending.len() - tones >= MAX_PENDING
        };
        if full {
            return Err(err(ErrorCode::Busy, "the phone has not answered yet"));
        }
        phone
            .tx
            .try_send(LinkRequest {
                request_id: request_id.clone(),
                action: wire,
                number: dial.cloned(),
                tone,
            })
            .map_err(|_| err(ErrorCode::NotConnected, "the phone link is busy or closed"))?;
        phone.pending.push((request_id, wire, Instant::now()));
        Ok(())
    }

    fn send_host(&self, source: &str, command: &CallCommand) -> Result<(), ErrorBody> {
        let (reply, wait) = std_mpsc::sync_channel(1);
        {
            let host = self.host.lock().unwrap_or_else(|e| e.into_inner());
            let host = host
                .as_ref()
                .ok_or_else(|| err(ErrorCode::Unsupported, "Bluetooth calling is not available"))?;
            host.commands
                .send(HostCommand {
                    source: source.to_owned(),
                    command: command.clone(),
                    reply,
                    deadline: Instant::now() + HOST_COMMAND_TIMEOUT,
                })
                .map_err(|_| err(ErrorCode::Failed, "Bluetooth calling stopped"))?;
        }
        match wait.recv_timeout(HOST_COMMAND_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(why)) => Err(err(ErrorCode::Failed, &why)),
            Err(_) => Err(err(
                ErrorCode::Failed,
                "Bluetooth calling did not answer in time; check the phone before retrying",
            )),
        }
    }

    // -------------------------------------------------------------- ringing

    /// The watchdog: runs every [`Timings::tick`]. Asks a quiet phone that is ringing or in a call
    /// for its state and clears the call when it never answers, forgets old link flaps, and lets the
    /// card supervisor reap or stop the card process.
    /// True while there is something left to watch.
    pub fn tick(self: &Arc<Self>) -> bool {
        let t = self.timings;
        let now = Instant::now();
        let busy = {
            let mut guard = self.inner();
            let inner = &mut *guard;
            let mut cleared = Vec::new();
            for (device, p) in inner.links.iter_mut() {
                let Some(report) = p.report.as_ref().filter(|_| p.enabled) else {
                    continue;
                };
                if report.state == link::PhoneState::Idle {
                    p.probe = None;
                    continue;
                }
                let quiet = if report.state == link::PhoneState::Ringing {
                    t.ring_quiet
                } else {
                    t.live_quiet
                };
                let ask = match p.probe {
                    None => now.saturating_duration_since(p.last_state) >= quiet,
                    Some((at, n)) if now.saturating_duration_since(at) >= t.probe_grace => {
                        if n >= t.probes {
                            // Never confirmed: end it here (the reducer records it as ended).
                            let idle = LinkCallReport {
                                state: link::PhoneState::Idle,
                                number: None,
                                name: None,
                                ..report.clone()
                            };
                            idle.apply(&mut p.state);
                            p.report = Some(idle);
                            p.probe = None;
                            cleared.push(device.clone());
                            continue;
                        }
                        true
                    }
                    Some(_) => false,
                };
                if ask {
                    inner.next_request += 1;
                    let _ = p.tx.try_send(LinkRequest {
                        request_id: format!("s{}", inner.next_request),
                        action: LinkAction::Report,
                        number: None,
                        tone: None,
                    });
                    p.probe = Some((now, p.probe.map_or(1, |(_, n)| n + 1)));
                }
            }
            for device in &cleared {
                // Nobody knows how this ring ended: not a missed call.
                let source = format!("{LINK_PREFIX}{device}");
                inner.history.forget_open(&source);
                // A late answer that the phone still rings is the same ring, not a new banner.
                Self::suspend(inner, device);
            }
            if !cleared.is_empty() {
                inner.notice = Some(STALE_CALL_NOTICE.to_owned());
            }
            !cleared.is_empty()
                || !inner.rings.is_empty()
                || !inner.suspended.is_empty()
                || !inner.unnamed_since.is_empty()
        };
        let card = self.overlay_running();
        if busy || card {
            self.announce_new_calls();
        }
        let inner = self.inner();
        busy || card
            || !inner.rings.is_empty()
            || !inner.suspended.is_empty()
            || inner.links.values().any(|p| {
                p.enabled
                    && p.report
                        .as_ref()
                        .is_some_and(|r| r.state != link::PhoneState::Idle)
            })
    }

    /// Number the rings in `view`, announce the new ones (banner once per ring, after
    /// [`UNNAMED_GRACE`] when there is no caller yet), keep the card supervisor in step and forget
    /// rings that are over.
    fn announce_new_calls(self: &Arc<Self>) {
        let _one_at_a_time = self.announcing.lock().unwrap_or_else(|e| e.into_inner());
        let snaps = self.snapshots();
        let view = view::build(&snaps);
        let live_sources: BTreeSet<String> = snaps.iter().map(|s| s.key.clone()).collect();
        let now = Instant::now();
        let flap = self.timings.flap_memory;
        let live_now = view.live().is_some();
        let quiet_now = view.rows.iter().any(|r| r.ringing())
            && self
                .quiet
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .is_some_and(|q| q());
        let (fresh, ringing_now, loud, missed) = {
            let mut guard = self.inner();
            let inner = &mut *guard;
            inner.trace.observe(&view);
            let missed = inner.history.observe(&view, &live_sources, now);
            inner
                .suspended
                .retain(|s| now.saturating_duration_since(s.at) < flap);
            let mut rings = BTreeMap::new();
            let mut ringing_now = BTreeSet::new();
            let mut rows = Vec::new();
            for r in view.rows.iter().filter(|r| r.ringing()) {
                let parts: Vec<String> = if r.parts.is_empty() {
                    vec![r.key.clone()]
                } else {
                    r.parts.clone()
                };
                let known = parts.iter().find_map(|p| inner.rings.get(p).copied());
                let resumed = known.is_none().then(|| {
                    inner.suspended.iter().find_map(|s| {
                        parts.iter().find_map(|p| {
                            s.rings
                                .get(p)
                                .map(|n| (*n, s.announced.contains(n), s.device.clone()))
                        })
                    })
                });
                let id = match (known, resumed.flatten()) {
                    (Some(id), _) => id,
                    (None, Some((id, was_announced, device))) => {
                        // The same ring after a reconnect: it keeps its number and its banner.
                        inner.suspended.retain(|s| s.device != device);
                        if was_announced {
                            inner.announced.insert(id);
                        }
                        id
                    }
                    (None, None) => {
                        inner.next_ring += 1;
                        // A new call starts without the previous call's refusal.
                        inner.notice = None;
                        inner.next_ring
                    }
                };
                for p in parts {
                    rings.insert(p, id);
                }
                if ringing_now.insert(id) {
                    if let std::collections::btree_map::Entry::Vacant(e) = inner.quiet.entry(id) {
                        e.insert(quiet_now);
                        if quiet_now {
                            crate::call_trace::record("ring", "kept quiet: Do Not Disturb");
                        }
                    }
                    rows.push((id, r));
                }
            }
            inner.rings = rings;
            inner.quiet.retain(|n, _| ringing_now.contains(n));
            let before = inner.announced.len();
            inner.announced.retain(|n| ringing_now.contains(n));
            if inner.announced.len() != before {
                crate::tray::poke(); // a call stopped ringing
            }
            inner.unnamed_since.retain(|n, _| ringing_now.contains(n));
            let mut recheck = false;
            let mut fresh = Vec::new();
            for (id, r) in rows {
                if inner.announced.contains(&id) {
                    continue;
                }
                if r.name.is_none() && r.number.is_none() {
                    let since = *inner.unnamed_since.entry(id).or_insert(now);
                    if now.saturating_duration_since(since) < UNNAMED_GRACE {
                        recheck = true;
                        continue;
                    }
                }
                inner.announced.insert(id);
                crate::tray::poke(); // a call started ringing
                if !inner.quiet.get(&id).copied().unwrap_or(false) {
                    fresh.push((id, r.clone()));
                }
            }
            if recheck {
                let hub = Arc::downgrade(self);
                std::thread::spawn(move || {
                    std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(100));
                    if let Some(hub) = hub.upgrade() {
                        hub.announce_new_calls();
                    }
                });
            }
            // The card is for the rings that are not held quiet.
            let loud: BTreeSet<u64> = ringing_now
                .iter()
                .copied()
                .filter(|n| !inner.quiet.get(n).copied().unwrap_or(false))
                .collect();
            (fresh, ringing_now, loud, missed)
        };
        // The card does not wait for the caller's name: it shows it when it comes.
        if let Some(o) = self
            .overlay
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            o.update(&loud, live_now, now);
            if o.card_running() {
                self.poke_watchdog();
            }
        }
        if !ringing_now.is_empty() || !self.inner().suspended.is_empty() {
            self.poke_watchdog();
        }
        let Some(notifier) = self
            .notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        else {
            return;
        };
        for call in &missed {
            notifier.missed(&call.caller_label(), call.callback_number().as_deref());
        }
        for (ring, row) in fresh {
            let hub = self.clone();
            let answer = row.answer.clone();
            let reject = row.reject.clone();
            let (can_answer, can_decline) = (answer.is_some(), reject.is_some());
            let replies = self.replies_for(&row);
            let labelled = replies.clone();
            notifier.ringing_with_replies(
                &row.caller_label(),
                can_answer,
                can_decline,
                &replies,
                Box::new(move |choice| {
                    let target = match (choice, answer, reject) {
                        (RingChoice::Answer, Some(t), _) => (t, CallAction::Answer),
                        (RingChoice::Decline, _, Some(t)) => (t, CallAction::Reject),
                        (RingChoice::DeclineWith(i), _, Some(t)) => {
                            if hub.still_ringing(ring) {
                                let i = usize::from(i);
                                let _ = hub.decline_with_reply_expecting(
                                    &t,
                                    i,
                                    labelled.get(i).map(String::as_str),
                                );
                            }
                            return;
                        }
                        _ => return,
                    };
                    // A banner outlives its ring (it waits up to 30 s). A click on it must not
                    // answer a later ring on the same phone, which has the same call id.
                    if !hub.still_ringing(ring) {
                        return;
                    }
                    let _ = hub.act(&target.0, target.1);
                }),
            );
        }
    }

    /// Whether ring number `ring` is still ringing.
    fn still_ringing(&self, ring: u64) -> bool {
        self.inner().rings.values().any(|n| *n == ring)
    }
}

// ------------------------------------------------------------------ Linux host

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use bridge_calls::{DeviceRef, Session};
    use bridge_core::calls::{CallFeature, UnsupportedReason};

    use bridge_calls::audio::Want;
    use bridge_core::calls::{AudioCodec, AudioLinkState, CallEvent, MuteScope};

    /// This computer as the Bluetooth hands-free unit, through `bridge-calls` (PipeWire telephony),
    /// with the call's audio on the headphones and microphone the user picked
    /// ([`crate::call_audio_host::HostAudio`]).
    pub struct LinuxHfp {
        session: Session,
        devices: BTreeMap<String, DeviceRef>,
        audio: Option<crate::call_audio_host::HostAudio>,
    }
    impl LinuxHfp {
        /// Call control only: the call's audio is left to WirePlumber's defaults.
        pub fn new(session: Session) -> Self {
            Self {
                session,
                devices: BTreeMap::new(),
                audio: None,
            }
        }
        /// Call control plus call audio on this computer; picks and the restore file live in
        /// `state_dir`.
        pub fn with_audio(session: Session, state_dir: &std::path::Path) -> Self {
            Self {
                session,
                devices: BTreeMap::new(),
                audio: Some(crate::call_audio_host::HostAudio::new(state_dir)),
            }
        }
    }
    impl Drop for LinuxHfp {
        fn drop(&mut self) {
            if let Some(a) = &mut self.audio {
                a.shutdown(&self.session);
            }
        }
    }
    impl HostCalls for LinuxHfp {
        fn label(&self) -> String {
            "Bluetooth hands-free (Linux)".into()
        }
        fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
            self.session.pump(wait).map_err(|e| e.to_string())?;
            if let Some(a) = &mut self.audio {
                a.tick(&self.session);
            }
            self.devices.clear();
            let mut out = Vec::new();
            for (device, state, caps) in self.session.phones() {
                let key = format!("hfp:{}", device.path);
                let mut caps = caps.clone();
                let mut state = state.clone();
                // Mute acts on the microphone the call uses, so it exists only while the call's
                // audio is on this computer.
                match self.audio.as_ref().and_then(|a| a.status(device)) {
                    Some(s) if self.audio.as_ref().is_some_and(|a| a.routed(device)) => {
                        caps.set(CallFeature::MuteMicrophone, Support::Declared);
                        // The call's voice streams exist and play here: that is the audio link,
                        // even if the telephony service has not reported its transport yet.
                        if !matches!(state.audio, AudioLinkState::Connected { .. }) {
                            let codec = match &s.place {
                                bridge_calls::audio::Place::Computer { codec, .. } => {
                                    match codec.as_deref() {
                                        Some("msbc") => AudioCodec::Msbc,
                                        Some("cvsd") => AudioCodec::Cvsd,
                                        Some("lc3_swb") => AudioCodec::Lc3Swb,
                                        _ => AudioCodec::Unknown,
                                    }
                                }
                                _ => AudioCodec::Unknown,
                            };
                            state.apply(CallEvent::Audio {
                                state: AudioLinkState::Connected { codec },
                            });
                        }
                        if let Some(m) = s.muted {
                            state.apply(CallEvent::MuteChanged {
                                scope: MuteScope::HostMicrophone,
                                muted: m,
                            });
                        }
                    }
                    _ => caps.set(
                        CallFeature::MuteMicrophone,
                        Support::Unsupported {
                            reason: UnsupportedReason::NoPlatformApi,
                        },
                    ),
                }
                self.devices.insert(key.clone(), device.clone());
                // Named after its GeekBridge name when it is the set-up phone, so its calls
                // merge with that phone's link and never with another phone's.
                let label = self
                    .audio
                    .as_ref()
                    .and_then(|a| a.phone_name(device))
                    .unwrap_or_else(|| "Bluetooth phone".into());
                out.push(SourceSnapshot {
                    key,
                    label,
                    caps,
                    state,
                });
            }
            Ok(out)
        }
        fn execute(&mut self, source: &str, command: &CallCommand) -> Result<(), String> {
            let device = self
                .devices
                .get(source)
                .ok_or_else(|| "that phone is no longer connected over Bluetooth".to_owned())?
                .clone();
            if let Some(a) = &mut self.audio {
                match command {
                    // Answered or dialled here: the voice comes here. The phone's voice link is
                    // accepted before the command goes out.
                    CallCommand::Answer { .. } | CallCommand::Dial { .. } => {
                        a.want(&self.session, &device, Want::Computer)
                    }
                    CallCommand::ConnectAudio => {
                        a.want(&self.session, &device, Want::Computer);
                        // The controller asks the phone for the link itself when none opens.
                        return Ok(());
                    }
                    CallCommand::DisconnectAudio => {
                        a.want(&self.session, &device, Want::Phone);
                        return Ok(());
                    }
                    CallCommand::SetMicrophoneMute { muted } => {
                        return a.set_mute(&device, *muted);
                    }
                    _ => {}
                }
            }
            match command {
                CallCommand::Reject { call } => self.session.reject(&device, call),
                other => self.session.execute(&device, other),
            }
            .map_err(|e| e.to_string())
        }
        fn notes(&self) -> Vec<String> {
            self.audio.as_ref().map(|a| a.lines()).unwrap_or_default()
        }
        fn set_link_phones(&mut self, names: Vec<String>) {
            if let Some(a) = &mut self.audio {
                a.set_link_phones(names);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::calls::{
        AudioCodec, AudioLinkState, CallCapabilities, CallDirection, CallEvent, CallId, CallParty,
        CallPhase, HfpEvidence, HfpHost, LinkState, Sensitive,
    };
    use bridge_core::calls_link::LinkFailure;
    use serde_json::{Map, Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn body(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn a_displaced_session_does_not_remove_its_successor() {
        let hub = CallHub::new();
        let (_old_rx, old) = hub.link_up_generation("dev", "Pixel", true);
        let (_new_rx, new) = hub.link_up_generation("dev", "Pixel", true);
        assert_ne!(old, new);
        hub.link_down_generation("dev", old);
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true}))
        ));
        assert!(hub.view().ringing().is_some(), "successor must survive");
        hub.link_down_generation("dev", new);
        assert!(hub.view().ringing().is_none());
    }

    #[test]
    fn link_ring_answer_and_end_flow() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        assert!(hub.view().rows.is_empty());
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+155","name":"Ada","canAnswer":true,"canEnd":true}))
        ));
        let v = hub.view();
        let row = v.ringing().unwrap();
        assert_eq!(row.caller_label(), "Ada");
        let target = row.answer.clone().unwrap();
        assert_eq!(target.path, ControlPath::Link);

        hub.act(&target, CallAction::Answer).unwrap();
        let req = rx.try_recv().unwrap();
        assert_eq!(req.action, LinkAction::Answer);
        // The phone confirms, then reports the call as active.
        assert!(hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&req.request_id, Ok(()))
        ));
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"offhook","canAnswer":true,"canEnd":true})),
        );
        let v = hub.view();
        assert!(v.ringing().is_none());
        let live = v.live().unwrap();
        assert!(live.hangup.is_some());
        assert!(
            v.audio
                .hint
                .as_deref()
                .unwrap()
                .contains("Answered on the phone")
        );

        let hang = live.hangup.clone().unwrap();
        hub.act(&hang, CallAction::Hangup).unwrap();
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Hangup);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().rows.is_empty());
    }

    #[test]
    fn a_call_leaves_a_redacted_trace_of_states_requests_and_the_route() {
        let (dev, number, name) = ("trace-dev-77", "+4915112345678", "Zelda Quux");
        let hub = CallHub::new();
        let mut rx = hub.link_up(dev, "Pixel", true);
        hub.on_packet(
            dev,
            link::STATE,
            &body(json!({"state":"ringing","number":number,"name":name,"canAnswer":true,"canEnd":true})),
        );
        let target = hub.view().ringing().unwrap().answer.clone().unwrap();
        hub.act(&target, CallAction::Answer).unwrap();
        let _ = rx.try_recv();
        let stale = ActionTarget {
            source: target.source.clone(),
            call: CallId::new("nope"),
            path: target.path,
        };
        assert!(hub.act(&stale, CallAction::Hangup).is_err());
        hub.on_packet(
            dev,
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );

        let phone = crate::call_trace::hash(dev);
        let all = crate::call_trace::snapshot();
        let mine: Vec<String> = all
            .iter()
            .map(|e| format!("{} {}", e.kind, e.detail))
            .filter(|l| {
                l.contains(&phone)
                    || l.contains("src=link")
                    || l.contains("answer")
                    || l.contains("hangup")
            })
            .collect();
        let text = mine.join("\n");
        assert!(text.contains(&format!("link up phone={phone}")), "{text}");
        assert!(
            text.contains("state phone") && text.contains("none -> ringing"),
            "{text}"
        );
        assert!(text.contains("-> idle"), "{text}");
        assert!(
            text.contains("new src=link") && text.contains("incoming"),
            "{text}"
        );
        assert!(
            text.contains("controls=ar-") || text.contains("controls=a"),
            "{text}"
        );
        assert!(
            text.contains("answer call=") && text.contains(": ok"),
            "{text}"
        );
        assert!(
            text.contains("refused") && text.contains("that call changed"),
            "{text}"
        );
        assert!(text.contains("number=..78"), "{text}");
        assert!(text.contains("gone src=link"), "{text}");
        let everything = crate::call_trace::render(&all);
        for secret in ["4915112345678", "12345678", "Zelda", "Quux", dev] {
            assert!(!everything.contains(secret), "trace leaked {secret}");
        }
    }

    #[test]
    fn send_link_refuses_once_consent_is_off() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev-consent", "Pixel", true);
        assert!(
            hub.send_link("link:dev-consent", LinkAction::Hangup, None, None)
                .is_ok()
        );
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Hangup);
        hub.set_enabled("dev-consent", false);
        assert!(
            hub.send_link("link:dev-consent", LinkAction::Hangup, None, None)
                .is_err()
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn consent_off_and_unpairing_erase_the_phones_recents() {
        let hub = CallHub::new();
        let _rx = hub.link_up("dev-forget", "Pixel", true);
        let state = |v: serde_json::Value| hub.on_packet("dev-forget", link::STATE, &body(v));
        let missed_call = || {
            state(
                json!({"state":"ringing","name":"Ada","number":"+1555","canAnswer":true,"canEnd":true}),
            );
            state(json!({"state":"idle","canAnswer":true,"canEnd":true}));
        };
        missed_call();
        missed_call();
        assert_eq!(hub.status().view.recent.len(), 2);
        hub.set_enabled("dev-forget", false);
        assert!(hub.status().view.recent.is_empty());
        hub.set_enabled("dev-forget", true);
        missed_call();
        assert_eq!(hub.status().view.recent.len(), 1);
        hub.forget_device("dev-forget");
        assert!(hub.status().view.recent.is_empty());
    }

    #[test]
    fn stale_or_forged_targets_send_nothing() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        let target = hub.view().ringing().unwrap().answer.clone().unwrap();
        // Wrong action for the slot.
        assert!(hub.act(&target, CallAction::Hangup).is_err());
        // The call ended in between.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.act(&target, CallAction::Answer).is_err());
        let mut forged = target.clone();
        forged.source = "link:other".into();
        assert!(hub.act(&forged, CallAction::Answer).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn nothing_is_offered_without_consent_or_phone_permission() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", false);
        // Without consent the phone's report is ignored entirely.
        assert!(hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true}))
        ));
        assert!(hub.view().rows.is_empty());
        let st = hub.status();
        assert!(st.sources.iter().any(|l| l.contains("allow Calls")));

        hub.set_enabled("dev", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":false,"canEnd":false})),
        );
        let v = hub.view();
        let row = v.ringing().unwrap();
        assert!(row.answer.is_none() && row.reject.is_none());
        assert!(row.missing_control.is_some());
        // Only the "report again" request after the consent came back; no control was sent.
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Report);
        assert!(rx.try_recv().is_err());

        hub.set_enabled("dev", false);
        assert!(hub.view().rows.is_empty());
    }

    #[test]
    fn refusal_from_the_phone_becomes_a_notice() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        let t = hub.view().ringing().unwrap().answer.clone().unwrap();
        hub.act(&t, CallAction::Answer).unwrap();
        let id = rx.try_recv().unwrap().request_id;
        hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&id, Err(LinkFailure::PermissionMissing)),
        );
        assert!(hub.status().notice.unwrap().contains("not allowed"));
        // An unsolicited result is misbehaviour, a malformed state too.
        assert!(!hub.on_packet("dev", link::RESULT, &link::result_body("zz", Ok(()))));
        assert!(!hub.on_packet("dev", link::STATE, &body(json!({"state":"bogus"}))));
    }

    #[test]
    fn a_decline_the_phone_refused_ends_as_a_miss_that_is_announced() {
        let misses = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Misses(misses.clone())));
        let mut rx = hub.link_up("dev-ref", "Pixel", true);
        let state = |v: serde_json::Value| hub.on_packet("dev-ref", link::STATE, &body(v));
        state(
            json!({"state":"ringing","name":"Ada","number":"+1555","canAnswer":true,"canEnd":true}),
        );
        let target = hub.view().rows[0].reject.clone().unwrap();
        hub.act(&target, CallAction::Reject).unwrap();
        let id = rx.try_recv().unwrap().request_id;
        hub.on_packet(
            "dev-ref",
            link::RESULT,
            &link::result_body(&id, Err(LinkFailure::PermissionMissing)),
        );
        state(json!({"state":"idle","canAnswer":true,"canEnd":true}));
        assert_eq!(
            misses.lock().unwrap().len(),
            1,
            "the ring was never declined"
        );
        let kinds: Vec<_> = hub.status().view.recent.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [bridge_core::calls_view::RecentKind::Missed]);
    }

    #[test]
    fn a_ring_the_watchdog_clears_is_neither_missed_nor_listed() {
        let misses = Arc::new(Mutex::new(Vec::new()));
        let ms = Duration::from_millis;
        let hub = CallHub::with_timings(Timings {
            tick: ms(10),
            ring_quiet: ms(0),
            live_quiet: ms(0),
            probe_grace: ms(0),
            probes: 1,
            flap_memory: ms(1),
            overlay_linger: ms(1),
        });
        hub.set_notifier(Arc::new(Misses(misses.clone())));
        let _rx = hub.link_up("dev-wd", "Pixel", true);
        hub.on_packet(
            "dev-wd",
            link::STATE,
            &body(json!({"state":"ringing","number":"+1555","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().ringing().is_some());
        hub.tick(); // asks the phone
        hub.tick(); // never confirmed: cleared
        assert!(hub.view().rows.is_empty());
        assert!(
            hub.status().view.recent.is_empty(),
            "nobody knows how it ended"
        );
        assert!(misses.lock().unwrap().is_empty());
    }

    #[test]
    fn link_down_ends_the_call_view() {
        let hub = CallHub::new();
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().ringing().is_some());
        hub.link_down("dev");
        assert!(hub.view().rows.is_empty());
    }

    // ---- a scripted host backend

    struct FakeHost {
        state: Arc<Mutex<SourceSnapshot>>,
        log: Arc<Mutex<Vec<CallCommand>>>,
    }
    impl HostCalls for FakeHost {
        fn label(&self) -> String {
            "Fake Bluetooth".into()
        }
        fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
            std::thread::sleep(wait.min(Duration::from_millis(10)));
            Ok(vec![self.state.lock().unwrap().clone()])
        }
        fn execute(&mut self, _source: &str, command: &CallCommand) -> Result<(), String> {
            self.log.lock().unwrap().push(command.clone());
            Ok(())
        }
    }
    fn host_snapshot(phase: CallPhase, audio: bool) -> SourceSnapshot {
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::LinuxPipeWire,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                host_call_control_api: true,
                host_audio_control_api: true,
                ..Default::default()
            },
        );
        let mut state = CallState::default();
        state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        state.apply(CallEvent::CallUpdated {
            id: CallId::new("c1"),
            direction: CallDirection::Incoming,
            phase,
            party: CallParty {
                number: Some(Sensitive::new("+1555")),
                name: None,
            },
        });
        if audio {
            state.apply(CallEvent::Audio {
                state: AudioLinkState::Connected {
                    codec: AudioCodec::Msbc,
                },
            });
        }
        SourceSnapshot {
            key: "hfp:ag1".into(),
            label: "Bluetooth phone".into(),
            caps,
            state,
        }
    }

    fn wait_for(hub: &CallHub, f: impl Fn(&CallsView) -> bool) -> CallsView {
        let end = Instant::now() + Duration::from_secs(3);
        loop {
            let v = hub.view();
            if f(&v) {
                return v;
            }
            assert!(Instant::now() < end, "view never became ready: {v:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn host_backend_answers_rejects_and_notifies_once() {
        struct Count(Arc<AtomicUsize>);
        impl CallNotifier for Count {
            fn ringing(&self, _: &str, a: bool, d: bool, _: Box<dyn FnOnce(RingChoice) + Send>) {
                assert!(a && d);
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let rings = Arc::new(AtomicUsize::new(0));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Count(rings.clone())));
        let state = Arc::new(Mutex::new(host_snapshot(CallPhase::Incoming, false)));
        let log = Arc::new(Mutex::new(Vec::new()));
        hub.start_host(Box::new(FakeHost {
            state: state.clone(),
            log: log.clone(),
        }));
        let v = wait_for(&hub, |v| v.ringing().is_some());
        let row = v.ringing().unwrap();
        assert_eq!(row.answer.as_ref().unwrap().path, ControlPath::Hfp);

        hub.act(&row.reject.clone().unwrap(), CallAction::Reject)
            .unwrap();
        hub.act(&row.answer.clone().unwrap(), CallAction::Answer)
            .unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                CallCommand::Reject {
                    call: CallId::new("c1")
                },
                CallCommand::Answer {
                    call: CallId::new("c1")
                }
            ]
        );
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(rings.load(Ordering::SeqCst), 1, "one notification per call");

        // The call goes live with audio on the computer: mute and hang up are offered.
        *state.lock().unwrap() = host_snapshot(CallPhase::Active, true);
        let v = wait_for(&hub, |v| v.live().is_some());
        hub.act(
            &v.live().unwrap().hangup.clone().unwrap(),
            CallAction::Hangup,
        )
        .unwrap();
        assert_eq!(
            log.lock().unwrap().last(),
            Some(&CallCommand::Hangup {
                call: CallId::new("c1")
            })
        );
    }

    #[test]
    fn a_caller_label_cannot_become_an_option_or_markup() {
        // A caller-ID name is chosen by the caller (S2-3 in docs/reports/security-review-2.md).
        let a = NotifySendCalls::args("--action=answer=Ignore me", true, true);
        let dashdash = a.iter().position(|s| s == "--").expect("positional marker");
        assert!(a[dashdash + 1..].iter().any(|s| s == "Incoming call"));
        assert!(a[..dashdash].iter().all(|s| !s.contains("Ignore me")));
        let b = NotifySendCalls::args("<a href=\"http://evil\">Bank</a>", false, false);
        assert!(!b.last().unwrap().contains(['<', '>']));
    }

    #[test]
    fn notify_send_arguments_follow_the_available_controls() {
        let a = NotifySendCalls::args("Ada\nLovelace", true, true);
        assert!(a.contains(&"--action=answer=Answer".to_owned()));
        assert!(a.contains(&"--action=decline=Decline".to_owned()));
        assert_eq!(a.last().unwrap(), "AdaLovelace");
        let b = NotifySendCalls::args("x", false, false);
        assert!(!b.iter().any(|s| s.starts_with("--action")) && !b.contains(&"--wait".to_owned()));
    }

    // ---- test-gaps-4: dialing from this computer, the unnamed-caller grace and the pending-request cap

    /// A consenting phone that is idle and says it may place calls.
    fn dialer(hub: &Arc<CallHub>, id: &str) -> mpsc::Receiver<LinkRequest> {
        let rx = hub.link_up(id, "Pixel", true);
        assert!(hub.on_packet(
            id,
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true,"canDial":true}))
        ));
        rx
    }

    fn number(s: &str) -> Sensitive {
        Sensitive::new(s)
    }

    #[test]
    fn dial_over_the_link_sends_exactly_the_number_once_and_a_refusal_becomes_a_notice() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let dial = hub.view().dial.expect("dial offered");
        assert_eq!(
            (dial.source.as_str(), dial.path),
            ("link:dev", ControlPath::Link)
        );
        assert_eq!(dial.phone, "Pixel");
        hub.dial(&dial.source, &number("+15550142")).unwrap();
        let req = rx.try_recv().unwrap();
        assert_eq!(req.action, LinkAction::Dial);
        assert_eq!(req.number.as_ref().unwrap().expose(), "+15550142");
        assert!(rx.try_recv().is_err(), "one request per dial");
        assert!(hub.status().notice.is_none());
        // The phone refuses (CALL_PHONE revoked meanwhile): the person sees why, the number is not logged.
        assert!(hub.on_packet(
            "dev",
            link::RESULT,
            &link::result_body(&req.request_id, Err(LinkFailure::PermissionMissing))
        ));
        let notice = hub.status().notice.unwrap();
        assert!(notice.contains("not allowed") && !notice.contains("5550142"));
        // A confirmed dial clears an older notice when the next one succeeds.
        hub.dial(&dial.source, &number("112")).unwrap();
        assert!(hub.status().notice.is_none());
        assert_eq!(rx.try_recv().unwrap().number.unwrap().expose(), "112");
    }

    #[test]
    fn dial_rejects_anything_that_is_not_a_plain_number_before_looking_at_the_source() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let too_long = "1".repeat(link::MAX_DIAL_DIGITS + 1);
        for bad in [
            "",
            "+",
            "12 34",
            "*21#",
            "#31#",
            "1,2",
            "12;3",
            "++1",
            "1+2",
            "abc",
            "\u{0661}\u{0662}",
            "12\n",
            &too_long,
        ] {
            let e = hub.dial("link:dev", &number(bad)).unwrap_err();
            assert_eq!(e.code, ErrorCode::BadRequest, "{bad:?}");
            // Same answer for a source that does not exist: the number is judged first.
            assert_eq!(
                hub.dial("nope", &number(bad)).unwrap_err().code,
                ErrorCode::BadRequest
            );
        }
        assert!(rx.try_recv().is_err(), "nothing was sent for any of them");
        assert!(
            hub.status().notice.is_none(),
            "a malformed number is not a phone refusal"
        );
        // The longest allowed number goes through.
        hub.dial(
            "link:dev",
            &number(&format!("+{}", "9".repeat(link::MAX_DIAL_DIGITS))),
        )
        .unwrap();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn dial_is_refused_unless_the_current_view_offers_that_exact_source() {
        let hub = CallHub::new();
        // No phone at all.
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        // A phone without the permission to place calls.
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        // Allowed on the phone, but the computer withdrew Telephony consent: also nothing.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        assert!(hub.view().dial.is_some());
        hub.set_enabled("dev", false);
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.set_enabled("dev", true);
        // consent back on, but the phone must report again before anything is offered
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        // A forged or foreign source never dials, and a hands-free source is not the link.
        for source in ["link:other", "link:", "hfp:dev", "dev", "", "LINK:dev"] {
            assert_eq!(
                hub.dial(source, &number("112")).unwrap_err().code,
                ErrorCode::NotFound,
                "{source}"
            );
        }
        // Only the "report again" request after the consent came back; no dial was sent.
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Report);
        assert!(rx.try_recv().is_err());
        hub.dial("link:dev", &number("112")).unwrap();
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Dial);
    }

    #[test]
    fn dial_is_not_offered_during_a_call_and_returns_when_it_ends() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+155","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        assert!(hub.view().dial.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"offhook","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        assert!(
            hub.dial("link:dev", &number("112")).is_err(),
            "a live call blocks a second one"
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true,"canDial":true})),
        );
        hub.dial("link:dev", &number("112")).unwrap();
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Dial);
    }

    #[test]
    fn dial_with_two_phones_goes_to_the_offered_one_only() {
        let hub = CallHub::new();
        let mut a = dialer(&hub, "a");
        let mut b = dialer(&hub, "b");
        let offered = hub.view().dial.unwrap().source;
        hub.dial(&offered, &number("112")).unwrap();
        let (got_a, got_b) = (a.try_recv().is_ok(), b.try_recv().is_ok());
        assert!(got_a ^ got_b, "exactly one phone dials, not both");
        // The other phone's source is not what the view offers, so it is refused.
        let other = if offered == "link:a" {
            "link:b"
        } else {
            "link:a"
        };
        assert_eq!(
            hub.dial(other, &number("112")).unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn switching_telephony_on_asks_the_phone_to_report_again_but_only_then() {
        let hub = CallHub::new();
        let mut rx = hub.link_up("dev", "Pixel", true);
        // Already on when the link came up: the phone reports by itself.
        assert!(rx.try_recv().is_err());
        hub.set_enabled("dev", true);
        assert!(rx.try_recv().is_err(), "no change, nothing to ask");
        hub.set_enabled("dev", false);
        assert!(rx.try_recv().is_err());
        hub.set_enabled("dev", true);
        let ask = rx.try_recv().expect("asked for the state again");
        assert_eq!(ask.action, LinkAction::Report);
        assert!(ask.number.is_none());
        assert!(link::parse_request(&link::request_body(&ask.request_id, ask.action)).is_ok());
        // It is not a pending dial/answer: it takes no slot and needs no result.
        assert!(hub.inner().links["dev"].pending.is_empty());
        // The answer is the phone's ordinary state packet.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        assert!(hub.view().dial.is_some());
    }

    #[test]
    fn a_phone_that_stops_answering_cannot_be_flooded_with_dials() {
        let hub = CallHub::new();
        let mut rx = dialer(&hub, "dev");
        let mut ids = Vec::new();
        for _ in 0..MAX_PENDING {
            hub.dial("link:dev", &number("112")).unwrap();
            ids.push(rx.try_recv().unwrap().request_id);
        }
        let e = hub.dial("link:dev", &number("112")).unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        assert!(rx.try_recv().is_err(), "the refused dial was not queued");
        assert_eq!(
            hub.status().notice.as_deref(),
            Some("the phone has not answered yet")
        );
        // Request ids are unique, so an answer can only match its own request.
        let unique: BTreeSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        // One answer frees one slot, and an answer is accepted once only.
        assert!(hub.on_packet("dev", link::RESULT, &link::result_body(&ids[0], Ok(()))));
        assert!(!hub.on_packet("dev", link::RESULT, &link::result_body(&ids[0], Ok(()))));
        hub.dial("link:dev", &number("112")).unwrap();
        assert!(hub.status().notice.is_none());
        assert_eq!(
            hub.dial("link:dev", &number("112")).unwrap_err().code,
            ErrorCode::Busy
        );
        // Turning Telephony off forgets what was pending, so a re-enabled phone starts clean.
        hub.set_enabled("dev", false);
        hub.set_enabled("dev", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canDial":true})),
        );
        hub.dial("link:dev", &number("112")).unwrap();
    }

    #[test]
    fn dial_on_a_dead_link_reports_not_connected_and_queues_nothing() {
        let hub = CallHub::new();
        let rx = dialer(&hub, "dev");
        drop(rx); // the link task is gone
        let e = hub.dial("link:dev", &number("112")).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotConnected);
        assert!(hub.status().notice.is_some());
        // The failed send must not use up one of the pending slots.
        for _ in 0..MAX_PENDING + 2 {
            assert_eq!(
                hub.dial("link:dev", &number("112")).unwrap_err().code,
                ErrorCode::NotConnected
            );
        }
    }

    #[test]
    fn a_full_link_queue_is_reported_instead_of_blocking() {
        let hub = CallHub::new();
        let _rx = dialer(&hub, "dev"); // never drained: capacity 8, pending cap 4
        // Answer each one so the pending cap is never what stops us; the channel is.
        let mut refused = None;
        for _ in 0..20 {
            match hub.dial("link:dev", &number("112")) {
                Ok(()) => {
                    let id = format!("c{}", hub.inner().next_request);
                    hub.on_packet("dev", link::RESULT, &link::result_body(&id, Ok(())));
                }
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        let e = refused.expect("the bounded queue pushed back");
        assert_eq!(e.code, ErrorCode::NotConnected);
        assert!(e.message.contains("busy or closed"));
    }

    #[test]
    fn dial_through_hands_free_runs_on_the_host_and_a_host_failure_is_shown() {
        use bridge_core::calls::{CallFeature, LinkState, Support};
        struct Flaky {
            snap: SourceSnapshot,
            log: Arc<Mutex<Vec<CallCommand>>>,
            fail: Arc<Mutex<Option<String>>>,
        }
        impl HostCalls for Flaky {
            fn label(&self) -> String {
                "Flaky".into()
            }
            fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
                std::thread::sleep(wait.min(Duration::from_millis(10)));
                Ok(vec![self.snap.clone()])
            }
            fn execute(&mut self, _s: &str, c: &CallCommand) -> Result<(), String> {
                self.log.lock().unwrap().push(c.clone());
                match self.fail.lock().unwrap().clone() {
                    Some(why) => Err(why),
                    None => Ok(()),
                }
            }
        }
        let mut snap = host_snapshot(CallPhase::Active, false);
        snap.state = CallState::default();
        snap.state.apply(CallEvent::Link {
            state: LinkState::Up,
        });
        snap.caps.set(CallFeature::Dial, Support::Declared);
        let (log, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(None)));
        let hub = CallHub::new();
        hub.start_host(Box::new(Flaky {
            snap,
            log: log.clone(),
            fail: fail.clone(),
        }));
        let v = wait_for(&hub, |v| v.dial.is_some());
        let dial = v.dial.unwrap();
        assert_eq!(
            (dial.source.as_str(), dial.path),
            ("hfp:ag1", ControlPath::Hfp)
        );
        hub.dial(&dial.source, &number("+15550142")).unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![CallCommand::Dial {
                number: number("+15550142")
            }]
        );
        // The stack refuses: the failure reaches the person and nothing else was attempted.
        *fail.lock().unwrap() = Some("adapter busy".into());
        let e = hub.dial(&dial.source, &number("112")).unwrap_err();
        assert_eq!(
            (e.code, e.message.as_str()),
            (ErrorCode::Failed, "adapter busy")
        );
        assert_eq!(hub.status().notice.as_deref(), Some("adapter busy"));
        // An invalid number never reaches the host.
        let before = log.lock().unwrap().len();
        assert!(hub.dial(&dial.source, &number("*21#")).is_err());
        assert_eq!(log.lock().unwrap().len(), before);
    }

    #[test]
    fn host_commands_without_a_host_fail_plainly() {
        let hub = CallHub::new();
        let e = hub
            .send_host("hfp:x", &CallCommand::ConnectAudio)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported);
        assert_eq!(
            hub.mute("hfp:x", true).unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert_eq!(
            hub.connect_audio("hfp:x").unwrap_err().code,
            ErrorCode::Unsupported
        );
        // A source that is not a link source cannot be sent a link request.
        assert_eq!(
            hub.send_link("hfp:x", LinkAction::Answer, None, None)
                .unwrap_err()
                .code,
            ErrorCode::BadRequest
        );
        assert_eq!(
            hub.send_link("link:gone", LinkAction::Answer, None, None)
                .unwrap_err()
                .code,
            ErrorCode::NotConnected
        );
    }

    struct Rings(Arc<Mutex<Vec<String>>>);
    impl CallNotifier for Rings {
        fn ringing(&self, caller: &str, _: bool, _: bool, _: Box<dyn FnOnce(RingChoice) + Send>) {
            self.0.lock().unwrap().push(caller.to_owned());
        }
    }

    #[test]
    fn a_ring_without_a_caller_waits_for_the_number_and_is_announced_once() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        // "ringing" arrives first, the number follows within the grace period.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        assert!(
            rings.lock().unwrap().is_empty(),
            "held back while the caller is unknown"
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(*rings.lock().unwrap(), vec!["+15550142".to_owned()]);
        // The delayed re-check that was queued finds it already announced and says nothing more.
        std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(400));
        assert_eq!(rings.lock().unwrap().len(), 1);
        // Repeated identical reports do not repeat the banner.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(rings.lock().unwrap().len(), 1);
        // The call ends; a later call from the same number rings (and is announced) again.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","number":"+15550142","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(rings.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_private_number_is_announced_as_unknown_after_the_grace_period_not_before() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        std::thread::sleep(UNNAMED_GRACE / 2);
        assert!(rings.lock().unwrap().is_empty());
        let end = Instant::now() + Duration::from_secs(5);
        while rings.lock().unwrap().is_empty() {
            assert!(Instant::now() < end, "never announced");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(*rings.lock().unwrap(), vec!["Unknown caller".to_owned()]);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(rings.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_call_that_stops_ringing_during_the_grace_period_is_never_announced() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        std::thread::sleep(UNNAMED_GRACE + Duration::from_millis(400));
        assert!(
            rings.lock().unwrap().is_empty(),
            "a missed call is not announced after it ended"
        );
    }

    type Missed = Arc<Mutex<Vec<(String, Option<String>)>>>;
    struct Misses(Missed);
    impl CallNotifier for Misses {
        fn ringing(&self, _: &str, _: bool, _: bool, _: Box<dyn FnOnce(RingChoice) + Send>) {}
        fn missed(&self, caller: &str, number: Option<&str>) {
            self.0
                .lock()
                .unwrap()
                .push((caller.to_owned(), number.map(str::to_owned)));
        }
    }

    #[test]
    fn a_missed_call_is_announced_once_with_its_number_and_listed_but_answered_and_declined_are_not()
     {
        let misses = Arc::new(Mutex::new(Vec::new()));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Misses(misses.clone())));
        let _rx = hub.link_up("dev", "Pixel", true);
        let state = |v: serde_json::Value| hub.on_packet("dev", link::STATE, &body(v));
        // Missed.
        state(
            json!({"state":"ringing","name":"Ada","number":"+1 555 0142","canAnswer":true,"canEnd":true}),
        );
        state(json!({"state":"idle","canAnswer":true,"canEnd":true}));
        // Answered on the phone.
        state(
            json!({"state":"ringing","name":"Bob","number":"+1555","canAnswer":true,"canEnd":true}),
        );
        state(json!({"state":"offhook","canAnswer":true,"canEnd":true}));
        state(json!({"state":"idle","canAnswer":true,"canEnd":true}));
        // Declined here.
        state(
            json!({"state":"ringing","name":"Cy","number":"+1556","canAnswer":true,"canEnd":true}),
        );
        let target = hub.view().rows[0].reject.clone().unwrap();
        hub.act(&target, CallAction::Reject).unwrap();
        state(json!({"state":"idle","canAnswer":true,"canEnd":true}));

        assert_eq!(
            *misses.lock().unwrap(),
            vec![("Ada".to_owned(), Some("+15550142".to_owned()))]
        );
        let kinds: Vec<_> = hub.status().view.recent.iter().map(|r| r.kind).collect();
        use bridge_core::calls_view::RecentKind::*;
        assert_eq!(kinds, vec![Declined, Incoming, Missed]);
        assert_eq!(
            hub.status().view.recent[2].name.as_ref().unwrap().expose(),
            "Ada"
        );
    }

    #[test]
    fn the_missed_call_banner_offers_call_back_and_message_only_for_a_plain_number() {
        let with = NotifySendCalls::missed_args("Ada", Some("+15550142"));
        assert!(with.contains(&"--action=callback=Call back".to_owned()));
        assert!(with.contains(&"--action=message=Message".to_owned()));
        assert!(with.contains(&"--action=default=Open".to_owned()));
        assert!(with.iter().any(|a| a == "--category=call.missed"));
        // The caller is text after `--`, and the number is never part of the arguments.
        assert_eq!(&with[with.len() - 3..], ["--", "Missed call", "Ada"]);
        assert!(!with.iter().any(|a| a.contains("5550142")));
        let without = NotifySendCalls::missed_args("--hostile", None);
        assert!(!without.iter().any(|a| a.starts_with("--action=callback")));
        assert!(without.iter().any(|a| a == "--action=default=Open"));
    }

    #[test]
    fn the_banner_buttons_follow_the_controls_and_press_through_the_hub() {
        type Reply = Box<dyn FnOnce(RingChoice) + Send>;
        struct Press(Mutex<Option<Reply>>);
        impl CallNotifier for Press {
            fn ringing(
                &self,
                _: &str,
                a: bool,
                d: bool,
                reply: Box<dyn FnOnce(RingChoice) + Send>,
            ) {
                assert!(a && d);
                *self.0.lock().unwrap() = Some(reply);
            }
        }
        let p = Arc::new(Press(Mutex::new(None)));
        let hub = CallHub::new();
        hub.set_notifier(p.clone());
        let mut rx = hub.link_up("dev", "Pixel", true);
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","name":"Ada","canAnswer":true,"canEnd":true})),
        );
        let reply = p.0.lock().unwrap().take().expect("banner shown");
        reply(RingChoice::Decline);
        assert_eq!(rx.try_recv().unwrap().action, LinkAction::Reject);
        // A late press on a stale banner after the call ended sends nothing.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","name":"Bob","canAnswer":true,"canEnd":true})),
        );
        let fresh = p.0.lock().unwrap().take().unwrap();
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        fresh(RingChoice::Answer);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_ring_in_do_not_disturb_shows_no_card_and_no_banner_but_is_still_recorded() {
        struct Count(Arc<Mutex<u32>>);
        impl crate::call_overlay::Launcher for Count {
            fn launch(
                &self,
            ) -> Result<Box<dyn crate::call_overlay::Card>, crate::call_overlay::Cannot>
            {
                *self.0.lock().unwrap() += 1;
                struct Up;
                impl crate::call_overlay::Card for Up {
                    fn running(&mut self) -> bool {
                        true
                    }
                    fn kill(&mut self) {}
                }
                Ok(Box::new(Up))
            }
        }
        for (dnd, cards, banners) in [(true, 0, 0), (false, 1, 1)] {
            let (launched, rings) = (Arc::new(Mutex::new(0)), Arc::new(Mutex::new(Vec::new())));
            let hub = CallHub::new();
            hub.set_notifier(Arc::new(Rings(rings.clone())));
            hub.set_overlay(Arc::new(Count(launched.clone())));
            hub.set_quiet(Arc::new(move || dnd));
            let _rx = hub.link_up("dev", "Pixel", true);
            let state = |v: serde_json::Value| hub.on_packet("dev", link::STATE, &body(v));
            state(
                json!({"state":"ringing","name":"Ada","number":"+1 555 0142","canAnswer":true,"canEnd":true}),
            );
            assert_eq!(*launched.lock().unwrap(), cards, "card, dnd={dnd}");
            assert_eq!(rings.lock().unwrap().len(), banners, "banner, dnd={dnd}");
            assert_eq!(hub.view().rows.len(), 1, "the ring is still in the view");
            state(json!({"state":"idle","canAnswer":true,"canEnd":true}));
            let recent = hub.status().view.recent;
            assert_eq!(recent.len(), 1, "Recents records the ring, dnd={dnd}");
            assert_eq!(recent[0].kind, bridge_core::calls_view::RecentKind::Missed);
        }
    }

    #[test]
    fn a_quiet_ring_stays_quiet_if_do_not_disturb_ends_while_it_rings() {
        let rings = Arc::new(Mutex::new(Vec::new()));
        let on = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let hub = CallHub::new();
        hub.set_notifier(Arc::new(Rings(rings.clone())));
        let flag = on.clone();
        hub.set_quiet(Arc::new(move || {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        }));
        let _rx = hub.link_up("dev", "Pixel", true);
        let ring = json!({"state":"ringing","name":"Ada","canAnswer":true,"canEnd":true});
        hub.on_packet("dev", link::STATE, &body(ring.clone()));
        on.store(false, std::sync::atomic::Ordering::SeqCst);
        hub.on_packet("dev", link::STATE, &body(ring));
        assert!(rings.lock().unwrap().is_empty());
        // The next ring, with Do Not Disturb off, is announced.
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"idle","canAnswer":true,"canEnd":true})),
        );
        hub.on_packet(
            "dev",
            link::STATE,
            &body(json!({"state":"ringing","name":"Bob","canAnswer":true,"canEnd":true})),
        );
        assert_eq!(rings.lock().unwrap().len(), 1);
    }
}
