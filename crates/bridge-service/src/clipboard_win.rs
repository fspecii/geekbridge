// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows clipboard through the Win32 API: no PowerShell process per poll, per read or per write.
//!
//! * Change detection is `GetClipboardSequenceNumber`, a counter read that needs no clipboard
//!   access; the text is read only after it moved.
//! * Text is `CF_UNICODETEXT` (UTF-16), so accents, CJK and emoji survive in both directions.
//! * Sensitivity: the registered formats on the clipboard are listed and checked against the markers
//!   of [`crate::clipboard_marks`].
//!
//! Type-checked from Linux (`scripts/xcheck.sh`); it has not run on a Windows machine, see
//! docs/reports/windows-macos-clipboard.md for what that leaves open.
use crate::clipboard::BackendError;
use crate::clipboard_marks::{
    WinFormat, utf16_for_clipboard, utf16_from_clipboard, win_wants_value, windows_sensitivity,
};
use bridge_core::clipboard::Sensitivity;
use windows::Win32::{
    Foundation::{HANDLE, HGLOBAL},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData,
            GetClipboardFormatNameW, GetClipboardSequenceNumber, OpenClipboard, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalFree, GlobalLock, GlobalSize, GlobalUnlock},
    },
    UI::WindowsAndMessaging::GetDesktopWindow,
};

/// `CF_UNICODETEXT`.
const CF_UNICODETEXT: u32 = 13;
/// Registered (application-defined) formats start here.
const FIRST_REGISTERED: u32 = 0xC000;
/// Another program may hold the clipboard for a moment.
const OPEN_ATTEMPTS: usize = 6;

/// The clipboard's change counter. It moves on every change, including our own writes.
pub fn sequence() -> u32 {
    // SAFETY: a plain counter read with no arguments.
    unsafe { GetClipboardSequenceNumber() }
}

/// An open clipboard, closed again on drop.
struct Open;
impl Open {
    fn new() -> Result<Self, BackendError> {
        for attempt in 0..OPEN_ATTEMPTS {
            // The desktop window as owner: a NULL owner makes SetClipboardData fail after Empty.
            // SAFETY: GetDesktopWindow returns a valid handle that is never destroyed.
            if unsafe { OpenClipboard(Some(GetDesktopWindow())) }.is_ok() {
                return Ok(Open);
            }
            if attempt + 1 < OPEN_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
        Err(BackendError::Failed)
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

/// The global memory block behind a clipboard handle, copied out as bytes (at most `limit`+1).
fn global_bytes(h: HANDLE, limit: usize) -> Option<Vec<u8>> {
    let g = HGLOBAL(h.0);
    // SAFETY: `g` came from GetClipboardData while the clipboard is open; the lock is released
    // before returning and the copy is bounded by the block's size.
    unsafe {
        let size = GlobalSize(g);
        let p = GlobalLock(g) as *const u8;
        if p.is_null() {
            return None;
        }
        let n = size.min(limit.saturating_add(1));
        let bytes = std::slice::from_raw_parts(p, n).to_vec();
        let _ = GlobalUnlock(g);
        Some(bytes)
    }
}

/// The clipboard text, `None` when there is none.
pub fn read_text(max_bytes: usize) -> Result<Option<String>, BackendError> {
    let _open = Open::new()?;
    // SAFETY: the clipboard is open on this thread.
    let Ok(h) = (unsafe { GetClipboardData(CF_UNICODETEXT) }) else {
        return Ok(None);
    };
    // UTF-16 text of `max_bytes` UTF-8 bytes never needs more than 2 * max_bytes + 2 bytes.
    let limit = max_bytes.saturating_mul(2).saturating_add(2);
    let Some(bytes) = global_bytes(h, limit) else {
        return Err(BackendError::Failed);
    };
    if bytes.len() > limit {
        return Err(BackendError::TooLarge);
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let Some(text) = utf16_from_clipboard(&units) else {
        return Err(BackendError::NotUtf8);
    };
    if text.len() > max_bytes {
        return Err(BackendError::TooLarge);
    }
    Ok((!text.is_empty()).then_some(text))
}

/// Replace the clipboard with `text`.
pub fn write_text(text: &str) -> Result<(), BackendError> {
    let units = utf16_for_clipboard(text);
    let bytes = units.len() * 2;
    // SAFETY: a moveable block of `bytes` is filled completely before it is handed to the system,
    // which owns it after a successful SetClipboardData; on any failure it is freed here.
    unsafe {
        let g = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(|_| BackendError::Failed)?;
        let p = GlobalLock(g) as *mut u16;
        if p.is_null() {
            let _ = GlobalFree(Some(g));
            return Err(BackendError::Failed);
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        let _ = GlobalUnlock(g);
        let _open = match Open::new() {
            Ok(o) => o,
            Err(e) => {
                let _ = GlobalFree(Some(g));
                return Err(e);
            }
        };
        if EmptyClipboard().is_err() || SetClipboardData(CF_UNICODETEXT, Some(HANDLE(g.0))).is_err()
        {
            let _ = GlobalFree(Some(g));
            return Err(BackendError::Failed);
        }
    }
    Ok(())
}

/// The formats on the clipboard that matter for [`windows_sensitivity`], with flag values read.
pub fn sensitivity() -> Sensitivity {
    let Ok(_open) = Open::new() else {
        return Sensitivity::Unknown;
    };
    let mut formats = Vec::new();
    let mut f = 0u32;
    loop {
        // SAFETY: the clipboard is open on this thread.
        f = unsafe { EnumClipboardFormats(f) };
        if f == 0 {
            break;
        }
        if f < FIRST_REGISTERED {
            continue;
        }
        let mut buf = [0u16; 128];
        // SAFETY: `buf` is a valid writable buffer; the call reports how much it wrote.
        let n = unsafe { GetClipboardFormatNameW(f, &mut buf) };
        if n <= 0 {
            continue;
        }
        let name = String::from_utf16_lossy(&buf[..n as usize]);
        let dword = if win_wants_value(&name) {
            // SAFETY: clipboard open; the handle is only read.
            unsafe { GetClipboardData(f) }
                .ok()
                .and_then(|h| global_bytes(h, 4))
                .and_then(|b| {
                    b.get(..4)
                        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                })
        } else {
            None
        };
        formats.push(WinFormat {
            name: Some(name),
            dword,
        });
    }
    windows_sensitivity(&formats)
}
