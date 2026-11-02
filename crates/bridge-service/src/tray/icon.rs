//! Tray icons drawn from code: a 5x7 dot-matrix "G" (the letter of `assets/icon.svg`) on an 8x8
//! dot grid, with a 3x3 state overlay in the bottom-right corner. Drawing them here means the
//! tray needs no PNG decoder and no embedded image files, and the same tables write the committed
//! SVG/PNG set (`scripts/gen-tray-icons.sh`, the `tray-icons` example).
use super::model::IconState;

pub const GRID: usize = 8;

/// The letter G: bit 4 is the leftmost of five columns.
const GLYPH: [u8; 7] = [
    0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
];

/// Overlay rows (bit 2 is the left column) for the state, `None` for no overlay.
fn overlay(state: IconState) -> [u8; 3] {
    match state {
        IconState::Connected => [0b001, 0b101, 0b010], // check mark
        IconState::Disconnected => [0b000, 0b111, 0b000], // dash
        IconState::Ringing => [0b111, 0b101, 0b111],   // ring
        IconState::UpdateAvailable => [0b010, 0b111, 0b101], // arrow up
        IconState::Transfer => [0b101, 0b111, 0b010],  // arrow down
    }
}

/// Lit dots as `(column, row, alpha)` on the 8x8 grid. Alpha is 255, or less for the dimmed
/// letter of a disconnected icon.
pub fn dots(state: IconState) -> Vec<(usize, usize, u8)> {
    let letter_alpha = if state == IconState::Disconnected {
        110
    } else {
        255
    };
    let mut out = Vec::new();
    for (row, bits) in GLYPH.iter().enumerate() {
        for col in 0..5 {
            if bits >> (4 - col) & 1 == 1 {
                out.push((col, row, letter_alpha));
            }
        }
    }
    for (row, bits) in overlay(state).iter().enumerate() {
        for col in 0..3 {
            if bits >> (2 - col) & 1 == 1 {
                out.push((5 + col, 5 + row, 255));
            }
        }
    }
    out
}

/// How the dots are coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Light dots on transparent: dark bars (waybar, GNOME top bar).
    Light,
    /// Dark dots on transparent: light bars and the macOS menu bar (a template image).
    Dark,
    /// White dots on the green tile of `assets/icon.svg`: Windows, where the tray is either shade.
    Tile,
}

pub struct Icon {
    pub size: usize,
    /// Straight (non-premultiplied) RGBA, row-major.
    pub rgba: Vec<u8>,
}

impl Icon {
    /// ARGB32 in network byte order, which is what a StatusNotifierItem `IconPixmap` carries.
    pub fn argb_be(&self) -> Vec<u8> {
        self.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[3], p[0], p[1], p[2]])
            .collect()
    }
}

