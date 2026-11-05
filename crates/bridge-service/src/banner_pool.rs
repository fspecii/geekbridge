// SPDX-License-Identifier: GPL-3.0-or-later
//! The call banners (`notify-send --wait`): a bounded number at once, one reaper thread for all of
//! them, and no process started on the thread that reported the ring.
//!
//! The soak (docs/reports/soak-3.md) held about 390 banner processes, each with a thread and a
//! descriptor, under a synthetic 8 rings per second and a notification server that never answers.
//! Policy:
//! - at most [`MAX_IN_FLIGHT`] banners are queued or running; a new one beyond that replaces the
//!   oldest (a call banner is worth more than a stale one);
//! - a banner with the same key as one in flight replaces it (the same caller ringing again);
//! - a banner is killed after its limit whatever the server does;
//! - [`submit`] only queues. The thread that spawns, polls and reaps exists only while a banner is
//!   queued or running and exits when none is, so an idle service has neither thread nor wake-up.
//!   A hung `fork`/`exec` stalls that thread, never an async worker.
use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Banners queued or running at once.
pub const MAX_IN_FLIGHT: usize = 4;
/// How often the reaper looks at its children.
const TICK: Duration = Duration::from_millis(100);

struct Job {
    id: u64,
    cmd: Command,
    limit: Duration,
    /// Called on its own thread with what the tool printed, only when that is not empty.
    on_output: Box<dyn FnOnce(String) + Send>,
}

#[derive(Default)]
struct Shared {
    next_id: u64,
    /// (id, key) of everything queued or running, oldest first.
    live: Vec<(u64, String)>,
    inbox: VecDeque<Job>,
    cancel: Vec<u64>,
    alive: bool,
    /// Processes started over the life of the service (tests and diagnostics).
    started: u64,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared {
    next_id: 0,
    live: Vec::new(),
    inbox: VecDeque::new(),
    cancel: Vec::new(),
    alive: false,
    started: 0,
});

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Banners queued or running now.
pub fn in_flight() -> usize {
    shared().live.len()
}

/// Whether the reaper thread exists now.
pub fn reaper_alive() -> bool {
    shared().alive
}

/// Processes the reaper started so far.
pub fn started() -> u64 {
    shared().started
}

/// Queue `cmd` (stdout is captured, stdin and stderr are closed). Returns at once. `key` names the
/// banner: a banner with the same key still in flight is cancelled. `on_output` gets the tool's
/// stdout when it is not empty (the button the user pressed).
pub fn submit(
    key: &str,
    mut cmd: Command,
    limit: Duration,
    on_output: impl FnOnce(String) + Send + 'static,
) {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut g = shared();
    if let Some(i) = g.live.iter().position(|(_, k)| k == key) {
        let (id, _) = g.live.remove(i);
        g.cancel.push(id);
    }
    while g.live.len() >= MAX_IN_FLIGHT {
        let (id, _) = g.live.remove(0);
        g.cancel.push(id);
    }
    let id = g.next_id;
    g.next_id += 1;
    g.live.push((id, key.to_owned()));
    g.inbox.push_back(Job {
        id,
        cmd,
        limit,
        on_output: Box::new(on_output),
    });
    if !g.alive {
        g.alive = true;
        drop(g);
        if std::thread::Builder::new()
            .name("banner-reaper".into())
            .spawn(reaper)
            .is_err()
        {
            let mut g = shared();
            g.alive = false;
            g.live.clear();
            g.inbox.clear();
        }
    }
}

