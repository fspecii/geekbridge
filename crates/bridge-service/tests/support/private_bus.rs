//! A private `dbus-daemon` and a fake `org.kde.StatusNotifierWatcher` for the tray tests. Include with
//! `#[path = "support/private_bus.rs"] mod private_bus;`.
#![allow(dead_code)]
#![cfg(target_os = "linux")]

use bridge_service::tray::sni::{WATCHER, WATCHER_PATH};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};
use zbus::Connection;

pub struct PrivateBus {
    child: Child,
    pub address: String,
    _dir: tempfile::TempDir,
}

impl PrivateBus {
    pub fn start() -> Option<Self> {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("bus.conf");
        std::fs::write(
            &conf,
            format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <busconfig><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                dir.path().join("bus").display()
            ),
        )
        .unwrap();
        let spawned = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", conf.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                if std::env::var("GEEKBRIDGE_REQUIRE_DBUS").as_deref() == Ok("1") {
                    panic!("dbus-daemon required but unavailable: {e}");
                }
                eprintln!("SKIPPED: dbus-daemon not available ({e})");
                return None;
            }
        };
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        Some(Self {
            child,
            address: line.trim().to_owned(),
            _dir: dir,
        })
    }
    pub async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Default)]
pub struct FakeWatcher {
    pub registered: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl FakeWatcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.registered.lock().unwrap().push(service.to_owned());
    }
    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn protocol_version(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.registered.lock().unwrap().clone()
    }
}

pub async fn watcher(bus: &PrivateBus) -> (Connection, Arc<Mutex<Vec<String>>>) {
    let fake = FakeWatcher::default();
    let log = fake.registered.clone();
    let conn = zbus::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .serve_at(WATCHER_PATH, fake)
        .unwrap()
        .name(WATCHER)
        .unwrap()
        .build()
        .await
        .unwrap();
    (conn, log)
}
