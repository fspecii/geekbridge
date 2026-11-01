//! End to end over a real pinned-TLS KDE link on loopback: the desktop service, a scripted phone
//! (`bridge_phone_sim::data::PhoneScript`), and the control API the Agenda and Messages windows use.
//! Covers consent gating, contacts/calendar/SMS import, send status and erasure on consent-off.
use bridge_core::{
    Snapshot,
    agenda::Date,
    ipc::{self, Feature, IpcError, LinkPhase, PairMode, Request, Response, Token},
    phone_view::{AgendaView, DataState, MessagesView, SendPhase},
};
use bridge_kde as kde;
use bridge_phone_sim::data::{self, PhoneScript};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    control::{self, Control},
    daemon::DaemonOptions,
    prepare_control,
};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{RwLock, mpsc},
    task::JoinHandle,
    time::{sleep, timeout},
};

struct Yes;
impl kde::Approver for Yes {
    async fn approve(&mut self, _: kde::ApprovalRequest) -> bool {
        true
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    token: Token,
    port: u16,
    tasks: Vec<JoinHandle<std::io::Result<()>>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.tasks.iter().for_each(|t| t.abort());
    }
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(DisabledCallingProbe));
        options.dev_loopback = true;
        options.share.save_dir = tmp.path().join("dl");
        let control = prepare_control(&state_dir, discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open");
        };
        let token = token.clone();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let tasks = vec![tokio::spawn(control::serve_control(l, Arc::new(control)))];
        Self {
            _tmp: tmp,
            token,
            port,
            tasks,
        }
    }

    async fn call(&self, request: Request) -> Result<Response, IpcError> {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
    }
    async fn ok(&self, request: Request) {
        match self.call(request).await {
            Ok(Response::Ack) => {}
            other => panic!("expected ack, got {other:?}"),
        }
    }
    async fn agenda(&self, query: &str, day: &str) -> AgendaView {
        match self
            .call(Request::AgendaView {
                query: query.into(),
                day: day.into(),
                utc_offset_minutes: 0,
            })
            .await
        {
            Ok(Response::Agenda(v)) => *v,
            other => panic!("expected agenda, got {other:?}"),
        }
    }
    async fn messages(&self, device: Option<String>, thread: Option<i64>) -> MessagesView {
        match self
            .call(Request::MessagesView {
                device_id: device,
                thread_id: thread,
            })
            .await
        {
            Ok(Response::Messages(v)) => *v,
            other => panic!("expected messages, got {other:?}"),
        }
    }
    async fn status(&self) -> ipc::Status {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::status(port, &token))
            .await
            .unwrap()
            .unwrap()
    }
    async fn until<T>(&self, what: &str, mut f: impl AsyncFnMut() -> Option<T>) -> T {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(v) = f().await {
                return v;
            }
            assert!(Instant::now() < end, "timed out waiting for {what}");
            sleep(Duration::from_millis(40)).await;
        }
    }

    async fn pair(&self, creds: &Arc<kde::DeviceCredentials>) -> kde::TrustedLink {
        let (incoming, outgoing) = data::capabilities();
        let mut config = kde::TransportConfig::default();
        config.address_policy.loopback = true;
        config.limits.packet_line = 2 * 1024 * 1024;
        let endpoint = kde::Endpoint::new(
            creds.clone(),
            "Test Phone",
            kde::DeviceType::Phone,
            incoming,
            outgoing,
            Arc::new(kde::MemoryTrustStore::new()),
            config,
        )
        .unwrap();
        let id = creds.device_id().to_string();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = ipc::Endpoint {
            address: "127.0.0.1".parse().unwrap(),
            port: l.local_addr().unwrap().port(),
        };
        let join = tokio::spawn(async move {
            let (stream, _) = l.accept().await.unwrap();
            match endpoint.accept(stream).await.unwrap() {
                kde::Handshake::Pending(p) => p
                    .pair(kde::PairMode::AwaitPeerRequest, &mut Yes)
                    .await
                    .unwrap(),
                kde::Handshake::Trusted(t) => t,
            }
        });
        self.ok(Request::Connect {
            device_id: id.clone(),
            endpoint: Some(address),
        })
        .await;
        self.until("unpaired", async || {
            self.status()
                .await
                .links
                .iter()
                .any(|l| l.device_id == id && l.phase == LinkPhase::Unpaired)
                .then_some(())
        })
        .await;
        self.ok(Request::Pair {
            device_id: id.clone(),
            mode: PairMode::Request,
        })
        .await;
        let challenge = self
            .until("challenge", async || {
                self.status()
                    .await
                    .challenges
                    .first()
                    .map(|c| c.challenge_id.clone())
            })
            .await;
        self.ok(Request::Approve {
            challenge_id: challenge,
        })
        .await;
        let link = timeout(Duration::from_secs(10), join)
            .await
            .unwrap()
            .unwrap();
        self.until("connected", async || {
            self.status()
                .await
                .links
                .iter()
                .any(|l| l.device_id == id && l.phase == LinkPhase::Connected)
                .then_some(())
        })
        .await;
        link
    }
}

fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        / 86_400;
    let d = Date::from_epoch_day(days as i64).unwrap();
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