struct Running {
    id: u64,
    child: Child,
    deadline: Instant,
    on_output: Box<dyn FnOnce(String) + Send>,
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn reaper() {
    let mut running: Vec<Running> = Vec::new();
    loop {
        let (mut fresh, cancel) = {
            let mut g = shared();
            let fresh: Vec<Job> = g.inbox.drain(..).collect();
            let cancel = std::mem::take(&mut g.cancel);
            if fresh.is_empty() && running.is_empty() {
                g.alive = false;
                return;
            }
            (fresh, cancel)
        };
        fresh.retain(|j| !cancel.contains(&j.id));
        running.retain_mut(|r| {
            let gone = cancel.contains(&r.id);
            if gone {
                stop(&mut r.child);
            }
            !gone
        });
        for mut job in fresh {
            // May block (fork/exec): on this thread only.
            match job.cmd.spawn() {
                Ok(child) => {
                    shared().started += 1;
                    running.push(Running {
                        id: job.id,
                        child,
                        deadline: Instant::now() + job.limit,
                        on_output: job.on_output,
                    });
                }
                Err(_) => shared().live.retain(|(id, _)| *id != job.id),
            }
        }
        let now = Instant::now();
        let mut done = Vec::new();
        running.retain_mut(|r| match r.child.try_wait() {
            Ok(None) if now < r.deadline => true,
            Ok(Some(_)) => {
                done.push(r.id);
                if let Some(out) = r.child.stdout.take() {
                    let mut text = String::new();
                    let _ = out.take(512).read_to_string(&mut text);
                    let on_output = std::mem::replace(&mut r.on_output, Box::new(|_| {}));
                    if !text.trim().is_empty() {
                        let _ = std::thread::Builder::new()
                            .name("banner-reply".into())
                            .spawn(move || on_output(text));
                    }
                }
                false
            }
            _ => {
                stop(&mut r.child);
                done.push(r.id);
                false
            }
        });
        if !done.is_empty() {
            shared().live.retain(|(id, _)| !done.contains(id));
        }
        std::thread::sleep(TICK);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    // The pool is process-wide; its tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn sleeper(secs: &str) -> Command {
        let mut c = Command::new("sleep");
        c.arg(secs);
        c
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_flood_keeps_at_most_four_in_flight_and_the_newest_survive() {
        let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let before = started();
        for i in 0..200 {
            submit(
                &format!("ring {i}"),
                sleeper("30"),
                Duration::from_secs(30),
                |_| {},
            );
            assert!(in_flight() <= MAX_IN_FLIGHT);
        }
        assert_eq!(in_flight(), MAX_IN_FLIGHT);
        let newest: Vec<String> = shared().live.iter().map(|(_, k)| k.clone()).collect();
        assert_eq!(newest, ["ring 196", "ring 197", "ring 198", "ring 199"]);
        wait_until("reaper catches up", || {
            // Everything older than the last four was cancelled before or just after its start.
            started() - before <= 200
        });
        std::thread::sleep(Duration::from_millis(400));
        #[cfg(target_os = "linux")]
        assert!(
            live_children() <= MAX_IN_FLIGHT,
            "{} children",
            live_children()
        );
        // Cleanup: replace all with quick ones.
        for i in 0..MAX_IN_FLIGHT {
            submit(
                &format!("end {i}"),
                sleeper("0"),
                Duration::from_secs(1),
                |_| {},
            );
        }
        wait_until("drained", || in_flight() == 0 && !reaper_alive());
    }

    #[cfg(target_os = "linux")]
    fn live_children() -> usize {
        // `sleep 30` children of this process.
        let me = std::process::id().to_string();
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter(|e| {
                std::fs::read_to_string(e.path().join("stat")).is_ok_and(|s| {
                    let after = s.rsplit(')').next().unwrap_or("");
                    let mut f = after.split_whitespace();
                    let (state, ppid) = (f.next(), f.next());
                    ppid == Some(me.as_str())
                        && state != Some("Z")
                        && std::fs::read(e.path().join("cmdline"))
                            .is_ok_and(|c| c == b"sleep\x0030\x00")
                })
            })
            .count()
    }

    #[test]
    fn the_same_key_replaces_the_banner_in_flight() {
        let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..10 {
            submit("same", sleeper("30"), Duration::from_secs(30), |_| {});
        }
        assert_eq!(in_flight(), 1);
        submit("same", sleeper("0"), Duration::from_secs(1), |_| {});
        wait_until("drained", || in_flight() == 0 && !reaper_alive());
    }

    #[test]
    fn a_stuck_banner_is_killed_after_its_limit_and_the_reaper_goes_away() {
        let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let t = Instant::now();
        submit("stuck", sleeper("30"), Duration::from_millis(300), |_| {});
        wait_until("killed", || in_flight() == 0);
        assert!(t.elapsed() < Duration::from_secs(5));
        wait_until("reaper exits", || !reaper_alive());
    }

    #[test]
    fn what_the_tool_printed_reaches_the_callback_and_nothing_is_not_a_choice() {
        let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
        let (tx, rx) = mpsc::channel();
        let mut c = Command::new("echo");
        c.arg("answer");
        submit("echo", c, Duration::from_secs(5), move |s| {
            tx.send(s).unwrap();
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap().trim(),
            "answer"
        );
        let called = std::sync::Arc::new(AtomicUsize::new(0));
        let c2 = called.clone();
        submit(
            "quiet",
            Command::new("true"),
            Duration::from_secs(5),
            move |_| {
                c2.fetch_add(1, Ordering::SeqCst);
            },
        );
        wait_until("drained", || in_flight() == 0 && !reaper_alive());
        assert_eq!(called.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_missing_tool_is_nothing_and_a_hung_spawn_does_not_block_the_caller() {
        use std::os::unix::process::CommandExt;
        let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
        submit(
            "missing",
            Command::new("/nonexistent/notify-send"),
            Duration::from_secs(1),
            |_| {},
        );
        wait_until("missing tool forgotten", || {
            in_flight() == 0 && !reaper_alive()
        });
        // A spawn that takes 2 s to get to exec (a stalled fork/exec).
        let mut slow = Command::new("true");
        unsafe {
            slow.pre_exec(|| {
                std::thread::sleep(Duration::from_secs(2));
                Ok(())
            });
        }
        let t = Instant::now();
        submit("slow", slow, Duration::from_secs(5), |_| {});
        submit("next", Command::new("true"), Duration::from_secs(5), |_| {});
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "submit blocked {:?}",
            t.elapsed()
        );
        wait_until("drained", || in_flight() == 0 && !reaper_alive());
    }
}
