// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings -> Calls: the short texts "Decline with message" offers on the incoming-call card and
//! banner. The service stores them (`call-replies` in its state folder); this editor shows what is
//! in effect, lets the person change up to three, and sends one request on Save. Nothing is sent to
//! anyone from here.
use crate::{
    call_feed::CallFeed,
    i18n::{tr, tr_args},
    nothing::S1,
    theme::{self, ButtonKind, Palette},
};
use bridge_core::{
    call_replies::{self, MAX_REPLIES, MAX_REPLY_CHARS},
    ipc::Request,
};
use eframe::egui;

#[derive(Default)]
pub struct ReplyEditor {
    drafts: [String; MAX_REPLIES],
    /// What the service last said was in effect; the drafts follow it until the person types.
    seen: Vec<String>,
    dirty: bool,
}

impl ReplyEditor {
    /// Follow the service's list while the person is not editing.
    pub fn sync(&mut self, in_effect: &[String]) {
        if self.dirty || self.seen == in_effect {
            return;
        }
        self.seen = in_effect.to_vec();
        for (i, d) in self.drafts.iter_mut().enumerate() {
            *d = in_effect.get(i).cloned().unwrap_or_default();
        }
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// The list the drafts would save (blanks dropped), or why not.
    pub fn problem(&self) -> Option<&'static str> {
        call_replies::clean(&self.drafts).err().map(|e| match e {
            call_replies::ReplyError::TooLong => "overlay.reply.edit.too_long",
            _ => "overlay.reply.edit.invalid",
        })
    }

    /// The save request; the drafts show what was saved from now on.
    pub fn save(&mut self) -> Option<Request> {
        let clean = call_replies::clean(&self.drafts).ok()?;
        self.dirty = false;
        let shown = call_replies::effective(&clean);
        for (i, d) in self.drafts.iter_mut().enumerate() {
            *d = shown.get(i).cloned().unwrap_or_default();
        }
        Some(Request::CallRepliesSet { texts: clean })
    }

    /// Back to the built-in texts.
    pub fn reset(&mut self) -> Request {
        self.dirty = false;
        let shown = call_replies::defaults();
        for (i, d) in self.drafts.iter_mut().enumerate() {
            *d = shown.get(i).cloned().unwrap_or_default();
        }
        Request::CallRepliesSet { texts: Vec::new() }
    }

    fn is_default(&self) -> bool {
        self.drafts
            .iter()
            .map(String::as_str)
            .eq(call_replies::DEFAULT_REPLIES)
    }
}

pub fn show(ui: &mut egui::Ui, pal: &Palette, editor: &mut ReplyEditor, feed: Option<&CallFeed>) {
    let Some(texts) = feed
        .map(|f| f.state())
        .and_then(|s| s.status)
        .map(|s| s.replies.texts)
        .filter(|t| !t.is_empty())
    else {
        ui.add(egui::Label::new(theme::caption(pal, tr("overlay.reply.edit.waiting"))).wrap());
        return;
    };
    editor.sync(&texts);
    ui.add(egui::Label::new(theme::caption(pal, tr("overlay.reply.edit.intro"))).wrap());
    ui.add_space(S1);
    for (i, draft) in editor.drafts.iter_mut().enumerate() {
        let name = tr_args("overlay.reply.edit.field", &[("n", &(i + 1))]);
        let r = ui.add(
            egui::TextEdit::singleline(draft)
                .char_limit(MAX_REPLY_CHARS)
                .desired_width(f32::INFINITY),
        );
        if r.changed() {
            editor.dirty = true;
        }
        crate::a11y::named(r, &name);
    }
    let problem = editor.problem();
    if let Some(key) = problem {
        ui.label(theme::caption(pal, tr(key)));
    }
    ui.add_space(S1);
    let mut request = None;
    ui.horizontal_wrapped(|ui| {
        if theme::pill_button(
            ui,
            pal,
            tr("overlay.reply.edit.save"),
            ButtonKind::Plain,
            editor.dirty && problem.is_none(),
        )
        .clicked()
        {
            request = editor.save();
        }
        if theme::pill_button(
            ui,
            pal,
            tr("overlay.reply.edit.reset"),
            ButtonKind::Plain,
            !editor.is_default() || editor.dirty,
        )
        .clicked()
        {
            request = Some(editor.reset());
        }
    });
    if let (Some(r), Some(feed)) = (request, feed) {
        feed.send(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_drafts_follow_the_service_until_the_person_types() {
        let mut e = ReplyEditor::default();
        e.sync(&call_replies::defaults());
        assert_eq!(e.drafts[1], "I'm in a meeting");
        e.drafts[1] = "Busy".into();
        e.dirty = true;
        e.sync(&v(&["x", "y", "z"]));
        assert_eq!(e.drafts[1], "Busy", "an edit is not overwritten by a poll");
    }

    #[test]
    fn save_sends_the_cleaned_list_and_keeps_showing_it() {
        let mut e = ReplyEditor::default();
        e.sync(&call_replies::defaults());
        e.drafts = ["  Driving ".into(), String::new(), "Later".into()];
        e.dirty = true;
        assert_eq!(e.problem(), None);
        let Some(Request::CallRepliesSet { texts }) = e.save() else {
            panic!("a save request");
        };
        assert_eq!(texts, v(&["Driving", "Later"]));
        assert!(!e.dirty());
        // The service's old answer arriving once more does not undo it.
        e.sync(&call_replies::defaults());
        assert_eq!(e.drafts[0], "Driving");
    }

    #[test]
    fn blank_drafts_save_as_the_defaults() {
        let mut e = ReplyEditor::default();
        e.sync(&v(&["A"]));
        e.drafts = Default::default();
        e.dirty = true;
        let Some(Request::CallRepliesSet { texts }) = e.save() else {
            panic!("a save request");
        };
        assert!(texts.is_empty());
        assert_eq!(e.drafts[2], "Call me in a few minutes");
    }

    #[test]
    fn a_text_that_cannot_be_saved_is_named_and_not_sent() {
        let mut e = ReplyEditor::default();
        e.drafts[0] = "a\nb".into();
        e.dirty = true;
        assert_eq!(e.problem(), Some("overlay.reply.edit.invalid"));
        assert!(e.save().is_none());
        e.drafts[0] = "x".repeat(MAX_REPLY_CHARS + 1);
        assert_eq!(e.problem(), Some("overlay.reply.edit.too_long"));
    }

    #[test]
    fn reset_asks_for_the_defaults() {
        let mut e = ReplyEditor::default();
        e.sync(&v(&["A"]));
        assert_eq!(e.reset(), Request::CallRepliesSet { texts: vec![] });
        assert!(e.is_default());
    }
}
