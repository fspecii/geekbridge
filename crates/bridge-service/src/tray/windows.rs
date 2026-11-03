// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows tray: a notification-area icon through `Shell_NotifyIconW` on its own thread, with a
//! message-only window as the callback target and a popup menu drawn by `TrackPopupMenu`. The
//! thread blocks in `GetMessageW`; state changes arrive as a posted message, so nothing polls.
//!
//! Type-checked from Linux by `scripts/xcheck.sh`; first run on a real Windows machine is on the
//! release checklist (docs/release-checklist.md).
use super::{
    icon::{self, Style},
    model::{ItemKind, TrayAction, TrayInput, action_for, menu},
};
use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicIsize, Ordering},
        mpsc,
    },
    thread::JoinHandle,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos,
                GetMessageW, HICON, HMENU, ICONINFO, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG,
                PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
                SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
                TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP,
                WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
            },
        },
    },
    core::{PCWSTR, w},
};

type OnAction = Arc<dyn Fn(TrayAction) + Send + Sync>;

const CALLBACK: u32 = WM_APP + 1;
const REFRESH: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;

struct Shared {
    input: TrayInput,
    on_action: OnAction,
    icon: HICON,
    taskbar_created: u32,
}

// SAFETY: `HICON` is a plain handle owned by the tray thread, the only code that uses it.
unsafe impl Send for Shared {}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

fn shared() -> MutexGuard<'static, Option<Shared>> {
    SHARED.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Backend {
    hwnd: Arc<AtomicIsize>,
    thread: Option<JoinHandle<()>>,
}

pub async fn start(input: &TrayInput, on_action: OnAction) -> Option<Backend> {
    let input = input.clone();
    let hwnd = Arc::new(AtomicIsize::new(0));
    let (ready_tx, ready_rx) = mpsc::channel::<bool>();
    let published = hwnd.clone();
    let thread = std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || tray_thread(input, on_action, published, ready_tx))
        .ok()?;
    // The icon is up (or failed) within milliseconds; do not park the async runtime on it.
    let ok = tokio::task::spawn_blocking(move || ready_rx.recv().unwrap_or(false))
        .await
        .unwrap_or(false);
    if !ok {
        let _ = thread.join();
        return None;
    }
    Some(Backend {
        hwnd,
        thread: Some(thread),
    })
}

