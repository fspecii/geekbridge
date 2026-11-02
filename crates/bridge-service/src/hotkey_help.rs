// SPDX-License-Identifier: GPL-3.0-or-later
//! Printed instructions for binding "paste from phone" on desktops where GeekBridge does not edit
//! a config file (GNOME, KDE, sway/i3, X11), and the key spellings those need.
use bridge_core::hotkey::Hotkey;

fn mods(k: &Hotkey) -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    // (sway/i3, GNOME, KDE) names, in a fixed order.
    [
        (k.ctrl, ("Ctrl", "<Control>", "Ctrl")),
        (k.alt, ("Mod1", "<Alt>", "Alt")),
        (k.shift, ("Shift", "<Shift>", "Shift")),
        (k.meta, ("Mod4", "<Super>", "Meta")),
    ]
    .into_iter()
    .filter_map(|(on, n)| on.then_some(n))
}

pub fn sway_keys(k: &Hotkey) -> String {
    mods(k)
        .map(|m| m.0.to_owned())
        .chain([k.key.to_ascii_lowercase()])
        .collect::<Vec<_>>()
        .join("+")
}

pub fn gnome_binding(k: &Hotkey) -> String {
    let m: String = mods(k).map(|m| m.1).collect();
    format!("{m}{}", k.key.to_ascii_lowercase())
}

pub fn kde_keys(k: &Hotkey) -> String {
    mods(k)
        .map(|m| m.2.to_owned())
        .chain([k.key.clone()])
        .collect::<Vec<_>>()
        .join("+")
}

/// Everything a user of a desktop other than Hyprland needs, for `command` and `key`.
pub fn instructions(k: &Hotkey, command: &str, popup_rules: bool) -> String {
    let path = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/geekbridge/";
    let schema = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
    let mut s = format!(
        "Shortcut: {}\nCommand:  {command}\n\n\
GNOME (Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts, or):\n\
  gsettings set org.gnome.settings-daemon.plugins.media-keys custom-keybindings \"['{path}']\"\n\
  gsettings set {schema}:{path} name 'GeekBridge: paste from phone'\n\
  gsettings set {schema}:{path} command '{command}'\n\
  gsettings set {schema}:{path} binding '{}'\n\
  (the first line replaces the list of custom shortcuts; if you already have some, add the path to it instead)\n\n\
KDE Plasma (System Settings > Keyboard > Shortcuts > Add New > Command or Script):\n\
  Command: {command}\n  Shortcut: {}\n\n\
sway / i3 (config file):\n\
  bindsym {} exec {command}\n",
        k.normalized(),
        gnome_binding(k),
        kde_keys(k),
        sway_keys(k),
    );
    if popup_rules {
        s.push_str(
            "  # keep the call and share windows on top of everything\n\
  # one command per line: sway runs only the first of a comma chain on a window that just became floating\n\
  for_window [app_id=\"dev.geekbridge.(Calls|Share)\"] floating enable\n\
  for_window [app_id=\"dev.geekbridge.(Calls|Share)\"] sticky enable\n\
  for_window [app_id=\"dev.geekbridge.(Calls|Share)\"] resize set 420 700\n\
  for_window [app_id=\"dev.geekbridge.(Calls|Share)\"] move position center\n\
  # i3 (X11): for_window [class=\"dev.geekbridge.(Calls|Share)\"] floating enable, sticky enable\n",
        );
    }
    s.push_str(
        "\nOther X11 desktops: bind the same command in your window manager or with xbindkeys.\n",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spellings() {
        let k = Hotkey::parse("Ctrl+Shift+V").unwrap();
        assert_eq!(sway_keys(&k), "Ctrl+Shift+v");
        assert_eq!(gnome_binding(&k), "<Control><Shift>v");
        assert_eq!(kde_keys(&k), "Ctrl+Shift+V");
        let k = Hotkey::parse("Super+Alt+F9").unwrap();
        assert_eq!(sway_keys(&k), "Mod1+Mod4+f9");
        assert_eq!(gnome_binding(&k), "<Alt><Super>f9");
    }

    #[test]
    fn instructions_name_every_desktop() {
        let k = Hotkey::parse("Super+Shift+V").unwrap();
        let t = instructions(
            &k,
            "geekbridge-ctl clipboard paste-from-phone --notify",
            true,
        );
        for needle in [
            "GNOME",
            "KDE Plasma",
            "bindsym Shift+Mod4+v exec geekbridge-ctl",
            "app_id",
            "<Shift><Super>v",
        ] {
            assert!(t.contains(needle), "{needle}");
        }
    }
}
