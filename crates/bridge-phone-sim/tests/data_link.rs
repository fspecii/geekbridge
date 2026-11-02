//! End to end over a real pinned-TLS KDE link on loopback: the desktop service, a scripted phone
//! (`bridge_phone_sim::data::PhoneScript`), and the control API the Agenda and Messages windows use.
//! Covers consent gating, contacts/calendar/SMS import, send status and erasure on consent-off.
use bridge_core::{
    Snapshot,
    agenda::Date,
    ipc::{self, Feature, IpcError, LinkPhase, PairMode, Request, Response, Token},
    phone_view::{
        AgendaView, CalendarEditAction, DataState, EditPhase, EventDraft, MessagesView, SendPhase,
    },
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
                days: 1,
                people_limit: 0,
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
                query: None,
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
            (m.conversations.len() == 5).then_some(m)
        })
        .await;
    assert_eq!(m.conversations[0].title, "Alan Turing", "newest first");
    assert_eq!(m.conversations[0].unread, 2);
    let ada = m
        .conversations
        .iter()
        .find(|c| c.title == "Ada Lovelace")
        .expect("name resolved from the synced phonebook");
    assert_eq!(ada.unread, 1);
    assert!(
        m.conversations
            .iter()
            .any(|c| c.title == "+49 151 1234 5678"),
        "an unknown number falls back to the number"
    );
    // The phone listed two SIMs and sends a picture message.
    assert_eq!(m.devices[0].sims.len(), 2);
    assert!(m.devices[0].sms_send);
    let thread = ada.thread_id;
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
    let grace = m
        .conversations
        .iter()
        .find(|c| c.title == "Grace Hopper")
        .unwrap();
    let g = rig.messages(Some(id.clone()), Some(grace.thread_id)).await;
    assert!(
        g.selected
            .unwrap()
            .messages
            .iter()
            .any(|x| x.mms && x.attachments == 1),
        "MMS arrives as an attachment count, never as content"
    );

    // Search by body, then mark read (local only: the phone is not asked to change anything).
    let found = match rig
        .call(Request::MessagesView {
            device_id: None,
            thread_id: None,
            query: Some("verification".into()),
        })
        .await
    {
        Ok(Response::Messages(v)) => v.conversations,
        other => panic!("expected messages, got {other:?}"),
    };
    assert_eq!(found.len(), 1);
    assert!(found[0].matched.as_deref().unwrap().contains("482913"));
    rig.ok(Request::MessagesMarkRead {
        device_id: id.clone(),
        thread_id: thread,
    })
    .await;
    let after = rig.messages(None, None).await;
    assert_eq!(
        after
            .conversations
            .iter()
            .find(|c| c.thread_id == thread)
            .unwrap()
            .unread,
        0
    );

    // Send: the phone's own reports raise the status step by step; same submission is idempotent.
    let send = || Request::MessagesSend {
        device_id: id.clone(),
        recipient: "+44 20 7946 0001".into(),
        text: "See you there".into(),
        submission_id: "click-1".into(),
        sub_id: None,
    };
    // A SIM the phone did not list is refused before anything leaves the computer.
    let bad_sim = Request::MessagesSend {
        device_id: id.clone(),
        recipient: "+44 20 7946 0001".into(),
        text: "wrong sim".into(),
        submission_id: "click-sim".into(),
        sub_id: Some(99),
    };
    assert!(rig.call(bad_sim).await.is_err());
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

fn draft(title: &str, start: i64) -> EventDraft {
    EventDraft {
        title: title.into(),
        all_day: false,
        start_ms: start,
        end_ms: start + 3_600_000,
        tz: "UTC".into(),
        location: Some("Desk".into()),
        description: None,
        reminder_min: Some(10),
    }
}

