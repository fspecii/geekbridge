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
    tokens: f64,
    refilled: Option<Instant>,
}

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

    /// May another banner be shown now? A small burst, then one per gap.
    pub fn banner_allowed(&self, now: Instant) -> bool {
        let mut g = self.lock();
        let last = g.refilled.unwrap_or(now);
        if g.refilled.is_none() {
            g.tokens = BANNER_BURST;
        }
        let gained = now.saturating_duration_since(last).as_secs_f64() / BANNER_GAP.as_secs_f64();
        g.tokens = (g.tokens + gained).min(BANNER_BURST);
        g.refilled = Some(now);
        if g.tokens >= 1.0 {
            g.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// The banner for `n`, with the text only when content is allowed.
    pub fn banner(&self, device_name: &str, n: &Mirrored) -> MirrorBanner {
        let slot: String =
            n.id.bytes()
                .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
                })
                .to_string();
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
            args.push(format!("--action=a{i}={}", clean(a, 32).replace('=', "-")));
        }
        args.push("--".into());
        args.push(clean(&b.app, 48));
        let body = match &b.text {
            Some(t) if !t.is_empty() => format!("{}: {}", clean(&b.title, 80), clean(t, 160)),
            Some(_) => clean(&b.title, 160),
            None => format!("New notification on {}", clean(&b.device_name, 48)),
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

impl MirrorNotifier for NotifySendMirror {
    fn show(&self, b: &MirrorBanner, reply: Box<dyn FnOnce(Choice) + Send>) {
        let args = Self::args(b);
        let actions = b.actions.clone();
        std::thread::spawn(move || {
            let out = std::process::Command::new("notify-send")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output();
            if let Ok(out) = out
                && let Some(c) = Self::choice(&String::from_utf8_lossy(&out.stdout), &actions)
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
        assert_eq!(m.banner("Pixel", &n("1")).text, None);
        m.set_show_content(true);
        assert_eq!(
            m.banner("Pixel", &n("1")).text.as_deref(),
            Some("code 123456")
        );
        assert_eq!(m.banner("P", &n("1")).slot, m.banner("P", &n("1")).slot);
        assert_ne!(m.banner("P", &n("1")).slot, m.banner("P", &n("2")).slot);
        let t = Instant::now();
        let allowed = (0..20).filter(|_| m.banner_allowed(t)).count();
        assert_eq!(allowed, BANNER_BURST as usize);
        assert!(m.banner_allowed(t + BANNER_GAP * 2));
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
}
