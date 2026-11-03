// SPDX-License-Identifier: GPL-3.0-or-later
//! Mirrored phone notifications on the computer (stock `kdeconnect.notification*`, see
//! `bridge_core::notification_link`).
//!
//! Held in memory only, bounded two ways: at most [`MAX_PER_DEVICE`] per phone (oldest dropped first) and
//! each for at most [`TTL`]. They exist only while the phone's `notifications` consent is on and its link
//! is up; turning the consent off, losing the link or unpairing erases them. Titles and texts are never
//! written to a log. A banner names the app and the phone; the text is part of it only when the user chose
//! to show message content (the same switch as texts), so a screen shared or left open shows no codes.
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bridge_core::ipc::{NotificationItem, NotificationsView};
use bridge_core::notification_link::Mirrored;

/// Most notifications kept per phone; the oldest go first.
pub const MAX_PER_DEVICE: usize = 50;
/// Most phones with held notifications.
pub const MAX_DEVICES: usize = 8;
/// How long one is kept without an update.
pub const TTL: Duration = Duration::from_secs(3 * 60 * 60);
/// Banners: a burst of this many, then one per [`BANNER_GAP`]. The rest are still listed.
const BANNER_BURST: f64 = 6.0;
const BANNER_GAP: Duration = Duration::from_secs(2);

/// What the user picked on a banner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Dismiss,
    Action(String),
}

/// What a notifier may know about one mirrored notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorBanner {
    pub device_name: String,
    pub app: String,
    pub title: String,
    /// `Some` only when the user chose to show content.
    pub text: Option<String>,
    pub clearable: bool,
    pub actions: Vec<String>,
    /// Stable per notification, for replacing the earlier banner of the same one.
    pub slot: String,
}

/// Desktop banner for a mirrored notification. `reply` is called at most once, with the button pressed.
pub trait MirrorNotifier: Send + Sync {
    fn show(&self, banner: &MirrorBanner, reply: Box<dyn FnOnce(Choice) + Send>);
}

struct Held {
    n: Mirrored,
    at: Instant,
}

