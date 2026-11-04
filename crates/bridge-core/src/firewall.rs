// SPDX-License-Identifier: GPL-3.0-or-later
//! What can be read about this computer's firewall without root, and the exact commands that open
//! the GeekBridge ports. Shared by `geekbridge-ctl doctor` and the desktop Home guide. Nothing here
//! runs a command: the commands are text the user copies and runs themselves.

/// First and last port of the range the service may use (TCP link, UDP discovery).
pub const PORT_LO: u16 = 1716;
pub const PORT_HI: u16 = 1764;

/// What could be read about the firewall without root.
#[derive(Clone, Debug, Default)]
pub struct Firewall {
    /// `Some(true)` when ufw is switched on (`/etc/ufw/ufw.conf`).
    pub ufw_enabled: Option<bool>,
    /// Contents of `/etc/ufw/user.rules`; `None` when it cannot be read (it is root-only on many systems).
    pub ufw_rules: Option<String>,
    pub firewalld_active: bool,
    /// Windows Defender Firewall: inbound rules read with `netsh` (no elevation needed to read).
    pub windows: Option<WindowsFw>,
    /// macOS application firewall, read with `socketfilterfw`.
    pub mac: Option<MacFw>,
}

/// What `netsh advfirewall` says, as raw text so the parsing stays testable.
#[derive(Clone, Debug, Default)]
pub struct WindowsFw {
    /// Output of `netsh advfirewall show allprofiles state`.
    pub state: String,
    /// Output of `netsh advfirewall firewall show rule name=all dir=in`.
    pub rules: String,
    /// The service program the rules should name (`...\geekbridge-service.exe`).
    pub exe: String,
}

/// What `socketfilterfw` says.
#[derive(Clone, Debug, Default)]
pub struct MacFw {
    /// Output of `--getglobalstate`.
    pub state: String,
    /// Output of `--getappblocked <service>`.
    pub app: String,
}

const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

/// One inbound rule from `netsh advfirewall firewall show rule`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetshRule {
    pub name: String,
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    pub program: String,
    pub protocol: String,
    pub local_port: String,
}

/// Parse the `Key:   value` blocks of `netsh advfirewall firewall show rule name=all`. The keys are
/// English only; a localized Windows yields no rules, which callers treat as "cannot tell".
pub fn parse_netsh_rules(text: &str) -> Vec<NetshRule> {
    let mut out: Vec<NetshRule> = Vec::new();
    let mut cur: Option<NetshRule> = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if k == "Rule Name" {
            out.extend(cur.take());
            cur = Some(NetshRule {
                name: v.to_owned(),
                ..Default::default()
            });
            continue;
        }
        let Some(r) = cur.as_mut() else { continue };
        match k {
            "Enabled" => r.enabled = v.eq_ignore_ascii_case("yes"),
            "Direction" => r.inbound = v.eq_ignore_ascii_case("in"),
            "Action" => r.allow = v.eq_ignore_ascii_case("allow"),
            "Program" => r.program = v.to_owned(),
            "Protocol" => r.protocol = v.to_owned(),
            "LocalPort" => r.local_port = v.to_owned(),
            _ => {}
        }
    }
    out.extend(cur);
    out
}

/// Do these ports (`Any`, `1716`, `1716-1764`, `1716,1717`) cover the whole GeekBridge range?
fn ports_cover_range(ports: &str) -> bool {
    ports.split(',').any(|p| {
        let p = p.trim();
        if p.eq_ignore_ascii_case("any") {
            return true;
        }
        let (lo, hi) = p.split_once('-').unwrap_or((p, p));
        matches!((lo.trim().parse::<u16>(), hi.trim().parse::<u16>()), (Ok(lo), Ok(hi)) if lo <= PORT_LO && hi >= PORT_HI)
    })
}

