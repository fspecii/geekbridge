// SPDX-License-Identifier: GPL-3.0-or-later
//! Live phone data (people, events, messages) for the Agenda and Messages windows. A worker thread
//! asks the service for a bounded view every couple of seconds and runs the user's actions (refresh,
//! send, allow a feature); the window only reads the latest result. All blocking I/O stays on the
//! worker. Contents pass through memory only and are never logged or written.
use crate::connection::Config;
use bridge_core::ipc::{Request, Response};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

pub const POLL: Duration = Duration::from_millis(2000);
pub const POLL_DOWN: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedState<V> {
    /// Shared, so handing the state out each frame copies a pointer, not the whole view.
    pub view: Option<Arc<V>>,
    /// Why the service could not be asked, or refused the last action; plain words.
    pub problem: Option<String>,
    /// Short confirmation of the last action ("Sent to the phone.").
    pub notice: Option<String>,
    /// How many actions the worker has finished (accepted or refused). `Feed::run` returns the number
    /// its action will have once finished, so a caller can tell when its own action is done.
    pub acted: u64,
    /// The reasons of the last few refused actions, by that number.
    pub refusals: Vec<(u64, String)>,
}

impl<V> FeedState<V> {
    /// Why action `ticket` (from `Feed::run`) was refused, if it was and is still remembered.
    pub fn refusal(&self, ticket: u64) -> Option<&str> {
        self.refusals
            .iter()
            .find(|(t, _)| *t == ticket)
            .map(|(_, why)| why.as_str())
    }
}

/// Refused actions remembered for `FeedState::refusal`.
const REFUSALS_KEPT: usize = 8;
impl<V> Default for FeedState<V> {
    fn default() -> Self {
        Self {
            view: None,
            problem: None,
            notice: None,
            acted: 0,
            refusals: Vec::new(),
        }
    }
}

enum Cmd {
    Poll,
    Run(Request),
}

pub struct Feed<V> {
    state: Arc<Mutex<FeedState<V>>>,
    params: Arc<Mutex<Request>>,
    commands: mpsc::Sender<Cmd>,
    stop: Arc<AtomicBool>,
    issued: std::sync::atomic::AtomicU64,
}

/// What an action returned: a notice to show, or a refusal.
pub type ActionResult = Result<Option<String>, String>;

