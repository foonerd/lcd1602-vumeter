//! A picture of the glass as SVG, from a simulated controller's memory:
//! each cell's five by eight pixels, the controller's own font for text and
//! the glyph memory for the user glyphs, on a module of a chosen look.

use crate::font;
use crate::sim::Memory;

/// The look of the module: the colours of the glass and its pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    pub glass: (u8, u8, u8),
    pub on: (u8, u8, u8),
    pub off: (u8, u8, u8),
    pub bezel: (u8, u8, u8),
}

impl Theme {
    /// Light pixels on a blue glass: the common 1602 with a blue backlight.
    pub const BLUE: Theme = Theme {
        glass: (18, 56, 176),
        on: (232, 240, 255),
        off: (28, 70, 190),
        bezel: (16, 16, 20),
    };
    /// Dark pixels on a yellow-green glass.
    pub const GREEN: Theme = Theme {
        glass: (156, 194, 60),
        on: (20, 36, 8),
        off: (144, 182, 56),
        bezel: (16, 16, 20),
    };
    /// A character OLED: bright pixels on black.
    pub const OLED: Theme = Theme {
        glass: (8, 8, 8),
        on: (250, 230, 90),
        off: (18, 18, 14),
        bezel: (16, 16, 20),
    };

    /// An RGB-backlit module lit in `rgb`: dark pixels on the colour.
    pub fn rgb(rgb: (u8, u8, u8)) -> Theme {
        let dim = |v: u8| (v as u16 * 9 / 10) as u8;
        Theme {
            glass: rgb,
            on: (24, 24, 28),
            off: (dim(rgb.0), dim(rgb.1), dim(rgb.2)),
            bezel: (16, 16, 20),
        }
    }

    pub fn by_name(name: &str) -> Option<Theme> {
        match name {
            "blue" => Some(Theme::BLUE),
            "green" => Some(Theme::GREEN),
            "oled" => Some(Theme::OLED),
            _ => name
                .strip_prefix("rgb:")
                .and_then(|hex| {
                    let hex = hex.trim_start_matches('#');
                    (hex.len() == 6)
                        .then(|| u32::from_str_radix(hex, 16).ok())
                        .flatten()
                })
                .map(|v| Theme::rgb(((v >> 16) as u8, (v >> 8) as u8, v as u8))),
        }
    }
}

fn hex(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// Pixel size and gaps, in SVG units.
const PIXEL: u32 = 6;
const PIXEL_GAP: u32 = 1;
const CELL_GAP: u32 = 4;
const GLASS_MARGIN: u32 = 14;
const BEZEL: u32 = 10;

/// The rows of the glyph a cell shows: user glyph memory for codes 0 to
/// 7, the full block for 0xFF, the font otherwise.
fn cell_rows(memory: &Memory, code: u8) -> [u8; 8] {
    match code {
        0..=7 => memory.cgram[code as usize],
        0xFF => [0x1F; 8],
        _ => font::rows(code),
    }
}

/// The glass with `cols` cells a row, the memory's first cells, in `theme`.
pub fn svg(memory: &Memory, cols: usize, theme: Theme) -> String {
    let cell_w = 5 * PIXEL + 4 * PIXEL_GAP;
    let cell_h = 8 * PIXEL + 7 * PIXEL_GAP;
    let glass_w = cols as u32 * cell_w + (cols as u32 - 1) * CELL_GAP + 2 * GLASS_MARGIN;
    let glass_h = 2 * cell_h + CELL_GAP + 2 * GLASS_MARGIN;
    let width = glass_w + 2 * BEZEL;
    let height = glass_h + 2 * BEZEL;
    let mut out = String::with_capacity(32 * 1024);
    out.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n"
    ));
    out.push_str(&format!(
        "<rect width=\"{width}\" height=\"{height}\" rx=\"6\" fill=\"{}\"/>\n",
        hex(theme.bezel)
    ));
    out.push_str(&format!(
        "<rect x=\"{BEZEL}\" y=\"{BEZEL}\" width=\"{glass_w}\" height=\"{glass_h}\" rx=\"3\" fill=\"{}\"/>\n",
        hex(theme.glass)
    ));
    let on = hex(theme.on);
    let off = hex(theme.off);
    for row in 0..2usize {
        for col in 0..cols {
            let code = memory.ddram[row][col.min(memory.ddram[row].len() - 1)];
            let rows = if memory.display_on {
                cell_rows(memory, code)
            } else {
                [0; 8]
            };
            let x0 = BEZEL + GLASS_MARGIN + col as u32 * (cell_w + CELL_GAP);
            let y0 = BEZEL + GLASS_MARGIN + row as u32 * (cell_h + CELL_GAP);
            for (r, bits) in rows.iter().enumerate() {
                for c in 0..5u32 {
                    let lit = bits & (0x10 >> c) != 0;
                    let x = x0 + c * (PIXEL + PIXEL_GAP);
                    let y = y0 + r as u32 * (PIXEL + PIXEL_GAP);
                    out.push_str(&format!(
                        "<rect x=\"{x}\" y=\"{y}\" width=\"{PIXEL}\" height=\"{PIXEL}\" fill=\"{}\"/>\n",
                        if lit { &on } else { &off }
                    ));
                }
            }
        }
    }
    out.push_str("</svg>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_has_a_pixel_for_every_dot_of_the_glass() {
        let mut memory = Memory::default();
        memory.display_on = true;
        memory.ddram[0][..2].copy_from_slice(b"Hi");
        let picture = svg(&memory, 16, Theme::BLUE);
        assert!(picture.starts_with("<svg"));
        assert_eq!(
            picture.matches("<rect").count(),
            2 + 16 * 2 * 40,
            "bezel, glass, and 40 pixels a cell"
        );
        assert!(picture.contains(&hex(Theme::BLUE.on)));
        assert_eq!(
            Theme::by_name("rgb:ff0000").map(|t| t.glass),
            Some((255, 0, 0))
        );
        assert_eq!(Theme::by_name("rgb:xyz"), None);
        assert_eq!(Theme::by_name("oled"), Some(Theme::OLED));
    }
}
