// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny 5x7 dot-matrix display drawn with plain circles. Original glyphs, no font files.
//!
//! Used sparingly by Messages for its status display and counters. Text is upper-cased;
//! characters without a glyph render as blanks.
use eframe::egui::{Color32, Painter, Pos2, Rect, Vec2, pos2, vec2};

pub const COLS: usize = 5;
pub const ROWS: usize = 7;

/// One glyph as seven rows of five bits (MSB = leftmost dot).
fn glyph(c: char) -> [u8; ROWS] {
    match c.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        ',' => [0, 0, 0, 0, 0b01100, 0b00100, 0b01000],
        ':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '+' => [0, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0],
        '/' => [
            0b00001, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b10000,
        ],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '#' => [
            0b01010, 0b01010, 0b11111, 0b01010, 0b11111, 0b01010, 0b01010,
        ],
        '*' => [0, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0],
        '…' => [0, 0, 0, 0, 0, 0, 0b10101],
        _ => [0; ROWS],
    }
}

/// Size of one text run. `pitch` is the distance between neighbouring dot centres.
pub fn measure(text: &str, pitch: f32) -> Vec2 {
    let n = text.chars().count() as f32;
    if n == 0.0 {
        return Vec2::ZERO;
    }
    // Each glyph is 5 columns wide with one blank column between glyphs.
    vec2(
        (n * (COLS as f32 + 1.0) - 1.0) * pitch,
        (ROWS as f32 - 1.0) * pitch,
    )
}

/// Paints `text` with its top-left dot centre at `origin`. Unlit dots (when `off` is
/// given) are drawn faintly so the display reads as a physical matrix.
pub fn paint(
    painter: &Painter,
    origin: Pos2,
    text: &str,
    pitch: f32,
    on: Color32,
    off: Option<Color32>,
) {
    let radius = pitch * 0.36;
    for (i, c) in text.chars().enumerate() {
        let rows = glyph(c);
        let x0 = origin.x + i as f32 * (COLS as f32 + 1.0) * pitch;
        for (r, bits) in rows.iter().enumerate() {
            for col in 0..COLS {
                let lit = bits & (1 << (COLS - 1 - col)) != 0;
                let center = pos2(x0 + col as f32 * pitch, origin.y + r as f32 * pitch);
                if lit {
                    painter.circle_filled(center, radius, on);
                } else if let Some(off) = off {
                    painter.circle_filled(center, radius * 0.7, off);
                }
            }
        }
    }
}

/// Largest candidate that fits in `max_width`, or the last (shortest) one.
pub fn fit<'a>(candidates: &[&'a str], pitch: f32, max_width: f32) -> &'a str {
    candidates
        .iter()
        .copied()
        .find(|t| measure(t, pitch).x <= max_width)
        .unwrap_or_else(|| candidates.last().copied().unwrap_or(""))
}

/// A horizontal row of dots, `lit` of `total` filled; used as a quiet progress meter.
pub fn meter(painter: &Painter, rect: Rect, total: usize, lit: usize, on: Color32, off: Color32) {
    if total == 0 {
        return;
    }
    let pitch = rect.width() / total as f32;
    for i in 0..total {
        let center = pos2(rect.left() + pitch * (i as f32 + 0.5), rect.center().y);
        let color = if i < lit { on } else { off };
        painter.circle_filled(center, (pitch * 0.30).min(rect.height() * 0.5), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_rows_use_five_columns_and_digits_are_lit() {
        for c in "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.:-+/?!#*,…".chars() {
            let g = glyph(c);
            assert!(
                g.iter().all(|row| *row < 1 << COLS),
                "{c} wider than 5 dots"
            );
            assert!(g.iter().any(|row| *row != 0), "{c} is blank");
        }
        assert_eq!(glyph(' '), [0; ROWS]);
        assert_eq!(glyph('a'), glyph('A'));
    }

    #[test]
    fn measure_and_fit() {
        assert_eq!(measure("", 3.0), Vec2::ZERO);
        assert_eq!(measure("AB", 3.0).x, (2.0 * 6.0 - 1.0) * 3.0);
        assert_eq!(
            fit(&["NO PHONE FOUND", "NO PHONE"], 3.0, 1000.0),
            "NO PHONE FOUND"
        );
        assert_eq!(fit(&["NO PHONE FOUND", "NO PHONE"], 3.0, 100.0), "NO PHONE");
        assert_eq!(fit(&["LONG TEXT", "SHORT"], 3.0, 1.0), "SHORT");
    }
}
