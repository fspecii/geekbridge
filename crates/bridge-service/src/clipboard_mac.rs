// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS pasteboard facts that need no process: the change counter (`NSPasteboard.changeCount`, a
//! counter read) and the list of types on the pasteboard, which carries the concealment markers of
//! [`crate::clipboard_marks`]. Text itself is still read and written with `pbpaste`/`pbcopy`, but
//! only after the counter moved.
//!
//! Type-checked from Linux (`scripts/xcheck.sh`); it has not run on a Mac, see
//! docs/reports/windows-macos-clipboard.md.
use crate::clipboard_marks::macos_sensitivity;
use bridge_core::clipboard::Sensitivity;
use objc2_app_kit::NSPasteboard;

/// The general pasteboard's change counter; it moves on every change, including our own writes.
pub fn change_count() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}

/// Sensitivity from the pasteboard types of the current contents.
pub fn sensitivity() -> Sensitivity {
    let Some(types) = NSPasteboard::generalPasteboard().types() else {
        return Sensitivity::Unknown;
    };
    let names: Vec<String> = types.iter().map(|t| t.to_string()).collect();
    macos_sensitivity(&names)
}
