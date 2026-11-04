// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS Bluetooth hands-free calling for the call hub, through the `geekbridge-macos-calls`
//! helper process (JSON lines, protocol v1; see `packaging/macos/README.md`).
//!
//! IOBluetooth objects must live on the main thread of a process with a run loop, so the service
//! never links the adapter: it spawns the helper, sends `core_command` requests and folds the
//! helper's `call` and `capabilities` pushes into a `CallState`. The helper never connects to a
//! phone on its own, and neither does this client unless the user chose one explicitly (the
//! Bluetooth address passed to [`MacHelperHost::spawn`]); without a choice it reports that plainly
//! and offers no controls. The protocol client compiles and is tested on every host with a stand-in
//! helper; the real helper has never run against a Mac and phone.
use crate::call_control::HostCalls;
use bridge_core::calls::{CallCapabilities, CallCommand, CallEvent, CallState};
use bridge_core::calls_view::SourceSnapshot;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

pub const SOURCE_KEY: &str = "hfp:macos";
const MAX_LINE: usize = 64 * 1024;
const RESPONSE_WAIT: Duration = Duration::from_secs(5);

pub struct MacHelperHost {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Option<String>>,
    next_id: u64,
    state: CallState,
    caps: Option<CallCapabilities>,
    problem: Option<String>,
    /// No phone was chosen: nothing is offered whatever the helper reports.
    unselected: bool,
    exited: bool,
}

impl MacHelperHost {
    /// Start the helper and, when the user picked a phone, ask it to connect to exactly that one.
    pub fn spawn(helper: &Path, phone_address: Option<&str>) -> Result<Self, String> {
        let mut child = Command::new(helper)
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "The macOS calling helper could not be started.".to_owned())?;
        let stdin = child.stdin.take().ok_or("The macOS helper has no input.")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("The macOS helper has no output.")?;
        let (tx, lines) = channel::<Option<String>>();
        std::thread::Builder::new()
            .name("geekbridge-mac-helper-read".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut buf = String::new();
                    match reader.by_ref().take(MAX_LINE as u64).read_line(&mut buf) {
                        Ok(0) | Err(_) => {
                            let _ = tx.send(None);
                            return;
                        }
                        Ok(_) if !buf.ends_with('\n') && buf.len() >= MAX_LINE => {
                            // Over the cap: skip the rest of the line, keep the helper's pipe moving.
                            let mut sink = Vec::new();
                            let _ = reader.read_until(b'\n', &mut sink);
                        }
                        Ok(_) => {
                            if tx.send(Some(buf)).is_err() {
                                return;
                            }
                        }
                    }
                }
            })
            .map_err(|_| "The macOS helper reader could not start.".to_owned())?;
        let mut host = Self {
            child,
            stdin,
            lines,
            next_id: 1,
            state: CallState::default(),
            caps: None,
            problem: None,
            unselected: phone_address.is_none(),
            exited: false,
        };
        if let Some(address) = phone_address {
            host.request(json!({"op": "connect", "address": address}))?;
        }
        Ok(host)
    }

    fn send(&mut self, op: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({"v": 1, "id": id});
        if let (Some(l), Some(o)) = (line.as_object_mut(), op.as_object()) {
            l.extend(o.clone());
        }
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|_| "The macOS helper stopped.".to_owned())?;
        Ok(id)
    }

    /// Send a request and wait for its response, folding every push that arrives meanwhile.
    fn request(&mut self, op: Value) -> Result<Value, String> {
        let id = self.send(op)?;
        let end = Instant::now() + RESPONSE_WAIT;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Some(line)) => {
                    if let Some(answer) = self.fold(&line, Some(id)) {
                        return answer;
                    }
                }
                Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                    self.exited = true;
                    return Err("The macOS helper stopped.".into());
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err("The macOS helper did not answer in time.".into());
                }
            }
        }
    }

    /// Apply one helper line. `Some(answer)` when it is the response to `waiting_for`.
    fn fold(&mut self, line: &str, waiting_for: Option<u64>) -> Option<Result<Value, String>> {
        let v: Value = serde_json::from_str(line.trim()).ok()?;
        if let Some(push) = v.get("push") {
            self.fold_push(push);
            return None;
        }
        let id = v.get("id").and_then(Value::as_u64)?;
        if Some(id) != waiting_for {
            return None;
        }
        Some(if v.get("ok") == Some(&Value::Bool(true)) {
            Ok(v.get("result").cloned().unwrap_or(Value::Null))
        } else {
            // The error kind only: error payloads never carry numbers, but stay conservative.
            let kind = v
                .pointer("/error/error")
                .and_then(Value::as_str)
                .unwrap_or("refused");
            Err(describe_error(kind))
        })
    }

    fn fold_push(&mut self, push: &Value) {
        match push.get("kind").and_then(Value::as_str) {
            Some("call") => {
                if let Some(event) = push
                    .get("event")
                    .and_then(|e| serde_json::from_value::<CallEvent>(e.clone()).ok())
                {
                    self.state.apply(event);
                    self.problem = None;
                }
            }
            Some("capabilities") => {
                if let Some(core) = push
                    .pointer("/capabilities/core")
                    .and_then(|c| serde_json::from_value::<CallCapabilities>(c.clone()).ok())
                {
                    self.caps = Some(core);
                }
            }
            Some("link")
                if push.pointer("/notice/state").and_then(Value::as_str) == Some("down") =>
            {
                self.problem = Some("The phone's Bluetooth link is down.".to_owned());
            }
            _ => {}
        }
    }
}