impl Icon {
    /// A valid PNG (8-bit RGBA, stored deflate blocks, so no compressor is needed). A few KB per
    /// icon; used by the macOS status item and by the headless verification.
    pub fn to_png(&self) -> Vec<u8> {
        let row = self.size * 4;
        let mut raw = Vec::with_capacity((row + 1) * self.size);
        for line in self.rgba.chunks_exact(row) {
            raw.push(0); // filter: none
            raw.extend_from_slice(line);
        }
        let mut z = vec![0x78, 0x01];
        let mut blocks = raw.chunks(65_535).peekable();
        while let Some(block) = blocks.next() {
            z.push(u8::from(blocks.peek().is_none()));
            z.extend_from_slice(&(block.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            z.extend_from_slice(block);
        }
        z.extend_from_slice(&adler32(&raw).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(self.size as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.size as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1_u32, 0_u32);
    for &x in bytes {
        a = (a + u32::from(x)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

const GREEN: [u8; 3] = [0x05, 0x96, 0x69];

/// Draw `state` at `size` x `size` pixels.
pub fn render(state: IconState, size: usize, style: Style) -> Icon {
    let cell = size as f32 / GRID as f32;
    let radius = (cell * 0.40).max(0.9);
    let (fg, tile): ([u8; 3], bool) = match style {
        Style::Light => ([0xf2, 0xf2, 0xf2], false),
        Style::Dark => ([0x10, 0x10, 0x10], false),
        Style::Tile => ([0xff, 0xff, 0xff], true),
    };
    let lit = dots(state);
    let corner = size as f32 * 14.0 / 64.0;
    let mut rgba = vec![0_u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut a = 0.0_f32;
            for &(c, r, alpha) in &lit {
                let (cx, cy) = ((c as f32 + 0.5) * cell, (r as f32 + 0.5) * cell);
                let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
                a = a.max((radius + 0.5 - d).clamp(0.0, 1.0) * f32::from(alpha) / 255.0);
            }
            let o = (y * size + x) * 4;
            let tile_a = if tile {
                rounded_cover(px, py, size as f32, corner)
            } else {
                0.0
            };
            // Dots over the tile (or over nothing).
            let out_a = a + tile_a * (1.0 - a);
            if out_a <= 0.0 {
                continue;
            }
            for k in 0..3 {
                let c = (f32::from(fg[k]) * a + f32::from(GREEN[k]) * tile_a * (1.0 - a)) / out_a;
                rgba[o + k] = c.round() as u8;
            }
            rgba[o + 3] = (out_a * 255.0).round() as u8;
        }
    }
    Icon { size, rgba }
}

/// Coverage of a rounded square of side `s` with corner radius `r` at a pixel centre.
fn rounded_cover(x: f32, y: f32, s: f32, r: f32) -> f32 {
    let dx = (x - s / 2.0).abs() - (s / 2.0 - r);
    let dy = (y - s / 2.0).abs() - (s / 2.0 - r);
    let d = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r;
    (0.5 - d).clamp(0.0, 1.0)
}

/// The sizes a StatusNotifierItem offers; the host picks the nearest to its panel.
pub const SNI_SIZES: [usize; 3] = [22, 32, 48];

/// An SVG of the same drawing, for `assets/tray/` (rendered to PNG by `scripts/gen-tray-icons.sh`).
pub fn svg(state: IconState, style: Style) -> String {
    let (fg, tile) = match style {
        Style::Light => ("#f2f2f2", false),
        Style::Dark => ("#101010", false),
        Style::Tile => ("#ffffff", true),
    };
    let mut s = String::from("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 64 64\">\n");
    if tile {
        s.push_str("  <rect width=\"64\" height=\"64\" rx=\"14\" fill=\"#059669\"/>\n");
    }
    for (c, r, alpha) in dots(state) {
        let opacity = if alpha == 255 {
            String::new()
        } else {
            format!(" fill-opacity=\"{:.2}\"", f32::from(alpha) / 255.0)
        };
        s.push_str(&format!(
            "  <circle cx=\"{}\" cy=\"{}\" r=\"3.2\" fill=\"{fg}\"{opacity}/>\n",
            c * 8 + 4,
            r * 8 + 4
        ));
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_pixels(i: &Icon) -> usize {
        i.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .count()
    }

    #[test]
    fn every_state_is_distinct_at_every_size() {
        for size in SNI_SIZES {
            let drawn: Vec<_> = IconState::ALL
                .iter()
                .map(|s| render(*s, size, Style::Light).rgba)
                .collect();
            for i in 0..drawn.len() {
                for j in i + 1..drawn.len() {
                    assert_ne!(
                        drawn[i],
                        drawn[j],
                        "{:?} vs {:?} at {size}",
                        IconState::ALL[i],
                        IconState::ALL[j]
                    );
                }
            }
        }
    }

    #[test]
    fn dots_stay_on_the_grid_and_the_overlay_never_overlaps_the_letter() {
        for s in IconState::ALL {
            let d = dots(s);
            assert!(d.iter().all(|(c, r, _)| *c < GRID && *r < GRID));
            let mut cells: Vec<_> = d.iter().map(|(c, r, _)| (*c, *r)).collect();
            let n = cells.len();
            cells.sort();
            cells.dedup();
            assert_eq!(cells.len(), n, "{s:?} draws a dot twice");
        }
    }

    #[test]
    fn disconnected_letter_is_dimmer_than_connected() {
        let off = render(IconState::Disconnected, 32, Style::Light);
        let on = render(IconState::Connected, 32, Style::Light);
        let peak = |i: &Icon| {
            i.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[3])
                .max()
                .unwrap()
        };
        assert_eq!(peak(&on), 255);
        assert_eq!(peak(&off), 255, "the dash overlay is full strength");
        let total = |i: &Icon| {
            i.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from(p[3]))
                .sum::<u32>()
        };
        assert!(total(&off) < total(&on));
    }

    #[test]
    fn tile_style_fills_the_rounded_square_and_corners_stay_clear() {
        let t = render(IconState::Connected, 48, Style::Tile);
        let at = |x: usize, y: usize| &t.rgba[(y * 48 + x) * 4..][..4];
        assert_eq!(at(0, 0)[3], 0, "corner is transparent");
        assert_eq!(at(24, 47)[3], 255, "edge midpoint is the tile");
        assert_eq!(&at(24, 47)[..3], &GREEN);
        assert!(lit_pixels(&t) > lit_pixels(&render(IconState::Connected, 48, Style::Light)));
    }

    #[test]
    fn argb_is_network_order_and_sized() {
        let i = render(IconState::Ringing, 22, Style::Light);
        let b = i.argb_be();
        assert_eq!(b.len(), 22 * 22 * 4);
        let k = i
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .position(|p| p[3] == 255)
            .unwrap()
            * 4;
        assert_eq!(&b[k..k + 4], &[255, 0xf2, 0xf2, 0xf2]);
    }

    #[test]
    fn png_has_the_signature_header_and_checksums_of_a_real_png() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        let png = render(IconState::Transfer, 48, Style::Tile).to_png();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 48);
        // Every chunk's CRC matches its content.
        let mut at = 8;
        let mut kinds = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let body = &png[at + 4..at + 8 + len];
            let crc = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            assert_eq!(crc32(body), crc);
            kinds.push(String::from_utf8_lossy(&body[..4]).into_owned());
            at += 12 + len;
        }
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    }

    #[test]
    fn svg_matches_the_dot_tables() {
        for s in IconState::ALL {
            let svg = svg(s, Style::Tile);
            assert_eq!(svg.matches("<circle").count(), dots(s).len());
            assert!(svg.contains("#059669"));
        }
    }
}
