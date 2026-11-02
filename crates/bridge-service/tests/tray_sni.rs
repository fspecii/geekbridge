// SPDX-License-Identifier: GPL-3.0-or-later
//! The Linux tray against a private session bus with a fake `org.kde.StatusNotifierWatcher`: what the
//! item registers, which properties and menu layout a host (waybar, Plasma, the GNOME AppIndicator
//! extension) reads from it, which signals a state change emits, and that a click reaches the
//! callback. Skipped with a loud message when `dbus-daemon` is missing (GEEKBRIDGE_REQUIRE_DBUS=1
//! makes that a failure).
#![cfg(target_os = "linux")]

use bridge_service::tray::{
    icon::Style,
    model::{TrayAction, TrayInput},
    sni::{ITEM_PATH, MENU_PATH, Sni},
};
#[path = "support/private_bus.rs"]
mod private_bus;
use futures_lite::StreamExt;
use private_bus::{PrivateBus, watcher};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use zbus::{
    Connection,
    zvariant::{OwnedValue, Structure, Value},
};

type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
type Tip = (String, Pixmaps, String, String);
type LayoutReply = (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>));

fn phone() -> TrayInput {
    TrayInput {
        connected: vec!["Pixel 8".into()],
        paired: 1,
        can_paste: true,
        ..TrayInput::default()
    }
}

type Actions = Arc<Mutex<Vec<TrayAction>>>;

async fn start(bus: &PrivateBus, input: &TrayInput) -> (Sni, Actions) {
    let actions: Actions = Arc::default();
    let sink = actions.clone();
    let sni = Sni::start(
        Some(&bus.address),
        Style::Light,
        input,
        Arc::new(move |a| sink.lock().unwrap().push(a)),
    )
    .await
    .expect("item starts on the private bus");
    (sni, actions)
}

async fn until<T>(mut f: impl FnMut() -> Option<T>) -> T {
    for _ in 0..100 {
        if let Some(v) = f() {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met within 5 s");
}

async fn item_proxy<'a>(c: &'a Connection, name: &'a str) -> zbus::Proxy<'a> {
    zbus::Proxy::new(c, name, ITEM_PATH, "org.kde.StatusNotifierItem")
        .await
        .unwrap()
}
/// A round trip to the item from a second connection: once it answers, the item has handled
/// everything it had queued before (no fixed sleep needed before asserting that nothing more
/// happened).
async fn settle(bus: &PrivateBus, sni: &Sni) {
    let client = bus.connect().await;
    let item = item_proxy(&client, &sni.bus_name).await;
    let _: String = item.get_property("Id").await.unwrap();
}

async fn menu_proxy<'a>(c: &'a Connection, name: &'a str) -> zbus::Proxy<'a> {
    zbus::Proxy::new(c, name, MENU_PATH, "com.canonical.dbusmenu")
        .await
        .unwrap()
}

#[derive(Debug)]
struct Row {
    id: i32,
    label: String,
    enabled: bool,
    visible: bool,
    separator: bool,
}

async fn layout(menu: &zbus::Proxy<'_>) -> (u32, Vec<Row>) {
    let (revision, (root, root_props, kids)): LayoutReply = menu
        .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
        .await
        .unwrap();
    assert_eq!(root, 0);
    assert_eq!(
        String::try_from(root_props["children-display"].try_clone().unwrap()).unwrap(),
        "submenu"
    );
    let rows = kids
        .into_iter()
        .map(|k| {
            let s = Structure::try_from(k).unwrap();
            let f = s.fields();
            let Value::I32(id) = f[0] else { panic!("id") };
            let props: HashMap<String, OwnedValue> = f[1].try_clone().unwrap().try_into().unwrap();
            let get_s = |k: &str| {
                props
                    .get(k)
                    .map(|v| String::try_from(v.try_clone().unwrap()).unwrap())
            };
            let get_b = |k: &str, d: bool| {
                props
                    .get(k)
                    .map_or(d, |v| bool::try_from(v.try_clone().unwrap()).unwrap())
            };
            Row {
                id,
                label: get_s("label").unwrap_or_default(),
                enabled: get_b("enabled", true),
                visible: get_b("visible", true),
                separator: get_s("type").as_deref() == Some("separator"),
            }
        })
        .collect();
    (revision, rows)
}

#[tokio::test]
async fn registers_with_a_watcher_that_is_already_there() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (_w, log) = watcher(&bus).await;
    let (sni, _) = start(&bus, &phone()).await;
    let seen = until(|| log.lock().unwrap().first().cloned()).await;
    assert_eq!(seen, sni.bus_name);
    assert!(seen.starts_with("org.kde.StatusNotifierItem-"));
    settle(&bus, &sni).await;
    assert_eq!(log.lock().unwrap().len(), 1, "registered exactly once");
}

#[tokio::test]
async fn no_watcher_is_silent_and_a_late_or_restarted_watcher_gets_the_item() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sni, _) = start(&bus, &TrayInput::default()).await;
    settle(&bus, &sni).await; // no host: the item answers, nothing fails
    {
        let (w, log) = watcher(&bus).await;
        let first = until(|| log.lock().unwrap().first().cloned()).await;
        assert_eq!(
            first, sni.bus_name,
            "a bar that starts after us finds the item"
        );
        drop(w);
    }
    let (_again, log) = watcher(&bus).await;
    until(|| (log.lock().unwrap().len() == 1).then_some(())).await; // bar restarted: registered again
}

