//! Start the service at login, and the firewall command for this computer.
//!
//! `geekbridge-ctl autostart enable` writes the platform's own per-user entry: an XDG autostart
//! `.desktop` file (Linux), a LaunchAgent plist (macOS) or the per-user `Run` registry value
//! (Windows, through `reg.exe`). Nothing needs administrator rights and nothing is installed
//! system-wide. The entry content and the `reg` command lines are pure functions so they are
//! unit-tested on every host; writing the file or the registry value is only verified on a real
//! machine (docs/platform-status.md).
use std::path::{Path, PathBuf};

/// What to do with the autostart entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Status,
    Enable,
    Disable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
    MacOs,
}

impl Os {
    pub const fn current() -> Self {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::MacOs
        } else {
            Os::Linux
        }
    }
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "GeekBridge";
const AGENT_LABEL: &str = "dev.geekbridge.service";
/// First port of the range and last one the service may walk to (TCP file/link, UDP discovery).
const PORT_RANGE: &str = "1716-1764";

pub fn service_file_name() -> &'static str {
    if cfg!(windows) {
        "geekbridge-service.exe"
    } else {
        "geekbridge-service"
    }
}

/// An executable path is only written into a startup entry when it is absolute and cannot break
/// out of the quoting of any of the three formats.
fn check_exe(exe: &Path) -> Result<&str, String> {
    let text = exe.to_str().ok_or("the service path is not valid text")?;
    if !exe.is_absolute() {
        return Err("the service path must be absolute".into());
    }
    if text.chars().any(|c| c.is_control() || c == '"') {
        return Err("the service path contains characters that cannot be quoted safely".into());
    }
    Ok(text)
}

/// XDG autostart entry (`~/.config/autostart/geekbridge-service.desktop`).
pub fn desktop_entry(exe: &Path) -> Result<String, String> {
    let text = check_exe(exe)?;
    // Exec= uses its own quoting: inside double quotes `"`, `` ` ``, `$` and `\` are escaped by `\`.
    let mut exec = String::from("\"");
    for c in text.chars() {
        // The key is unescaped twice, as a string and as a command line, so each special
        // character needs two layers of backslash (and a backslash itself four).
        match c {
            '`' | '$' => {
                exec.push_str("\\\\");
                exec.push(c);
            }
            '\\' => exec.push_str("\\\\\\\\"),
            _ => exec.push(c),
        }
    }
    exec.push('"');
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=GeekBridge Service\n\
         Comment=Background service for phone pairing, clipboard and file sharing\n\
         Exec={exec}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
    ))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// LaunchAgent plist (`~/Library/LaunchAgents/dev.geekbridge.service.plist`).
pub fn launch_agent(exe: &Path) -> Result<String, String> {
    let text = xml_escape(check_exe(exe)?);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key><string>{AGENT_LABEL}</string>\n\
         \t<key>ProgramArguments</key><array><string>{text}</string></array>\n\
         \t<key>RunAtLoad</key><true/>\n\
         \t<key>ProcessType</key><string>Background</string>\n\
         </dict>\n</plist>\n"
    ))
}

/// Where the entry file lives for `os` under `home` (and `config_home` on Linux). `None` on Windows.
pub fn entry_path(os: Os, home: &Path, config_home: Option<&Path>) -> Option<PathBuf> {
    match os {
        Os::Linux => Some(
            config_home
                .map_or_else(|| home.join(".config"), Path::to_path_buf)
                .join("autostart")
                .join("geekbridge-service.desktop"),
        ),
        Os::MacOs => Some(
            home.join("Library/LaunchAgents")
                .join(format!("{AGENT_LABEL}.plist")),
        ),
        Os::Windows => None,
    }
}

