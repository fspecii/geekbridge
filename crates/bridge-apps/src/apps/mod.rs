// SPDX-License-Identifier: GPL-3.0-or-later
//! The four independent app bodies. Nothing here switches between apps.
mod calendar;
mod calls;
pub(crate) mod clipboard;
mod contacts;
mod messages;

use crate::shell::AppView;
use bridge_core::AppKind;

pub fn view_for(app: AppKind) -> Box<dyn AppView> {
    match app {
        AppKind::Calls => Box::<calls::Calls>::default(),
        AppKind::Messages => Box::<messages::Messages>::default(),
        AppKind::Agenda => Box::<calendar::Calendar>::default(),
        AppKind::Contacts => Box::<contacts::Contacts>::default(),
        AppKind::Clipboard => Box::<clipboard::Clipboard>::default(),
    }
}
