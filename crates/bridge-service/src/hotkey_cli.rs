// SPDX-License-Identifier: GPL-3.0-or-later
//! `geekbridge-ctl hotkey`: install or remove the Hyprland binding and window rules, and print the
//! equivalent steps for other desktops. Pure over an [`Env`] so tests run in a temp directory; the
//! binary builds the real `Env` from `$HOME` / `$XDG_CONFIG_HOME`.
//!
//! Nothing is written without `--apply`; the default is a dry run that prints the diff.
use crate::hotkey_help;
use crate::hypr_config::{self as hc, Flavor, Options, RuleSyntax};
use bridge_core::hotkey::{DEFAULT_HOTKEY, Hotkey};
use std::path::PathBuf;

pub const USAGE: &str = "geekbridge-ctl hotkey COMMAND [OPTIONS]

Commands:
  install      add a marked block to your Hyprland config: the paste-from-phone binding and
               floating/centred/pinned rules for the call and share windows (dry run: prints the diff)
  uninstall    remove those blocks again (dry run unless --apply)
  print        steps for GNOME, KDE, sway/i3 and other desktops
  status       what is installed and which key would be used

Options:
  --apply         write the change (a backup <file>.bak.geekbridge.<time> is made first)
  --key COMBO     shortcut, e.g. \"Super+Shift+V\" (default: the key set in GeekBridge, with
                  Ctrl+Shift+V replaced by Super+Shift+V because terminals use it to paste)
  --no-rules      only the binding            --no-bind   only the window rules
  --no-pin        rules without pinning       --legacy-rules   pre-0.53 windowrulev2 syntax (.conf only)";

/// Everything the command reads from the machine.
pub struct Env {
    /// `~/.config/hypr`.
    pub hypr_dir: PathBuf,
    /// Timestamp for backup file names.
    pub stamp: String,
    /// Files with the distribution's shipped bindings (read only), to warn about shadowed keys.
    pub default_binding_files: Vec<PathBuf>,
    /// Absolute path of `geekbridge-ctl`, when it is a plain path.
    pub ctl: Option<String>,
    /// The hotkey stored in the running service, when it answered.
    pub configured: Option<String>,
    /// `GEEKBRIDGE_STATE_DIR` and `GEEKBRIDGE_CONTROL_PORT` of the session this was run in, when set: the
    /// compositor starts the key's command with its own environment, which would not find a service
    /// that was started with a private state dir (scripts/run-desktop.sh does that).
    pub service_env: Option<(String, u16)>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub text: String,
    /// A file was written (the caller may reload the compositor and check for errors).
    pub wrote: bool,
}

struct Args {
    sub: String,
    apply: bool,
    key: Option<String>,
    bind: bool,
    rules: bool,
    pin: bool,
    syntax: RuleSyntax,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut a = Args {
        sub: String::new(),
        apply: false,
        key: None,
        bind: true,
        rules: true,
        pin: true,
        syntax: RuleSyntax::Current,
    };
    let mut it = args.iter();
    while let Some(w) = it.next() {
        match w.as_str() {
            "--apply" => a.apply = true,
            "--key" => a.key = Some(it.next().ok_or("--key expects a shortcut")?.clone()),
            "--no-rules" => a.rules = false,
            "--no-bind" => a.bind = false,
            "--no-pin" => a.pin = false,
            "--legacy-rules" => a.syntax = RuleSyntax::Legacy,
            "-h" | "--help" => a.sub = "help".into(),
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n\n{USAGE}")),
            s if a.sub.is_empty() => a.sub = s.to_owned(),
            s => return Err(format!("unexpected argument {s}\n\n{USAGE}")),
        }
    }
    if !a.bind && !a.rules {
        return Err("--no-bind and --no-rules together leave nothing to do".into());
    }
    Ok(a)
}

/// The shortcut to use and, when it differs from what the service has, the reason.
fn choose_key(a: &Args, env: &Env) -> Result<(Hotkey, Option<String>), String> {
    if let Some(k) = &a.key {
        return Hotkey::parse(k)
            .map(|h| (h, None))
            .map_err(|e| e.to_string());
    }
    let configured = env.configured.as_deref().unwrap_or(DEFAULT_HOTKEY);
    let k = Hotkey::parse(configured).map_err(|e| e.to_string())?;
    if k.normalized() == DEFAULT_HOTKEY {
        let alt = Hotkey::parse("Super+Shift+V").map_err(|e| e.to_string())?;
        return Ok((
            alt,
            Some(format!(
                "Using Super+Shift+V: {DEFAULT_HOTKEY} is how terminals paste, so a global binding would swallow it. \
Pass --key {DEFAULT_HOTKEY} to use it anyway."
            )),
        ));
    }
    Ok((k, None))
}

