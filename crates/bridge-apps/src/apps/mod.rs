//! The four independent app bodies. Nothing here switches between apps.
mod agenda;
mod calls;
pub(crate) mod clipboard;
mod messages;

use crate::shell::AppView;
use bridge_core::AppKind;

pub fn view_for(app: AppKind) -> Box<dyn AppView> {
    match app {
        AppKind::Calls => Box::<calls::Calls>::default(),
        AppKind::Messages => Box::<messages::Messages>::default(),
        AppKind::Agenda => Box::<agenda::Agenda>::default(),
        AppKind::Clipboard => Box::<clipboard::Clipboard>::default(),
    }
}
