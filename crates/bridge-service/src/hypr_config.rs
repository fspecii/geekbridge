//! Edits of the user's Hyprland configuration: a global "paste from phone" binding and window rules
//! for the call and share windows (docs/omarchy.md).
//!
//! Everything GeekBridge writes sits between two marker lines and nowhere else:
//!
//! ```text
//! -- >>> GeekBridge hotkey (managed by `geekbridge-ctl hotkey`; remove with `hotkey uninstall`) >>>
//! ...
//! -- <<< GeekBridge hotkey <<<
//! ```
//!
//! Installing again replaces the block's body, uninstalling removes the block, and every byte
//! outside the markers is preserved. Only the user's own files are candidates: the target is the
//! first of `bindings.lua`, `bindings.conf`, `hyprland.lua`, `hyprland.conf` found in the Hyprland
//! config directory (Omarchy documents `bindings.*` as the place for personal bindings), and a file
//! that resolves into a distribution-managed directory (`/usr`, `.../omarchy/default`) is refused.
//! All functions here work on explicit paths and strings; nothing reads `$HOME`.
use bridge_core::hotkey::Hotkey;
use std::path::{Path, PathBuf};

/// Names of the two independently managed blocks.
pub const BLOCK_HOTKEY: &str = "hotkey";
pub const BLOCK_RULES: &str = "window rules";

/// Window classes (Wayland app_id) of the windows the rules apply to, with their popup size.
/// `dev.geekbridge.Calls` shows the incoming call, `dev.geekbridge.Share` the file offers.
pub const POPUP_WINDOWS: [(&str, u32, u32); 2] = [
    ("dev.geekbridge.Calls", 400, 700),
    ("dev.geekbridge.Share", 480, 640),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    /// Hyprland's Lua configuration (`hl.bind`, `hl.window_rule`), used by current Omarchy.
    Lua,
    /// Classic hyprlang (`bind =`, `windowrule =`).
    Conf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub path: PathBuf,
    pub flavor: Flavor,
}

/// Which window-rule syntax a `.conf` file takes. Hyprland 0.53 replaced `windowrulev2 = float,
/// class:X` by `windowrule = float on, match:class X`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleSyntax {
    Current,
    Legacy,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub key: Hotkey,
    /// Shell command the binding runs.
    pub command: String,
    pub bind: bool,
    pub rules: bool,
    /// Pin the popups so they follow the user across workspaces and stay above fullscreen.
    pub pin: bool,
    pub syntax: RuleSyntax,
    /// Existing bindings of the same combination that must be released first (`hl.unbind`).
    pub unbind_first: bool,
}

/// First usable Hyprland config file for personal bindings in `hypr_dir` (`~/.config/hypr`).
pub fn detect_target(hypr_dir: &Path) -> Result<Target, String> {
    for (name, flavor) in [
        ("bindings.lua", Flavor::Lua),
        ("bindings.conf", Flavor::Conf),
        ("hyprland.lua", Flavor::Lua),
        ("hyprland.conf", Flavor::Conf),
    ] {
        let path = hypr_dir.join(name);
        if path.is_file() {
            refuse_managed(&path)?;
            return Ok(Target { path, flavor });
        }
    }
    Err(format!(
        "no Hyprland config found in {} (looked for bindings.lua, bindings.conf, hyprland.lua, hyprland.conf)",
        hypr_dir.display()
    ))
}

/// A file that is, or links into, a package-managed location must not be edited.
fn refuse_managed(path: &Path) -> Result<(), String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = real.to_string_lossy();
    let managed = real.starts_with("/usr")
        || real.starts_with("/etc")
        || text.contains("/omarchy/default/")
        || text.contains("/share/omarchy/");
    if managed {
        return Err(format!(
            "{} resolves to {}, which is managed by your distribution; refusing to edit it",
            path.display(),
            real.display()
        ));
    }
    Ok(())
}

fn comment(flavor: Flavor) -> &'static str {
    match flavor {
        Flavor::Lua => "--",
        Flavor::Conf => "#",
    }
}