struct Dev {
    name: String,
    items: VecDeque<Held>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Posted {
    New,
    Updated,
}

#[derive(Default)]
struct Inner {
    devices: HashMap<String, Dev>,
    /// One banner allowance per phone, so a chatty phone cannot use up the banners of the others.
    buckets: HashMap<String, Bucket>,
}

struct Bucket {
    tokens: f64,
    refilled: Instant,
}

/// Phones remembered by the banner limiter (the service pairs far fewer).
const MAX_BUCKETS: usize = 32;

#[derive(Default)]
pub struct Mirror {
    inner: Mutex<Inner>,
    show_content: std::sync::atomic::AtomicBool,
}

impl Mirror {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_show_content(&self, show: bool) {
        self.show_content
            .store(show, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn show_content(&self) -> bool {
        self.show_content.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn prune(g: &mut Inner, now: Instant) {
        for d in g.devices.values_mut() {
            d.items
                .retain(|h| now.saturating_duration_since(h.at) <= TTL);
        }
        g.devices.retain(|_, d| !d.items.is_empty());
    }

    /// Keep (or update) one notification.
    pub fn post(&self, device: &str, name: &str, n: Mirrored, now: Instant) -> Posted {
        let mut g = self.lock();
        Self::prune(&mut g, now);
        if !g.devices.contains_key(device) && g.devices.len() >= MAX_DEVICES {
            return Posted::New; // not kept; the banner policy still applies to the caller
        }
        let d = g.devices.entry(device.to_owned()).or_insert_with(|| Dev {
            name: String::new(),
            items: VecDeque::new(),
        });
        d.name = name.chars().filter(|c| !c.is_control()).take(64).collect();
        let existing = d.items.iter().position(|h| h.n.id == n.id);
        let outcome = if let Some(i) = existing {
            d.items.remove(i);
            Posted::Updated
        } else {
            Posted::New
        };
        d.items.push_back(Held { n, at: now });
        while d.items.len() > MAX_PER_DEVICE {
            d.items.pop_front();
        }
        outcome
    }

    /// Remove one; true if it was held.
    pub fn remove(&self, device: &str, id: &str) -> bool {
        let mut g = self.lock();
        let Some(d) = g.devices.get_mut(device) else {
            return false;
        };
        let before = d.items.len();
        d.items.retain(|h| h.n.id != id);
        let removed = d.items.len() != before;
        if d.items.is_empty() {
            g.devices.remove(device);
        }
        removed
    }

    /// Erase everything held for a phone (consent withdrawn, link lost, unpaired).
    pub fn clear(&self, device: &str) {
        self.lock().devices.remove(device);
    }

    pub fn get(&self, device: &str, id: &str, now: Instant) -> Option<Mirrored> {
        let mut g = self.lock();
        Self::prune(&mut g, now);
        g.devices
            .get(device)?
            .items
            .iter()
            .find(|h| h.n.id == id)
            .map(|h| h.n.clone())
    }

    /// Newest first, optionally for one phone.
    pub fn view(
        &self,
        only: Option<&str>,
        enabled: Vec<String>,
        now: Instant,
    ) -> NotificationsView {
        let mut g = self.lock();
        Self::prune(&mut g, now);
        let mut rows: Vec<(Instant, NotificationItem)> = Vec::new();
        for (id, d) in &g.devices {
            if only.is_some_and(|o| o != id) {
                continue;
            }
            for h in &d.items {
                rows.push((
                    h.at,
                    NotificationItem {
                        device_id: id.clone(),
                        device_name: d.name.clone(),
                        id: h.n.id.clone(),
                        app: h.n.app.clone(),
                        title: h.n.title.clone(),
                        text: h.n.text.clone(),
                        time_ms: h.n.time_ms,
                        clearable: h.n.clearable,
                        actions: h.n.actions.clone(),
                    },
                ));
            }
        }
        rows.sort_by_key(|a| std::cmp::Reverse(a.0));
        NotificationsView {
            items: rows.into_iter().map(|(_, i)| i).collect(),
            enabled_devices: enabled,
        }
    }

    pub fn count(&self, device: &str) -> usize {
        self.lock().devices.get(device).map_or(0, |d| d.items.len())
    }

    /// May another banner of `device` be shown now? A small burst, then one per gap, for each phone separately.
    pub fn banner_allowed(&self, device: &str, now: Instant) -> bool {
        let mut g = self.lock();
        if !g.buckets.contains_key(device) && g.buckets.len() >= MAX_BUCKETS {
            // Forget the phone that was last seen the longest ago.
            if let Some(old) = g
                .buckets
                .iter()
                .min_by_key(|(_, b)| b.refilled)
                .map(|(k, _)| k.clone())
            {
                g.buckets.remove(&old);
            }
        }
        let b = g.buckets.entry(device.to_owned()).or_insert(Bucket {
            tokens: BANNER_BURST,
            refilled: now,
        });
        let gained =
            now.saturating_duration_since(b.refilled).as_secs_f64() / BANNER_GAP.as_secs_f64();
        b.tokens = (b.tokens + gained).min(BANNER_BURST);
        b.refilled = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// The banner for `n`, with the text only when content is allowed.
    pub fn banner(&self, device_id: &str, device_name: &str, n: &Mirrored) -> MirrorBanner {
        // The slot is what a notification server replaces a banner by: it must name the phone as well, or
        // a second phone could pick the same notification id and wipe the first one's banner (and its
        // buttons) from the screen. The phone id is ours (a paired id); only letters and digits are kept.
        let hash = n.id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        });
        let dev: String = device_id
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(64)
            .collect();
        let slot = format!("{dev}-{hash}");
        MirrorBanner {
            device_name: device_name.to_owned(),
            app: if n.app.is_empty() {
                "Phone".to_owned()
            } else {
                n.app.clone()
            },
            title: n.title.clone(),
            text: self.show_content().then(|| n.text.clone()),
            clearable: n.clearable,
            actions: n.actions.clone(),
            slot,
        }
    }
}

/// Freedesktop banner through `notify-send` (libnotify >= 0.8 for buttons): the app name as the title, a
/// Dismiss button when the phone lets the notification be cleared, and up to three of the phone's own
/// buttons. A button press comes back on stdout (`--wait`). Text from the phone goes through
/// [`crate::notify_text::plain`] and follows `--`, so it can never be an option or markup.
pub struct NotifySendMirror;

impl NotifySendMirror {
    pub fn args(b: &MirrorBanner) -> Vec<String> {
        let clean = |s: &str, max: usize| crate::notify_text::plain(s, max);
        let isolate = |s: &str, max: usize| crate::notify_text::isolate(s, max);
        let mut args = vec![
            "--app-name=GeekBridge".to_owned(),
            "--expire-time=10000".into(),
            "--category=im.received".into(),
            format!(
                "--hint=string:x-canonical-private-synchronous:geekbridge-mirror-{}",
                b.slot
            ),
            "--wait".into(),
        ];
        if b.clearable {
            args.push("--action=dismiss=Dismiss".into());
        }
        for (i, a) in b
            .actions
            .iter()
            .take(bridge_core::notification_link::MAX_ACTIONS)
            .enumerate()
        {
            // The key is an index, never the phone's text; the label is cleaned and has no `=`.
            let label = clean(a, 32).replace('=', "-");
            let label = if label.trim().is_empty() {
                "Action".to_owned()
            } else {
                label
            };
            args.push(format!("--action=a{i}={label}"));
        }
        args.push("--".into());
        args.push(clean(&b.app, 48));
        let body = match &b.text {
            // Each phone-chosen piece is isolated so an Arabic or Hebrew title cannot reorder the colon and text.
            Some(t) if !t.is_empty() => format!("{}: {}", isolate(&b.title, 80), isolate(t, 160)),
            Some(_) => clean(&b.title, 160),
            None => format!("New notification on {}", isolate(&b.device_name, 48)),
        };
        args.push(body);
        args
    }

