// SPDX-License-Identifier: GPL-3.0-or-later
//! Live call state for the Calls window: a worker thread asks the service for the merged call view
//! about once a second and runs the user's decisions (answer, decline, hang up, mute). All blocking
//! I/O stays on the worker; the window only reads the latest result. Phone numbers and names pass
//! through here in memory and are never logged.
use crate::connection::Config;
use bridge_core::ipc::{CallsStatus, Request, Response};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Calls must feel immediate, so this is deliberately not slowed down like the other pollers.
pub const POLL: Duration = Duration::from_millis(1000);
/// Back-off while the service is unreachable.
pub const POLL_DOWN: Duration = Duration::from_secs(4);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeedState {
    pub status: Option<CallsStatus>,
    /// Why the service could not be asked or refused the last decision; plain words.
    pub problem: Option<String>,
}

pub struct CallFeed {
    state: Arc<Mutex<FeedState>>,
    commands: mpsc::Sender<Request>,
    stop: Arc<AtomicBool>,
}

impl CallFeed {
    pub fn spawn(wake: impl Fn() + Send + 'static) -> Self {
        let fetch_cfg = Config::from_environment();
        let send_cfg = Config::from_environment();
        Self::spawn_with(
            POLL,
            move || match fetch_cfg.request(&Request::Calls)? {
                Response::Calls(c) => Ok(*c),
                _ => Err(crate::i18n::tr("feed.unexpected").into()),
            },
            move |request| match send_cfg.request(request)? {
                Response::Ack => Ok(()),
                Response::Error(e) => Err(e.message),
                _ => Err(crate::i18n::tr("feed.unexpected").into()),
            },
            wake,
        )
    }

    /// `wake` runs only when the shown state changed.
    pub fn spawn_with(
        interval: Duration,
        mut fetch: impl FnMut() -> Result<CallsStatus, String> + Send + 'static,
        mut send: impl FnMut(&Request) -> Result<(), String> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(FeedState::default()));
        let (commands, command_rx) = mpsc::channel::<Request>();
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, halt) = (state.clone(), stop.clone());
        std::thread::Builder::new()
            .name("geekbridge-call-feed".into())
            .spawn(move || {
                let mut action_problem: Option<String> = None;
                let mut next = FeedState::default();
                loop {
                    let mut down = false;
                    match fetch() {
                        Ok(status) => {
                            next.status = Some(status);
                            next.problem = action_problem.clone();
                        }
                        Err(why) => {
                            next.status = None;
                            next.problem = Some(why);
                            down = true;
                        }
                    }
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if *g != next {
                            *g = next.clone();
                            drop(g);
                            wake();
                        }
                    }
                    let wait = if down { POLL_DOWN } else { interval };
                    match command_rx.recv_timeout(wait) {
                        Ok(request) => {
                            action_problem = send(&request).err();
                            // A decision is followed by an immediate refresh to show its effect.
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            // A refusal stays visible until the next poll shows a changed call.
                            if !down {
                                action_problem = None;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                }
            })
            .expect("spawn call feed thread");
        Self {
            state,
            commands,
            stop,
        }
    }

    pub fn state(&self) -> FeedState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Queue a decision; the worker runs it and refreshes.
    pub fn send(&self, request: Request) {
        let _ = self.commands.send(request);
    }
}

impl Drop for CallFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_core::calls_view::CallsView;
    use std::sync::atomic::AtomicUsize;

    fn status() -> CallsStatus {
        CallsStatus {
            view: CallsView::empty(),
            notice: None,
            sources: vec![],
            audio_note: None,
            wifi: None,
            replies: Default::default(),
        }
    }

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(30);
        while !f() {
            assert!(std::time::Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn wakes_on_change_only_and_reports_an_unreachable_service() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let w = wakes.clone();
        let fail = Arc::new(AtomicBool::new(false));
        let f = fail.clone();
        let feed = CallFeed::spawn_with(
            Duration::from_millis(5),
            move || {
                if f.load(Ordering::SeqCst) {
                    Err("down".into())
                } else {
                    Ok(status())
                }
            },
            |_| Ok(()),
            move || {
                w.fetch_add(1, Ordering::SeqCst);
            },
        );
        wait_until("first state", || feed.state().status.is_some());
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            1,
            "unchanged polls must not repaint"
        );
        fail.store(true, Ordering::SeqCst);
        feed.send(Request::Calls); // wakes the worker out of its back-off
        wait_until("problem", || feed.state().problem.is_some());
        assert!(feed.state().status.is_none());
    }

    #[test]
    fn a_refused_decision_is_shown_and_then_cleared() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        let feed = CallFeed::spawn_with(
            Duration::from_millis(1000), // the refusal shows for one cycle; keep it observable
            || Ok(status()),
            move |r| {
                s.lock().unwrap().push(format!("{r:?}"));
                Err("The phone is busy.".into())
            },
            || {},
        );
        wait_until("first state", || feed.state().status.is_some());
        feed.send(Request::CallAudio { source: "x".into() });
        wait_until("problem shown", || {
            feed.state().problem.as_deref() == Some("The phone is busy.")
        });
        assert_eq!(seen.lock().unwrap().len(), 1);
        wait_until("problem cleared", || feed.state().problem.is_none());
    }
}
