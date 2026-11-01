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

/// Starts a sibling app with one prefill variable set. Returns a short error for the window.
pub fn launch_with(binary: &str, var: &str, value: &str) -> Result<(), String> {
    let exe = std::env::current_exe().ok();
    let path: PathBuf = crate::home_app::sibling(binary, exe.as_deref());
    Command::new(path)
        .env(var, value)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not start {binary}: {e}"))
}

static DIAL_TAKEN: AtomicBool = AtomicBool::new(false);
static MESSAGE_TAKEN: AtomicBool = AtomicBool::new(false);

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
    fn only_dialable_characters_survive() {
        assert_eq!(
            clean_number("+44 (20) 7946-0001").as_deref(),
            Some("+44 (20) 7946-0001")
        );
        let odd = clean_number("; rm -rf / 555").unwrap();
        assert!(
            odd.chars()
                .all(|c| c.is_ascii_digit() || "+()-. ".contains(c))
        );
        assert_eq!(clean_number("abc"), None);
        assert_eq!(clean_number("1".repeat(80).as_str()).unwrap().len(), 32);
    }
}
