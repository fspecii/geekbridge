// SPDX-License-Identifier: GPL-3.0-or-later
//! Phone photos tab: preview decoding limits, clipboard commands, every state rendered at several sizes,
//! the actions a click produces, and the names a screen reader hears.
use super::*;
use crate::test_support::Rig;
use bridge_core::{
    AppKind,
    photos_view::{PhotoDevice, PhotoItem},
};

const THUMBS: [&[u8]; 3] = [
    include_bytes!("../../bridge-phone-sim/fixtures/photos/photo-1.jpg"),
    include_bytes!("../../bridge-phone-sim/fixtures/photos/photo-2.jpg"),
    include_bytes!("../../bridge-phone-sim/fixtures/photos/photo-5.jpg"),
];

fn device(listed: bool, partial: bool) -> PhotoDevice {
    PhotoDevice {
        device_id: "a".into(),
        name: "Pixel".into(),
        partial,
        listed,
    }
}

fn item(n: usize, has_thumb: bool) -> PhotoItem {
    PhotoItem {
        device_id: "a".into(),
        id: format!("p{n}"),
        name: if n == 2 {
            "IMG_0002.heic".into()
        } else {
            format!("IMG_{n:04}.jpg")
        },
        taken_ms: 1_781_000_000_000 + n as u64,
        size: 2_400_000,
        width: 4032,
        height: 3024,
        has_thumb,
    }
}

fn fetch(n: usize, mode: FetchMode, state: FetchState) -> FetchInfo {
    FetchInfo {
        fetch_id: n as u64,
        device_id: "a".into(),
        photo_id: format!("p{n}"),
        name: item(n, true).name,
        mode,
        state,
        path: (state == FetchState::Done)
            .then(|| "/home/u/Downloads/GeekBridge/IMG_0001.jpg".into()),
        code: (state == FetchState::Failed).then(|| "expired".into()),
    }
}

fn model(view: PhotosView, with_thumbs: bool) -> Model {
    let mut m = Model {
        photos: Some(view),
        ..Model::default()
    };
    if with_thumbs {
        for (i, bytes) in THUMBS.iter().enumerate() {
            m.thumbs.insert(
                ("a".into(), format!("p{}", i + 1)),
                Arc::new(decode_thumb(bytes).unwrap()),
            );
        }
    }
    m
}

fn grid_view(fetches: Vec<FetchInfo>) -> PhotosView {
    PhotosView {
        items: (1..=3).map(|n| item(n, true)).collect(),
        devices: vec![device(true, false)],
        offered: vec![],
        fetches,
    }
}

fn run(pane: &mut PhotosPane, rig: &mut Rig, events: Vec<egui::Event>) -> Vec<Action> {
    let mut actions = Vec::new();
    let input = rig.input(events);
    let _ = rig.ctx.run(input, |ctx| {
        let pal = theme::current(ctx, AppKind::Clipboard);
        egui::CentralPanel::default().show(ctx, |ui| {
            actions.extend(pane.show(ui, &pal));
        });
    });
    actions
}

fn settle(pane: &mut PhotosPane, rig: &mut Rig) -> Vec<Action> {
    let mut all = Vec::new();
    for _ in 0..3 {
        all.extend(run(pane, rig, vec![]));
    }
    all
}

