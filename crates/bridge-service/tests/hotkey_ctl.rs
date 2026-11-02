//! `geekbridge-ctl hotkey` and `status --waybar` end to end, in a temporary home. HOME and
//! XDG_CONFIG_HOME are overridden and the Hyprland instance variable is removed, so the real
//! `~/.config` is never read or written and the real compositor is never asked to reload.
//! Hyprland and Waybar are Linux desktop tools: the test uses a POSIX-style home and is Unix-only.
#![cfg(unix)]
use std::{fs, path::Path, process::Command};

fn ctl(home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_geekbridge-ctl"))
        .args(["--state-dir"])
        .arg(home.join("state"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .env_remove("OMARCHY_PATH")
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn install_apply_uninstall_in_a_temp_home() {
    let home = tempfile::tempdir().unwrap();
    let hypr = home.path().join(".config/hypr");
    fs::create_dir_all(&hypr).unwrap();
    let base = "-- personal\n";
    fs::write(hypr.join("bindings.lua"), base).unwrap();
    fs::write(hypr.join("looknfeel.lua"), "-- look\n").unwrap();

    let (ok, text) = ctl(home.path(), &["hotkey", "install"]);
    assert!(ok && text.contains("Dry run"), "{text}");
    assert_eq!(fs::read_to_string(hypr.join("bindings.lua")).unwrap(), base);

    let (ok, text) = ctl(home.path(), &["hotkey", "install", "--apply"]);
    assert!(ok && text.contains("Backup:"), "{text}");
    let installed = fs::read_to_string(hypr.join("bindings.lua")).unwrap();
    assert!(installed.starts_with(base) && installed.contains("hl.bind("));
    assert_eq!(
        fs::read_to_string(hypr.join("looknfeel.lua")).unwrap(),
        "-- look\n"
    );

    let (ok, text) = ctl(home.path(), &["hotkey", "status"]);
    assert!(ok && text.contains("binding: installed"), "{text}");

    let (ok, _) = ctl(home.path(), &["hotkey", "uninstall", "--apply"]);
    assert!(ok);
    assert_eq!(fs::read_to_string(hypr.join("bindings.lua")).unwrap(), base);
}

#[test]
fn no_hyprland_config_is_an_explained_error_and_print_works() {
    let home = tempfile::tempdir().unwrap();
    let (ok, text) = ctl(home.path(), &["hotkey", "install", "--apply"]);
    assert!(!ok && text.contains("hotkey print"), "{text}");
    assert!(!home.path().join(".config").exists(), "nothing was created");
    let (ok, text) = ctl(home.path(), &["hotkey", "print"]);
    assert!(ok && text.contains("GNOME") && text.contains("sway"));
}

#[test]
fn waybar_line_without_a_service_is_valid_json() {
    let home = tempfile::tempdir().unwrap();
    let (ok, text) = ctl(home.path(), &["status", "--waybar"]);
    assert!(ok, "{text}");
    let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert_eq!(v["alt"], "offline");
}
