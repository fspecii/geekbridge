// SPDX-License-Identifier: GPL-3.0-or-later
//! A small, local, redacted trace of what the call machinery did: call-state transitions, card
//! launches and kills, answer/decline/hang-up requests and their results, the audio route, and the
//! Bluetooth and Wi-Fi hub state. It exists so a call that "did nothing" can be understood after the
//! fact (`geekbridge-ctl calls --trace`), which the service log cannot do.
//!
//! What it never holds: names, full phone numbers, message text. Calls and devices appear as a
//! short hash salted per process, numbers as their last two digits. It is capped (about 128 KiB in
//! memory, 256 KiB in the private `call-trace.log` in the state folder), lives only on this
//! computer, and is emptied when the service starts or on `calls --trace --clear`. Recording is a
//! lock and a `VecDeque` push at the moment something changes: no thread, no timer.

use bridge_core::ipc::TraceEvent;
use std::collections::{BTreeMap, VecDeque};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub const FILE: &str = "call-trace.log";
/// Memory cap (sum of line sizes) and file cap. The file is rewritten from memory when it passes its
/// cap, so it shrinks to at most the memory cap and grows again.
const MEMORY_CAP: usize = 128 * 1024;
const FILE_CAP: usize = 256 * 1024;
const MAX_DETAIL: usize = 240;

fn line(e: &TraceEvent) -> String {
    format!("{}\t{}\t{}\n", e.ms, e.kind, e.detail)
}

/// The ring itself; the global one is [`record`]'s target. Separate so tests can use their own.
pub struct Trace {
    events: VecDeque<TraceEvent>,
    bytes: usize,
    path: Option<PathBuf>,
    file: Option<std::fs::File>,
    file_bytes: usize,
    memory_cap: usize,
    file_cap: usize,
}

impl Trace {
    pub const fn new(memory_cap: usize, file_cap: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            path: None,
            file: None,
            file_bytes: 0,
            memory_cap,
            file_cap,
        }
    }

    /// Start on a fresh, private file in `dir` (an old one is emptied: a restart clears the trace).
    pub fn attach(&mut self, dir: &std::path::Path) {
        self.clear();
        let path = dir.join(FILE);
        self.file = open_private(&path);
        self.path = Some(path);
    }

    pub fn push(&mut self, ms: u64, kind: &str, detail: &str) {
        let mut detail = detail.to_owned();
        if detail.len() > MAX_DETAIL {
            let mut cut = MAX_DETAIL;
            while !detail.is_char_boundary(cut) {
                cut -= 1;
            }
            detail.truncate(cut);
        }
        let e = TraceEvent {
            ms,
            kind: kind.to_owned(),
            detail,
        };
        let text = line(&e);
        self.bytes += text.len();
        self.events.push_back(e);
        while self.bytes > self.memory_cap {
            match self.events.pop_front() {
                Some(old) => self.bytes -= line(&old).len(),
                None => break,
            }
        }
        if let Some(f) = self.file.as_mut() {
            if self.file_bytes + text.len() > self.file_cap {
                self.rewrite();
            } else if f.write_all(text.as_bytes()).is_ok() {
                self.file_bytes += text.len();
            }
        }
    }

    fn rewrite(&mut self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        self.file = open_private(&path);
        self.file_bytes = 0;
        if let Some(f) = self.file.as_mut() {
            let all: String = self.events.iter().map(line).collect();
            if f.write_all(all.as_bytes()).is_ok() {
                self.file_bytes = all.len();
            }
        }
    }

    pub fn snapshot(&self) -> Vec<TraceEvent> {
        self.events.iter().cloned().collect()
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
        self.file_bytes = 0;
        if let Some(path) = self.path.clone() {
            self.file = open_private(&path);
        }
    }
}

/// Create/truncate `path` as 0600. `None` when it cannot (the in-memory trace still works).
fn open_private(path: &std::path::Path) -> Option<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let f = o.open(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    }
    Some(f)
}

static TRACE: Mutex<Trace> = Mutex::new(Trace::new(MEMORY_CAP, FILE_CAP));

fn global() -> std::sync::MutexGuard<'static, Trace> {
    TRACE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Give the global trace its file in the state folder (and empty it). Call once at start.
pub fn init(state_dir: &std::path::Path) {
    global().attach(state_dir);
    record("service", "trace started");
}

pub fn record(kind: &str, detail: &str) {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    global().push(ms, kind, detail);
}

pub fn snapshot() -> Vec<TraceEvent> {
    global().snapshot()
}

pub fn clear() {
    global().clear();
}

// ------------------------------------------------------------------------------------ redaction

fn salt() -> &'static RandomState {
    static SALT: OnceLock<RandomState> = OnceLock::new();
    SALT.get_or_init(RandomState::new)
}

/// A short stable-in-this-run label for a call, phone or source id. Not reversible, and different
/// after a restart.
pub fn hash(id: &str) -> String {
    let mut h = salt().build_hasher();
    h.write(id.as_bytes());
    format!("{:06x}", h.finish() & 0xff_ffff)
}

