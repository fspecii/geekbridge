//! Background discovery polling. All blocking network I/O stays on the worker thread.
use crate::state::DiscoveryModel;
use bridge_core::{AppKind, Snapshot, fetch_snapshot};
use std::{io, sync::mpsc, time::Duration};

pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct Poller {
    commands: mpsc::Sender<()>,
    results: mpsc::Receiver<DiscoveryModel>,
}

impl Poller {
    pub fn spawn(app: AppKind, api_port: u16, wake: impl Fn() + Send + 'static) -> Self {
        Self::spawn_with(app, POLL_INTERVAL, move || fetch_snapshot(api_port), wake)
    }

    /// `wake` runs only when the model changed, so an idle window is not repainted every poll.
    pub fn spawn_with(
        app: AppKind,
        interval: Duration,
        mut fetch: impl FnMut() -> io::Result<Snapshot> + Send + 'static,
        wake: impl Fn() + Send + 'static,
    ) -> Self {
        let (commands, command_rx) = mpsc::channel::<()>();
        let (result_tx, results) = mpsc::channel();
        std::thread::Builder::new()
            .name("geekbridge-discovery".into())
            .spawn(move || {
                let mut last: Option<DiscoveryModel> = None;
                loop {
                    let model = DiscoveryModel::from_fetch(app, fetch());
                    if last.as_ref() != Some(&model) {
                        last = Some(model.clone());
                        if result_tx.send(model).is_err() {
                            return;
                        }
                        wake();
                    }
                    // A refresh request polls early; a dropped Poller ends the thread.
                    match command_rx.recv_timeout(crate::pace::scaled(interval)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("spawn discovery thread");
        Self { commands, results }
    }

    pub fn refresh(&self) {
        let _ = self.commands.send(());
    }

    /// Newest model delivered since the last call, if any.
    pub fn latest(&self) -> Option<DiscoveryModel> {
        self.results.try_iter().last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn wakes_only_on_change() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = polls.clone();
        let poller = Poller::spawn_with(
            AppKind::Calls,
            Duration::from_millis(5),
            move || {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(Snapshot::empty(1716))
            },
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
            },
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while polls.load(Ordering::SeqCst) < 5 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(polls.load(Ordering::SeqCst) >= 5);
        assert_eq!(wakes.load(Ordering::SeqCst), 1);
        assert!(poller.latest().is_some());
        assert!(poller.latest().is_none());
    }

    #[test]
    fn reports_unavailable_service() {
        let poller = Poller::spawn_with(
            AppKind::Calls,
            Duration::from_secs(60),
            || Err(io::Error::from(io::ErrorKind::ConnectionRefused)),
            || {},
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(model) = poller.latest() {
                assert!(!model.service_reachable());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no result");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
