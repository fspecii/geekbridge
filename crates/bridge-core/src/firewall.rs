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
        let Some(ports) = word_after("--dport") else {
            return false;
        };
        let (lo, hi) = ports.split_once(':').unwrap_or((ports, ports));
        matches!((lo.parse::<u16>(), hi.parse::<u16>()), (Ok(lo), Ok(hi)) if lo <= PORT_LO && hi >= PORT_HI)
    })
}

/// Reads ufw's switch and rules and whether firewalld runs. Linux paths; elsewhere nothing is found.
pub fn read() -> Firewall {
    let read_or_none = |p: &str| std::fs::read_to_string(p).ok();
    Firewall {
        ufw_enabled: read_or_none("/etc/ufw/ufw.conf").map(|c| {
            c.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
        }),
        ufw_rules: read_or_none("/etc/ufw/user.rules"),
        firewalld_active: std::path::Path::new("/run/firewalld").exists(),
    }
}

/// Does this firewall provably stop phones (ufw on, its rules readable, and no rule for the range)?
pub fn firewall_blocks(fw: &Firewall) -> bool {
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

pub fn verdict(fw: &Firewall) -> Verdict {
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

/// The exact `sudo` lines that open the range for `tool`, one per line, to copy and run by hand.
pub fn commands(tool: Tool) -> &'static str {
    match tool {
        Tool::Ufw => {
            "sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto tcp\n\
             sudo ufw allow from 192.168.0.0/16 to any port 1716:1764 proto udp"
        }
        Tool::Firewalld => {
            "sudo firewall-cmd --permanent --add-port=1716-1764/tcp --add-port=1716-1764/udp\n\
             sudo firewall-cmd --reload"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTH: &str = "-A ufw-user-input -p tcp --dport 1716:1764 -j ACCEPT\n-A ufw-user-input -p udp --dport 1716:1764 -j ACCEPT\n";

    fn ufw(rules: Option<&str>) -> Firewall {
        Firewall {
            ufw_enabled: Some(true),
            ufw_rules: rules.map(Into::into),
            firewalld_active: false,
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
            let text = commands(tool);
            assert!(text.lines().all(|l| l.starts_with("sudo ")), "{text}");
            assert!(text.contains("1716") && text.contains("1764"));
            assert!(text.contains("tcp") && text.contains("udp") || text.contains("reload"));
        }
        assert_eq!(commands(Tool::Ufw).lines().count(), 2);
    }
}
