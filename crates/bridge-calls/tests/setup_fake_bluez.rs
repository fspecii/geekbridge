// SPDX-License-Identifier: GPL-3.0-or-later
//! "Set up call audio" (`bridge_calls::setup`) against the fake BlueZ
//! (scripts/e2e/call-audio/fake_bluez.py) on a private bus: no PipeWire, no hardware, no real
//! Bluetooth stack. Each test starts its own `dbus-daemon` and fake; without `dbus-daemon` or
//! python-dbus/python-gobject the tests say so and pass.
//!
//! What they pin down are the real-bluetoothd behaviours a one-pass design can get wrong
//! (docs/reports/call-audio-backend-review.md): a phone whose Bluetooth name is not the name
//! GeekBridge knows it by, `Pair()` using the caller's own agent, the phone cancelling while the
//! code is on screen, the user cancelling during `Pair()`, the phone's LE twin, trusting the phone
//! before the agent goes, and putting the adapter's modes back.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use bridge_calls::setup::{self, Bus, Event, Options};

struct Fake {
    kids: Vec<Child>,
    addr: String,
    _dir: tempfile::TempDir,
}

impl Drop for Fake {
    fn drop(&mut self) {
        for c in &mut self.kids {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn usable() -> bool {
    let bus = Command::new("dbus-daemon")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let py = Command::new("python3")
        .args(["-I", "-c", "import dbus, gi"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !(bus && py) {
        eprintln!("skipped: needs dbus-daemon and python3 with python-dbus and python-gobject");
    }
    bus && py
}

impl Fake {
    fn start(env: &[(&str, &str)]) -> Option<Self> {
        if !usable() {
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>system</type>
  <listen>unix:path={}/bus</listen>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                dir.path().display()
            ),
        )
        .unwrap();
        let mut bus = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .args(["--nofork", "--print-address=1", "--nopidfile"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon");
        let mut line = String::new();
        BufReader::new(bus.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let addr = line.trim().to_owned();
        let log = std::fs::File::create(dir.path().join("fake_bluez.log")).unwrap();
        let fake = Command::new("python3")
            .arg("-I")
            .arg(root().join("scripts/e2e/call-audio/fake_bluez.py"))
            .env("DBUS_SYSTEM_BUS_ADDRESS", &addr)
            .envs(env.iter().copied())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("fake_bluez.py");
        let f = Fake {
            kids: vec![bus, fake],
            addr,
            _dir: dir,
        };
        let c = f.conn();
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let owned = zbus::blocking::fdo::DBusProxy::new(&c)
                .and_then(|p| {
                    p.name_has_owner("org.bluez".try_into().unwrap())
                        .map_err(Into::into)
                })
                .unwrap_or(false);
            if owned {
                break;
            }
            assert!(Instant::now() < end, "the fake BlueZ did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
        Some(f)
    }

    fn conn(&self) -> zbus::blocking::Connection {
        zbus::blocking::connection::Builder::address(self.addr.as_str())
            .unwrap()
            .build()
            .unwrap()
    }

    fn calls(&self) -> Vec<String> {
        self.conn()
            .call_method(
                Some("org.bluez"),
                "/geekbridge/FakePhone",
                Some("org.geekbridge.FakePhone"),
                "Calls",
                &(),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap()
    }

    fn adapter<T: TryFrom<zbus::zvariant::OwnedValue>>(&self, prop: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        let v: zbus::zvariant::OwnedValue = self
            .conn()
            .call_method(
                Some("org.bluez"),
                "/org/bluez/hci0",
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &("org.bluez.Adapter1", prop),
            )
            .unwrap()
            .body()
            .deserialize()
            .unwrap();
        T::try_from(v).unwrap()
    }

    fn options(&self, name: Option<&str>, timeout: u64) -> Options {
        Options {
            bus: Bus(Some(self.addr.clone())),
            phone_name: name.map(str::to_owned),
            timeout: Duration::from_secs(timeout),
            ..Options::default()
        }
    }
}

fn pos(calls: &[String], what: &str) -> usize {
    calls
        .iter()
        .position(|c| c.starts_with(what))
        .unwrap_or_else(|| panic!("no {what:?} in {calls:?}"))
}

/// Everything the setup reported, as text (the confirmation itself is answered by `answer`).
type Seen = Arc<Mutex<Vec<String>>>;

fn run_setup(
    opts: &Options,
    answer: Option<bool>,
    cancel: &AtomicBool,
) -> (Result<setup::Remembered, String>, Vec<String>) {
    let seen: Seen = Arc::default();
    let s = seen.clone();
    // Unanswered confirmations stay open (the user looks at the code and does nothing).
    let mut parked: Vec<mpsc::Sender<bool>> = Vec::new();
    let r = setup::run(
        opts,
        |e| match e {
            Event::Step(l) => s.lock().unwrap().push(format!("step: {l}")),
            Event::Confirm {
                device,
                passkey,
                reply,
            } => {
                s.lock()
                    .unwrap()
                    .push(format!("confirm: {device} {passkey}"));
                match answer {
                    Some(a) => {
                        let _ = reply.send(a);
                    }
                    None => parked.push(reply),
                }
            }
            #[allow(unreachable_patterns)]
            other => s.lock().unwrap().push(format!("event: {other:?}")),
        },
        cancel,
    );
    drop(parked);
    let seen = seen.lock().unwrap().clone();
    (r, seen)
}

#[test]
fn a_phone_whose_bluetooth_name_differs_is_set_up_when_it_pairs_from_its_own_side() {
    // GeekBridge knows a Galaxy Z Fold5 as "SM-F946B" (Build.MODEL); its Bluetooth name is
    // "Galaxy Z Fold5". The user taps the computer in the phone's list and compares the code.
    let Some(f) = Fake::start(&[
        ("FAKE_SETUP_NAME", "Galaxy Z Fold5"),
        ("FAKE_PAIR_FROM_PHONE", "1"),
        ("FAKE_PAIRED_CONNECTED", "0"),
    ]) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let (r, seen) = run_setup(&f.options(Some("SM-F946B"), 12), Some(true), &cancel);
    let r = r.unwrap_or_else(|e| panic!("setup failed: {e}; {seen:#?}"));
    assert_eq!(r.address, "AA:BB:CC:00:33:44");
    assert_eq!(r.name, "Galaxy Z Fold5");
    assert!(
        seen.iter()
            .any(|l| l == "confirm: Galaxy Z Fold5 (AA:BB:CC:00:33:44) 482910"),
        "{seen:#?}"
    );
    // The already paired, absent sandbox phone was not taken for it.
    let calls = f.calls();
    assert!(
        calls.iter().any(|c| c == "IncomingPairing done"),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("ConnectProfile dev_AA_BB_CC_00_33_44 0000111f")),
        "{calls:?}"
    );
}

#[test]
fn pairing_from_here_uses_the_setups_own_agent_skips_the_le_twin_and_trusts_before_letting_go() {
    let Some(f) = Fake::start(&[
        ("FAKE_PAIRED_CONNECTED", "0"),
        ("FAKE_ADAPTER_PAIRABLE", "0"),
    ]) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let (r, seen) = run_setup(&f.options(Some("setup phone"), 12), Some(true), &cancel);
    let r = r.unwrap_or_else(|e| panic!("setup failed: {e}; {seen:#?}"));
    assert_eq!(r.address, "AA:BB:CC:00:33:44");
    let calls = f.calls();
    // Only Classic devices are looked for; the LE twin (random address) never shows up.
    assert!(pos(&calls, "SetDiscoveryFilter Transport=bredr") < pos(&calls, "StartDiscovery"));
    assert!(!calls.iter().any(|c| c.contains("dev_5A_11")), "{calls:?}");
    // BlueZ uses the caller's own agent for Pair(), whoever is the default.
    assert!(
        calls
            .iter()
            .any(|c| c == "Pair dev_AA_BB_CC_00_33_44 agent=own"),
        "{calls:?}"
    );
    // Trusted while the agent can still authorize the phone's first connections.
    assert!(
        pos(&calls, "Set Device1.Trusted=1") < pos(&calls, "UnregisterAgent"),
        "{calls:?}"
    );
    // The adapter's modes are put back as they were.
    assert!(!f.adapter::<bool>("Discoverable"));
    assert!(
        !f.adapter::<bool>("Pairable"),
        "Pairable was off before the setup"
    );
    assert_eq!(f.adapter::<u32>("DiscoverableTimeout"), 180);
    assert!(
        seen.iter()
            .any(|l| l.contains("phones list this computer as \"sandbox\"")),
        "{seen:#?}"
    );
}

#[test]
fn the_phone_cancelling_takes_the_code_down_at_once_and_says_why() {
    let Some(f) = Fake::start(&[("FAKE_PHONE_CANCELS", "1"), ("FAKE_PAIRED_CONNECTED", "0")])
    else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let t0 = Instant::now();
    // Nobody answers on the computer; the phone cancels after 1 s.
    let (r, seen) = run_setup(&f.options(Some("setup phone"), 6), None, &cancel);
    assert!(r.is_err());
    assert!(
        t0.elapsed() < Duration::from_secs(20),
        "the setup waited for the agent timeout instead of BlueZ's Cancel: {:?}",
        t0.elapsed()
    );
    let i = seen
        .iter()
        .position(|l| l.starts_with("confirm: Setup Phone"))
        .unwrap_or_else(|| panic!("{seen:#?}"));
    assert!(
        seen[i..].iter().any(|l| l == "event: Withdrawn"),
        "the code was not withdrawn: {seen:#?}"
    );
    assert!(
        seen.iter()
            .any(|l| l
                .contains("Pairing with Setup Phone failed: the code was not confirmed in time")),
        "{seen:#?}"
    );
}

#[test]
fn cancelling_during_pairing_cancels_the_pairing_and_returns_quickly() {
    let Some(f) = Fake::start(&[("FAKE_PAIRED_CONNECTED", "0")]) else {
        return;
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let opts = f.options(Some("setup phone"), 30);
    let t0 = Instant::now();
    let seen: Seen = Arc::default();
    let s = seen.clone();
    let mut parked = Vec::new();
    let r = setup::run(
        &opts,
        |e| {
            if let Event::Confirm { reply, .. } = e {
                // The user presses Cancel while the code is shown.
                parked.push(reply);
                c2.store(true, std::sync::atomic::Ordering::SeqCst);
                s.lock().unwrap().push("confirm".into());
            }
        },
        &cancel,
    );
    assert_eq!(r, Err("setup cancelled".into()));
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "cancel waited for Pair(): {:?}",
        t0.elapsed()
    );
    let calls = f.calls();
    assert!(calls.iter().any(|c| c == "CancelPairing"), "{calls:?}");
    assert!(
        pos(&calls, "CancelPairing") < pos(&calls, "UnregisterAgent"),
        "{calls:?}"
    );
    assert!(!f.adapter::<bool>("Discoverable"));
    assert_eq!(*seen.lock().unwrap(), vec!["confirm".to_owned()]);
}

#[test]
fn an_already_paired_phone_by_another_name_is_taken_when_it_connects_from_its_side() {
    // The Fold is paired already (by hand, or an earlier setup) but GeekBridge knows it as
    // "SM-F946B": the user connects it from the phone's Bluetooth list during the setup.
    let Some(f) = Fake::start(&[("FAKE_PAIRED_CONNECTED", "0"), ("FAKE_PHONE_CONNECTS", "1")])
    else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let (r, seen) = run_setup(&f.options(Some("SM-F946B"), 8), Some(true), &cancel);
    let r = r.unwrap_or_else(|e| panic!("setup failed: {e}; {seen:#?}"));
    assert_eq!(r.address, "AA:BB:CC:00:11:22");
    let calls = f.calls();
    assert!(!calls.iter().any(|c| c.starts_with("Pair")), "{calls:?}");
}

#[test]
fn a_phone_already_connected_under_another_name_is_not_taken_for_the_wanted_one() {
    // Another phone is paired and connected; the wanted "Setup Phone" gets paired, not it.
    let Some(f) = Fake::start(&[]) else {
        return;
    };
    let cancel = AtomicBool::new(false);
    let (r, seen) = run_setup(&f.options(Some("setup phone"), 8), Some(true), &cancel);
    let r = r.unwrap_or_else(|e| panic!("setup failed: {e}; {seen:#?}"));
    assert_eq!(r.address, "AA:BB:CC:00:33:44");
}
