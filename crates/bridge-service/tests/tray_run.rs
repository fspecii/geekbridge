// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray task end to end: a real service core and a stand-in phone on one side, a private session
//! bus with a fake StatusNotifierWatcher on the other. Pairing a phone must change what the tray shows
//! with no timer involved, and "Quit" must reach the service.
#![cfg(target_os = "linux")]

#[path = "support/private_bus.rs"]
mod private_bus;
mod support;

use bridge_service::tray::{self, sni::MENU_PATH};
use private_bus::{PrivateBus, watcher};
use std::{collections::HashMap, time::Duration};
use support::{Phone, Rig, pair_phone};
use zbus::zvariant::{OwnedValue, Structure};

type LayoutReply = (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>));

async fn labels(menu: &zbus::Proxy<'_>) -> Vec<(i32, String)> {
    let (_, (_, _, kids)): LayoutReply = menu
        .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
        .await
        .unwrap();
    kids.into_iter()
        .map(|k| {
            let s = Structure::try_from(k).unwrap();
            let f = s.fields();
            let zbus::zvariant::Value::I32(id) = f[0] else {
                panic!("id")
            };
            let props: HashMap<String, OwnedValue> = f[1].try_clone().unwrap().try_into().unwrap();
            let label = props
                .get("label")
                .map(|v| String::try_from(v.try_clone().unwrap()).unwrap())
                .unwrap_or_default();
            (id, label)
        })
        .collect()
}

async fn wait_for<T>(mut f: impl AsyncFnMut() -> Option<T>) -> T {
    for _ in 0..200 {
        if let Some(v) = f().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("not reached within 10 s");
}

#[tokio::test]
async fn pairing_a_phone_updates_the_tray_and_quit_stops_the_service() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    // SAFETY: this test binary has one test, so nothing else reads the environment concurrently.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &bus.address) };
    let (_watcher, log) = watcher(&bus).await;
    let rig = Rig::start().await;
    let task = tokio::spawn(tray::run(rig.daemon.clone(), rig.state_dir.clone()));

    let name = wait_for(async || log.lock().unwrap().first().cloned()).await;
    let client = bus.connect().await;
    let menu = zbus::Proxy::new(&client, name.as_str(), MENU_PATH, "com.canonical.dbusmenu")
        .await
        .unwrap();
    let first = labels(&menu).await;
    assert_eq!(first[0].1, "GeekBridge");
    assert_eq!(first[1].1, "No phone paired yet");

    // A phone pairs and connects: the link events poke the tray, which redraws without being polled.
    let phone = Phone::new();
    let _link = pair_phone(&rig, &phone).await;
    let after = wait_for(async || {
        let l = labels(&menu).await;
        (l[1].1 == "Connected").then_some(l)
    })
    .await;
    assert_eq!(after[0].1, "Test Phone");
    assert!(after.iter().any(|(_, l)| l == "Send file\u{2026}"));

    // Quit (menu id 15) resolves the future the service selects on.
    let data = zbus::zvariant::Value::from(0_i32);
    menu.call_method("Event", &(15_i32, "clicked", &data, 0_u32))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), tray::quit_requested())
        .await
        .expect("Quit reaches the service");
    task.abort();
}