    /// The button named by `notify-send`'s output.
    pub fn choice(stdout: &str, actions: &[String]) -> Option<Choice> {
        let key = stdout.trim();
        if key == "dismiss" {
            return Some(Choice::Dismiss);
        }
        let i: usize = key.strip_prefix('a')?.parse().ok()?;
        actions.get(i).cloned().map(Choice::Action)
    }
}

/// Banners (one `notify-send --wait` process and thread each) open at once. A notification server that
/// ignores the expiry, or a phone that floods, must not grow processes without bound.
pub const MAX_OPEN_BANNERS: usize = 8;
/// A banner is abandoned (its process killed) after this long, whatever the server does.
pub const BANNER_LIFETIME: Duration = Duration::from_secs(60);

static OPEN_BANNERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// One of at most `max` open slots; released on drop.
pub struct OpenSlot<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> OpenSlot<'a> {
    pub fn acquire(counter: &'a std::sync::atomic::AtomicUsize, max: usize) -> Option<Self> {
        use std::sync::atomic::Ordering::SeqCst;
        let mut n = counter.load(SeqCst);
        loop {
            if n >= max {
                return None;
            }
            match counter.compare_exchange_weak(n, n + 1, SeqCst, SeqCst) {
                Ok(_) => return Some(Self(counter)),
                Err(now) => n = now,
            }
        }
    }
}

impl Drop for OpenSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Run `cmd`, return its stdout, and kill it if it is still running after `limit`.
pub fn run_bounded(mut cmd: std::process::Command, limit: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()?
        .take(256)
        .read_to_string(&mut out)
        .ok()?;
    Some(out)
}

impl MirrorNotifier for NotifySendMirror {
    fn show(&self, b: &MirrorBanner, reply: Box<dyn FnOnce(Choice) + Send>) {
        let Some(slot) = OpenSlot::acquire(&OPEN_BANNERS, MAX_OPEN_BANNERS) else {
            return; // too many still open: this one is only listed (ctl, tray, Home)
        };
        let args = Self::args(b);
        let actions = b.actions.clone();
        std::thread::spawn(move || {
            let _slot = slot;
            let mut cmd = std::process::Command::new("notify-send");
            cmd.args(&args);
            if let Some(out) = run_bounded(cmd, BANNER_LIFETIME)
                && let Some(c) = Self::choice(&out, &actions)
            {
                reply(c);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str) -> Mirrored {
        Mirrored {
            id: id.to_owned(),
            app: "Chat".into(),
            title: "Ada".into(),
            text: "code 123456".into(),
            time_ms: None,
            clearable: true,
            silent: false,
            only_once: false,
            actions: vec![],
        }
    }

    #[test]
    fn memory_is_bounded_per_phone_and_by_age() {
        let m = Mirror::default();
        let t0 = Instant::now();
        for i in 0..(MAX_PER_DEVICE + 20) {
            m.post("a", "Pixel", n(&format!("n{i}")), t0);
        }
        assert_eq!(m.count("a"), MAX_PER_DEVICE);
        assert!(m.get("a", "n0", t0).is_none() && m.get("a", "n69", t0).is_some());
        let late = t0 + TTL + Duration::from_secs(1);
        assert_eq!(m.view(None, vec![], late).items.len(), 0);
        for d in 0..(MAX_DEVICES + 3) {
            m.post(&format!("d{d}"), "P", n("x"), t0);
        }
        let seen: std::collections::HashSet<_> = m
            .view(None, vec![], t0)
            .items
            .into_iter()
            .map(|i| i.device_id)
            .collect();
        assert_eq!(seen.len(), MAX_DEVICES);
    }

    #[test]
    fn an_update_replaces_and_a_removal_and_clear_erase() {
        let m = Mirror::default();
        let t = Instant::now();
        assert_eq!(m.post("a", "P", n("1"), t), Posted::New);
        let mut u = n("1");
        u.title = "Bob".into();
        assert_eq!(m.post("a", "P", u, t), Posted::Updated);
        assert_eq!(m.count("a"), 1);
        assert_eq!(m.get("a", "1", t).unwrap().title, "Bob");
        assert!(m.remove("a", "1") && !m.remove("a", "1"));
        m.post("a", "P", n("2"), t);
        m.clear("a");
        assert_eq!(m.count("a"), 0);
    }

    #[test]
    fn banners_hide_the_text_unless_content_is_allowed_and_are_rate_limited() {
        let m = Mirror::default();
        assert_eq!(m.banner("dev", "Pixel", &n("1")).text, None);
        m.set_show_content(true);
        assert_eq!(
            m.banner("dev", "Pixel", &n("1")).text.as_deref(),
            Some("code 123456")
        );
        assert_eq!(
            m.banner("dev", "P", &n("1")).slot,
            m.banner("dev", "P", &n("1")).slot
        );
        assert_ne!(
            m.banner("dev", "P", &n("1")).slot,
            m.banner("dev", "P", &n("2")).slot
        );
        let t = Instant::now();
        let allowed = (0..20).filter(|_| m.banner_allowed("dev", t)).count();
        assert_eq!(allowed, BANNER_BURST as usize);
        assert!(m.banner_allowed("dev", t + BANNER_GAP * 2));
    }

    #[test]
    fn notify_send_arguments_keep_phone_text_after_the_separator() {
        let b = MirrorBanner {
            device_name: "Pixel".into(),
            app: "--evil <b>app</b>".into(),
            title: "Ada".into(),
            text: None,
            clearable: true,
            actions: vec!["Mark=read".into(), "Archive".into()],
            slot: "1".into(),
        };
        let a = NotifySendMirror::args(&b);
        let sep = a.iter().position(|x| x == "--").unwrap();
        assert!(a[..sep].iter().all(|x| x.starts_with("--")));
        assert!(a.contains(&"--action=dismiss=Dismiss".to_owned()));
        assert!(a.contains(&"--action=a0=Mark-read".to_owned()));
        assert_eq!(a.len(), sep + 3);
        assert!(!a[sep + 1].contains('<'));
        assert!(!a[sep + 2].contains("Ada"), "title hidden without content");
        assert_eq!(
            NotifySendMirror::choice("a1\n", &b.actions),
            Some(Choice::Action("Archive".into()))
        );
        assert_eq!(
            NotifySendMirror::choice("dismiss", &[]),
            Some(Choice::Dismiss)
        );
        assert_eq!(NotifySendMirror::choice("a9", &b.actions), None);
        assert_eq!(NotifySendMirror::choice("", &b.actions), None);
    }

    #[test]
    fn two_phones_with_the_same_notification_id_never_share_a_banner_slot() {
        let m = Mirror::default();
        let a = m.banner("aaaa", "A", &n("same")).slot;
        let b = m.banner("bbbb", "B", &n("same")).slot;
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        // A hostile id cannot add hint syntax either.
        let h = m.banner("aaaa", "A", &n("x:y;z\n")).slot;
        assert!(h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn open_banners_are_capped_and_a_stuck_one_is_killed() {
        use std::sync::atomic::AtomicUsize;
        let c = AtomicUsize::new(0);
        let held: Vec<_> = (0..3).filter_map(|_| OpenSlot::acquire(&c, 3)).collect();
        assert_eq!(held.len(), 3);
        assert!(OpenSlot::acquire(&c, 3).is_none());
        drop(held);
        assert!(OpenSlot::acquire(&c, 3).is_some());
        let t = Instant::now();
        let mut cmd = std::process::Command::new("sleep");
        cmd.arg("30");
        assert!(run_bounded(cmd, Duration::from_millis(300)).is_none());
        assert!(t.elapsed() < Duration::from_secs(5));
        let mut cmd = std::process::Command::new("echo");
        cmd.arg("a1");
        assert_eq!(
            run_bounded(cmd, Duration::from_secs(5)).as_deref(),
            Some("a1\n")
        );
    }

    #[test]
    fn a_button_whose_label_cleans_to_nothing_still_gets_a_label() {
        let b = MirrorBanner {
            device_name: "P".into(),
            app: "A".into(),
            title: "t".into(),
            text: None,
            clearable: false,
            actions: vec!["\u{200B}\u{202E}".into()],
            slot: "1".into(),
        };
        assert!(NotifySendMirror::args(&b).contains(&"--action=a0=Action".to_owned()));
    }

    #[test]
    fn a_chatty_phone_cannot_use_up_another_phones_banners() {
        let m = Mirror::default();
        let t0 = Instant::now();
        let burst = (0..100).filter(|_| m.banner_allowed("loud", t0)).count();
        assert!(burst >= 2);
        assert!(!m.banner_allowed("loud", t0), "the loud phone is out");
        // The quiet phone still gets its whole burst, and the loud one's refill is its own.
        assert_eq!(
            (0..100).filter(|_| m.banner_allowed("quiet", t0)).count(),
            burst
        );
        assert!(!m.banner_allowed("quiet", t0));
        assert!(m.banner_allowed("loud", t0 + Duration::from_millis(2500)));
    }

    #[test]
    fn the_banner_limiter_remembers_a_bounded_number_of_phones() {
        let m = Mirror::default();
        let t0 = Instant::now();
        for i in 0..500u64 {
            assert!(m.banner_allowed(&format!("phone{i}"), t0 + Duration::from_millis(i)));
        }
    }
}