type Push = mpsc::Sender<(&'static str, serde_json::Map<String, serde_json::Value>)>;

/// The phone: answers requests from the script, and forwards pushes from the test.
fn run_phone(
    mut link: kde::TrustedLink,
    mut script: PhoneScript,
) -> (Push, mpsc::UnboundedReceiver<String>) {
    let (push_tx, mut push_rx) = mpsc::channel(8);
    let (seen_tx, seen_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                ev = link.recv() => match ev {
                    Ok(kde::LinkEvent::Packet(p)) => {
                        let _ = seen_tx.send(p.kind.clone());
                        for (kind, body) in script.handle(&p.kind, &p.body) {
                            if link.send(&kde::Packet::new(kind, body)).await.is_err() { return; }
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                },
                Some((kind, body)) = push_rx.recv() => {
                    if link.send(&kde::Packet::new(kind, body)).await.is_err() { return; }
                }
            }
        }
    });
    (push_tx, seen_rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn consent_gates_every_feature_and_data_flows_end_to_end() {
    let rig = Rig::start().await;
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let id = creds.device_id().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let link = rig.pair(&creds).await;
    let (push, mut seen) = run_phone(link, PhoneScript::demo(now));

    // Consent is off by default: the phone is never asked for anything.
    sleep(Duration::from_millis(400)).await;
    while let Ok(kind) = seen.try_recv() {
        assert!(
            kind == "kdeconnect.ping",
            "unexpected request before consent: {kind}"
        );
    }
    assert!(rig.agenda("", &today()).await.people.is_empty());

    for f in [Feature::Contacts, Feature::Calendar, Feature::Sms] {
        rig.ok(Request::SetConsent {
            device_id: id.clone(),
            feature: f,
            enabled: true,
        })
        .await;
    }

    // Agenda: contacts and today's events arrive.
    let v = rig
        .until("agenda data", async || {
            let v = rig.agenda("", &today()).await;
            (v.people.len() == 5 && !v.events.is_empty()).then_some(v)
        })
        .await;
    assert_eq!(v.people[0].name, "Ada Lovelace");
    assert_eq!(v.devices[0].contacts, DataState::Ready);
    assert!(
        v.events.first().unwrap().all_day,
        "all-day events sort first"
    );
    assert!(v.events.iter().any(|e| e.title == "Standup"));
    assert_eq!(rig.agenda("grace", &today()).await.people.len(), 1);

    // Messages: conversations, contact names resolved, history on demand.
    let m = rig
        .until("conversations", async || {
            let m = rig.messages(None, None).await;
            (m.conversations.len() == 3).then_some(m)
        })
        .await;
    assert_eq!(m.conversations[0].title, "Ada Lovelace");
    assert_eq!(m.conversations[0].unread, 1);
    let thread = m.conversations[0].thread_id;
    rig.ok(Request::MessagesRefresh {
        device_id: id.clone(),
        thread_id: Some(thread),
    })
    .await;
    rig.until("history", async || {
        let m = rig.messages(Some(id.clone()), Some(thread)).await;
        (m.selected.as_ref()?.messages.len() == 3).then_some(())
    })
    .await;

    // Send: the phone's own reports raise the status step by step; same submission is idempotent.
    let send = || Request::MessagesSend {
        device_id: id.clone(),
        recipient: "+44 20 7946 0001".into(),
        text: "See you there".into(),
        submission_id: "click-1".into(),
    };
    let Ok(Response::MessageQueued { send_id }) = rig.call(send()).await else {
        panic!("send refused")
    };
    let Ok(Response::MessageQueued { send_id: again }) = rig.call(send()).await else {
        panic!("resend refused")
    };
    assert_eq!(send_id, again);
    let sent = rig
        .until("delivered", async || {
            let m = rig.messages(None, None).await;
            (m.sends.first()?.phase == SendPhase::Delivered).then_some(m)
        })
        .await;
    assert_eq!(sent.sends.len(), 1, "one click, one send");

    // An incoming text updates the conversation.
    let (kind, body) = {
        // Build the push the phone app would make.
        let mut s = PhoneScript::demo(now);
        s.incoming_sms(1, "+44 20 7946 0001", "Running late", now + 1000)
    };
    push.send((kind, body)).await.unwrap();
    rig.until("incoming", async || {
        let m = rig.messages(None, None).await;
        (m.conversations
            .iter()
            .find(|c| c.thread_id == thread)?
            .last_body
            == "Running late")
            .then_some(())
    })
    .await;

    // Turning a consent off erases that feature's data immediately.
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::Sms,
        enabled: false,
    })
    .await;
    let m = rig.messages(None, None).await;
    assert!(m.conversations.is_empty() && m.sends.is_empty());
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::Contacts,
        enabled: false,
    })
    .await;
    assert!(rig.agenda("", &today()).await.people.is_empty());
    assert!(
        !rig.agenda("", &today()).await.events.is_empty(),
        "calendar consent is separate"
    );

    // Unpairing forgets everything.
    rig.ok(Request::Unpair {
        device_id: id.clone(),
    })
    .await;
    assert!(rig.agenda("", &today()).await.devices.is_empty());
}
