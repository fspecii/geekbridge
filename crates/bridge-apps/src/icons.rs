//! Small line icons drawn with the painter, so no icon font or image assets are needed.
use eframe::egui::{
    Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Compose,
    Sidebar,
    Chat,
    Phone,
    Person,
    Send,
    Backspace,
    Copy,
    Trash,
    ChevronUp,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Refresh,
    Clipboard,
    Calendar,
    Keypad,
    Clock,
    Folder,
    Home,
    Sliders,
}

/// Paints `icon` centred in `rect` (which should be roughly square).
pub fn paint(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) / 2.0; // half-size
    let w = (s * 0.17).clamp(1.2, 2.2);
    let stroke = Stroke::new(w, color);
    let p = |x: f32, y: f32| pos2(c.x + x * s, c.y + y * s);
    let line = |pts: &[Pos2]| {
        painter.add(Shape::line(pts.to_vec(), stroke));
    };
    let round = CornerRadius::same((s * 0.28).round().max(1.0) as u8);
    let outline = |r: Rect, cr: CornerRadius| {
        painter.rect_stroke(r, cr, stroke, StrokeKind::Inside);
    };
    match icon {
        Icon::Compose => {
            // Square with the top-right corner left open for the pencil.
            line(&[
                p(0.55, 0.0),
                p(0.55, 0.85),
                p(-0.85, 0.85),
                p(-0.85, -0.7),
                p(-0.1, -0.7),
            ]);
            line(&[p(0.9, -0.85), p(0.15, -0.1)]);
            line(&[p(0.15, -0.1), p(-0.05, 0.1), p(0.2, 0.0), p(0.9, -0.85)]);
        }
        Icon::Sidebar => {
            let r = Rect::from_center_size(c, vec2(s * 2.0, s * 1.6));
            outline(r, round);
            line(&[p(-0.35, -0.8), p(-0.35, 0.8)]);
        }
        Icon::Chat => {
            let r = Rect::from_center_size(c - vec2(0.0, s * 0.1), vec2(s * 1.9, s * 1.35));
            outline(r, CornerRadius::same((s * 0.5) as u8));
            line(&[p(-0.5, 0.55), p(-0.7, 0.95), p(-0.1, 0.6)]);
        }
        Icon::Phone => paint_handset(painter, c, s, color, 0.0),
        Icon::Person => {
            painter.circle_stroke(p(0.0, -0.4), s * 0.38, stroke);
            painter.add(Shape::line(
                arc(p(0.0, 1.0), s * 0.85, 200.0, 340.0),
                stroke,
            ));
        }
        Icon::Send => {
            line(&[p(0.0, 0.8), p(0.0, -0.75)]);
            line(&[p(-0.65, -0.1), p(0.0, -0.8), p(0.65, -0.1)]);
        }
        Icon::Backspace => {
            line(&[
                p(-0.9, 0.0),
                p(-0.45, -0.65),
                p(0.9, -0.65),
                p(0.9, 0.65),
                p(-0.45, 0.65),
                p(-0.9, 0.0),
            ]);
            line(&[p(-0.1, -0.25), p(0.45, 0.25)]);
            line(&[p(0.45, -0.25), p(-0.1, 0.25)]);
        }
        Icon::Copy => {
            outline(Rect::from_min_max(p(-0.2, -0.2), p(0.85, 0.9)), round);
            line(&[p(-0.2, -0.75), p(-0.7, -0.75), p(-0.7, 0.4), p(-0.2, 0.4)]);
            line(&[p(-0.2, -0.75), p(0.5, -0.75)]);
        }
        Icon::Trash => {
            line(&[p(-0.8, -0.55), p(0.8, -0.55)]);
            line(&[
                p(-0.25, -0.55),
                p(-0.25, -0.85),
                p(0.25, -0.85),
                p(0.25, -0.55),
            ]);
            line(&[p(-0.6, -0.55), p(-0.5, 0.9), p(0.5, 0.9), p(0.6, -0.55)]);
        }
        Icon::ChevronUp => line(&[p(-0.6, 0.3), p(0.0, -0.3), p(0.6, 0.3)]),
        Icon::ChevronDown => line(&[p(-0.6, -0.3), p(0.0, 0.3), p(0.6, -0.3)]),
        Icon::ChevronLeft => line(&[p(0.3, -0.6), p(-0.3, 0.0), p(0.3, 0.6)]),
        Icon::ChevronRight => line(&[p(-0.3, -0.6), p(0.3, 0.0), p(-0.3, 0.6)]),
        Icon::Refresh => {
            painter.add(Shape::line(arc(c, s * 0.75, -60.0, 230.0), stroke));
            line(&[p(0.35, -0.95), p(0.5, -0.45), p(0.0, -0.5)]);
        }
        Icon::Home => {
            line(&[
                p(-0.85, -0.05),
                p(0.0, -0.85),
                p(0.85, -0.05),
            ]);
            line(&[
                p(-0.6, 0.1),
                p(-0.6, 0.85),
                p(0.6, 0.85),
                p(0.6, 0.1),
            ]);
        }
        Icon::Sliders => {
            line(&[p(-0.85, -0.45), p(0.85, -0.45)]);
            line(&[p(-0.85, 0.45), p(0.85, 0.45)]);
            painter.circle_filled(p(0.3, -0.45), s * 0.26, color);
            painter.circle_filled(p(-0.3, 0.45), s * 0.26, color);
        }
        Icon::Folder => {
            line(&[
                p(-0.9, -0.6),
                p(-0.35, -0.6),
                p(-0.1, -0.3),
                p(0.9, -0.3),
                p(0.9, 0.75),
                p(-0.9, 0.75),
                p(-0.9, -0.6),
            ]);
        }
        Icon::Clipboard => {
            outline(Rect::from_min_max(p(-0.7, -0.75), p(0.7, 0.95)), round);
            painter.rect_filled(
                Rect::from_min_max(p(-0.3, -0.95), p(0.3, -0.55)),
                CornerRadius::same(2),
                color,
            );
            line(&[p(-0.35, 0.0), p(0.35, 0.0)]);
            line(&[p(-0.35, 0.4), p(0.2, 0.4)]);
        }
        Icon::Calendar => {
            outline(Rect::from_min_max(p(-0.85, -0.65), p(0.85, 0.85)), round);
            line(&[p(-0.85, -0.2), p(0.85, -0.2)]);
            line(&[p(-0.4, -0.9), p(-0.4, -0.45)]);
            line(&[p(0.4, -0.9), p(0.4, -0.45)]);
        }
        Icon::Keypad => {
            for row in 0..3 {
                for col in 0..3 {
                    painter.circle_filled(
                        p(-0.6 + col as f32 * 0.6, -0.6 + row as f32 * 0.6),
                        s * 0.14,
                        color,
                    );
                }
            }
        }
        Icon::Clock => {
            painter.circle_stroke(c, s * 0.9, stroke);
            line(&[p(0.0, -0.5), p(0.0, 0.0), p(0.4, 0.25)]);
        }
    }
}

