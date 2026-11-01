//! The "paste from phone" hotkey: parsing, validation and the strings other tools need.
//!
//! A global hotkey must include a modifier, so plain typing is never hijacked. The normalized form
//! (`Ctrl+Shift+V`) is what is stored and shown.
use std::fmt;

pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+V";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Windows / Super / Command key.
    pub meta: bool,
    /// One uppercase ASCII letter or digit, or `F1`..`F24`.
    pub key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyError {
    Empty,
    NoModifier,
    NoKey,
    UnknownKey,
    TooLong,
}
impl fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "enter a shortcut such as Ctrl+Shift+V",
            Self::NoModifier => "a shortcut needs at least one of Ctrl, Alt, Shift or Super",
            Self::NoKey => "a shortcut needs a key",
            Self::UnknownKey => "use a letter, a digit or F1-F24 as the key",
            Self::TooLong => "that shortcut is too long",
        })
    }
}
impl std::error::Error for HotkeyError {}

impl Hotkey {
    pub fn parse(input: &str) -> Result<Self, HotkeyError> {
        if input.len() > 48 {
            return Err(HotkeyError::TooLong);
        }
        let mut h = Self {
            ctrl: false,
            alt: false,
            shift: false,
            meta: false,
            key: String::new(),
        };
        if input.trim().is_empty() {
            return Err(HotkeyError::Empty);
        }
        for part in input.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "ctl" => h.ctrl = true,
                "alt" | "option" | "opt" => h.alt = true,
                "shift" => h.shift = true,
                "super" | "win" | "windows" | "meta" | "cmd" | "command" => h.meta = true,
                "" => return Err(HotkeyError::NoKey),
                other => {
                    if !h.key.is_empty() {
                        return Err(HotkeyError::UnknownKey);
                    }
                    h.key = normalize_key(other).ok_or(HotkeyError::UnknownKey)?;
                }
            }
        }
        if h.key.is_empty() {
            return Err(HotkeyError::NoKey);
        }
        if !(h.ctrl || h.alt || h.shift || h.meta) {
            return Err(HotkeyError::NoModifier);
        }
        Ok(h)
    }

    /// `Ctrl+Alt+Shift+Super+<key>`: modifiers in a fixed order.
    pub fn normalized(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.meta, "Super"),
        ] {
            if on {
                out.push_str(name);
                out.push('+');
            }
        }
        out.push_str(&self.key);
        out
    }

    /// Arguments of Hyprland's `bind` keyword: `MODS,key`.
    pub fn hyprland_spec(&self) -> String {
        let mut mods = Vec::new();
        if self.ctrl {
            mods.push("CTRL");
        }
        if self.alt {
            mods.push("ALT");
        }
        if self.shift {
            mods.push("SHIFT");
        }
        if self.meta {
            mods.push("SUPER");
        }
        format!("{},{}", mods.join(" "), self.key)
    }
}

fn normalize_key(k: &str) -> Option<String> {
    let mut chars = k.chars();
    let first = chars.next()?;
    if chars.next().is_none() && first.is_ascii_alphanumeric() {
        return Some(first.to_ascii_uppercase().to_string());
    }
    let n: u8 = k.strip_prefix('f')?.parse().ok()?;
    ((1..=24).contains(&n)).then(|| format!("F{n}"))
}

/// Normalize `input` or explain why it is not usable.
pub fn normalize(input: &str) -> Result<String, HotkeyError> {
    Hotkey::parse(input).map(|h| h.normalized())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_aliases_normalize() {
        assert_eq!(normalize(DEFAULT_HOTKEY).unwrap(), "Ctrl+Shift+V");
        assert_eq!(normalize("shift + control + v").unwrap(), "Ctrl+Shift+V");
        assert_eq!(normalize("cmd+alt+f12").unwrap(), "Alt+Super+F12");
        assert_eq!(normalize("ctrl+9").unwrap(), "Ctrl+9");
    }

    #[test]
    fn unusable_shortcuts_are_refused() {
        assert_eq!(normalize(""), Err(HotkeyError::Empty));
        assert_eq!(normalize("V"), Err(HotkeyError::NoModifier));
        assert_eq!(normalize("Ctrl+Shift"), Err(HotkeyError::NoKey));
        assert_eq!(normalize("Ctrl+"), Err(HotkeyError::NoKey));
        assert_eq!(normalize("Ctrl+V+B"), Err(HotkeyError::UnknownKey));
        assert_eq!(normalize("Ctrl+Enter"), Err(HotkeyError::UnknownKey));
        assert_eq!(normalize("Ctrl+F25"), Err(HotkeyError::UnknownKey));
        assert_eq!(normalize(&"a".repeat(60)), Err(HotkeyError::TooLong));
    }

    #[test]
    fn compositor_spec() {
        let h = Hotkey::parse("Ctrl+Shift+V").unwrap();
        assert_eq!(h.hyprland_spec(), "CTRL SHIFT,V");
    }
}