/// Is there an enabled inbound allow rule that lets `proto` reach the service `exe`? Either a rule
/// for that program (the Defender prompt creates one for Any/Any) or a program-less rule for the
/// whole port range.
pub fn windows_allows(rules: &[NetshRule], proto: &str, exe: &str) -> bool {
    // Choosing "Cancel" in the Defender prompt stores enabled Block rules for the program, and a
    // block rule beats any allow rule.
    let blocked = rules.iter().any(|r| {
        r.enabled
            && r.inbound
            && !r.allow
            && r.program.eq_ignore_ascii_case(exe)
            && (r.protocol.eq_ignore_ascii_case("any") || r.protocol.eq_ignore_ascii_case(proto))
            && ports_cover_range(&r.local_port)
    });
    !blocked && windows_allow_rule_exists(rules, proto, exe)
}

fn windows_allow_rule_exists(rules: &[NetshRule], proto: &str, exe: &str) -> bool {
    rules.iter().any(|r| {
        let program_ok = if r.program.eq_ignore_ascii_case("any") || r.program.is_empty() {
            true
        } else {
            r.program.eq_ignore_ascii_case(exe)
        };
        r.enabled
            && r.inbound
            && r.allow
            && program_ok
            && (r.protocol.eq_ignore_ascii_case("any") || r.protocol.eq_ignore_ascii_case(proto))
            && ports_cover_range(&r.local_port)
    })
}