#[tokio::test]
async fn item_properties_are_what_a_host_reads() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sni, _) = start(&bus, &phone()).await;
    let client = bus.connect().await;
    let item = item_proxy(&client, &sni.bus_name).await;

    let intro: String = client
        .call_method(
            Some(sni.bus_name.as_str()),
            ITEM_PATH,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    for needle in [
        "org.kde.StatusNotifierItem",
        "name=\"IconPixmap\"",
        "name=\"ToolTip\"",
        "name=\"Menu\"",
        "name=\"Activate\"",
        "name=\"NewIcon\"",
        "name=\"NewStatus\"",
    ] {
        assert!(intro.contains(needle), "introspection lacks {needle}");
    }
    let menu_intro: String = client
        .call_method(
            Some(sni.bus_name.as_str()),
            MENU_PATH,
            Some("org.freedesktop.DBus.Introspectable"),
            "Introspect",
            &(),
        )
        .await
        .unwrap()
        .body()
        .deserialize()
        .unwrap();
    for needle in [
        "com.canonical.dbusmenu",
        "name=\"GetLayout\"",
        "name=\"Event\"",
        "name=\"LayoutUpdated\"",
        "name=\"AboutToShow\"",
    ] {
        assert!(
            menu_intro.contains(needle),
            "menu introspection lacks {needle}"
        );
    }

    assert_eq!(
        item.get_property::<String>("Category").await.unwrap(),
        "ApplicationStatus"
    );
    assert_eq!(
        item.get_property::<String>("Id").await.unwrap(),
        "geekbridge"
    );
    assert_eq!(
        item.get_property::<String>("Status").await.unwrap(),
        "Active"
    );
    assert!(!item.get_property::<bool>("ItemIsMenu").await.unwrap());
    let menu_path: zbus::zvariant::OwnedObjectPath = item.get_property("Menu").await.unwrap();
    assert_eq!(menu_path.as_str(), MENU_PATH);
    let pix: Pixmaps = item.get_property("IconPixmap").await.unwrap();
    assert_eq!(
        pix.iter().map(|p| p.0).collect::<Vec<_>>(),
        vec![22, 32, 48]
    );
    for (w, h, data) in &pix {
        assert_eq!(w, h);
        assert_eq!(data.len(), (w * h * 4) as usize);
        assert!(data.chunks(4).any(|p| p[0] == 255), "some dot is opaque");
    }
    let tip: Tip = item.get_property("ToolTip").await.unwrap();
    assert_eq!((tip.2.as_str(), tip.3.as_str()), ("Pixel 8", "Connected"));
    let m = menu_proxy(&client, &sni.bus_name).await;
    assert_eq!(m.get_property::<u32>("Version").await.unwrap(), 3);
    assert_eq!(m.get_property::<String>("Status").await.unwrap(), "normal");
}