fn describe_error(kind: &str) -> String {
    match kind {
        "permission_denied" => "macOS has not allowed GeekBridge to use Bluetooth.".to_owned(),
        "not_connected" | "no_link" => "The phone is not connected over Bluetooth.".to_owned(),
        "ambiguous_calls" | "identity_uncertain" => {
            "More than one call is active; use the phone for this one.".to_owned()
        }
        "no_platform_mapping" | "feature_not_advertised" => {
            "The phone does not support that over Bluetooth.".to_owned()
        }
        _ => "The Mac could not do that.".to_owned(),
    }
}

impl HostCalls for MacHelperHost {
    fn label(&self) -> String {
        "macOS Bluetooth calling".into()
    }

    fn poll(&mut self, wait: Duration) -> Result<Vec<SourceSnapshot>, String> {
        let end = Instant::now() + wait;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Some(line)) => {
                    self.fold(&line, None);
                }
                Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                    self.exited = true;
                    return Err("The macOS calling helper stopped.".into());
                }
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        if self.unselected {
            return Err(
                "Choose which linked phone to use for Mac calling; none is selected yet."
                    .to_owned(),
            );
        }
        if let Some(p) = &self.problem {
            return Err(p.clone());
        }
        let Some(caps) = self.caps.clone() else {
            return Ok(Vec::new());
        };
        Ok(vec![SourceSnapshot {
            key: SOURCE_KEY.into(),
            label: "Phone".into(),
            caps,
            state: self.state.clone(),
        }])
    }

    fn execute(&mut self, _source: &str, command: &CallCommand) -> Result<(), String> {
        let command = serde_json::to_value(command).map_err(|_| "Bad command.".to_owned())?;
        self.request(json!({"op": "core_command", "command": command}))
            .map(|_| ())
    }
}