impl Rig {
    async fn edit(
        &self,
        id: &str,
        op: &str,
        action: CalendarEditAction,
        calendar: Option<&str>,
        event: Option<&str>,
        d: Option<EventDraft>,
    ) -> Result<Response, IpcError> {
        self.call(Request::CalendarEdit {
            device_id: id.into(),
            op_id: op.into(),
            action,
            calendar_id: calendar.map(str::to_owned),
            event_id: event.map(str::to_owned),
            draft: d.map(Box::new),
        })
        .await
    }
    async fn week(&self) -> AgendaView {
        match self
            .call(Request::AgendaView {
                query: String::new(),
                day: today(),
                utc_offset_minutes: 0,
                days: 14,
                people_limit: 0,
            })
            .await
        {
            Ok(Response::Agenda(v)) => *v,
            other => panic!("expected agenda, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn calendar_edits_need_explicit_consent_and_end_up_on_the_phone() {
    let rig = Rig::start().await;
    let creds = Arc::new(kde::DeviceCredentials::generate().unwrap());
    let id = creds.device_id().to_string();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let link = rig.pair(&creds).await;
    let (_push, mut seen) = run_phone(link, PhoneScript::demo(now));
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::Calendar,
        enabled: true,
    })
    .await;
    let v = rig
        .until("calendars", async || {
            let v = rig.week().await;
            (v.calendars.len() == 2 && !v.events.is_empty()).then_some(v)
        })
        .await;
    assert_eq!(v.calendars[0].account.as_deref(), Some("demo@gmail.com"));
    assert!(v.calendars[0].writable && !v.calendars[1].writable);
    assert!(
        v.events
            .iter()
            .any(|e| e.title == "Weekly sync" && e.recurring)
    );
    assert!(
        v.events
            .iter()
            .any(|e| e.title == "Team offsite" && e.attendee_count == 2)
    );
    assert_eq!(v.devices[0].calendar_edit, DataState::ConsentOff);

    // Reading is allowed, writing is not: refused here, and the phone never sees a write packet.
    let refused = rig
        .edit(
            &id,
            "op-a",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(draft("Nope", now)),
        )
        .await;
    assert!(
        matches!(refused, Err(IpcError::Service(ref e)) if e.code == ipc::ErrorCode::Unsupported),
        "{refused:?}"
    );

    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::CalendarEdit,
        enabled: true,
    })
    .await;
    // Still no implicit calendar and no write into a read-only one.
    for cal in [None, Some("2")] {
        let r = rig
            .edit(
                &id,
                "op-b",
                CalendarEditAction::Create,
                cal,
                None,
                Some(draft("Nope", now)),
            )
            .await;
        assert!(
            matches!(r, Err(IpcError::Service(ref e)) if e.code == ipc::ErrorCode::BadRequest),
            "{r:?}"
        );
    }
    // Recurring events are shown but never editable from here.
    let weekly = v.events.iter().find(|e| e.title == "Weekly sync").unwrap();
    let r = rig
        .edit(
            &id,
            "op-c",
            CalendarEditAction::Delete,
            None,
            weekly.event_id.as_deref(),
            None,
        )
        .await;
    assert!(r.is_err());
    sleep(Duration::from_millis(300)).await;
    while let Ok(kind) = seen.try_recv() {
        assert_ne!(
            kind, "geekbridge.calendar.write",
            "a refused edit leaked to the phone"
        );
    }

    // A valid create goes out once, the phone answers, and the event shows up.
    let start = now + 3 * 86_400_000;
    rig.ok_edit(
        &id,
        "op-d",
        CalendarEditAction::Create,
        Some("1"),
        None,
        Some(draft("Planning lunch", start)),
    )
    .await;
    let v = rig
        .until("applied", async || {
            let v = rig.week().await;
            (v.edits.first()?.phase == EditPhase::Applied
                && v.events.iter().any(|e| e.title == "Planning lunch"))
            .then_some(v)
        })
        .await;
    let created = v
        .events
        .iter()
        .find(|e| e.title == "Planning lunch")
        .unwrap();
    assert_eq!(created.location.as_deref(), Some("Desk"));
    assert_eq!(created.reminders_min, vec![10]);
    // Reusing the operation id is refused before it reaches the phone.
    let again = rig
        .edit(
            &id,
            "op-d",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(draft("Planning lunch", start)),
        )
        .await;
    assert!(again.is_err());

    // Edit, then delete, by the phone's event id.
    let eid = created.event_id.clone().unwrap();
    rig.ok_edit(
        &id,
        "op-e",
        CalendarEditAction::Update,
        None,
        Some(&eid),
        Some(draft("Planning dinner", start)),
    )
    .await;
    rig.until("renamed", async || {
        rig.week()
            .await
            .events
            .iter()
            .any(|e| e.title == "Planning dinner")
            .then_some(())
    })
    .await;
    rig.ok_edit(
        &id,
        "op-f",
        CalendarEditAction::Delete,
        None,
        Some(&eid),
        None,
    )
    .await;
    rig.until("deleted", async || {
        let v = rig.week().await;
        (!v.events.iter().any(|e| e.title.starts_with("Planning "))
            && v.edits.iter().all(|e| e.phase == EditPhase::Applied))
        .then_some(())
    })
    .await;

    // Turning the consent off drops the edit history and refuses again.
    rig.ok(Request::SetConsent {
        device_id: id.clone(),
        feature: Feature::CalendarEdit,
        enabled: false,
    })
    .await;
    assert!(rig.week().await.edits.is_empty());
    assert!(
        rig.edit(
            &id,
            "op-g",
            CalendarEditAction::Create,
            Some("1"),
            None,
            Some(draft("x", now))
        )
        .await
        .is_err()
    );
}

impl Rig {
    async fn ok_edit(
        &self,
        id: &str,
        op: &str,
        action: CalendarEditAction,
        calendar: Option<&str>,
        event: Option<&str>,
        d: Option<EventDraft>,
    ) {
        match self.edit(id, op, action, calendar, event, d).await {
            Ok(Response::Ack) => {}
            other => panic!("expected ack, got {other:?}"),
        }
    }
}