fn begin_line(flavor: Flavor, name: &str) -> String {
    format!(
        "{} >>> GeekBridge {name} (managed by `geekbridge-ctl hotkey`; remove with `geekbridge-ctl hotkey uninstall`) >>>",
        comment(flavor)
    )
}

fn end_line(flavor: Flavor, name: &str) -> String {
    format!("{} <<< GeekBridge {name} <<<", comment(flavor))
}

fn is_begin(line: &str, flavor: Flavor, name: &str) -> bool {
    line.starts_with(&format!("{} >>> GeekBridge {name} (", comment(flavor)))
}

/// Line range `[begin, end]` of the block called `name`, `None` when absent, an error when the
/// markers are damaged (an unterminated or repeated block is never guessed at).
fn find_block(
    lines: &[&str],
    flavor: Flavor,
    name: &str,
) -> Result<Option<(usize, usize)>, String> {
    let end = end_line(flavor, name);
    let mut found = None;
    let mut i = 0;
    while i < lines.len() {
        if is_begin(lines[i], flavor, name) {
            if found.is_some() {
                return Err(format!(
                    "more than one GeekBridge {name} block; fix it by hand"
                ));
            }
            let Some(off) = lines[i + 1..].iter().position(|l| l.trim_end() == end) else {
                return Err(format!(
                    "GeekBridge {name} block has no end marker; fix it by hand"
                ));
            };
            found = Some((i, i + 1 + off));
            i += off + 1;
        }
        i += 1;
    }
    Ok(found)
}

/// `text` with the block `name` set to `body` (replaced in place, or appended at the end).
pub fn set_block(
    text: &str,
    flavor: Flavor,
    name: &str,
    body: &[String],
) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut block = vec![begin_line(flavor, name)];
    block.extend(body.iter().cloned());
    block.push(end_line(flavor, name));
    let mut out: Vec<String> = Vec::new();
    match find_block(&lines, flavor, name)? {
        Some((b, e)) => {
            out.extend(lines[..b].iter().map(|l| (*l).to_owned()));
            out.extend(block);
            out.extend(lines[e + 1..].iter().map(|l| (*l).to_owned()));
            return Ok(join(&out, text.ends_with('\n')));
        }
        None => {
            out.extend(lines.iter().map(|l| (*l).to_owned()));
            if !lines.is_empty() {
                out.push(String::new());
            }
            out.extend(block);
        }
    }
    Ok(join(&out, true))
}

/// `text` without the block `name` (and the one blank line that separated it), or unchanged.
pub fn remove_block(text: &str, flavor: Flavor, name: &str) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let Some((b, e)) = find_block(&lines, flavor, name)? else {
        return Ok(text.to_owned());
    };
    // set_block puts exactly one blank line before an appended block; take it back with the block.
    let start = if b > 0 && lines[b - 1].trim().is_empty() {
        b - 1
    } else {
        b
    };
    let mut out: Vec<String> = lines[..start].iter().map(|l| (*l).to_owned()).collect();
    out.extend(lines[e + 1..].iter().map(|l| (*l).to_owned()));
    Ok(join(&out, text.ends_with('\n')))
}

/// `lines` as text; `newline` says whether the last line ends with one (a file whose last line had
/// none keeps it that way).
fn join(lines: &[String], newline: bool) -> String {
    match (lines.is_empty(), newline) {
        (true, _) => String::new(),
        (false, true) => format!("{}\n", lines.join("\n")),
        (false, false) => lines.join("\n"),
    }
}

fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `SHIFT + SUPER + V`, the combination as Hyprland's Lua API spells it.
pub fn lua_keys(key: &Hotkey) -> String {
    let spec = key.hyprland_spec();
    let (mods, k) = spec.split_once(',').unwrap_or(("", &spec));
    mods.split_whitespace()
        .chain([k])
        .collect::<Vec<_>>()
        .join(" + ")
}

