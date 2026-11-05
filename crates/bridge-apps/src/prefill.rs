// SPDX-License-Identifier: GPL-3.0-or-later
//! Hand-off between the apps: Contacts opens the dialer or a new message with a number already
//! typed. The number travels in an environment variable of the new process, never on a command
//! line (visible to other users) and never over the network.
use std::{
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
};

pub const DIAL_VAR: &str = "GEEKBRIDGE_DIAL";
pub const MESSAGE_VAR: &str = "GEEKBRIDGE_MESSAGE_TO";
/// Set (to `1`) by the in-call card's Keypad button: the new Calls window opens with the call's keypad shown.
pub const KEYPAD_VAR: &str = "GEEKBRIDGE_KEYPAD";

/// Starts a sibling app with one prefill variable set. Returns a short error for the window.
pub fn launch_with(binary: &str, var: &str, value: &str) -> Result<(), String> {
    let mut child = spawn_with(binary, var, value)?;
    // Reap it when it exits (no zombie); the window is not waited for here.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Like [`launch_with`], but hands the child to the caller (to reap it, or to see whether it still runs).
pub fn spawn_with(binary: &str, var: &str, value: &str) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe().ok();
    let path: PathBuf = crate::home_app::sibling(binary, exe.as_deref());
    command(path, var, value).spawn().map_err(|e| {
        crate::i18n::tr_args("home.launch_failed", &[("name", &binary), ("error", &e)])
    })
}

/// The command with only `var` set among the prefill variables: the others this process may have
/// been started with must not reach the new app.
fn command(path: PathBuf, var: &str, value: &str) -> Command {
    let mut cmd = Command::new(path);
    for other in [DIAL_VAR, MESSAGE_VAR, KEYPAD_VAR] {
        cmd.env_remove(other);
    }
    cmd.env(var, value);
    cmd
}

static DIAL_TAKEN: AtomicBool = AtomicBool::new(false);
static MESSAGE_TAKEN: AtomicBool = AtomicBool::new(false);
static KEYPAD_TAKEN: AtomicBool = AtomicBool::new(false);

/// Whether this window was started from the in-call card's Keypad button, once.
pub fn take_keypad() -> bool {
    take(KEYPAD_VAR, &KEYPAD_TAKEN).is_some()
}

/// The number the dialer was started with, once.
pub fn take_dial() -> Option<String> {
    take(DIAL_VAR, &DIAL_TAKEN)
}

/// The recipient a new message was started with, once.
pub fn take_message_to() -> Option<String> {
    take(MESSAGE_VAR, &MESSAGE_TAKEN)
}

/// The prefill this process was started with, once. Only digits, `+ ( ) - .` and spaces survive,
/// at most 32 characters, so nothing odd can reach the dial pad or the recipient field.
pub fn take(var: &str, flag: &AtomicBool) -> Option<String> {
    if flag.swap(true, Ordering::Relaxed) {
        return None;
    }
    let raw = std::env::var(var).ok()?;
    clean_number(&raw)
}

pub fn clean_number(raw: &str) -> Option<String> {
    // An e-mail gateway sender (`91234@txt.example.com`) or a name with digits is not a number: keeping
    // only its digits would prefill a plausible but wrong recipient.
    if raw.contains('@') || raw.chars().any(char::is_alphabetic) {
        return None;
    }
    let s: String = raw
        .chars()
        .filter(|c| c.is_ascii_digit() || "+()-. ".contains(*c))
        .take(32)
        .collect();
    let s = s.trim().to_owned();
    s.chars().any(|c| c.is_ascii_digit()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_child_gets_only_its_own_prefill_variable() {
        let cmd = command(PathBuf::from("x"), MESSAGE_VAR, "+155");
        let envs: std::collections::HashMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_owned())))
            .collect();
        assert_eq!(
            envs[MESSAGE_VAR].as_deref(),
            Some(std::ffi::OsStr::new("+155"))
        );
        assert_eq!(envs[DIAL_VAR], None, "removed, not inherited");
        assert_eq!(envs[KEYPAD_VAR], None, "removed, not inherited");
    }

    #[test]
    fn only_dialable_characters_survive() {
        assert_eq!(
            clean_number("+44 (20) 7946-0001").as_deref(),
            Some("+44 (20) 7946-0001")
        );
        let odd = clean_number("; $(1) / 555").unwrap();
        assert!(
            odd.chars()
                .all(|c| c.is_ascii_digit() || "+()-. ".contains(c))
        );
        assert_eq!(clean_number("abc"), None);
        assert_eq!(
            clean_number("91234@txt.example.com"),
            None,
            "e-mail gateway"
        );
        assert_eq!(clean_number("Bank12345"), None, "a name with digits");
        assert_eq!(clean_number("1".repeat(80).as_str()).unwrap().len(), 32);
    }
}