#[tokio::test]
async fn menu_layout_and_clicks() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sni, actions) = start(&bus, &phone()).await;
    let client = bus.connect().await;
    let menu = menu_proxy(&client, &sni.bus_name).await;

    let (rev, rows) = layout(&menu).await;
    assert_eq!(rows.len(), 15);
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        (1..=15).collect::<Vec<_>>()
    );
    let labels: Vec<_> = rows
        .iter()
        .filter(|r| !r.separator && r.visible)
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(
        labels,
        [
            "Pixel 8",
            "Connected",
            "Open GeekBridge",
            "Paste clipboard from phone",
            "Send file\u{2026}",
            "Pause sync",
            "Check for updates",
            "Quit"
        ]
    );
    assert!(
        !rows[0].enabled && !rows[1].enabled,
        "header lines are not clickable"
    );
    assert!(
        !rows[4].visible && !rows[5].visible,
        "Answer/Decline hidden when nothing rings"
    );
    assert_eq!(rows.iter().filter(|r| r.separator).count(), 4);

    // Clicks: an enabled item reaches the callback, a disabled or header item does not.
    let data = Value::from(0_i32);
    for id in [8_i32, 9, 1, 5, 15] {
        menu.call_method("Event", &(id, "clicked", &data, 0_u32))
            .await
            .unwrap();
    }
    menu.call_method("Event", &(10_i32, "hovered", &data, 0_u32))
        .await
        .unwrap();
    until(|| (actions.lock().unwrap().len() == 3).then_some(())).await;
    assert_eq!(
        *actions.lock().unwrap(),
        [
            TrayAction::Open,
            TrayAction::PasteFromPhone,
            TrayAction::Quit
        ]
    );
    let missing: Vec<i32> = menu
        .call(
            "EventGroup",
            &(vec![
                (8_i32, "clicked", Value::from(0_i32), 0_u32),
                (99, "clicked", Value::from(0_i32), 0_u32),
            ],),
        )
        .await
        .unwrap();
    assert_eq!(missing, vec![99]);

    let props: Vec<(i32, HashMap<String, OwnedValue>)> = menu
        .call(
            "GetGroupProperties",
            &(vec![8_i32], vec!["label".to_owned()]),
        )
        .await
        .unwrap();
    assert_eq!(props.len(), 1);
    assert_eq!(
        String::try_from(props[0].1["label"].try_clone().unwrap()).unwrap(),
        "Open GeekBridge"
    );
    assert_eq!(props[0].1.len(), 1, "only the requested property");
    assert!(
        menu.call::<_, _, OwnedValue>("GetProperty", &(99_i32, "label"))
            .await
            .is_err()
    );
    let _ = rev;
}

#[tokio::test]
async fn a_state_change_signals_the_icon_status_tooltip_and_layout() {
    let Some(bus) = PrivateBus::start() else {
        return;
    };
    let (sni, _) = start(&bus, &phone()).await;
    let client = bus.connect().await;
    let item = item_proxy(&client, &sni.bus_name).await;
    let menu = menu_proxy(&client, &sni.bus_name).await;
    let mut new_icon = item.receive_signal("NewIcon").await.unwrap();
    let mut new_status = item.receive_signal("NewStatus").await.unwrap();
    let mut new_tip = item.receive_signal("NewToolTip").await.unwrap();
    let mut layout_updated = menu.receive_signal("LayoutUpdated").await.unwrap();
    let (rev0, _) = layout(&menu).await;

    // Same input again: nothing changed, so no signal at all.
    sni.update(&phone()).await;
    // Then a call starts ringing.
    let ringing = TrayInput {
        ringing: true,
        can_answer: true,
        can_decline: true,
        ..phone()
    };
    sni.update(&ringing).await;

    let timeout = Duration::from_secs(5);
    tokio::time::timeout(timeout, new_icon.next())
        .await
        .unwrap()
        .unwrap();
    let st = tokio::time::timeout(timeout, new_status.next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(st.body().deserialize::<String>().unwrap(), "NeedsAttention");
    tokio::time::timeout(timeout, new_tip.next())
        .await
        .unwrap()
        .unwrap();
    let lu = tokio::time::timeout(timeout, layout_updated.next())
        .await
        .unwrap()
        .unwrap();
    let (rev, parent): (u32, i32) = lu.body().deserialize().unwrap();
    assert_eq!(parent, 0);
    assert_eq!(rev, rev0 + 1, "exactly one layout change, not two");

    assert_eq!(
        item.get_property::<String>("Status").await.unwrap(),
        "NeedsAttention"
    );
    let (rev_now, rows) = layout(&menu).await;
    assert_eq!(rev_now, rev);
    assert!(rows[4].visible && rows[4].enabled && rows[4].label == "Answer");
    assert!(rows[5].visible && rows[5].enabled && rows[5].label == "Decline");
    assert_eq!(rows[1].label, "Incoming call");
    let tip: Tip = item.get_property("ToolTip").await.unwrap();
    assert_eq!(tip.3, "Incoming call");
}