impl<V: Clone + PartialEq + Send + Sync + 'static> Feed<V> {
    /// `fetch` asks for the view described by the current parameters; `act` runs one action.
    pub fn spawn_with(
        interval: Duration,
        initial: Request,
        mut fetch: impl FnMut(&Request) -> Result<V, String> + Send + 'static,
        mut act: impl FnMut(&Request) -> ActionResult + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(FeedState::default()));
        let params = Arc::new(Mutex::new(initial));
        let (commands, rx) = mpsc::channel::<Cmd>();
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, wanted, halt) = (state.clone(), params.clone(), stop.clone());
        std::thread::Builder::new()
            .name("geekbridge-phone-feed".into())
            .spawn(move || {
                let mut sticky: (Option<String>, Option<String>) = (None, None);
                let mut acted = 0_u64;
                let mut refusals: Vec<(u64, String)> = Vec::new();
                loop {
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                    let request = wanted.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let mut down = false;
                    let next = match fetch(&request) {
                        Ok(view) => FeedState {
                            view: Some(Arc::new(view)),
                            problem: sticky.0.clone(),
                            notice: sticky.1.clone(),
                            acted,
                            refusals: refusals.clone(),
                        },
                        Err(why) => {
                            down = true;
                            FeedState {
                                view: None,
                                problem: Some(why),
                                notice: None,
                                acted,
                                refusals: refusals.clone(),
                            }
                        }
                    };
                    {
                        let mut g = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if *g != next {
                            *g = next;
                            drop(g);
                            wake();
                        }
                    }
                    let wait = if down { POLL_DOWN } else { interval };
                    match rx.recv_timeout(crate::pace::scaled(wait)) {
                        Ok(Cmd::Run(action)) => {
                            acted += 1;
                            sticky = match act(&action) {
                                Ok(notice) => (None, notice),
                                Err(why) => {
                                    if refusals.len() == REFUSALS_KEPT {
                                        refusals.remove(0);
                                    }
                                    refusals.push((acted, why.clone()));
                                    (Some(why), None)
                                }
                            };
                        }
                        Ok(Cmd::Poll) | Err(mpsc::RecvTimeoutError::Timeout) => {
                            // A notice is shown for one poll cycle, a problem until the next action.
                            sticky.1 = None;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn phone feed thread");
        Self {
            state,
            params,
            commands,
            stop,
            issued: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn state(&self) -> FeedState<V> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Changes what is asked for; polls at once if it differs from before.
    pub fn set_params(&self, request: Request) {
        let mut g = self.params.lock().unwrap_or_else(|e| e.into_inner());
        if *g != request {
            *g = request;
            drop(g);
            let _ = self.commands.send(Cmd::Poll);
        }
    }

    /// Queue an action; the worker runs it and then refreshes. Returns the action's number: it is
    /// finished once `FeedState::acted` reaches it, and `FeedState::refusal` says why if it failed.
    pub fn run(&self, request: Request) -> u64 {
        let ticket = self.issued.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.commands.send(Cmd::Run(request));
        ticket
    }
}

impl<V> Drop for Feed<V> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn unexpected() -> String {
    crate::i18n::tr("feed.unexpected").into()
}

fn act_on(cfg: &Config, request: &Request) -> ActionResult {
    match cfg.request(request)? {
        Response::Ack => Ok(match request {
            Request::SetConsent { enabled: true, .. } => {
                Some(crate::i18n::tr("feed.consent_on").into())
            }
            Request::SetConsent { .. } => Some(crate::i18n::tr("feed.consent_off").into()),
            Request::CalendarEdit { .. } => Some(crate::i18n::tr("feed.edit_sent").into()),
            Request::CalendarEditRetry { .. } => Some(crate::i18n::tr("feed.edit_retry").into()),
            _ => None,
        }),
        Response::MessageQueued { .. } => Ok(Some(crate::i18n::tr("feed.message_queued").into())),
        Response::Error(e) => Err(e.message),
        _ => Err(unexpected()),
    }
}

pub fn agenda_feed(
    wake: impl Fn() + Send + 'static,
    initial: Request,
) -> Feed<bridge_core::phone_view::AgendaView> {
    let (fetch_cfg, act_cfg) = (Config::from_environment(), Config::from_environment());
    Feed::spawn_with(
        POLL,
        initial,
        move |r| match fetch_cfg.request(r)? {
            Response::Agenda(v) => Ok(*v),
            Response::Error(e) => Err(e.message),
            _ => Err(unexpected()),
        },
        move |r| act_on(&act_cfg, r),
        wake,
    )
}

pub fn messages_feed(
    wake: impl Fn() + Send + 'static,
    initial: Request,
) -> Feed<bridge_core::phone_view::MessagesView> {
    let (fetch_cfg, act_cfg) = (Config::from_environment(), Config::from_environment());
    Feed::spawn_with(
        Duration::from_millis(1500),
        initial,
        move |r| match fetch_cfg.request(r)? {
            Response::Messages(v) => Ok(*v),
            Response::Error(e) => Err(e.message),
            _ => Err(unexpected()),
        },
        move |r| act_on(&act_cfg, r),
        wake,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(what: &str, f: impl Fn() -> bool) {
        let end = std::time::Instant::now() + Duration::from_secs(30);
        while !f() {
            assert!(std::time::Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn cloning_the_state_shares_the_view() {
        let state = FeedState::<String> {
            view: Some(Arc::new("big".into())),
            ..FeedState::default()
        };
        let copy = state.clone();
        let (a, b) = (state.view.unwrap(), copy.view.unwrap());
        assert!(Arc::ptr_eq(&a, &b), "a copy is a pointer, not the view");
    }

    #[test]
    fn follows_params_shows_action_results_and_reports_a_down_service() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        // The notice lives for one poll cycle (20 ms here), shorter than a loaded runner can
        // stall the test thread, so record every state the worker publishes instead of sampling.
        let shared: Arc<std::sync::OnceLock<Arc<Mutex<FeedState<String>>>>> = Arc::default();
        let notices = Arc::new(Mutex::new(Vec::<String>::new()));
        let (sh, nt) = (shared.clone(), notices.clone());
        let feed = Feed::<String>::spawn_with(
            Duration::from_millis(20),
            Request::Status,
            move |r| {
                s.lock().unwrap().push(format!("{r:?}"));
                match r {
                    Request::Events { after } => Ok(format!("events{after}")),
                    _ => Err("down".into()),
                }
            },
            |r| match r {
                Request::Ping { .. } => Ok(Some("pinged".into())),
                _ => Err("refused".into()),
            },
            move || {
                if let Some(state) = sh.get() {
                    let n = state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .notice
                        .clone();
                    nt.lock().unwrap().extend(n);
                }
            },
        );
        let _ = shared.set(feed.state.clone());
        wait_until("down", || feed.state().problem.as_deref() == Some("down"));
        feed.set_params(Request::Events { after: 7 });
        wait_until("view", || {
            feed.state().view.as_ref().map(|v| v.as_str()) == Some("events7")
        });
        feed.run(Request::Ping {
            device_id: "x".into(),
        });
        wait_until("notice", || {
            notices.lock().unwrap().iter().any(|n| n == "pinged")
        });
        wait_until("notice expires", || feed.state().notice.is_none());
        feed.run(Request::Unpair {
            device_id: "x".into(),
        });
        wait_until("refusal", || {
            feed.state().problem.as_deref() == Some("refused")
        });
        assert!(
            feed.state().view.is_some(),
            "a refused action keeps the view"
        );
        let st = feed.state();
        assert_eq!(st.acted, 2);
        assert_eq!(st.refusal(1), None, "the first action was accepted");
        assert_eq!(st.refusal(2), Some("refused"));
        assert_eq!(feed.run(Request::Status), 3, "tickets count up");
    }
}