/// `reg.exe` arguments for `action` (the value data is the quoted executable path).
pub fn reg_args(action: Action, exe: &Path) -> Result<Vec<String>, String> {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    Ok(match action {
        Action::Status => s(&["query", RUN_KEY, "/v", RUN_VALUE]),
        Action::Disable => s(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]),
        Action::Enable => {
            let data = format!("\"{}\"", check_exe(exe)?);
            let mut a = s(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d"]);
            a.push(data);
            a.push("/f".into());
            a
        }
    })
}

/// How to let phones reach this computer through its firewall, as text to show the user.
pub fn firewall_help_for(os: Os, exe: &Path) -> String {
    let exe = exe.display();
    match os {
        Os::Windows => format!(
            "Windows Defender Firewall normally asks the first time geekbridge-service listens: choose \"Private networks\" and Allow access.\n\
             To add the rules by hand, run this in an administrator PowerShell:\n\
             \x20 netsh advfirewall firewall add rule name=\"GeekBridge TCP\" dir=in action=allow program=\"{exe}\" protocol=TCP localport={PORT_RANGE} profile=private\n\
             \x20 netsh advfirewall firewall add rule name=\"GeekBridge UDP\" dir=in action=allow program=\"{exe}\" protocol=UDP localport={PORT_RANGE} profile=private\n\
             To remove them: netsh advfirewall firewall delete rule name=\"GeekBridge TCP\" (and \"GeekBridge UDP\")."
        ),
        Os::MacOs => format!(
            "macOS asks \"Do you want the application geekbridge-service to accept incoming network connections?\" the first time it listens: choose Allow.\n\
             If the application firewall is on and you dismissed the prompt, allow it by hand:\n\
             \x20 sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add \"{exe}\"\n\
             \x20 sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp \"{exe}\"\n\
             (System Settings > Network > Firewall > Options shows the same list.) Ports: TCP and UDP {PORT_RANGE}."
        ),
        Os::Linux => format!(
            "Allow TCP and UDP {PORT_RANGE} from your local network, for example:\n\
             \x20 sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto tcp\n\
             \x20 sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto udp\n\
             or, with firewalld: sudo firewall-cmd --permanent --add-port=1716-1764/tcp --add-port=1716-1764/udp && sudo firewall-cmd --reload\n\
             (service: {exe})"
        ),
    }
}

/// [`firewall_help_for`] this computer, naming the service next to the running `geekbridge-ctl`.
pub fn firewall_help() -> String {
    let exe = std::env::current_exe()
        .map(|e| e.with_file_name(service_file_name()))
        .unwrap_or_else(|_| PathBuf::from(service_file_name()));
    firewall_help_for(Os::current(), &exe)
}

fn file_action(
    action: Action,
    path: &Path,
    content: Result<String, String>,
) -> Result<String, String> {
    let shown = path.display();
    match action {
        Action::Status => Ok(if path.is_file() {
            format!("autostart: on ({shown})")
        } else {
            "autostart: off".into()
        }),
        Action::Enable => {
            let content = content?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            }
            std::fs::write(path, content).map_err(|e| format!("cannot write {shown}: {e}"))?;
            Ok(format!(
                "autostart: on ({shown}); the service starts at your next login"
            ))
        }
        Action::Disable => match std::fs::remove_file(path) {
            Ok(()) => Ok("autostart: off".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("autostart: off".into()),
            Err(e) => Err(format!("cannot remove {shown}: {e}")),
        },
    }
}

