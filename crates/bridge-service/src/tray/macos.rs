// SPDX-License-Identifier: GPL-3.0-or-later
//! macOS menu-bar item: an `NSStatusItem` with an `NSMenu`, the icon a template image (the system
//! tints it for light and dark menu bars).
//!
//! **Type-checked only** (`scripts/xcheck.sh`); it has not run on a Mac. AppKit wants the status
//! item created and changed on the main thread inside a running `NSApplication` run loop, and
//! `geekbridge-service` keeps its main thread in the tokio runtime, so [`start`] returns `None`
//! from the tray task (not the main thread) and the macOS service shows no icon yet. The code is
//! complete for the day the service (or a small menu-bar helper) owns the main run loop; see
//! docs/platform-status.md.
use super::{
    icon::{self, Style},
    model::{ItemKind, TrayAction, TrayInput, action_for, menu},
};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{
    NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSData, NSObject, NSSize, NSString};
use std::sync::{Arc, Mutex};

type OnAction = Arc<dyn Fn(TrayAction) + Send + Sync>;

struct Ivars {
    on_action: OnAction,
    input: Mutex<TrayInput>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class has no `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "GeekBridgeTrayTarget"]
    #[ivars = Ivars]
    struct Target;

    impl Target {
        /// Every menu item's action; the item's tag is its stable menu id.
        #[unsafe(method(fire:))]
        fn fire(&self, sender: &NSMenuItem) {
            let ivars = self.ivars();
            let input = ivars.input.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(action) = action_for(&input, sender.tag() as i32) {
                (ivars.on_action)(action);
            }
        }
    }
);

/// Main-thread-only: holds AppKit objects.
pub struct MenuBar {
    item: Retained<NSStatusItem>,
    target: Retained<Target>,
    mtm: MainThreadMarker,
}

impl MenuBar {
    pub fn new(mtm: MainThreadMarker, input: &TrayInput, on_action: OnAction) -> Self {
        let target = mtm.alloc::<Target>().set_ivars(Ivars {
            on_action,
            input: Mutex::new(input.clone()),
        });
        // SAFETY: `init` on a freshly allocated NSObject subclass.
        let target: Retained<Target> = unsafe { msg_send![super(target), init] };
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let bar = Self { item, target, mtm };
        bar.show(input);
        bar
    }

    pub fn show(&self, input: &TrayInput) {
        *self
            .target
            .ivars()
            .input
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = input.clone();
        if let Some(button) = self.item.button(self.mtm) {
            let png = icon::render(input.icon_state(), 36, Style::Dark).to_png();
            let data = NSData::with_bytes(&png);
            if let Some(image) = NSImage::initWithData(self.mtm.alloc(), &data) {
                image.setSize(NSSize::new(18.0, 18.0));
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            let (title, body) = input.tooltip();
            button.setToolTip(Some(&NSString::from_str(&format!("{title}: {body}"))));
        }
        let menu_ = NSMenu::new(self.mtm);
        menu_.setAutoenablesItems(false);
        for entry in menu(input) {
            let item = match entry.kind {
                ItemKind::Separator => NSMenuItem::separatorItem(self.mtm),
                _ => {
                    // SAFETY: `fire:` is defined on `Target` with the signature AppKit expects.
                    let item = unsafe {
                        NSMenuItem::initWithTitle_action_keyEquivalent(
                            self.mtm.alloc(),
                            &NSString::from_str(&entry.label),
                            Some(sel!(fire:)),
                            &NSString::from_str(""),
                        )
                    };
                    // SAFETY: the target outlives the menu (both are owned by `self`).
                    unsafe { item.setTarget(Some(&self.target)) };
                    item.setEnabled(entry.enabled);
                    item
                }
            };
            item.setTag(entry.id as isize);
            item.setHidden(!entry.visible);
            menu_.addItem(&item);
        }
        self.item.setMenu(Some(&menu_));
    }
}

// SAFETY: a `Backend` is only ever created by `start` after proving it runs on the main thread,
// and `update` refuses to touch AppKit from any other thread.
pub struct Backend(Mutex<Option<MenuBar>>);
unsafe impl Send for Backend {}
unsafe impl Sync for Backend {}

pub async fn start(input: &TrayInput, on_action: OnAction) -> Option<Backend> {
    let mtm = MainThreadMarker::new()?;
    Some(Backend(Mutex::new(Some(MenuBar::new(
        mtm, input, on_action,
    )))))
}

impl Backend {
    pub async fn update(&self, input: &TrayInput) {
        if MainThreadMarker::new().is_some()
            && let Some(bar) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
        {
            bar.show(input);
        }
    }
}
