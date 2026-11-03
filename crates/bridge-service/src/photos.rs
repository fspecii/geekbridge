// SPDX-License-Identifier: GPL-3.0-or-later
//! Recent phone photos on the computer: a short list and small thumbnails, held in memory only.
//! The wire format is `bridge_core::photos_link`; the design is `docs/design/recent-photos.md`.
//!
//! * **Asked for, never pushed.** A list is accepted only as the answer to a refresh this computer asked for in the
//!   last [`LIST_WINDOW`], and a thumbnail only for an id this store requested. Anything else is dropped unread.
//! * **Bounded.** At most [`MAX_DEVICES`] phones, `photos_link::MAX_ITEMS` photos each, and [`MAX_THUMB_BYTES_TOTAL`]
//!   of thumbnails in all (a thumbnail over the budget is simply not kept). Refreshing is limited to one every
//!   [`REFRESH_GAP`] per phone.
//! * **Memory only, erased early.** Nothing is written to disk. A phone's photos are erased when its `photos`
//!   consent is withdrawn, its link ends, it is unpaired, or after [`TTL`] without a refresh.
//! * **No log.** Names and image bytes are never logged.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bridge_core::photos_link::{List, Photo, Thumb};
use bridge_core::photos_view::PhotoItem;

pub const MAX_DEVICES: usize = 8;
/// All thumbnails together, over every phone.
pub const MAX_THUMB_BYTES_TOTAL: usize = 2 * 1024 * 1024;
pub const TTL: Duration = Duration::from_secs(30 * 60);
pub const REFRESH_GAP: Duration = Duration::from_secs(3);
/// How long after asking a list is still welcome.
pub const LIST_WINDOW: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub enum Refresh {
    Go,
    /// Asked too recently.
    TooSoon,
    /// Too many phones are being tracked.
    Full,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Heard {
    Kept,
    /// Not asked for (or too late): dropped unread.
    Unsolicited,
}

struct Thumbnail {
    width: u32,
    height: u32,
    jpeg: Vec<u8>,
}

struct Dev {
    items: Vec<Photo>,
    thumbs: HashMap<String, Thumbnail>,
    /// Thumbnails requested and not yet answered.
    pending: HashSet<String>,
    partial: bool,
    listed: bool,
    asked_at: Option<Instant>,
    last_refresh: Option<Instant>,
    touched: Instant,
}

#[derive(Default)]
struct Inner {
    devices: HashMap<String, Dev>,
}

impl Inner {
    fn prune(&mut self, now: Instant) {
        self.devices
            .retain(|_, d| now.saturating_duration_since(d.touched) <= TTL);
    }
    fn thumb_bytes(&self) -> usize {
        self.devices
            .values()
            .flat_map(|d| d.thumbs.values())
            .map(|t| t.jpeg.len())
            .sum()
    }
}

#[derive(Default)]
pub struct Photos {
    inner: Mutex<Inner>,
}

impl Photos {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The user (or a window) asks for a fresh list from `device`. `Go` means send the request.
    pub fn begin_refresh(&self, device: &str, now: Instant) -> Refresh {
        let mut g = self.lock();
        g.prune(now);
        if !g.devices.contains_key(device) && g.devices.len() >= MAX_DEVICES {
            return Refresh::Full;
        }
        let d = g.devices.entry(device.to_owned()).or_insert_with(|| Dev {
            items: Vec::new(),
            thumbs: HashMap::new(),
            pending: HashSet::new(),
            partial: false,
            listed: false,
            asked_at: None,
            last_refresh: None,
            touched: now,
        });
        if d.last_refresh
            .is_some_and(|t| now.saturating_duration_since(t) < REFRESH_GAP)
        {
            return Refresh::TooSoon;
        }
        d.last_refresh = Some(now);
        d.asked_at = Some(now);
        d.touched = now;
        Refresh::Go
    }

    /// The phone's list. On `Kept`, the ids whose thumbnails are still needed (ask for them now).
    pub fn on_list(&self, device: &str, mut list: List, now: Instant) -> (Heard, Vec<String>) {
        // Not a picture by its name (`run.sh`, `x.lnk`): never shown and never fetchable, because Save would put it
        // in the save folder without a receive prompt.
        list.items
            .retain(|p| bridge_core::photos_link::is_image_name(&p.name));
        let mut g = self.lock();
        let Some(d) = g.devices.get_mut(device) else {
            return (Heard::Unsolicited, Vec::new());
        };
        let fresh = d
            .asked_at
            .is_some_and(|t| now.saturating_duration_since(t) <= LIST_WINDOW);
        if !fresh {
            return (Heard::Unsolicited, Vec::new());
        }
        d.asked_at = None;
        let keep: HashSet<&str> = list.items.iter().map(|p| p.id.as_str()).collect();
        d.thumbs.retain(|id, _| keep.contains(id.as_str()));
        d.pending = list
            .items
            .iter()
            .filter(|p| !d.thumbs.contains_key(&p.id))
            .map(|p| p.id.clone())
            .collect();
        let need: Vec<String> = list
            .items
            .iter()
            .filter(|p| d.pending.contains(&p.id))
            .map(|p| p.id.clone())
            .collect();
        d.partial = list.partial;
        d.listed = true;
        d.items = list.items;
        d.touched = now;
        (Heard::Kept, need)
    }

    pub fn on_thumb(&self, device: &str, thumb: Thumb, now: Instant) -> Heard {
        let mut g = self.lock();
        let total = g.thumb_bytes();
        let Some(d) = g.devices.get_mut(device) else {
            return Heard::Unsolicited;
        };
        let id = match &thumb {
            Thumb::Image { id, .. } | Thumb::Gone { id } => id.clone(),
        };
        if !d.pending.remove(&id) {
            return Heard::Unsolicited;
        }
        d.touched = now;
        match thumb {
            Thumb::Gone { .. } => d.items.retain(|p| p.id != id),
            Thumb::Image {
                width,
                height,
                jpeg,
                ..
            } => {
                if total + jpeg.len() <= MAX_THUMB_BYTES_TOTAL {
                    d.thumbs.insert(
                        id,
                        Thumbnail {
                            width,
                            height,
                            jpeg,
                        },
                    );
                }
            }
        }
        Heard::Kept
    }

    pub fn photo(&self, device: &str, id: &str) -> Option<Photo> {
        let g = self.lock();
        g.devices
            .get(device)?
            .items
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }

    /// (width, height, JPEG bytes) of a held thumbnail.
    pub fn thumb(&self, device: &str, id: &str) -> Option<(u32, u32, Vec<u8>)> {
        let g = self.lock();
        let t = g.devices.get(device)?.thumbs.get(id)?;
        Some((t.width, t.height, t.jpeg.clone()))
    }

    /// (listed, partial) for one phone.
    pub fn state(&self, device: &str, now: Instant) -> (bool, bool) {
        let mut g = self.lock();
        g.prune(now);
        g.devices
            .get(device)
            .map_or((false, false), |d| (d.listed, d.partial))
    }

    pub fn items(&self, device: &str, now: Instant) -> Vec<PhotoItem> {
        let mut g = self.lock();
        g.prune(now);
        let Some(d) = g.devices.get(device) else {
            return Vec::new();
        };
        d.items
            .iter()
            .map(|p| PhotoItem {
                device_id: device.to_owned(),
                id: p.id.clone(),
                name: p.name.clone(),
                taken_ms: p.taken_ms,
                size: p.size,
                width: p.width,
                height: p.height,
                has_thumb: d.thumbs.contains_key(&p.id),
            })
            .collect()
    }

    /// Erase everything held for a phone.
    pub fn clear(&self, device: &str) {
        self.lock().devices.remove(device);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(n: usize) -> Photo {
        Photo {
            id: format!("p{n}"),
            name: format!("IMG_{n}.jpg"),
            taken_ms: n as u64,
            size: 1000,
            width: 40,
            height: 30,
        }
    }
    fn list(n: usize) -> List {
        List {
            items: (1..=n).map(photo).collect(),
            partial: false,
        }
    }
    fn img(n: usize, bytes: usize) -> Thumb {
        let mut jpeg = vec![0xFF, 0xD8, 0xFF];
        jpeg.resize(bytes, 1);
        Thumb::Image {
            id: format!("p{n}"),
            width: 10,
            height: 10,
            jpeg,
        }
    }

    #[test]
    fn nothing_is_kept_that_was_not_asked_for() {
        let s = Photos::default();
        let t0 = Instant::now();
        assert_eq!(s.on_list("a", list(3), t0), (Heard::Unsolicited, vec![]));
        assert_eq!(s.begin_refresh("a", t0), Refresh::Go);
        // Too late.
        let late = t0 + LIST_WINDOW + Duration::from_secs(1);
        assert_eq!(s.on_list("a", list(3), late).0, Heard::Unsolicited);
        // On time, once.
        let (h, need) = s.on_list("a", list(3), t0 + Duration::from_secs(1));
        assert_eq!(
            (h, need),
            (Heard::Kept, vec!["p1".into(), "p2".into(), "p3".into()])
        );
        assert_eq!(
            s.on_list("a", list(3), t0 + Duration::from_secs(2)).0,
            Heard::Unsolicited,
            "one list per request"
        );
        // A thumbnail for an id that was not requested is dropped.
        assert_eq!(s.on_thumb("a", img(9, 100), t0), Heard::Unsolicited);
        assert_eq!(s.on_thumb("a", img(1, 100), t0), Heard::Kept);
        assert_eq!(
            s.on_thumb("a", img(1, 100), t0),
            Heard::Unsolicited,
            "answered once"
        );
        assert!(s.thumb("a", "p1").is_some());
        assert!(s.thumb("a", "p2").is_none());
    }

    #[test]
    fn a_listed_name_that_is_not_a_picture_is_never_kept() {
        let s = Photos::default();
        let t0 = Instant::now();
        s.begin_refresh("a", t0);
        let mut l = list(2);
        l.items[0].name = "run.sh".into();
        l.items.push(Photo {
            name: "IMG.jpg.desktop".into(),
            id: "p3".into(),
            ..photo(3)
        });
        let (_, need) = s.on_list("a", l, t0);
        assert_eq!(need, ["p2"]);
        assert!(s.photo("a", "p1").is_none() && s.photo("a", "p3").is_none());
        assert_eq!(s.items("a", t0).len(), 1);
    }

    #[test]
    fn refreshing_is_rate_limited_and_phones_are_bounded() {
        let s = Photos::default();
        let t0 = Instant::now();
        assert_eq!(s.begin_refresh("a", t0), Refresh::Go);
        assert_eq!(
            s.begin_refresh("a", t0 + Duration::from_secs(1)),
            Refresh::TooSoon
        );
        assert_eq!(s.begin_refresh("a", t0 + REFRESH_GAP), Refresh::Go);
        for i in 0..MAX_DEVICES - 1 {
            assert_eq!(s.begin_refresh(&format!("d{i}"), t0), Refresh::Go);
        }
        assert_eq!(s.begin_refresh("one-too-many", t0), Refresh::Full);
    }

    #[test]
    fn a_new_list_keeps_thumbnails_it_still_has_and_forgets_the_rest() {
        let s = Photos::default();
        let t0 = Instant::now();
        s.begin_refresh("a", t0);
        s.on_list("a", list(2), t0);
        s.on_thumb("a", img(1, 50), t0);
        s.on_thumb("a", img(2, 50), t0);
        s.begin_refresh("a", t0 + REFRESH_GAP);
        let newer = List {
            items: vec![photo(2), photo(7)],
            partial: true,
        };
        let (_, need) = s.on_list("a", newer, t0 + REFRESH_GAP);
        assert_eq!(need, ["p7"]);
        assert!(s.thumb("a", "p1").is_none());
        assert!(s.thumb("a", "p2").is_some());
        let items = s.items("a", t0 + REFRESH_GAP);
        assert_eq!(items.len(), 2);
        assert!(items[0].has_thumb && !items[1].has_thumb);
        assert_eq!(s.state("a", t0 + REFRESH_GAP), (true, true));
    }

    #[test]
    fn thumbnails_stop_at_the_memory_budget() {
        let s = Photos::default();
        let t0 = Instant::now();
        let each = 24 * 1024;
        let mut held = 0;
        // Eight phones with a full list of the largest allowed thumbnails: 4.5 MiB offered, 2 MiB kept.
        for d in 0..MAX_DEVICES {
            let dev = format!("d{d}");
            s.begin_refresh(&dev, t0);
            s.on_list(&dev, list(24), t0);
            for n in 1..=24 {
                s.on_thumb(&dev, img(n, each), t0);
            }
            held += (1..=24)
                .filter(|n| s.thumb(&dev, &format!("p{n}")).is_some())
                .count();
        }
        assert_eq!(held, MAX_THUMB_BYTES_TOTAL / each);
    }

    #[test]
    fn gone_photos_leave_the_list_and_clear_erases_all() {
        let s = Photos::default();
        let t0 = Instant::now();
        s.begin_refresh("a", t0);
        s.on_list("a", list(2), t0);
        s.on_thumb("a", Thumb::Gone { id: "p1".into() }, t0);
        assert_eq!(s.items("a", t0).len(), 1);
        assert!(s.photo("a", "p1").is_none() && s.photo("a", "p2").is_some());
        s.clear("a");
        assert!(s.items("a", t0).is_empty());
        assert_eq!(s.state("a", t0), (false, false));
    }

    #[test]
    fn everything_expires() {
        let s = Photos::default();
        let t0 = Instant::now();
        s.begin_refresh("a", t0);
        s.on_list("a", list(1), t0);
        assert_eq!(s.items("a", t0 + TTL).len(), 1);
        assert!(s.items("a", t0 + TTL + Duration::from_secs(1)).is_empty());
    }
}