pub fn hotkey_body(flavor: Flavor, o: &Options) -> Vec<String> {
    let desc = "GeekBridge: paste from phone";
    match flavor {
        Flavor::Lua => {
            let keys = lua_keys(&o.key);
            let mut v = Vec::new();
            if o.unbind_first {
                v.push(format!("hl.unbind({})", lua_str(&keys)));
            }
            v.push(format!(
                "hl.bind({}, hl.dsp.exec_cmd({}), {{ description = {} }})",
                lua_str(&keys),
                lua_str(&o.command),
                lua_str(desc)
            ));
            v
        }
        Flavor::Conf => {
            let spec = o.key.hyprland_spec();
            let (mods, key) = spec.split_once(',').unwrap_or(("", &spec));
            let mut v = Vec::new();
            if o.unbind_first {
                v.push(format!("unbind = {mods}, {key}"));
            }
            v.push(format!(
                "bindd = {mods}, {key}, {desc}, exec, {}",
                o.command
            ));
            v
        }
    }
}

pub fn rules_body(flavor: Flavor, o: &Options) -> Vec<String> {
    let mut v = Vec::new();
    for (class, w, h) in POPUP_WINDOWS {
        let re = format!("^({})$", class.replace('.', "\\."));
        match flavor {
            Flavor::Lua => {
                let re = re.replace('\\', "\\\\");
                let pin = if o.pin { ", pin = true" } else { "" };
                v.push(format!(
                    "hl.window_rule({{ match = {{ class = \"{re}\" }}, float = true, center = true, size = {{ {w}, {h} }}{pin} }})"
                ));
            }
            Flavor::Conf => match o.syntax {
                RuleSyntax::Current => {
                    let m = format!("match:class {re}");
                    v.push(format!("windowrule = float on, {m}"));
                    v.push(format!("windowrule = center on, {m}"));
                    v.push(format!("windowrule = size {w} {h}, {m}"));
                    if o.pin {
                        v.push(format!("windowrule = pin on, {m}"));
                    }
                }
                RuleSyntax::Legacy => {
                    let m = format!("class:{re}");
                    v.push(format!("windowrulev2 = float, {m}"));
                    v.push(format!("windowrulev2 = center, {m}"));
                    v.push(format!("windowrulev2 = size {w} {h}, {m}"));
                    if o.pin {
                        v.push(format!("windowrulev2 = pin, {m}"));
                    }
                }
            },
        }
    }
    v
}

/// The file after installing the blocks `o` asks for. Blocks it does not ask for are left alone.
pub fn install(text: &str, flavor: Flavor, o: &Options) -> Result<String, String> {
    let mut out = text.to_owned();
    if o.bind {
        out = set_block(&out, flavor, BLOCK_HOTKEY, &hotkey_body(flavor, o))?;
    }
    if o.rules {
        out = set_block(&out, flavor, BLOCK_RULES, &rules_body(flavor, o))?;
    }
    Ok(out)
}

pub fn uninstall(text: &str, flavor: Flavor) -> Result<String, String> {
    let out = remove_block(text, flavor, BLOCK_HOTKEY)?;
    remove_block(&out, flavor, BLOCK_RULES)
}

/// Whether a GeekBridge block exists in `text`.
pub fn installed(text: &str, flavor: Flavor, name: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    matches!(find_block(&lines, flavor, name), Ok(Some(_)))
}

/// A minimal unified-style diff of one changed region (everything between the common first and
/// last lines), enough to show what a block install or removal does.
pub fn diff(path: &Path, old: &str, new: &str) -> String {
    if old == new {
        return format!("{}: no changes\n", path.display());
    }
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut head = 0;
    while head < a.len() && head < b.len() && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0;
    while tail < a.len() - head
        && tail < b.len() - head
        && a[a.len() - 1 - tail] == b[b.len() - 1 - tail]
    {
        tail += 1;
    }
    let mut out = format!(
        "--- {}\n+++ {} (proposed)\n",
        path.display(),
        path.display()
    );
    out.push_str(&format!(
        "@@ -{},{} +{},{} @@\n",
        head + 1,
        a.len() - head - tail,
        head + 1,
        b.len() - head - tail
    ));
    for l in &a[head.saturating_sub(2)..head] {
        out.push_str(&format!(" {l}\n"));
    }
    for l in &a[head..a.len() - tail] {
        out.push_str(&format!("-{l}\n"));
    }
    for l in &b[head..b.len() - tail] {
        out.push_str(&format!("+{l}\n"));
    }
    out
}