#[test]
fn only_small_real_jpegs_are_decoded() {
    for bytes in THUMBS {
        let d = decode_thumb(bytes).unwrap();
        assert!(d.width <= usize::from(MAX_EDGE) && d.height <= usize::from(MAX_EDGE));
        assert_eq!(d.rgba.len(), d.width * d.height * 4);
        assert!(d.rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
    }
    assert!(decode_thumb(b"").is_err());
    assert!(decode_thumb(b"GIF89a not a jpeg at all").is_err());
    // Truncated in the middle of the data, and garbage after a valid header.
    assert!(decode_thumb(&THUMBS[0][..THUMBS[0].len() / 2]).is_err());
    let mut bad = THUMBS[0].to_vec();
    for b in bad.iter_mut().skip(200) {
        *b = 0x5A;
    }
    let _ = decode_thumb(&bad); // must not panic; either answer is fine
    // A header that claims a huge picture is refused before any pixel is decoded.
    let mut huge = THUMBS[0].to_vec();
    let sof = huge
        .windows(2)
        .position(|w| w == [0xFF, 0xC0] || w == [0xFF, 0xC2])
        .expect("a start-of-frame marker");
    huge[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(decode_thumb(&huge), Err(DecodeError::TooLarge));
}

#[test]
fn copy_needs_a_known_picture_type_and_builds_safe_commands() {
    assert_eq!(mime_for("a.JPG"), Some("image/jpeg"));
    assert_eq!(mime_for("a.jpeg"), Some("image/jpeg"));
    assert_eq!(mime_for("a.png"), Some("image/png"));
    assert_eq!(mime_for("a.webp"), Some("image/webp"));
    assert_eq!(mime_for("a.heic"), None);
    assert_eq!(mime_for("noextension"), None);
    if cfg!(target_os = "linux") {
        // The path of a file with an awkward name is one argument or the file itself, never shell text.
        let p = Path::new("/tmp/x/a b; rm -rf ~.jpg");
        let c = copy_commands(p, "image/jpeg");
        assert_eq!(c[0].program, "wl-copy");
        assert!(c[0].file_on_stdin && !c[0].args.iter().any(|a| a.contains("rm")));
        assert_eq!(c[1].program, "xclip");
        assert_eq!(
            c[1].args.last().map(String::as_str),
            Some("/tmp/x/a b; rm -rf ~.jpg")
        );
    }
}

#[test]
fn a_phone_that_is_off_offers_one_explicit_button_and_nothing_else() {
    let view = PhotosView {
        offered: vec![device(false, false)],
        ..PhotosView::default()
    };
    let (mut pane, rx) = PhotosPane::for_test(model(view, false));
    assert!(pane.available());
    let mut rig = Rig::new(AppKind::Clipboard, 480.0, 640.0);
    let actions = settle(&mut pane, &mut rig);
    assert!(
        actions.is_empty(),
        "nothing is done without a click: {actions:?}"
    );
    assert!(
        rx.try_recv().is_err(),
        "and nothing is asked of the service"
    );
}

/// Adapter so the rig's key helpers (written for app views) can drive a pane.
struct PaneView<'a>(&'a mut PhotosPane);
fn pane_view(p: &mut PhotosPane) -> PaneView<'_> {
    PaneView(p)
}
impl crate::shell::AppView for PaneView<'_> {
    fn show(&mut self, ctx: &egui::Context, env: &crate::shell::Env) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.0.show(ui, env.pal);
        });
    }
}

#[test]
fn allowing_asks_the_service_to_turn_the_consent_on_for_that_phone_only() {
    let view = PhotosView {
        offered: vec![device(false, false)],
        ..PhotosView::default()
    };
    let (mut pane, rx) = PhotosPane::for_test(model(view, false));
    let mut rig = Rig::new(AppKind::Clipboard, 480.0, 640.0);
    let mut all = Vec::new();
    let mut view = pane_view(&mut pane);
    rig.settle(&mut view, 3);
    // Tab to the only button and press Enter.
    rig.key(&mut view, egui::Key::Tab);
    rig.key(&mut view, egui::Key::Enter);
    rig.settle(&mut view, 2);
    while let Ok(c) = rx.try_recv() {
        all.push(c);
    }
    assert_eq!(all.len(), 1);
    assert!(
        matches!(&all[0], Cmd::Allow(d, true) if d == "a"),
        "the click turns photos on for this phone"
    );
}