/// `Some(true)` when any profile's state is ON in `netsh advfirewall show allprofiles state`.
pub fn parse_netsh_state(text: &str) -> Option<bool> {
    let states: Vec<bool> = text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            if w.next()? != "State" {
                return None;
            }
            Some(w.next()?.eq_ignore_ascii_case("on"))
        })
        .collect();
    if states.is_empty() {
        None
    } else {
        Some(states.contains(&true))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacState {
    Off,
    On,
    /// "Block all incoming connections": even allowed programs are refused.
    BlockAll,
}

/// `socketfilterfw --getglobalstate`: `Firewall is enabled. (State = 1)`, `... disabled. (State = 0)`,
/// `State = 2` for block-all.
pub fn parse_mac_state(text: &str) -> Option<MacState> {
    if let Some(n) = text.split("State = ").nth(1) {
        return match n.trim_start().chars().next()? {
            '0' => Some(MacState::Off),
            '1' => Some(MacState::On),
            '2' => Some(MacState::BlockAll),
            _ => None,
        };
    }
    let t = text.to_ascii_lowercase();
    if t.contains("block all") {
        Some(MacState::BlockAll)
    } else if t.contains("disabled") {
        Some(MacState::Off)
    } else if t.contains("enabled") {
        Some(MacState::On)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacApp {
    /// Incoming connections are permitted for the program.
    Allowed,
    /// The program is listed and blocked.
    Blocked,
    /// Not in the firewall's list: macOS shows the "Allow incoming connections" dialog on first listen.
    NotListed,
}

/// `socketfilterfw --getappblocked <path>`. The wording varies between releases, so this reads the
/// meaning: "not part of" means unlisted, "blocked" without "not blocked" means blocked.
pub fn parse_mac_app(text: &str) -> Option<MacApp> {
    let t = text.to_ascii_lowercase();
    if t.contains("not part of") {
        Some(MacApp::NotListed)
    } else if t.contains("is not blocked") || t.contains("permitted") || t.contains("allowed") {
        Some(MacApp::Allowed)
    } else if t.contains("blocked") {
        Some(MacApp::Blocked)
    } else {
        None
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn run_capture(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    {
        // The desktop apps call this from a GUI-subsystem process: no console window per netsh.
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The service program next to the running executable (where the installer puts it).
pub fn default_service_exe() -> std::path::PathBuf {
    let name = if cfg!(windows) {
        "geekbridge-service.exe"
    } else {
        "geekbridge-service"
    };
    std::env::current_exe()
        .map(|e| e.with_file_name(name))
        .unwrap_or_else(|_| std::path::PathBuf::from(name))
}

/// Do these `ufw` rules (the `user.rules` text) accept `proto` on the whole GeekBridge range?
pub fn ufw_allows(rules: &str, proto: &str) -> bool {
    rules.lines().any(|line| {
        let line = line.trim();
        if !line.starts_with("-A ufw-user-input") || !line.ends_with("-j ACCEPT") {
            return false;
        }
        let words: Vec<&str> = line.split_whitespace().collect();
        let word_after = |flag: &str| {
            words
                .iter()
                .position(|w| *w == flag)
                .and_then(|i| words.get(i + 1))
                .copied()
        };
        if word_after("-p") != Some(proto) {
            return false;
        }
        // A single range is `--dport 1716:1764`; ufw writes a range given as `port 1716:1764` (and any
        // list) as `-m multiport --dports 22,1716:1764`.
        let Some(ports) = word_after("--dport").or_else(|| word_after("--dports")) else {
            return false;
        };
        ports.split(',').any(|part| {
            let (lo, hi) = part.split_once(':').unwrap_or((part, part));
            matches!((lo.parse::<u16>(), hi.parse::<u16>()), (Ok(lo), Ok(hi)) if lo <= PORT_LO && hi >= PORT_HI)
        })
    })
}

/// Reads the firewall this OS has: ufw/firewalld on Linux, Defender via `netsh` on Windows, the
/// application firewall via `socketfilterfw` on macOS. Read-only; nothing needs elevation.
pub fn read() -> Firewall {
    #[cfg(windows)]
    {
        let exe = default_service_exe().display().to_string();
        let state = run_capture("netsh", &["advfirewall", "show", "allprofiles", "state"]);
        let rules = run_capture(
            "netsh",
            &[
                "advfirewall",
                "firewall",
                "show",
                "rule",
                "name=all",
                "dir=in",
            ],
        );
        Firewall {
            windows: state
                .zip(rules)
                .map(|(state, rules)| WindowsFw { state, rules, exe }),
            ..Default::default()
        }
    }
    #[cfg(target_os = "macos")]
    {
        let exe = default_service_exe().display().to_string();
        let state = run_capture(SOCKETFILTERFW, &["--getglobalstate"]);
        let app = run_capture(SOCKETFILTERFW, &["--getappblocked", &exe]);
        Firewall {
            mac: state.zip(app).map(|(state, app)| MacFw { state, app }),
            ..Default::default()
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    read_linux()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_linux() -> Firewall {
    let read_or_none = |p: &str| std::fs::read_to_string(p).ok();
    Firewall {
        ufw_enabled: read_or_none("/etc/ufw/ufw.conf").map(|c| {
            c.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
        }),
        ufw_rules: read_or_none("/etc/ufw/user.rules"),
        firewalld_active: std::path::Path::new("/run/firewalld").exists(),
        ..Default::default()
    }
}

/// Does this firewall provably stop phones (ufw on, its rules readable, and no rule for the range)?
pub fn firewall_blocks(fw: &Firewall) -> bool {
    if fw.windows.is_some() || fw.mac.is_some() {
        return matches!(verdict(fw), Verdict::Blocking(_));
    }
    fw.ufw_enabled == Some(true)
        && fw
            .ufw_rules
            .as_ref()
            .is_some_and(|r| !(ufw_allows(r, "tcp") && ufw_allows(r, "udp")))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Ufw,
    Firewalld,
    WindowsDefender,
    MacAppFirewall,
}

/// What the desktop can say about the firewall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No firewall found, ufw is off, or its rules already allow the range: nothing to do.
    Fine,
    /// The firewall is known to drop the phone's traffic.
    Blocking(Tool),
    /// A firewall is on but its rules need root to read, so it cannot be told whether it blocks.
    Unverified(Tool),
}

fn windows_verdict(w: &WindowsFw) -> Verdict {
    let rules = parse_netsh_rules(&w.rules);
    match parse_netsh_state(&w.state) {
        Some(false) => Verdict::Fine,
        None => Verdict::Unverified(Tool::WindowsDefender),
        // On, but the rules could not be read (non-English Windows): cannot tell.
        Some(true) if rules.is_empty() => Verdict::Unverified(Tool::WindowsDefender),
        Some(true) => {
            if windows_allows(&rules, "TCP", &w.exe) && windows_allows(&rules, "UDP", &w.exe) {
                Verdict::Fine
            } else {
                Verdict::Blocking(Tool::WindowsDefender)
            }
        }
    }
}

fn mac_verdict(m: &MacFw) -> Verdict {
    match (parse_mac_state(&m.state), parse_mac_app(&m.app)) {
        (Some(MacState::Off), _) => Verdict::Fine,
        (Some(MacState::BlockAll), _) => Verdict::Blocking(Tool::MacAppFirewall),
        (Some(MacState::On), Some(MacApp::Allowed)) => Verdict::Fine,
        (Some(MacState::On), Some(MacApp::Blocked)) => Verdict::Blocking(Tool::MacAppFirewall),
        // Unlisted: macOS asks on first listen; if the dialog was dismissed nothing is listening.
        _ => Verdict::Unverified(Tool::MacAppFirewall),
    }
}

pub fn verdict(fw: &Firewall) -> Verdict {
    if let Some(w) = &fw.windows {
        return windows_verdict(w);
    }
    if let Some(m) = &fw.mac {
        return mac_verdict(m);
    }
    if firewall_blocks(fw) {
        return Verdict::Blocking(Tool::Ufw);
    }
    if fw.ufw_enabled == Some(true) {
        if fw.ufw_rules.is_none() {
            return Verdict::Unverified(Tool::Ufw);
        }
        return Verdict::Fine;
    }
    if fw.firewalld_active {
        return Verdict::Unverified(Tool::Firewalld);
    }
    Verdict::Fine
}

/// The exact lines that open the range for `tool`, one per line, to copy and run by hand (`sudo`
/// on Linux, an elevated PowerShell on Windows, `sudo socketfilterfw` on macOS). `exe` is the
/// service program the Windows and macOS commands name.
pub fn commands_for(tool: Tool, exe: &str) -> String {
    match tool {
        Tool::Ufw => "sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto tcp\n\
             sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto udp"
            .to_owned(),
        Tool::Firewalld => {
            "sudo firewall-cmd --permanent --add-port=1716-1764/tcp --add-port=1716-1764/udp\n\
             sudo firewall-cmd --reload"
                .to_owned()
        }
        Tool::WindowsDefender => format!(
            "netsh advfirewall firewall add rule name=\"GeekBridge TCP\" dir=in action=allow program=\"{exe}\" protocol=TCP localport=1716-1764 profile=private\n\
             netsh advfirewall firewall add rule name=\"GeekBridge UDP\" dir=in action=allow program=\"{exe}\" protocol=UDP localport=1716-1764 profile=private"
        ),
        Tool::MacAppFirewall => format!(
            "sudo {SOCKETFILTERFW} --add \"{exe}\"\n\
             sudo {SOCKETFILTERFW} --unblockapp \"{exe}\""
        ),
    }
}

/// [`commands_for`] with the service program next to the running executable.
pub fn commands(tool: Tool) -> String {
    commands_for(tool, &default_service_exe().display().to_string())
}

/// What to do in the OS's own dialog or settings, shown next to the commands (empty on Linux).
pub fn guidance(tool: Tool) -> &'static str {
    match tool {
        Tool::Ufw | Tool::Firewalld => "",
        Tool::WindowsDefender => {
            "Windows asks \"Allow GeekBridge to communicate on these networks?\" the first time the service listens: tick Private networks and choose Allow access. If you dismissed it, run the commands in an administrator PowerShell (Start, type PowerShell, right-click, Run as administrator). A network marked Public needs profile=public instead, or switch it to Private in Settings > Network."
        }
        Tool::MacAppFirewall => {
            "macOS asks \"Do you want the application geekbridge-service to accept incoming network connections?\" the first time the service listens: choose Allow. If you dismissed it, run the commands in Terminal, or add geekbridge-service in System Settings > Network > Firewall > Options and set it to Allow incoming connections. \"Block all incoming connections\" must be off."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &str = "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT\n-A ufw-user-input -p udp --dport 1716:1764 -j ACCEPT\n";

    #[test]
    fn ufw_range_rules_are_written_with_multiport() {
        // What `ufw allow from 192.168.1.0/24 to any port 1716:1764 proto tcp` really stores in user.rules.
        let real = "-A ufw-user-input -p tcp -m multiport --dports 1716:1764 -s 192.168.1.0/24 -j ACCEPT\n\
-A ufw-user-input -p udp -m multiport --dports 1716:1764 -s 192.168.1.0/24 -j ACCEPT\n";
        assert!(ufw_allows(real, "tcp") && ufw_allows(real, "udp"));
        let list = "-A ufw-user-input -p tcp -m multiport --dports 22,1700:1800 -j ACCEPT";
        assert!(ufw_allows(list, "tcp") && !ufw_allows(list, "udp"));
        let narrow = "-A ufw-user-input -p tcp -m multiport --dports 22,1716:1730 -j ACCEPT";
        assert!(!ufw_allows(narrow, "tcp"));
        assert!(!ufw_allows(
            "-A ufw-user-input -p tcp -m multiport --dports -j ACCEPT",
            "tcp"
        ));
    }

    fn ufw(rules: Option<&str>) -> Firewall {
        Firewall {
            ufw_enabled: Some(true),
            ufw_rules: rules.map(Into::into),
            firewalld_active: false,
            ..Default::default()
        }
    }

    #[test]
    fn the_verdict_follows_what_can_be_read() {
        assert_eq!(verdict(&Firewall::default()), Verdict::Fine);
        assert_eq!(
            verdict(&Firewall {
                ufw_enabled: Some(false),
                ..Default::default()
            }),
            Verdict::Fine
        );
        assert_eq!(verdict(&ufw(Some(BOTH))), Verdict::Fine);
        assert_eq!(
            verdict(&ufw(Some("-A ufw-user-input -p tcp --dport 22 -j ACCEPT"))),
            Verdict::Blocking(Tool::Ufw)
        );
        // Only one protocol open is still blocking: discovery (UDP) or the link (TCP) fails.
        assert_eq!(
            verdict(&ufw(Some(
                "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT"
            ))),
            Verdict::Blocking(Tool::Ufw)
        );
        assert_eq!(verdict(&ufw(None)), Verdict::Unverified(Tool::Ufw));
        assert_eq!(
            verdict(&Firewall {
                firewalld_active: true,
                ..Default::default()
            }),
            Verdict::Unverified(Tool::Firewalld)
        );
    }

    #[test]
    fn the_commands_are_sudo_lines_for_the_whole_range_and_both_protocols() {
        for tool in [Tool::Ufw, Tool::Firewalld] {
            let text = commands_for(tool, "/x");
            assert!(text.lines().all(|l| l.starts_with("sudo ")), "{text}");
            assert!(text.contains("1716") && text.contains("1764"));
            assert!(text.contains("tcp") && text.contains("udp") || text.contains("reload"));
        }
        assert_eq!(commands_for(Tool::Ufw, "/x").lines().count(), 2);
    }

    const EXE: &str = r"C:\Program Files\GeekBridge\geekbridge-service.exe";

    const NETSH_STATE_ON: &str = "\r\nDomain Profile Settings: \r\n----------------------------------------------------------------------\r\nState                                 OFF\r\n\r\nPrivate Profile Settings: \r\n----------------------------------------------------------------------\r\nState                                 ON\r\n\r\nPublic Profile Settings: \r\n----------------------------------------------------------------------\r\nState                                 ON\r\nOk.\r\n";

    fn rule(
        name: &str,
        program: &str,
        proto: &str,
        port: &str,
        action: &str,
        enabled: &str,
    ) -> String {
        let prog = if program.is_empty() {
            String::new()
        } else {
            format!("Program:                              {program}\r\n")
        };
        format!(
            "Rule Name:                            {name}\r\n----------------------------------------------------------------------\r\nEnabled:                              {enabled}\r\nDirection:                            In\r\nProfiles:                             Private\r\nGrouping:                             \r\nLocalIP:                              Any\r\nRemoteIP:                             Any\r\nProtocol:                             {proto}\r\nLocalPort:                            {port}\r\nRemotePort:                           Any\r\nEdge traversal:                       No\r\nAction:                               {action}\r\n{prog}\r\n"
        )
    }

    fn win(rules: &str, state: &str) -> Firewall {
        Firewall {
            windows: Some(WindowsFw {
                state: state.into(),
                rules: rules.into(),
                exe: EXE.into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn netsh_output_is_parsed() {
        assert_eq!(parse_netsh_state(NETSH_STATE_ON), Some(true));
        assert_eq!(
            parse_netsh_state("State                                 OFF\nState   OFF"),
            Some(false)
        );
        assert_eq!(parse_netsh_state("Zustand  EIN"), None);
        let text = rule("GeekBridge TCP", EXE, "TCP", "1716-1764", "Allow", "Yes");
        let r = parse_netsh_rules(&text);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "GeekBridge TCP");
        assert!(r[0].enabled && r[0].inbound && r[0].allow);
        assert_eq!(
            (r[0].protocol.as_str(), r[0].local_port.as_str()),
            ("TCP", "1716-1764")
        );
        assert_eq!(r[0].program, EXE);
        assert!(parse_netsh_rules("No rules match the specified criteria.\r\n").is_empty());
    }

    #[test]
    fn windows_verdicts() {
        let both = rule("GeekBridge TCP", EXE, "TCP", "1716-1764", "Allow", "Yes")
            + &rule("GeekBridge UDP", EXE, "UDP", "1716-1764", "Allow", "Yes");
        assert_eq!(verdict(&win(&both, NETSH_STATE_ON)), Verdict::Fine);
        // What the Defender prompt creates: the program, any protocol, any port.
        let prompt = rule("geekbridge-service.exe", EXE, "Any", "Any", "Allow", "Yes");
        assert_eq!(verdict(&win(&prompt, NETSH_STATE_ON)), Verdict::Fine);
        // Only TCP, a disabled rule, a block rule, another program: all still blocking.
        let tcp = rule("GeekBridge TCP", EXE, "TCP", "1716-1764", "Allow", "Yes");
        let blocking = Verdict::Blocking(Tool::WindowsDefender);
        assert_eq!(verdict(&win(&tcp, NETSH_STATE_ON)), blocking);
        let off = rule("a", EXE, "Any", "Any", "Allow", "No");
        assert_eq!(verdict(&win(&off, NETSH_STATE_ON)), blocking);
        let block = rule("a", EXE, "Any", "Any", "Block", "Yes");
        assert_eq!(verdict(&win(&block, NETSH_STATE_ON)), blocking);
        // What "Cancel" in the Defender prompt leaves behind next to an allow rule: block wins.
        let cancelled = prompt.clone() + &rule("b", EXE, "TCP", "Any", "Block", "Yes");
        assert_eq!(verdict(&win(&cancelled, NETSH_STATE_ON)), blocking);
        let other_block =
            prompt.clone() + &rule("b", r"C:\other.exe", "Any", "Any", "Block", "Yes");
        assert_eq!(verdict(&win(&other_block, NETSH_STATE_ON)), Verdict::Fine);
        let other = rule("a", r"C:\other.exe", "Any", "Any", "Allow", "Yes");
        assert_eq!(verdict(&win(&other, NETSH_STATE_ON)), blocking);
        let narrow = rule("a", "", "TCP", "1716", "Allow", "Yes");
        assert_eq!(verdict(&win(&narrow, NETSH_STATE_ON)), blocking);
        // A port rule without a program counts; firewall off and unreadable output do not block.
        let anyprog = rule("a", "", "Any", "1700-1800", "Allow", "Yes");
        assert_eq!(verdict(&win(&anyprog, NETSH_STATE_ON)), Verdict::Fine);
        assert_eq!(verdict(&win("", "State  OFF")), Verdict::Fine);
        assert_eq!(
            verdict(&win("", NETSH_STATE_ON)),
            Verdict::Unverified(Tool::WindowsDefender)
        );
        assert!(firewall_blocks(&win(&tcp, NETSH_STATE_ON)));
        assert!(!firewall_blocks(&win(&both, NETSH_STATE_ON)));
    }

    #[test]
    fn windows_commands_are_the_exact_netsh_lines() {
        let t = commands_for(Tool::WindowsDefender, EXE);
        let lines: Vec<&str> = t.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            format!(
                "netsh advfirewall firewall add rule name=\"GeekBridge TCP\" dir=in action=allow program=\"{EXE}\" protocol=TCP localport=1716-1764 profile=private"
            )
        );
        assert!(lines[1].contains("protocol=UDP") && lines[1].contains("GeekBridge UDP"));
        assert!(!t.contains("sudo"));
    }

    #[test]
    fn mac_output_is_parsed() {
        assert_eq!(
            parse_mac_state("Firewall is enabled. (State = 1)\n"),
            Some(MacState::On)
        );
        assert_eq!(
            parse_mac_state("Firewall is disabled. (State = 0)"),
            Some(MacState::Off)
        );
        assert_eq!(
            parse_mac_state(
                "Firewall is set to block all non-essential incoming connections (State = 2)"
            ),
            Some(MacState::BlockAll)
        );
        assert_eq!(parse_mac_state("Firewall is enabled."), Some(MacState::On));
        assert_eq!(parse_mac_state(""), None);
        assert_eq!(
            parse_mac_app("The application is not part of the application firewall.\n"),
            Some(MacApp::NotListed)
        );
        assert_eq!(
            parse_mac_app("Incoming connections to this application are blocked\n"),
            Some(MacApp::Blocked)
        );
        assert_eq!(
            parse_mac_app("Incoming connections to this application are permitted\n"),
            Some(MacApp::Allowed)
        );
        assert_eq!(parse_mac_app("something new"), None);
    }

    #[test]
    fn mac_verdicts() {
        let mac = |state: &str, app: &str| Firewall {
            mac: Some(MacFw {
                state: state.into(),
                app: app.into(),
            }),
            ..Default::default()
        };
        let on = "Firewall is enabled. (State = 1)";
        let tool = Tool::MacAppFirewall;
        assert_eq!(
            verdict(&mac("Firewall is disabled. (State = 0)", "")),
            Verdict::Fine
        );
        assert_eq!(
            verdict(&mac(on, "Incoming connections ... permitted")),
            Verdict::Fine
        );
        assert_eq!(verdict(&mac(on, "is blocked")), Verdict::Blocking(tool));
        assert_eq!(
            verdict(&mac(on, "not part of the application firewall")),
            Verdict::Unverified(tool)
        );
        assert_eq!(
            verdict(&mac("(State = 2)", "permitted")),
            Verdict::Blocking(tool)
        );
        assert_eq!(
            verdict(&mac("garbage", "garbage")),
            Verdict::Unverified(tool)
        );
    }

    #[test]
    fn mac_commands_use_socketfilterfw_with_sudo() {
        let t = commands_for(Tool::MacAppFirewall, "/opt/gb/geekbridge-service");
        assert_eq!(
            t,
            "sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add \"/opt/gb/geekbridge-service\"\nsudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp \"/opt/gb/geekbridge-service\""
        );
        assert!(guidance(Tool::MacAppFirewall).contains("Allow"));
        assert!(guidance(Tool::WindowsDefender).contains("administrator"));
    }
}