/// Write `new` over `path` after copying the current file to `<path>.bak.geekbridge.<stamp>`.
/// Returns the backup path. The write goes through a temporary file in the same directory and a
/// rename, so a crash leaves either the old or the new file; a symlink (dotfile managers) is
/// followed so the link itself stays.
pub fn apply(path: &Path, new: &str, stamp: &str) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    refuse_managed(&real)?;
    let name = real
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("config file has no name")?;
    let backup = real.with_file_name(format!("{name}.bak.geekbridge.{stamp}"));
    std::fs::copy(&real, &backup).map_err(|e| format!("cannot back up {}: {e}", real.display()))?;
    let tmp = real.with_file_name(format!(".{name}.geekbridge.tmp"));
    let result = (|| -> std::io::Result<()> {
        std::fs::write(&tmp, new)?;
        std::fs::set_permissions(&tmp, std::fs::metadata(&real)?.permissions())?;
        std::fs::rename(&tmp, &real)
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "cannot write {}: {e} (backup: {})",
            real.display(),
            backup.display()
        ));
    }
    Ok(backup)
}

/// Combinations bound by the files in `defaults` (read-only; Omarchy's shipped bindings), as
/// `(file, line)` pairs, so installing can warn about or release a shadowed shortcut.
pub fn conflicts(key: &Hotkey, files: &[PathBuf]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for f in files {
        if let Ok(text) = std::fs::read_to_string(f) {
            out.extend(conflicts_in(key, &text).into_iter().map(|l| (f.clone(), l)));
        }
    }
    out
}

/// Lines of `text` that bind `key`.
pub fn conflicts_in(key: &Hotkey, text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|t| !t.starts_with("--") && !t.starts_with('#'))
        .filter(|t| binding_of(t).is_some_and(|k| k == *key))
        .map(str::to_owned)
        .collect()
}

