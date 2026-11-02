//! A stand-in `org.kde.StatusNotifierWatcher` for headless verification (scripts/e2e/tray-headless.sh).
//! It owns the watcher name on the session bus it is started on, records every
//! `RegisterStatusNotifierItem`, then reads the item the way a bar would and prints what it saw:
//!
//!   REGISTERED <bus name>
//!   PROP Status=Active Title=GeekBridge ToolTip=<title>|<body> Pixmaps=22,32,48
//!   MENU <id> <enabled> <visible> <label>
//!
//! and writes the 48 px icon it received to DIR/received-icon.png (the picture a bar would draw).
//!
//!   tray-fake-watcher [--out DIR] [--click ID]
#[cfg(target_os = "linux")]
mod linux {
    use bridge_service::tray::icon::Icon;
    use std::{collections::HashMap, path::PathBuf};
    use tokio::sync::mpsc;
    use zbus::zvariant::{OwnedValue, Structure, Value};

    struct Watcher(mpsc::UnboundedSender<String>);

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: &str) {
            let _ = self.0.send(service.to_owned());
        }
        #[zbus(property)]
        fn is_status_notifier_host_registered(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn protocol_version(&self) -> i32 {
            0
        }
    }

    type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
    type LayoutReply = (u32, (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>));

    #[tokio::main(flavor = "current_thread")]
    pub async fn main() -> zbus::Result<()> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let flag = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
        };
        let out = flag("--out").map(PathBuf::from);
        let click: Option<i32> = flag("--click").and_then(|s| s.parse().ok());
        let (tx, mut rx) = mpsc::unbounded_channel();
        let conn = zbus::connection::Builder::session()?
            .serve_at("/StatusNotifierWatcher", Watcher(tx))?
            .name("org.kde.StatusNotifierWatcher")?
            .build()
            .await?;
        println!("WATCHER READY");
        while let Some(name) = rx.recv().await {
            println!("REGISTERED {name}");
            let item = zbus::Proxy::new(
                &conn,
                name.as_str(),
                "/StatusNotifierItem",
                "org.kde.StatusNotifierItem",
            )
            .await?;
            let status: String = item.get_property("Status").await?;
            let title: String = item.get_property("Title").await?;
            let tip: (String, Pixmaps, String, String) = item.get_property("ToolTip").await?;
            let pix: Pixmaps = item.get_property("IconPixmap").await?;
            println!(
                "PROP Status={status} Title={title} ToolTip={}|{} Pixmaps={}",
                tip.2,
                tip.3,
                pix.iter()
                    .map(|p| p.0.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            if let (Some(dir), Some((w, _, argb))) = (&out, pix.iter().find(|p| p.0 == 48)) {
                let rgba = argb
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[1], p[2], p[3], p[0]])
                    .collect();
                std::fs::create_dir_all(dir).ok();
                std::fs::write(
                    dir.join("received-icon.png"),
                    Icon {
                        size: *w as usize,
                        rgba,
                    }
                    .to_png(),
                )
                .ok();
            }
            let menu = zbus::Proxy::new(&conn, name.as_str(), "/MenuBar", "com.canonical.dbusmenu")
                .await?;
            let (revision, (_, _, kids)): LayoutReply = menu
                .call("GetLayout", &(0_i32, -1_i32, Vec::<String>::new()))
                .await?;
            println!("LAYOUT revision={revision} items={}", kids.len());
            for kid in kids {
                let s = Structure::try_from(kid).unwrap();
                let f = s.fields();
                let Value::I32(id) = f[0] else { continue };
                let props: HashMap<String, OwnedValue> =
                    f[1].try_clone().unwrap().try_into().unwrap();
                let text = |k: &str| {
                    props
                        .get(k)
                        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
                };
                let flag = |k: &str| {
                    props
                        .get(k)
                        .and_then(|v| bool::try_from(v.try_clone().ok()?).ok())
                        .unwrap_or(true)
                };
                let label = if text("type").as_deref() == Some("separator") {
                    "-----".to_owned()
                } else {
                    text("label").unwrap_or_default()
                };
                println!(
                    "MENU {id} enabled={} visible={} {label}",
                    flag("enabled"),
                    flag("visible")
                );
            }
            if let Some(id) = click {
                menu.call_method("Event", &(id, "clicked", &Value::from(0_i32), 0_u32))
                    .await?;
                println!("CLICKED {id}");
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn main() -> zbus::Result<()> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("the StatusNotifierItem check is Linux only");
}