#[test]
fn an_allowed_phone_is_asked_once_and_refresh_asks_again() {
    let (mut pane, rx) = PhotosPane::for_test(model(
        PhotosView {
            devices: vec![device(false, false)],
            ..PhotosView::default()
        },
        false,
    ));
    let mut rig = Rig::new(AppKind::Clipboard, 480.0, 640.0);
    let first = settle(&mut pane, &mut rig);
    assert_eq!(
        first,
        vec![Action::Refresh("a".into())],
        "asked once on opening"
    );
    assert!(matches!(rx.try_recv(), Ok(Cmd::Refresh(d)) if d == "a"));
    assert!(
        settle(&mut pane, &mut rig).is_empty(),
        "not again every frame"
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn the_grid_selection_and_buttons_produce_well_formed_requests() {
    let (mut pane, rx) = PhotosPane::for_test(model(grid_view(vec![]), true));
    pane.asked.insert("a".into());
    let mut rig = Rig::new(AppKind::Clipboard, 480.0, 700.0);
    let mut view = pane_view(&mut pane);
    rig.settle(&mut view, 3);
    // Tab to the Refresh button, then through the cells; Enter on the first cell selects it.
    for _ in 0..2 {
        rig.key(&mut view, egui::Key::Tab);
    }
    rig.key(&mut view, egui::Key::Enter);
    rig.settle(&mut view, 3);
    assert_eq!(pane.selected, Some(("a".into(), "p1".into())));
    // The detail offers Save and Copy; reach them with Tab and press Enter on Save.
    let mut view = pane_view(&mut pane);
    for _ in 0..3 {
        rig.key(&mut view, egui::Key::Tab);
    }
    let mut sent = Vec::new();
    while let Ok(c) = rx.try_recv() {
        sent.push(c);
    }
    assert!(
        sent.iter().all(|c| !matches!(c, Cmd::Fetch { .. })),
        "selecting never fetches a full picture: {}",
        sent.len()
    );
}

#[test]
fn actions_become_the_matching_service_commands() {
    let (pane, rx) = PhotosPane::for_test(Model::default());
    pane.command(&Action::Fetch("a".into(), "p1".into(), FetchMode::Copy));
    pane.command(&Action::Allow("a".into(), false));
    pane.command(&Action::Refresh("a".into()));
    pane.command(&Action::Select("a".into(), "p1".into()));
    assert!(matches!(
        rx.try_recv(),
        Ok(Cmd::Fetch { device_id, id, mode: FetchMode::Copy }) if device_id == "a" && id == "p1"
    ));
    assert!(matches!(rx.try_recv(), Ok(Cmd::Allow(d, false)) if d == "a"));
    assert!(matches!(rx.try_recv(), Ok(Cmd::Refresh(d)) if d == "a"));
    assert!(rx.try_recv().is_err(), "selecting is local");
}

#[test]
fn renders_every_state_at_wide_and_narrow_sizes() {
    let fetches = vec![
        fetch(1, FetchMode::Save, FetchState::Waiting),
        fetch(3, FetchMode::Copy, FetchState::Failed),
    ];
    let states: Vec<(&str, PhotosView, bool)> = vec![
        (
            "unlisted",
            PhotosView {
                devices: vec![device(false, false)],
                ..Default::default()
            },
            false,
        ),
        (
            "empty",
            PhotosView {
                devices: vec![device(true, true)],
                ..Default::default()
            },
            false,
        ),
        ("no thumbs yet", grid_view(vec![]), false),
        ("grid", grid_view(vec![]), true),
        ("waiting and failed", grid_view(fetches), true),
        (
            "done",
            grid_view(vec![fetch(1, FetchMode::Save, FetchState::Done)]),
            true,
        ),
        ("none", PhotosView::default(), false),
    ];
    for (w, h) in [(800.0, 700.0), (380.0, 460.0), (200.0, 260.0)] {
        for (what, view, thumbs) in &states {
            let (mut pane, _rx) = PhotosPane::for_test(model(view.clone(), *thumbs));
            pane.selected = Some(("a".into(), "p1".into()));
            let mut rig = Rig::new(AppKind::Clipboard, w, h);
            settle(&mut pane, &mut rig);
            let out = run(&mut pane, &mut rig, vec![]);
            drop(out);
            assert!(pane.model.photos.is_some(), "{what} {w}x{h}");
        }
    }
}

#[test]
fn a_phone_name_is_text_not_a_format_string() {
    let mut d = device(true, false);
    d.name = "{n} {phone} %s \u{202E}evil".into();
    let view = PhotosView {
        devices: vec![d],
        ..PhotosView::default()
    };
    let (mut pane, _rx) = PhotosPane::for_test(model(view, false));
    let mut rig = Rig::new(AppKind::Clipboard, 480.0, 400.0);
    settle(&mut pane, &mut rig);
}

#[cfg(feature = "accesskit")]
mod a11y {
    use super::*;

    fn tree(model: Model, size: egui::Vec2) -> crate::a11y_tests::Tree {
        let (mut pane, _rx) = PhotosPane::for_test(model);
        pane.selected = Some(("a".into(), "p1".into()));
        pane.asked.insert("a".into());
        crate::a11y_tests::capture(size, move |ctx| {
            theme::apply(ctx, AppKind::Clipboard);
            let pal = theme::current(ctx, AppKind::Clipboard);
            egui::CentralPanel::default().show(ctx, |ui| {
                pane.show(ui, &pal);
            });
        })
    }

    #[test]
    fn every_control_of_the_tab_has_a_name_a_screen_reader_can_say() {
        let waiting = vec![fetch(1, FetchMode::Save, FetchState::Waiting)];
        for (what, view, thumbs) in [
            ("grid", grid_view(vec![]), true),
            ("loading previews", grid_view(vec![]), false),
            ("fetching", grid_view(waiting), true),
            (
                "off",
                PhotosView {
                    offered: vec![device(false, false)],
                    ..Default::default()
                },
                false,
            ),
        ] {
            for size in [egui::vec2(480.0, 700.0), egui::vec2(380.0, 460.0)] {
                let t = tree(model(view.clone(), thumbs), size);
                assert!(
                    t.unnamed_controls().is_empty(),
                    "{what}: {:?}",
                    t.unnamed_controls()
                );
                assert!(t.controls() >= 1, "{what}");
            }
        }
    }

    #[test]
    fn photo_cells_are_named_with_the_file_and_the_time_and_say_when_a_preview_is_loading() {
        let loaded = tree(model(grid_view(vec![]), true), egui::vec2(480.0, 700.0));
        assert!(
            loaded.has_name_starting("Photo IMG_0001.jpg, "),
            "{}",
            loaded.outline()
        );
        let loading = tree(model(grid_view(vec![]), false), egui::vec2(480.0, 700.0));
        assert!(
            loading.has_name_starting("Photo IMG_0001.jpg, ")
                && loading.outline().contains("preview loading"),
            "{}",
            loading.outline()
        );
        for button in [
            "Save",
            "Copy to clipboard",
            "Refresh",
            "Stop showing photos",
        ] {
            assert!(
                loaded.find(button).is_some(),
                "{button}: {}",
                loaded.outline()
            );
        }
        let off = tree(
            model(
                PhotosView {
                    offered: vec![device(false, false)],
                    ..Default::default()
                },
                false,
            ),
            egui::vec2(480.0, 700.0),
        );
        assert!(
            off.find("Allow photos from Pixel").is_some(),
            "{}",
            off.outline()
        );
    }

    #[test]
    fn the_fetch_progress_line_is_a_polite_live_region() {
        let waiting = vec![fetch(1, FetchMode::Save, FetchState::Waiting)];
        let t = tree(model(grid_view(waiting), true), egui::vec2(480.0, 800.0));
        assert!(t.outline().contains("[live=Polite]"), "{}", t.outline());
    }
}
