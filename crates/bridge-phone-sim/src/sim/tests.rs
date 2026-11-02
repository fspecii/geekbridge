// SPDX-License-Identifier: GPL-3.0-or-later
//! Unit tests for reconciliation, and service tests over a private `dbus-daemon`. The bus tests
//! check the simulated service from a second connection with plain zbus calls; nothing touches
//! the user's session or system bus.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::*;
use crate::ag::{AG, MANAGER};

const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
use crate::modem::{FakeModem, RemoteParty};

// ------------------------------------------------------------------------------- plan (pure)

fn cm(id: u32, state: &'static str, line: &str) -> CallModel {
    CallModel {
        id,
        state,
        line: line.into(),
    }
}
fn ring(n: &str) -> ModemState {
    ModemState::Ringing { number: n.into() }
}
fn act(n: &str) -> ModemState {
    ModemState::Active { number: n.into() }
}

#[test]
fn plan_incoming_then_active_then_idle() {
    let add = plan(None, ("idle", 0), 1, &ring("555"));
    assert_eq!(
        add,
        [Change::AddCall {
            id: 1,
            state: "incoming",
            line: "555".into()
        }]
    );
    let c = cm(1, "incoming", "555");
    assert_eq!(plan(Some(&c), ("idle", 0), 2, &ring("555")), []);
    assert_eq!(
        plan(Some(&c), ("idle", 0), 2, &act("555")),
        [
            Change::SetCallState {
                id: 1,
                state: "active"
            },
            Change::Transport {
                state: "active",
                codec: 1
            }
        ]
    );
    let c = cm(1, "active", "555");
    assert_eq!(
        plan(Some(&c), ("active", 1), 2, &ModemState::Idle),
        [
            Change::RemoveCall { id: 1 },
            Change::Transport {
                state: "idle",
                codec: 0
            }
        ]
    );
}

#[test]
fn plan_dialing_and_unknown_number_learned_later() {
    let d = ModemState::Dialing {
        number: "555".into(),
    };
    let c = cm(3, "dialing", "555");
    assert_eq!(plan(Some(&c), ("idle", 0), 4, &d), []);
    let c = cm(3, "dialing", "");
    assert_eq!(
        plan(Some(&c), ("idle", 0), 4, &act("555")),
        [
            Change::SetCallState {
                id: 3,
                state: "active"
            },
            Change::SetCallLine {
                id: 3,
                line: "555".into()
            },
            Change::Transport {
                state: "active",
                codec: 1
            }
        ]
    );
}

#[test]
fn plan_never_morphs_one_call_into_another_and_never_reuses_ids() {
    // Active call, then the modem reports a different ringing call without an Idle in between.
    let c = cm(5, "active", "111");
    let p = plan(Some(&c), ("active", 1), 6, &ring("222"));
    assert_eq!(
        p,
        [
            Change::RemoveCall { id: 5 },
            Change::AddCall {
                id: 6,
                state: "incoming",
                line: "222".into()
            },
            Change::Transport {
                state: "idle",
                codec: 0
            }
        ]
    );
    // Different number while active-> active is also a different call.
    let p = plan(Some(&c), ("active", 1), 6, &act("333"));
    assert_eq!(p[0], Change::RemoveCall { id: 5 });
    assert!(matches!(p[1], Change::AddCall { id: 6, .. }));
    // Starting mid-call (simulator restarted while a call is up).
    assert_eq!(
        plan(None, ("idle", 0), 1, &act("111")),
        [
            Change::AddCall {
                id: 1,
                state: "active",
                line: "111".into()
            },
            Change::Transport {
                state: "active",
                codec: 1
            }
        ]
    );
}

// ------------------------------------------------------------------------------ private bus

