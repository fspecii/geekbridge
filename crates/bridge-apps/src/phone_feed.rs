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
    pub view: Option<V>,
    /// Why the service could not be asked, or refused the last action; plain words.
    pub problem: Option<String>,
    /// Short confirmation of the last action ("Sent to the phone.").
    pub notice: Option<String>,
}
impl<V> Default for FeedState<V> {
    fn default() -> Self {
        Self {
            view: None,
            problem: None,
            notice: None,
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
}

/// What an action returned: a notice to show, or a refusal.
pub type ActionResult = Result<Option<String>, String>;

impl<V: Clone + PartialEq + Send + 'static> Feed<V> {
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
                loop {
                    if halt.load(Ordering::Relaxed) {
                        return;
                    }
                    let request = wanted.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let mut down = false;
                    let next = match fetch(&request) {
                        Ok(view) => FeedState {
                            view: Some(view),
                            problem: sticky.0.clone(),
                            notice: sticky.1.clone(),
                        },
                        Err(why) => {
                            down = true;
                            FeedState {
                                view: None,
                                problem: Some(why),
                                notice: None,
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
                            sticky = match act(&action) {
                                Ok(notice) => (None, notice),
                                Err(why) => (Some(why), None),
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

    /// Queue an action; the worker runs it and then refreshes.
    pub fn run(&self, request: Request) {
        let _ = self.commands.send(Cmd::Run(request));
    }
}

impl<V> Drop for Feed<V> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn unexpected() -> String {
    "The service answered with something unexpected.".into()
}

fn act_on(cfg: &Config, request: &Request) -> ActionResult {
    match cfg.request(request)? {
        Response::Ack => Ok(match request {
            Request::SetConsent { enabled: true, .. } => {
                Some("Allowed. Asking the phone now.".into())
            }
            Request::SetConsent { .. } => {
                Some("Turned off. What was shared has been erased.".into())
            }
            Request::CalendarEdit { .. } => {
                Some("Sent to the phone. Waiting for it to confirm.".into())
            }
            Request::CalendarEditRetry { .. } => Some("Asked the phone again.".into()),
            _ => None,
        }),
        Response::MessageQueued { .. } => Ok(Some(
            "Handed to the phone. Waiting for it to confirm.".into(),
        )),
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
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while !f() {
            assert!(std::time::Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn follows_params_shows_action_results_and_reports_a_down_service() {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
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
            || {},
        );
        wait_until("down", || feed.state().problem.as_deref() == Some("down"));
        feed.set_params(Request::Events { after: 7 });
        wait_until("view", || feed.state().view.as_deref() == Some("events7"));
        feed.run(Request::Ping {
            device_id: "x".into(),
        });
        wait_until("notice", || {
            feed.state().notice.as_deref() == Some("pinged")
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
    }
}