/// The last two digits of a number, nothing else.
pub fn tail(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(char::is_ascii_digit).collect();
    match digits.len() {
        0 => "-".into(),
        n => format!(
            "..{}",
            digits[n.saturating_sub(2)..].iter().collect::<String>()
        ),
    }
}

/// `link:<device>|<call>` style row keys: the source kind in clear, the whole key hashed.
fn source_kind(key: &str) -> &str {
    key.split([':', '|']).next().unwrap_or("?")
}

/// What the last recorded view looked like, so only changes are written.
#[derive(Default)]
pub struct ViewMemo {
    rows: BTreeMap<String, String>,
    route: Option<String>,
    mute: Option<bool>,
}

impl ViewMemo {
    pub fn observe(&mut self, view: &bridge_core::calls_view::CallsView) {
        let mut now = BTreeMap::new();
        for r in &view.rows {
            let ctl = format!(
                "{}{}{}",
                if r.answer.is_some() { 'A' } else { '-' },
                if r.reject.is_some() { 'R' } else { '-' },
                if r.hangup.is_some() { 'H' } else { '-' }
            );
            let state = format!(
                "{:?} {:?} controls={ctl} number={} named={}",
                r.phase,
                r.direction,
                r.number
                    .as_ref()
                    .map_or("-".to_owned(), |n| tail(n.expose())),
                r.name.is_some()
            )
            .to_lowercase();
            now.insert(r.key.clone(), (state, r.missing_control.is_some()));
        }
        for (key, (state, _)) in &now {
            if self.rows.get(key) != Some(state) {
                let from = self.rows.get(key).map_or("new", |_| "changed");
                record(
                    "call",
                    &format!("{from} src={} id={} {state}", source_kind(key), hash(key)),
                );
            }
        }
        for key in self.rows.keys().filter(|k| !now.contains_key(*k)) {
            record(
                "call",
                &format!("gone src={} id={}", source_kind(key), hash(key)),
            );
        }
        self.rows = now.into_iter().map(|(k, (s, _))| (k, s)).collect();
        let route = format!("{:?}", view.audio.route).to_lowercase();
        if self.route.as_deref() != Some(route.as_str()) {
            record(
                "audio",
                &format!(
                    "route={route} connect_offered={} hint={}",
                    view.audio.connect.is_some(),
                    view.audio.hint.is_some()
                ),
            );
            self.route = Some(route);
        }
        let muted = view.mute.as_ref().and_then(|m| m.muted);
        if view.mute.is_some() && self.mute != muted {
            record("audio", &format!("mute={muted:?}"));
        }
        self.mute = muted;
    }
}

// --------------------------------------------------------------------------------- text output

/// `2026-11-05 12:00:00.123Z` for `ms` since the epoch (UTC; the trace is for reading next to the
/// service log, not for showing times of day).
pub fn clock(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms % 1000
    )
}

pub fn render(events: &[TraceEvent]) -> String {
    if events.is_empty() {
        return "no call events recorded since the service started\n".into();
    }
    events
        .iter()
        .map(|e| format!("{}  {:<8} {}\n", clock(e.ms), e.kind, e.detail))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_two_digits_only() {
        assert_eq!(tail("+1 (555) 010-1234"), "..34");
        assert_eq!(tail("7"), "..7");
        assert_eq!(tail("Mum"), "-");
    }

    #[test]
    fn hash_is_stable_short_and_hides_the_input() {
        let h = hash("link:phone-1|c1");
        assert_eq!(h, hash("link:phone-1|c1"));
        assert_eq!(h.len(), 6);
        assert!(!h.contains("phone"));
        assert_ne!(h, hash("link:phone-1|c2"));
    }

    #[test]
    fn ring_is_bounded_by_bytes_and_keeps_the_newest() {
        let mut t = Trace::new(1000, 4000);
        for i in 0..500 {
            t.push(i, "call", &format!("event number {i}"));
        }
        let s = t.snapshot();
        assert!(t.bytes <= 1000);
        assert!(s.len() < 100);
        assert_eq!(s.last().unwrap().ms, 499);
        assert!(s.windows(2).all(|w| w[0].ms < w[1].ms));
    }

    #[test]
    fn long_details_are_cut_on_a_char_boundary() {
        let mut t = Trace::new(10_000, 10_000);
        t.push(1, "x", &"é".repeat(500));
        assert!(t.snapshot()[0].detail.len() <= MAX_DETAIL);
    }

    #[test]
    fn file_is_private_capped_and_cleared() {
        let dir = std::env::temp_dir().join(format!("gb-trace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(FILE), "old secret\n").unwrap();
        let mut t = Trace::new(2000, 5000);
        t.attach(&dir);
        let path = dir.join(FILE);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "",
            "restart clears"
        );
        for i in 0..400 {
            t.push(i, "call", &format!("event number {i}"));
            assert!(std::fs::metadata(&path).unwrap().len() <= 5000);
        }
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.trim_end().ends_with("event number 399"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        t.clear();
        assert!(t.snapshot().is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clock_formats_utc() {
        assert_eq!(clock(0), "1970-01-01 00:00:00.000Z");
        assert_eq!(clock(1_793_880_000_123), "2026-11-05 12:00:00.123Z");
    }
}