/// Shortcuts tried in order when the preferred one is taken by an existing binding.
const ALTERNATIVES: [&str; 4] = [
    "Super+Shift+V",
    "Super+Alt+V",
    "Super+Ctrl+Alt+V",
    "Ctrl+Alt+V",
];

/// The user's other Hyprland config files (`.lua` / `.conf`, no backups), which may bind keys too.
fn other_user_files(hypr_dir: &std::path::Path, target: &std::path::Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(hypr_dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p != target && p.is_file())
        .filter(|p| p.extension().is_some_and(|e| e == "lua" || e == "conf"))
        .collect();
    v.sort();
    v
}

fn command(env: &Env) -> String {
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'))
    };
    let prefix = match &env.service_env {
        Some((dir, port)) if plain(dir) => {
            format!("env GEEKBRIDGE_STATE_DIR={dir} GEEKBRIDGE_CONTROL_PORT={port} ")
        }
        _ => String::new(),
    };
    format!(
        "{prefix}{} --notify",
        crate::hotkey_bind::paste_command(env.ctl.as_deref())
    )
}

pub fn run(args: &[String], env: &Env) -> Result<Report, String> {
    let a = parse(args)?;
    let mut r = Report::default();
    let out = &mut r.text;
    match a.sub.as_str() {
        "" | "help" => out.push_str(USAGE),
        "print" => {
            let (key, note) = choose_key(&a, env)?;
            if let Some(n) = note {
                out.push_str(&format!("{n}\n\n"));
            }
            out.push_str(&hotkey_help::instructions(&key, &command(env), true));
            out.push_str("\nHyprland: `geekbridge-ctl hotkey install` edits your config for you (dry run first).\n");
        }
        "status" => {
            let (key, _) = choose_key(&a, env)?;
            match hc::detect_target(&env.hypr_dir) {
                Err(e) => out.push_str(&format!("hyprland: {e}\n")),
                Ok(t) => {
                    let text = std::fs::read_to_string(&t.path).map_err(|e| e.to_string())?;
                    let yes = |n| {
                        if hc::installed(&text, t.flavor, n) {
                            "installed"
                        } else {
                            "not installed"
                        }
                    };
                    out.push_str(&format!(
                        "config:  {}\nbinding: {}\nrules:   {}\nkey:     {}\n",
                        t.path.display(),
                        yes(hc::BLOCK_HOTKEY),
                        yes(hc::BLOCK_RULES),
                        key.normalized()
                    ));
                }
            }
        }
        "install" | "uninstall" => return change(&a, env),
        other => return Err(format!("unknown hotkey command {other}\n\n{USAGE}")),
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(r)
}

fn change(a: &Args, env: &Env) -> Result<Report, String> {
    let target = hc::detect_target(&env.hypr_dir).map_err(|e| {
        format!(
            "{e}\nNot using Hyprland? Run `geekbridge-ctl hotkey print` for GNOME, KDE and sway."
        )
    })?;
    let old = std::fs::read_to_string(&target.path)
        .map_err(|e| format!("{}: {e}", target.path.display()))?;
    let mut r = Report::default();
    let out = &mut r.text;
    let new = if a.sub == "uninstall" {
        hc::uninstall(&old, target.flavor)?
    } else {
        let (key, note) = choose_key(a, env)?;
        if let Some(n) = note {
            out.push_str(&format!("{n}\n"));
        }
        // Shortcuts already bound elsewhere: the shipped defaults, the user's own file outside our
        // block, and the user's other config files.
        let own = hc::uninstall(&old, target.flavor)?;
        let taken = |k: &Hotkey| {
            let mut found = hc::conflicts(k, &env.default_binding_files);
            found.extend(hc::conflicts(
                k,
                &other_user_files(&env.hypr_dir, &target.path),
            ));
            found.extend(
                hc::conflicts_in(k, &own)
                    .into_iter()
                    .map(|l| (target.path.clone(), l)),
            );
            found
        };
        let mut key = key;
        let mut found = taken(&key);
        // A key the user did not ask for is never taken from an existing binding: pick a free one.
        if !found.is_empty()
            && a.key.is_none()
            && let Some(free) = ALTERNATIVES
                .iter()
                .filter_map(|k| Hotkey::parse(k).ok())
                .find(|k| *k != key && taken(k).is_empty())
        {
            let (f, line) = &found[0];
            out.push_str(&format!(
                "{} is already bound ({}: {line}); using {} instead. Pass --key to choose another.\n",
                key.normalized(),
                f.display(),
                free.normalized()
            ));
            key = free;
            found.clear();
        }
        for (f, line) in &found {
            out.push_str(&format!(
                "Note: {} is already bound ({}: {line}). The GeekBridge block releases it first.\n",
                key.normalized(),
                f.display()
            ));
        }
        if let Some(c) = &env.ctl
            && (c.contains("/target/debug/") || c.contains("/target/release/"))
        {
            out.push_str(&format!(
                "Note: the binding runs {c}, a build directory. Install geekbridge-ctl (or run this from the installed copy) so the shortcut survives a rebuild.\n"
            ));
        }
        let o = Options {
            key,
            command: command(env),
            bind: a.bind,
            rules: a.rules,
            pin: a.pin,
            syntax: a.syntax,
            unbind_first: !found.is_empty(),
        };
        hc::install(&old, target.flavor, &o)?
    };
    if target.flavor == Flavor::Conf && a.syntax == RuleSyntax::Legacy && a.sub == "install" {
        out.push_str("Using the legacy windowrulev2 syntax.\n");
    }
    out.push_str(&hc::diff(&target.path, &old, &new));
    if old == new {
        return Ok(r);
    }
    if !a.apply {
        out.push_str(
            "\nDry run: nothing was written. Run again with --apply to make this change.\n",
        );
        return Ok(r);
    }
    let backup = hc::apply(&target.path, &new, &env.stamp)?;
    r.wrote = true;
    let out = &mut r.text;
    out.push_str(&format!(
        "\nWritten. Backup: {}\nUndo with `geekbridge-ctl hotkey uninstall --apply` or by restoring the backup.\n",
        backup.display()
    ));
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn env(dir: &std::path::Path) -> Env {
        Env {
            hypr_dir: dir.to_path_buf(),
            stamp: "T".into(),
            default_binding_files: vec![],
            ctl: Some("/opt/gb/geekbridge-ctl".into()),
            configured: None,
            service_env: None,
        }
    }
    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn a_private_state_dir_is_carried_into_the_key_command() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("bindings.lua"), "-- mine\n").unwrap();
        let mut e = env(d.path());
        e.service_env = Some(("/home/u/gb/target/realdevice-state".into(), 17381));
        let r = run(&s(&["install", "--apply"]), &e).unwrap();
        assert!(r.wrote);
        let installed = fs::read_to_string(d.path().join("bindings.lua")).unwrap();
        assert!(installed.contains(
            "env GEEKBRIDGE_STATE_DIR=/home/u/gb/target/realdevice-state GEEKBRIDGE_CONTROL_PORT=17381 /opt/gb/geekbridge-ctl clipboard paste-from-phone --notify"
        ));
        // a path the compositor's parser could split is not baked in (the bare command stays)
        e.service_env = Some(("/home/u/my dir".into(), 17381));
        run(&s(&["install", "--apply"]), &e).unwrap();
        let again = fs::read_to_string(d.path().join("bindings.lua")).unwrap();
        assert!(
            !again.contains("GEEKBRIDGE_STATE_DIR")
                && again.contains("/opt/gb/geekbridge-ctl clipboard")
        );
    }

    #[test]
    fn dry_run_by_default_then_apply_then_uninstall_restores() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("bindings.lua");
        let base = "-- mine\no.bind(\"SUPER + B\", \"Browser\", \"x\")\n";
        fs::write(&f, base).unwrap();
        let e = env(d.path());

        let r = run(&s(&["install"]), &e).unwrap();
        assert!(!r.wrote && r.text.contains("Dry run") && r.text.contains("+hl.bind("));
        assert!(r.text.contains("Super+Shift+V"));
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            base,
            "dry run writes nothing"
        );
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);

        let r = run(&s(&["install", "--apply"]), &e).unwrap();
        assert!(r.wrote);
        let installed = fs::read_to_string(&f).unwrap();
        assert!(installed.contains("/opt/gb/geekbridge-ctl clipboard paste-from-phone --notify"));
        assert!(installed.contains("float = true"));
        let backups: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.contains(".bak.geekbridge."))
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            fs::read_to_string(d.path().join(&backups[0])).unwrap(),
            base
        );

        // Second install changes nothing and makes no second backup.
        let r = run(&s(&["install", "--apply"]), &e).unwrap();
        assert!(!r.wrote && r.text.contains("no changes"));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 2);

        let r = run(&s(&["uninstall"]), &e).unwrap();
        assert!(!r.wrote);
        assert_eq!(fs::read_to_string(&f).unwrap(), installed);
        let r = run(&s(&["uninstall", "--apply"]), &e).unwrap();
        assert!(r.wrote);
        assert_eq!(fs::read_to_string(&f).unwrap(), base);
    }

    #[test]
    fn only_the_chosen_file_is_touched() {
        let d = tempfile::tempdir().unwrap();
        for n in [
            "bindings.lua",
            "hyprland.lua",
            "looknfeel.lua",
            "monitors.lua",
        ] {
            fs::write(d.path().join(n), format!("-- {n}\n")).unwrap();
        }
        run(&s(&["install", "--apply"]), &env(d.path())).unwrap();
        for n in ["hyprland.lua", "looknfeel.lua", "monitors.lua"] {
            assert_eq!(
                fs::read_to_string(d.path().join(n)).unwrap(),
                format!("-- {n}\n")
            );
        }
    }

    #[test]
    fn key_choice_and_conflicts() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("bindings.conf"), "# x\n").unwrap();
        let defaults = d.path().join("clipboard.lua");
        fs::write(&defaults, "o.bind(\"CTRL + ALT + P\", \"Pick\", y)\n").unwrap();
        let mut e = env(d.path());
        e.configured = Some("Ctrl+Alt+P".into());
        e.default_binding_files = vec![defaults];
        // Not asked for explicitly: a free key is chosen instead of taking over the binding.
        let t = run(&s(&["install"]), &e).unwrap().text;
        assert!(
            t.contains("already bound") && t.contains("using Shift+Super+V instead"),
            "{t}"
        );
        assert!(
            t.contains("bindd = SHIFT SUPER, V") && !t.contains("unbind"),
            "{t}"
        );
        // Asked for explicitly: honoured, the existing binding is released first.
        let t = run(&s(&["install", "--key", "ctrl+alt+p"]), &e)
            .unwrap()
            .text;
        assert!(
            t.contains("already bound") && t.contains("unbind = CTRL ALT, P"),
            "{t}"
        );
        assert!(t.contains("bindd = CTRL ALT, P"));
        // Explicit Ctrl+Shift+V is honoured without the terminal-paste substitution.
        let t = run(
            &s(&["install", "--key", "ctrl+shift+v", "--no-rules"]),
            &env(d.path()),
        )
        .unwrap()
        .text;
        assert!(t.contains("bindd = CTRL SHIFT, V") && !t.contains("Using Super+Shift+V"));
        assert!(!t.contains("windowrule"));
        assert!(run(&s(&["install", "--key", "V"]), &env(d.path())).is_err());
    }

    #[test]
    fn missing_config_and_bad_args_explain_themselves() {
        let d = tempfile::tempdir().unwrap();
        let e = run(&s(&["install", "--apply"]), &env(d.path())).unwrap_err();
        assert!(e.contains("no Hyprland config") && e.contains("hotkey print"));
        assert!(run(&s(&["install", "--bogus"]), &env(d.path())).is_err());
        assert!(run(&s(&["install", "--no-bind", "--no-rules"]), &env(d.path())).is_err());
        assert!(
            run(&s(&["print"]), &env(d.path()))
                .unwrap()
                .text
                .contains("GNOME")
        );
    }

    #[test]
    fn alternative_skips_keys_bound_in_any_user_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("bindings.lua"),
            "-- x\no.bind(\"SUPER + SHIFT + V\", \"a\", \"b\")\n",
        )
        .unwrap();
        fs::write(
            d.path().join("other.lua"),
            "hl.bind(\"SUPER + ALT + V\", f)\n",
        )
        .unwrap();
        fs::write(
            d.path().join("other.lua.bak.1"),
            "hl.bind(\"SUPER + CTRL + ALT + V\", f)\n",
        )
        .unwrap();
        let t = run(&s(&["install", "--no-rules"]), &env(d.path()))
            .unwrap()
            .text;
        assert!(t.contains("using Ctrl+Alt+Super+V instead"), "{t}");
        assert!(
            t.contains("\"CTRL + ALT + SUPER + V\"") && !t.contains("hl.unbind"),
            "{t}"
        );
        // A build-directory ctl path is called out.
        let mut e = env(d.path());
        e.ctl = Some("/w/target/debug/geekbridge-ctl".into());
        assert!(
            run(&s(&["install"]), &e)
                .unwrap()
                .text
                .contains("build directory")
        );
    }
}