/// Apply `action` for the service at `service` on this computer.
pub fn run(action: Action, service: &Path) -> Result<String, String> {
    let os = Os::current();
    if os == Os::Windows {
        let args = reg_args(action, service)?;
        let out = std::process::Command::new("reg")
            .args(&args)
            .output()
            .map_err(|e| format!("cannot run reg.exe: {e}"))?;
        return Ok(match (action, out.status.success()) {
            (Action::Status, ok) => format!("autostart: {}", if ok { "on" } else { "off" }),
            (Action::Enable, true) => {
                "autostart: on; the service starts at your next sign-in".into()
            }
            (Action::Disable, _) => "autostart: off".into(),
            (Action::Enable, false) => {
                return Err(format!(
                    "reg.exe failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        });
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or("$HOME is not set")?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let path = entry_path(os, &home, config_home.as_deref()).ok_or("no autostart file here")?;
    let content = match os {
        Os::MacOs => launch_agent(service),
        _ => desktop_entry(service),
    };
    file_action(action, &path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn abs(p: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }).join(p)
    }

    #[test]
    fn exe_paths_that_could_break_quoting_are_refused() {
        assert!(desktop_entry(Path::new("relative/geekbridge-service")).is_err());
        assert!(launch_agent(&abs("a\"b")).is_err());
        assert!(reg_args(Action::Enable, &abs("a\nb")).is_err());
    }

    #[test]
    fn desktop_entry_quotes_the_path() {
        let e = desktop_entry(&abs("Program Files/gb svc")).unwrap();
        assert!(e.contains("Exec=\"") && e.contains("Program Files"));
        let tricky = desktop_entry(&abs("a$b")).unwrap();
        assert!(tricky.contains("\\\\$"), "{tricky}");
    }

    #[test]
    fn launch_agent_escapes_xml_and_runs_at_load() {
        let p = launch_agent(&abs("Apps & Tools/geekbridge-service")).unwrap();
        assert!(p.contains("Apps &amp; Tools") && p.contains("<key>RunAtLoad</key><true/>"));
        assert!(p.contains(AGENT_LABEL));
    }

    #[test]
    fn entry_locations() {
        let home = abs("home/ada");
        assert_eq!(
            entry_path(Os::Linux, &home, None).unwrap(),
            home.join(".config/autostart/geekbridge-service.desktop")
        );
        let cfg = abs("cfg");
        assert_eq!(
            entry_path(Os::Linux, &home, Some(&cfg)).unwrap(),
            cfg.join("autostart/geekbridge-service.desktop")
        );
        assert!(
            entry_path(Os::MacOs, &home, None)
                .unwrap()
                .ends_with("Library/LaunchAgents/dev.geekbridge.service.plist")
        );
        assert!(entry_path(Os::Windows, &home, None).is_none());
    }

    #[test]
    fn reg_commands() {
        let exe = abs("gb/geekbridge-service.exe");
        let add = reg_args(Action::Enable, &exe).unwrap();
        assert_eq!(&add[..2], ["add", RUN_KEY]);
        assert!(add[6] == "/d" || add.contains(&"/d".to_owned()));
        assert!(add.iter().any(|a| a.starts_with('"') && a.ends_with('"')));
        assert_eq!(reg_args(Action::Disable, &exe).unwrap()[0], "delete");
        assert_eq!(reg_args(Action::Status, &exe).unwrap()[0], "query");
    }

    #[test]
    fn enable_status_disable_roundtrip_on_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart/x.desktop");
        let entry = desktop_entry(&abs("gb/geekbridge-service"));
        assert!(
            file_action(Action::Status, &path, entry.clone())
                .unwrap()
                .contains("off")
        );
        assert!(
            file_action(Action::Enable, &path, entry.clone())
                .unwrap()
                .contains("on")
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("[Desktop Entry]")
        );
        assert!(
            file_action(Action::Status, &path, entry.clone())
                .unwrap()
                .contains("on (")
        );
        file_action(Action::Disable, &path, entry.clone()).unwrap();
        // Disabling twice is fine.
        assert!(
            file_action(Action::Disable, &path, entry)
                .unwrap()
                .contains("off")
        );
    }

    #[test]
    fn firewall_text_names_ports_and_the_right_tool() {
        let exe = abs("gb/geekbridge-service");
        assert!(firewall_help_for(Os::Windows, &exe).contains("netsh advfirewall"));
        assert!(firewall_help_for(Os::MacOs, &exe).contains("socketfilterfw"));
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert!(firewall_help_for(os, &exe).contains("1716"));
        }
    }
}