impl Drop for MacHelperHost {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.send(json!({"op": "shutdown"}));
        }
        // The helper exits after `shutdown`; kill is the bounded fallback.
        std::thread::sleep(Duration::from_millis(50));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use bridge_core::calls::{
        AudioCodec, CallDirection, CallId, CallParty, CallPhase, HfpEvidence, HfpHost, LinkState,
        Sensitive,
    };
    use bridge_core::calls_view;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in helper: prints the scripted pushes, answers every request with ok and appends
    /// each request line to `log`.
    fn fake_helper(dir: &Path, pushes: &[Value], log: &Path) -> std::path::PathBuf {
        let script = dir.join("helper.sh");
        let mut body = String::from("#!/bin/sh\n");
        for p in pushes {
            body.push_str(&format!("echo '{}'\n", json!({"v": 1, "push": p})));
        }
        body.push_str(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> '{}'\n  id=$(printf '%s' \"$line\" | sed 's/.*\"id\":\\([0-9]*\\).*/\\1/')\n  echo \"{{\\\"v\\\":1,\\\"id\\\":$id,\\\"ok\\\":true,\\\"result\\\":{{\\\"ticket\\\":1}}}}\"\ndone\n",
            log.display()
        ));
        std::fs::write(&script, body).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Executing a script another test just wrote can fail with "text file busy" while a
    /// concurrently forked child still holds its write descriptor; a short retry is enough.
    fn spawn(helper: &Path, phone: Option<&str>) -> MacHelperHost {
        for _ in 0..20 {
            if let Ok(h) = MacHelperHost::spawn(helper, phone) {
                return h;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the stand-in helper could not be started");
    }

    fn pushes() -> Vec<Value> {
        let caps = CallCapabilities::from_hfp_evidence(
            HfpHost::MacOs,
            &HfpEvidence {
                service_level_connection: true,
                call_indicators: true,
                caller_id_notification: true,
                host_call_control_api: true,
                audio_codec: Some(AudioCodec::Msbc),
                ..Default::default()
            },
        );
        vec![
            json!({"kind": "capabilities", "capabilities": {"core": caps}}),
            json!({"kind": "call", "event": CallEvent::Link { state: LinkState::Up }}),
            json!({"kind": "call", "event": CallEvent::CallUpdated {
                id: CallId::new("mac-g1-c1"),
                direction: CallDirection::Incoming,
                phase: CallPhase::Incoming,
                party: CallParty { number: Some(Sensitive::new("+15550100")), name: None },
            }}),
        ]
    }

    fn poll_until_call(h: &mut MacHelperHost) -> Vec<SourceSnapshot> {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(s) = h.poll(Duration::from_millis(50))
                && s.first().is_some_and(|s| !s.state.calls.is_empty())
            {
                return s;
            }
            assert!(Instant::now() < end, "no call arrived");
        }
    }

    #[test]
    fn calls_and_capabilities_from_the_helper_become_controls_and_commands_go_back() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("requests.log");
        let helper = fake_helper(dir.path(), &pushes(), &log);
        let mut h = spawn(&helper, Some("aa-bb-cc-dd-ee-ff"));
        let view = calls_view::build(&poll_until_call(&mut h));
        let row = view.ringing().unwrap();
        assert!(row.answer.is_some() && row.reject.is_some());
        h.execute(
            SOURCE_KEY,
            &CallCommand::Reject {
                call: row.reject.as_ref().unwrap().call.clone(),
            },
        )
        .unwrap();
        let logged = std::fs::read_to_string(&log).unwrap();
        // First the explicit connect to the chosen phone, then the core command, as the helper expects.
        let lines: Vec<Value> = logged
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[0]["op"], "connect");
        assert_eq!(lines[0]["address"], "aa-bb-cc-dd-ee-ff");
        assert_eq!(lines[1]["op"], "core_command");
        assert_eq!(lines[1]["command"]["type"], "reject");
        assert_eq!(lines[1]["command"]["call"], "mac-g1-c1");
        assert_eq!(lines[1]["v"], 1);
    }

    #[test]
    fn without_a_chosen_phone_nothing_connects_and_the_reason_is_shown() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("requests.log");
        let helper = fake_helper(dir.path(), &pushes(), &log);
        let mut h = spawn(&helper, None);
        let err = h.poll(Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("Choose which linked phone"));
        assert!(!log.exists(), "the helper must not be asked to connect");
    }

    #[test]
    fn a_dead_helper_is_reported_not_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("dead.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 3\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = MacHelperHost::spawn(&script, Some("x"));
        let mut h = match r {
            Ok(h) => h,
            Err(e) => {
                assert!(e.contains("stopped") || e.contains("answer"));
                return;
            }
        };
        assert!(h.poll(Duration::from_millis(200)).is_err());
    }
}