fn arc(center: Pos2, radius: f32, from_deg: f32, to_deg: f32) -> Vec<Pos2> {
    let steps = 24;
    (0..=steps)
        .map(|i| {
            let a = (from_deg + (to_deg - from_deg) * i as f32 / steps as f32).to_radians();
            pos2(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

/// A telephone handset: a thick arc whose two ends carry the ear and mouth pieces, all
/// opening toward the upper right. `half` is the glyph's half-size; `tilt_deg` rotates it
/// about `center`.
pub fn paint_handset(painter: &Painter, center: Pos2, half: f32, color: Color32, tilt_deg: f32) {
    let (sin, cos) = tilt_deg.to_radians().sin_cos();
    let scale = 0.82;
    let at = |x: f32, y: f32| {
        let (x, y) = ((x + 0.12) * half * scale, (y - 0.12) * half * scale);
        pos2(center.x + x * cos - y * sin, center.y + x * sin + y * cos)
    };
    let (ox, oy, radius) = (0.55_f32, -0.55_f32, 1.25_f32);
    let point = |deg: f32| {
        (
            ox + radius * deg.to_radians().cos(),
            oy + radius * deg.to_radians().sin(),
        )
    };
    let body: Vec<Pos2> = (0..=24)
        .map(|i| {
            let (x, y) = point(86.0 + 98.0 * i as f32 / 24.0);
            at(x, y)
        })
        .collect();
    painter.add(Shape::line(body, Stroke::new(half * scale * 0.42, color)));
    for deg in [86.0_f32, 184.0] {
        let (x, y) = point(deg);
        // Unit vector from the arc point toward its centre.
        let (ux, uy) = ((ox - x) / radius, (oy - y) / radius);
        let tip = (x + ux * 0.62, y + uy * 0.62);
        painter.add(Shape::line_segment(
            [at(x, y), at(tip.0, tip.1)],
            Stroke::new(half * scale * 0.78, color),
        ));
    }
}
