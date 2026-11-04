// SPDX-License-Identifier: GPL-3.0-or-later
//! The display-privacy choices through the control API: defaults, a live change that is stored, and a
//! marker file that still forces a value. Black box; no phone.

use bridge_core::{
    Snapshot,
    ipc::{self, Request, Response, Token},
    privacy::{ForcedBy, PrivacyKey, PrivacyView},
};
use bridge_service::{
    SharedState,
    calling::DisabledCallingProbe,
    control::{self, Control},
    daemon::DaemonOptions,
    prepare_control,
};
use std::{path::PathBuf, sync::Arc};
use tokio::{net::TcpListener, sync::RwLock, task::JoinHandle};

struct Rig {
    _tmp: tempfile::TempDir,
    state: PathBuf,
    token: Token,
    port: u16,
    task: JoinHandle<std::io::Result<()>>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let discovery: SharedState = Arc::new(RwLock::new(Snapshot::empty(1716)));
        let mut options = DaemonOptions::new("Test Desktop", Arc::new(DisabledCallingProbe));
        options.dev_loopback = true;
        options.privacy_dir = Some(state.clone());
        let control = prepare_control(&state, discovery, options);
        let Control::Ready { token, .. } = &control else {
            panic!("private state should open");
        };
        let token = token.clone();
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let task = tokio::spawn(control::serve_control(l, Arc::new(control)));
        Self {
            _tmp: tmp,
            state,
            token,
            port,
            task,
        }
    }
    async fn call(&self, request: Request) -> Response {
        let (port, token) = (self.port, self.token.clone());
        tokio::task::spawn_blocking(move || ipc::call(port, &token, &request))
            .await
            .unwrap()
            .expect("service answers")
    }
    async fn view(&self) -> PrivacyView {
        match self.call(Request::PrivacyStatus).await {
            Response::Privacy(v) => v,
            other => panic!("expected the privacy view, got {other:?}"),
        }
    }
    async fn set(&self, key: PrivacyKey, on: bool) {
        assert_eq!(
            self.call(Request::PrivacySet { key, on }).await,
            Response::Ack
        );
    }
}

#[tokio::test]
async fn a_change_is_live_stored_and_a_marker_file_still_forces_the_text() {
    let rig = Rig::start().await;
    let v = rig.view().await;
    assert!(!v.get(PrivacyKey::MessageText).unwrap().on, "default off");
    assert!(v.get(PrivacyKey::CallerNumber).unwrap().on);

    rig.set(PrivacyKey::MessageText, true).await;
    rig.set(PrivacyKey::CallerNumber, false).await;
    let v = rig.view().await;
    assert!(v.get(PrivacyKey::MessageText).unwrap().on);
    assert!(!v.get(PrivacyKey::CallerNumber).unwrap().on);
    assert!(!v.get(PrivacyKey::PhoneNotificationText).unwrap().on);
    let stored = std::fs::read_to_string(rig.state.join("privacy")).unwrap();
    assert!(stored.contains("message_text=on") && stored.contains("caller_number=off"));

    // Off again, then the old marker file: it forces the value and the switch cannot undo it.
    rig.set(PrivacyKey::PhoneNotificationText, false).await;
    std::fs::write(rig.state.join("sms-notify-content"), b"").unwrap();
    rig.set(PrivacyKey::PhoneNotificationText, false).await;
    let i = *rig
        .view()
        .await
        .get(PrivacyKey::PhoneNotificationText)
        .unwrap();
    assert!(i.on && i.forced == Some(ForcedBy::File));
    std::fs::remove_file(rig.state.join("sms-notify-content")).unwrap();
    assert!(
        !rig.view()
            .await
            .get(PrivacyKey::PhoneNotificationText)
            .unwrap()
            .on
    );
}