/// The combination a `o.bind("SUPER + V", ...)` / `hl.bind(...)` / `bind = SUPER, V, ...` line binds.
fn binding_of(line: &str) -> Option<Hotkey> {
    let spec = if line.contains("bind(") {
        let rest = &line[line.find("bind(")? + 5..];
        let rest = rest.trim_start().strip_prefix('"')?;
        rest[..rest.find('"')?].replace(' ', "")
    } else if line.starts_with("bind") && line.contains('=') {
        let rest = line.split_once('=')?.1;
        let mut parts = rest.split(',');
        let mods = parts.next()?;
        let key = parts.next()?;
        format!(
            "{}+{}",
            mods.split_whitespace().collect::<Vec<_>>().join("+"),
            key.trim()
        )
    } else {
        return None;
    };
    Hotkey::parse(&spec).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn opts() -> Options {
        Options {
            key: Hotkey::parse("Super+Shift+V").unwrap(),
            command: "geekbridge-ctl clipboard paste-from-phone --notify".into(),
            bind: true,
            rules: true,
            pin: true,
            syntax: RuleSyntax::Current,
            unbind_first: false,
        }
    }

    const LUA: &str = "-- my bindings\no.bind(\"SUPER + B\", \"Browser\", \"chromium\")\n";
    const CONF: &str = "# mine\nbindd = SUPER, B, Browser, exec, chromium\n";

    #[test]
    fn target_prefers_user_bindings_files_in_order() {
        let d = tempfile::tempdir().unwrap();
        assert!(detect_target(d.path()).is_err());
        fs::write(d.path().join("hyprland.conf"), "").unwrap();
        assert_eq!(detect_target(d.path()).unwrap().flavor, Flavor::Conf);
        fs::write(d.path().join("hyprland.lua"), "").unwrap();
        assert!(
            detect_target(d.path())
                .unwrap()
                .path
                .ends_with("hyprland.lua")
        );
        fs::write(d.path().join("bindings.conf"), "").unwrap();
        assert!(
            detect_target(d.path())
                .unwrap()
                .path
                .ends_with("bindings.conf")
        );
        fs::write(d.path().join("bindings.lua"), "").unwrap();
        let t = detect_target(d.path()).unwrap();
        assert!(t.path.ends_with("bindings.lua") && t.flavor == Flavor::Lua);
    }

    #[test]
    fn install_is_idempotent_and_uninstall_restores_both_flavors() {
        for (flavor, base) in [(Flavor::Lua, LUA), (Flavor::Conf, CONF)] {
            let once = install(base, flavor, &opts()).unwrap();
            assert_ne!(once, base);
            assert!(once.starts_with(base), "existing text is untouched");
            assert_eq!(install(&once, flavor, &opts()).unwrap(), once);
            assert!(
                installed(&once, flavor, BLOCK_HOTKEY) && installed(&once, flavor, BLOCK_RULES)
            );
            assert_eq!(uninstall(&once, flavor).unwrap(), base);
            // Nothing installed: uninstall is a no-op.
            assert_eq!(uninstall(base, flavor).unwrap(), base);
        }
    }

    #[test]
    fn reinstall_with_other_key_replaces_only_the_block() {
        let once = install(LUA, Flavor::Lua, &opts()).unwrap();
        let mut o = opts();
        o.key = Hotkey::parse("Ctrl+Alt+P").unwrap();
        let twice = install(&once, Flavor::Lua, &o).unwrap();
        assert!(twice.contains("CTRL + ALT + P") && !twice.contains("SHIFT + SUPER + V"));
        assert_eq!(twice.matches(">>> GeekBridge hotkey").count(), 1);
        assert_eq!(uninstall(&twice, Flavor::Lua).unwrap(), LUA);
    }

    #[test]
    fn text_around_the_blocks_survives_byte_for_byte() {
        let around = "head\n\n-- keep me\n";
        let tail = "\n-- tail without newline";
        let mut text = install(around, Flavor::Lua, &opts()).unwrap();
        text.push_str(tail);
        let reinstall = install(&text, Flavor::Lua, &opts()).unwrap();
        assert_eq!(reinstall, text);
        let removed = uninstall(&text, Flavor::Lua).unwrap();
        assert!(removed.starts_with(around) && removed.contains("tail without newline"));
    }

    #[test]
    fn damaged_markers_are_refused_not_guessed() {
        let broken = format!("a\n{}\nhl.bind()\n", begin_line(Flavor::Lua, BLOCK_HOTKEY));
        assert!(install(&broken, Flavor::Lua, &opts()).is_err());
        assert!(uninstall(&broken, Flavor::Lua).is_err());
        let once = install(LUA, Flavor::Lua, &opts()).unwrap();
        let double = format!("{once}{once}");
        assert!(install(&double, Flavor::Lua, &opts()).is_err());
    }

    #[test]
    fn rules_cover_both_windows_and_respect_pin_and_syntax() {
        let lua = rules_body(Flavor::Lua, &opts()).join("\n");
        assert!(lua.contains("dev\\\\.geekbridge\\\\.Calls") && lua.contains("pin = true"));
        assert!(lua.contains("float = true, center = true, size = { 400, 700 }"));
        let mut o = opts();
        o.pin = false;
        assert!(!rules_body(Flavor::Lua, &o).join("").contains("pin"));
        assert!(!rules_body(Flavor::Conf, &o).join("").contains("pin"));
        let cur = rules_body(Flavor::Conf, &opts()).join("\n");
        assert!(cur.contains("windowrule = pin on, match:class ^(dev\\.geekbridge\\.Share)$"));
        o.syntax = RuleSyntax::Legacy;
        let leg = rules_body(Flavor::Conf, &o).join("\n");
        assert!(leg.contains("windowrulev2 = float, class:^(dev\\.geekbridge\\.Calls)$"));
    }

    #[test]
    fn binding_text_and_unbind() {
        let mut o = opts();
        let b = hotkey_body(Flavor::Lua, &o).join("\n");
        assert_eq!(
            b,
            "hl.bind(\"SHIFT + SUPER + V\", hl.dsp.exec_cmd(\"geekbridge-ctl clipboard paste-from-phone --notify\"), { description = \"GeekBridge: paste from phone\" })"
        );
        o.unbind_first = true;
        assert!(hotkey_body(Flavor::Lua, &o)[0].starts_with("hl.unbind(\"SHIFT + SUPER + V\")"));
        assert_eq!(
            hotkey_body(Flavor::Conf, &o)[1],
            "bindd = SHIFT SUPER, V, GeekBridge: paste from phone, exec, geekbridge-ctl clipboard paste-from-phone --notify"
        );
    }

    #[test]
    fn diff_shows_only_the_changed_region() {
        let new = install(LUA, Flavor::Lua, &opts()).unwrap();
        let d = diff(Path::new("bindings.lua"), LUA, &new);
        assert!(d.contains("+hl.bind(") && d.contains("+-- >>> GeekBridge hotkey"));
        assert!(!d.contains("-o.bind"));
        assert!(diff(Path::new("x"), LUA, LUA).contains("no changes"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_backs_up_follows_symlinks_and_leaves_other_files() {
        let d = tempfile::tempdir().unwrap();
        let other = d.path().join("looknfeel.lua");
        fs::write(&other, "untouched\n").unwrap();
        let real = d.path().join("dotfiles-bindings.lua");
        fs::write(&real, LUA).unwrap();
        let link = d.path().join("bindings.lua");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let new = install(LUA, Flavor::Lua, &opts()).unwrap();
        let backup = apply(&link, &new, "t1").unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), LUA);
        assert!(backup.to_string_lossy().contains(".bak.geekbridge.t1"));
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&link).unwrap(), new);
        assert_eq!(fs::read_to_string(&other).unwrap(), "untouched\n");
        let mut names: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names.len(), 4, "only the backup was added: {names:?}");
    }

    #[cfg(unix)]
    #[test]
    fn managed_locations_are_refused() {
        assert!(
            refuse_managed(Path::new("/usr/share/omarchy/default/hypr/bindings.lua")).is_err()
                || !Path::new("/usr/share/omarchy/default/hypr/bindings.lua").exists()
        );
        let d = tempfile::tempdir().unwrap();
        let managed = d.path().join("share/omarchy/default/hypr");
        fs::create_dir_all(&managed).unwrap();
        fs::write(managed.join("bindings.lua"), "").unwrap();
        assert!(refuse_managed(&managed.join("bindings.lua")).is_err());
        // A user file that links into a managed directory is refused too.
        let hypr = d.path().join("hypr");
        fs::create_dir_all(&hypr).unwrap();
        std::os::unix::fs::symlink(managed.join("bindings.lua"), hypr.join("bindings.lua"))
            .unwrap();
        assert!(detect_target(&hypr).is_err());
    }

    #[test]
    fn shipped_bindings_are_recognised_as_conflicts() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("clip.lua");
        fs::write(
            &f,
            "-- o.bind(\"SHIFT + SUPER + V\", \"x\", y)\no.bind(\"SUPER + V\", \"Universal paste\", y)\nhl.bind(\"SHIFT + SUPER + C\", z)\n",
        )
        .unwrap();
        let v = Hotkey::parse("Super+V").unwrap();
        assert_eq!(conflicts(&v, std::slice::from_ref(&f)).len(), 1);
        assert!(
            conflicts(
                &Hotkey::parse("Super+Shift+V").unwrap(),
                std::slice::from_ref(&f)
            )
            .is_empty()
        );
        assert_eq!(
            conflicts(&Hotkey::parse("Super+Shift+C").unwrap(), &[f]).len(),
            1
        );
        assert_eq!(
            binding_of("bindd = SUPER SHIFT, V, d, exec, x")
                .unwrap()
                .normalized(),
            "Shift+Super+V"
        );
    }
}