struct PrivateBus {
    child: Child,
    address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bus");
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                socket.display()
            ),
        )
        .unwrap();
        let spawned = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .arg("--nofork")
            .arg("--print-address=1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                if std::env::var("GEEKBRIDGE_REQUIRE_DBUS").as_deref() == Ok("1") {
                    panic!("dbus-daemon required but unavailable: {e}");
                }
                eprintln!(
                    "SKIPPED: dbus-daemon not available ({e}); set GEEKBRIDGE_REQUIRE_DBUS=1 to fail instead"
                );
                return None;
            }
        };
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_string();
        assert!(address.starts_with("unix:"), "bad bus address {address:?}");
        Some(Self {
            child,
            address,
            _dir: dir,
        })
    }

    fn connect(&self) -> Connection {
        zbus::blocking::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn cfg(&self) -> SimConfig {
        SimConfig {
            bus_address: Some(self.address.clone()),
            ..SimConfig::default()
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type Managed = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

struct Fixture {
    _bus: PrivateBus,
    client: Connection,
    sim: Sim,
    fake: FakeModem,
}

impl Fixture {
    fn new() -> Option<Self> {
        let bus = PrivateBus::start()?;
        let fake = FakeModem::new();
        let sim = Sim::start(bus.cfg(), Arc::new(fake.clone())).unwrap();
        let client = bus.connect();
        Some(Self {
            _bus: bus,
            client,
            sim,
            fake,
        })
    }

    fn call(&self, path: &str, iface: &str, member: &str, arg: Option<&str>) -> Result<(), String> {
        let r = match arg {
            Some(a) => self
                .client
                .call_method(Some(SERVICE), path, Some(iface), member, &(a,)),
            None => self
                .client
                .call_method(Some(SERVICE), path, Some(iface), member, &()),
        };
        r.map(|_| ()).map_err(|e| match e {
            zbus::Error::MethodError(name, _, _) => name.to_string(),
            other => format!("transport: {other}"),
        })
    }

    fn prop(&self, path: &str, iface: &str, name: &str) -> OwnedValue {
        self.client
            .call_method(Some(SERVICE), path, Some(PROPERTIES), "Get", &(iface, name))
            .unwrap()
            .body()
            .deserialize::<OwnedValue>()
            .unwrap()
    }

    fn sprop(&self, path: &str, iface: &str, name: &str) -> String {
        String::try_from(self.prop(path, iface, name)).unwrap()
    }

    fn managed(&self, path: &str) -> Managed {
        self.client
            .call_method(
                Some(SERVICE),
                path,
                Some(OBJECT_MANAGER),
                "GetManagedObjects",
                &(),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .managed(AG_PATH)
            .keys()
            .map(|p| p.to_string())
            .collect();
        v.sort();
        v
    }

    fn watch(&self) -> Receiver<(String, String, String)> {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path_namespace(MANAGER)
            .unwrap()
            .build();
        let iter = MessageIterator::for_match_rule(rule, &self.client, None).unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for msg in iter.flatten() {
                let h = msg.header();
                let path = h.path().map(|p| p.to_string()).unwrap_or_default();
                let member = h.member().map(|m| m.to_string()).unwrap_or_default();
                let detail = if member == "PropertiesChanged" {
                    let (iface, props, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
                        msg.body().deserialize().unwrap();
                    let mut keys: Vec<String> =
                        props.iter().map(|(k, v)| format!("{k}={v:?}")).collect();
                    keys.sort();
                    format!("{iface} {}", keys.join(" "))
                } else {
                    String::new()
                };
                if tx.send((path, member, detail)).is_err() {
                    break;
                }
            }
        });
        rx
    }
}

fn next(rx: &Receiver<(String, String, String)>) -> (String, String, String) {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("expected a signal")
}

const CALL1: &str = "/org/pipewire/Telephony/ag1/call1";
const CALL2: &str = "/org/pipewire/Telephony/ag1/call2";

macro_rules! fixture {
    () => {
        match Fixture::new() {
            Some(f) => f,
            None => return,
        }
    };
}

// ----------------------------------------------------------------------------- service tests

#[test]
fn initial_tree_has_one_phone_with_idle_transport_and_no_calls() {
    let f = fixture!();
    assert!(f.sim.unique_name().starts_with(':'));
    let root = f.managed(MANAGER);
    assert_eq!(root.len(), 1);
    let ag = &root[&OwnedObjectPath::try_from(AG_PATH).unwrap()];
    assert_eq!(
        String::try_from(ag[AG]["Address"].try_clone().unwrap()).unwrap(),
        "02:00:00:00:00:01"
    );
    assert_eq!(f.sprop(AG_PATH, TRANSPORT, "State"), "idle");
    assert_eq!(
        u8::try_from(f.prop(AG_PATH, TRANSPORT, "Codec")).unwrap(),
        0
    );
    assert!(f.calls().is_empty());
    // Ticking an idle modem changes nothing.
    f.sim.tick().unwrap();
    assert!(f.calls().is_empty());
}

#[test]
fn incoming_call_answer_and_hangup_full_cycle_with_signals() {
    let f = fixture!();
    let rx = f.watch();

    f.fake.ring("5550100").unwrap();
    f.sim.tick().unwrap();
    assert_eq!(
        next(&rx),
        (AG_PATH.into(), "InterfacesAdded".into(), "".into())
    );
    assert_eq!(f.calls(), [CALL1]);
    assert_eq!(f.sprop(CALL1, CALL, "State"), "incoming");
    assert_eq!(f.sprop(CALL1, CALL, "LineIdentification"), "5550100");

    // Reject-vs-answer rules: Hangup is valid on incoming, Answer is too; Activate is not.
    assert_eq!(
        f.call(AG_PATH, TRANSPORT, "Activate", None),
        Err("org.pipewire.Telephony.Error.InvalidState".into())
    );
    f.call(CALL1, CALL, "Answer", None).unwrap();
    assert!(matches!(f.fake.state().unwrap(), ModemState::Active { .. }));
    // Not yet reconciled: the service reports what it last saw.
    assert_eq!(f.sprop(CALL1, CALL, "State"), "incoming");
    f.sim.tick().unwrap();
    let (path, member, detail) = next(&rx);
    assert_eq!(
        (path.as_str(), member.as_str()),
        (CALL1, "PropertiesChanged")
    );
    assert!(
        detail.starts_with(CALL) && detail.contains("State="),
        "{detail}"
    );
    let (path, member, detail) = next(&rx);
    assert_eq!(
        (path.as_str(), member.as_str()),
        (AG_PATH, "PropertiesChanged")
    );
    assert!(detail.starts_with(TRANSPORT), "{detail}");
    assert_eq!(f.sprop(CALL1, CALL, "State"), "active");
    assert_eq!(f.sprop(AG_PATH, TRANSPORT, "State"), "active");
    assert_eq!(
        u8::try_from(f.prop(AG_PATH, TRANSPORT, "Codec")).unwrap(),
        1
    );

    // Answer is only valid on `incoming`.
    assert_eq!(
        f.call(CALL1, CALL, "Answer", None),
        Err("org.pipewire.Telephony.Error.InvalidState".into())
    );
    // Activate succeeds while a call is active (the simulated transport already follows it).
    f.call(AG_PATH, TRANSPORT, "Activate", None).unwrap();

    f.call(CALL1, CALL, "Hangup", None).unwrap();
    assert_eq!(f.fake.state().unwrap(), ModemState::Idle);
    f.sim.tick().unwrap();
    assert_eq!(next(&rx).1, "PropertiesChanged"); // call -> disconnected
    assert_eq!(
        next(&rx),
        (AG_PATH.into(), "InterfacesRemoved".into(), "".into())
    );
    assert_eq!(next(&rx).1, "PropertiesChanged"); // transport -> idle
    assert!(f.calls().is_empty());
    assert_eq!(f.sprop(AG_PATH, TRANSPORT, "State"), "idle");
    assert_eq!(
        u8::try_from(f.prop(AG_PATH, TRANSPORT, "Codec")).unwrap(),
        0
    );

    // Ids are never reused.
    f.fake.ring("5550101").unwrap();
    f.sim.tick().unwrap();
    assert_eq!(f.calls(), [CALL2]);
}

#[test]
fn reject_is_hangup_on_a_ringing_call_and_remote_cancel_removes_it() {
    let f = fixture!();
    f.fake.ring("5550100").unwrap();
    f.sim.tick().unwrap();
    f.call(CALL1, CALL, "Hangup", None).unwrap();
    f.sim.tick().unwrap();
    assert!(f.calls().is_empty());

    f.fake.ring("5550100").unwrap();
    f.sim.tick().unwrap();
    assert_eq!(f.calls(), [CALL2]);
    f.fake.remote_hangup("5550100").unwrap();
    f.sim.tick().unwrap();
    assert!(f.calls().is_empty());
}

#[test]
fn dial_validates_and_reaches_the_modem() {
    let f = fixture!();
    for bad in ["", "abc", "12;3", "+1 555", &"1".repeat(33)] {
        assert_eq!(
            f.call(AG_PATH, AG, "Dial", Some(bad)),
            Err("org.pipewire.Telephony.Error.InvalidFormat".into()),
            "{bad:?}"
        );
    }
    assert_eq!(f.fake.actions(), Vec::<&str>::new());

    f.call(AG_PATH, AG, "Dial", Some("+15550100")).unwrap();
    assert!(matches!(
        f.fake.state().unwrap(),
        ModemState::Dialing { .. }
    ));
    f.sim.tick().unwrap();
    assert_eq!(f.sprop(CALL1, CALL, "State"), "dialing");
    assert_eq!(f.sprop(CALL1, CALL, "LineIdentification"), "+15550100");
    assert_eq!(f.sprop(AG_PATH, TRANSPORT, "State"), "idle");

    // A second dial while one is in progress.
    assert_eq!(
        f.call(AG_PATH, AG, "Dial", Some("5550101")),
        Err("org.pipewire.Telephony.Error.InProgress".into())
    );
    // Answer is not valid on a dialing call; Hangup (cancel) is.
    assert_eq!(
        f.call(CALL1, CALL, "Answer", None),
        Err("org.pipewire.Telephony.Error.InvalidState".into())
    );

    f.fake.remote_accept("+15550100").unwrap();
    f.sim.tick().unwrap();
    assert_eq!(f.sprop(CALL1, CALL, "State"), "active");
    assert_eq!(f.sprop(AG_PATH, TRANSPORT, "State"), "active");
    // Dial during an active call is refused.
    assert_eq!(
        f.call(AG_PATH, AG, "Dial", Some("5550101")),
        Err("org.pipewire.Telephony.Error.InvalidState".into())
    );
    f.call(AG_PATH, AG, "HangupAll", None).unwrap();
    f.sim.tick().unwrap();
    assert!(f.calls().is_empty());
    // HangupAll with nothing to hang up.
    assert_eq!(
        f.call(AG_PATH, AG, "HangupAll", None),
        Err("org.pipewire.Telephony.Error.InvalidState".into())
    );
}

#[test]
fn cancelling_a_dialing_call_with_hangup() {
    let f = fixture!();
    f.call(AG_PATH, AG, "Dial", Some("5550100")).unwrap();
    f.sim.tick().unwrap();
    f.call(CALL1, CALL, "Hangup", None).unwrap();
    f.sim.tick().unwrap();
    assert!(f.calls().is_empty());
}

#[test]
fn multi_call_features_are_honestly_not_supported() {
    let f = fixture!();
    for m in [
        "SwapCalls",
        "ReleaseAndAnswer",
        "ReleaseAndSwap",
        "HoldAndAnswer",
        "CreateMultiparty",
    ] {
        assert_eq!(
            f.call(AG_PATH, AG, m, None),
            Err("org.pipewire.Telephony.Error.NotSupported".into()),
            "{m}"
        );
    }
    assert_eq!(
        f.call(AG_PATH, AG, "SendTones", Some("123")),
        Err("org.pipewire.Telephony.Error.NotSupported".into())
    );
    assert!(f.fake.actions().is_empty());
}

#[test]
fn modem_errors_map_to_failed() {
    struct Broken;
    impl Modem for Broken {
        fn state(&self) -> Result<ModemState, ModemError> {
            Ok(ModemState::Ringing {
                number: "5550100".into(),
            })
        }
        fn answer(&self) -> Result<(), ModemError> {
            Err(ModemError::Adb("device offline (call from 5550100)".into()))
        }
        fn hangup(&self) -> Result<(), ModemError> {
            Ok(())
        }
        fn dial(&self, _: &str) -> Result<(), ModemError> {
            Ok(())
        }
    }
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let sim = Sim::start(bus.cfg(), Arc::new(Broken)).unwrap();
    sim.tick().unwrap();
    let client = bus.connect();
    let err = client
        .call_method(Some(SERVICE), CALL1, Some(CALL), "Answer", &())
        .unwrap_err();
    match err {
        zbus::Error::MethodError(name, Some(msg), _) => {
            assert_eq!(name.as_str(), "org.pipewire.Telephony.Error.Failed");
            assert!(!msg.contains("5550100"), "number leaked into error: {msg}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn modem_read_failure_is_reported_by_tick_and_changes_nothing() {
    struct Offline;
    impl Modem for Offline {
        fn state(&self) -> Result<ModemState, ModemError> {
            Err(ModemError::Adb("no devices".into()))
        }
        fn answer(&self) -> Result<(), ModemError> {
            Ok(())
        }
        fn hangup(&self) -> Result<(), ModemError> {
            Ok(())
        }
        fn dial(&self, _: &str) -> Result<(), ModemError> {
            Ok(())
        }
    }
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let sim = Sim::start(bus.cfg(), Arc::new(Offline)).unwrap();
    assert!(matches!(sim.tick(), Err(SimError::Modem(_))));
}

#[test]
fn refuses_to_start_when_the_name_is_owned() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let _first = Sim::start(bus.cfg(), Arc::new(FakeModem::new())).unwrap();
    // A different service already holds the name: the second must fail, not queue.
    match Sim::start(bus.cfg(), Arc::new(FakeModem::new())) {
        Err(SimError::NameTaken) => {}
        Err(e) => panic!("wrong error: {e}"),
        Ok(_) => panic!("second simulator must not start"),
    }
}

#[test]
fn run_until_reconciles_in_the_background_and_stops() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let fake = FakeModem::new();
    let cfg = SimConfig {
        poll_interval: Duration::from_millis(20),
        ..bus.cfg()
    };
    let sim = Arc::new(Sim::start(cfg, Arc::new(fake.clone())).unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let t = {
        let (sim, stop) = (sim.clone(), stop.clone());
        std::thread::spawn(move || sim.run_until(&stop))
    };
    let client = bus.connect();
    fake.ring("5550100").unwrap();
    let mut seen = false;
    for _ in 0..200 {
        let m: Managed = client
            .call_method(
                Some(SERVICE),
                AG_PATH,
                Some(OBJECT_MANAGER),
                "GetManagedObjects",
                &(),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        if !m.is_empty() {
            seen = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    stop.store(true, Ordering::Relaxed);
    t.join().unwrap();
    assert!(seen, "polling loop never published the call");
}