impl Backend {
    pub async fn update(&self, input: &TrayInput) {
        if let Some(s) = shared().as_mut() {
            s.input = input.clone();
        }
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        // SAFETY: posting to a window this backend created; failure (window gone) is harmless.
        let _ = unsafe { PostMessageW(Some(hwnd), REFRESH, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        // SAFETY: as in `update`.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn tray_thread(
    input: TrayInput,
    on_action: OnAction,
    published: Arc<AtomicIsize>,
    ready: mpsc::Sender<bool>,
) {
    // SAFETY: plain Win32 calls on this thread; every pointer passed is to a local that outlives
    // the call, and the handles created here are released before the thread ends.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            let _ = ready.send(false);
            return;
        };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: module.into(),
            lpszClassName: w!("GeekBridgeTray"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // A hidden top-level window, never shown. Not a message-only window (HWND_MESSAGE parent): those
        // receive no broadcast messages, so `TaskbarCreated` (explorer.exe restarted, icon must be added
        // again) would never arrive, and SetForegroundWindow, which the popup menu needs to close on an
        // outside click, does not work for them.
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("GeekBridgeTray"),
            w!("GeekBridge"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            let _ = ready.send(false);
            return;
        };
        let hicon = make_icon(&input);
        *shared() = Some(Shared {
            input: input.clone(),
            on_action,
            icon: hicon,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        });
        if !notify(NIM_ADD, hwnd, hicon, &input) {
            let _ = DestroyWindow(hwnd);
            *shared() = None;
            let _ = ready.send(false);
            return;
        }
        published.store(hwnd.0 as isize, Ordering::SeqCst);
        let _ = ready.send(true);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(s) = shared().take() {
            let _ = DestroyIcon(s.icon);
        }
    }
}

fn tip(input: &TrayInput) -> [u16; 128] {
    let (title, body) = input.tooltip();
    let text = format!("GeekBridge \u{2013} {title}: {body}");
    let mut out = [0_u16; 128];
    let mut at = 0;
    // Whole characters only: a cut must not end in half a surrogate pair.
    for c in text.chars() {
        let mut buf = [0_u16; 2];
        let units = c.encode_utf16(&mut buf);
        if at + units.len() > 127 {
            break;
        }
        out[at..at + units.len()].copy_from_slice(units);
        at += units.len();
    }
    out
}

/// `NIM_ADD` / `NIM_MODIFY` / `NIM_DELETE` for our one icon.
unsafe fn notify(
    op: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,
    hwnd: HWND,
    hicon: HICON,
    input: &TrayInput,
) -> bool {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: CALLBACK,
        hIcon: hicon,
        szTip: tip(input),
        ..Default::default()
    };
    // SAFETY: `data` is fully initialised and lives across the call.
    unsafe { Shell_NotifyIconW(op, &data).as_bool() }
}

/// A 32-bit colour icon from the same dot-matrix drawing the other platforms use.
fn make_icon(input: &TrayInput) -> HICON {
    const SIZE: usize = 32;
    let img = icon::render(input.icon_state(), SIZE, Style::Tile);
    // Windows wants BGRA, premultiplied.
    let bgra: Vec<u8> = img
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = u32::from(p[3]);
            let m = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            [m(p[2]), m(p[1]), m(p[0]), p[3]]
        })
        .collect();
    // SAFETY: `bgra` outlives `CreateBitmap`, which copies it; the bitmaps are deleted after the
    // icon (which owns its own copies) is made.
    unsafe {
        let color = CreateBitmap(SIZE as i32, SIZE as i32, 1, 32, Some(bgra.as_ptr().cast()));
        let mask = CreateBitmap(SIZE as i32, SIZE as i32, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_default();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: called by the system on the tray thread with valid arguments; the unsafe calls below
    // use handles created by this thread.
    unsafe {
        match message {
            CALLBACK => {
                match (lparam.0 & 0xffff) as u32 {
                    WM_LBUTTONUP => {
                        let cb = shared().as_ref().map(|s| s.on_action.clone());
                        if let Some(cb) = cb {
                            cb(TrayAction::Open);
                        }
                    }
                    WM_RBUTTONUP => popup(hwnd),
                    _ => {}
                }
                LRESULT(0)
            }
            REFRESH => {
                let mut guard = shared();
                if let Some(s) = guard.as_mut() {
                    let fresh = make_icon(&s.input);
                    notify(NIM_MODIFY, hwnd, fresh, &s.input);
                    let _ = DestroyIcon(s.icon);
                    s.icon = fresh;
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Some(s) = shared().as_ref() {
                    notify(NIM_DELETE, hwnd, s.icon, &s.input);
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if shared().as_ref().is_some_and(|s| s.taskbar_created == m) => {
                // Explorer restarted: the notification area is empty again.
                if let Some(s) = shared().as_ref() {
                    notify(NIM_ADD, hwnd, s.icon, &s.input);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

unsafe fn popup(hwnd: HWND) {
    let (input, on_action) = match shared().as_ref() {
        Some(s) => (s.input.clone(), s.on_action.clone()),
        None => return,
    };
    // SAFETY: Win32 menu calls on the tray thread; the menu is destroyed before returning.
    unsafe {
        let Ok(popup): Result<HMENU, _> = CreatePopupMenu() else {
            return;
        };
        for item in menu(&input).iter().filter(|i| i.visible) {
            match item.kind {
                ItemKind::Separator => {
                    let _ = AppendMenuW(popup, MF_SEPARATOR, 0, PCWSTR::null());
                }
                _ => {
                    let text: Vec<u16> = item.label.encode_utf16().chain(Some(0)).collect();
                    let flags = if item.enabled {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    };
                    let _ = AppendMenuW(popup, flags, item.id as usize, PCWSTR(text.as_ptr()));
                }
            }
        }
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu does not close when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(
            popup,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(popup);
        if chosen.0 > 0
            && let Some(action) = action_for(&input, chosen.0)
        {
            on_action(action);
        }
    }
}
