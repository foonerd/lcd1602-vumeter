//! The user glyphs and the bar they draw. A controller has eight glyph
//! slots; five hold the bar style's cells, the rest a peak marker, play
//! and pause. Each glyph is eight rows of five bits, the top bit on the
//! left. A full cell in the block style is the controller's own block,
//! 0xFF.

/// How the bar is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    /// Solid fill, five steps per cell.
    #[default]
    Blocks,
    /// Thin lines, three per cell.
    Lines,
    /// Two broad segments per cell, like an LED chain.
    Segments,
    /// A single line at the level, nothing behind it.
    Needle,
}

impl Style {
    pub const ALL: [Style; 4] = [Style::Blocks, Style::Lines, Style::Segments, Style::Needle];

    pub fn by_name(name: &str) -> Option<Style> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    pub fn name(&self) -> &'static str {
        match self {
            Style::Blocks => "blocks",
            Style::Lines => "lines",
            Style::Segments => "segments",
            Style::Needle => "needle",
        }
    }

    /// Steps of the bar one cell holds.
    pub fn steps_per_cell(&self) -> u16 {
        match self {
            Style::Blocks | Style::Needle => 5,
            Style::Lines => 3,
            Style::Segments => 2,
        }
    }
}

/// The glyph codes with a fixed slot, whatever the style.
pub const PEAK: u8 = 5;
pub const PLAY: u8 = 6;
pub const PAUSE: u8 = 7;
pub const FULL: u8 = 0xFF;
pub const SPACE: u8 = b' ';

/// The large digits' glyphs, loaded in the bar glyphs' place while the
/// player stands still: a top bar, a bottom bar, both, a vertical stroke
/// and a colon dot, each four columns wide so neighbouring digits keep a
/// column between them.
pub const BIG_UP: u8 = 0;
pub const BIG_LO: u8 = 1;
pub const BIG_BOTH: u8 = 2;
pub const BIG_BAR: u8 = 3;
pub const BIG_DOT: u8 = 4;
/// Cells one large digit takes.
pub const BIG_DIGIT_CELLS: usize = 3;

pub fn big_digits() -> [[u8; 8]; 8] {
    let mut rows = [[0u8; 8]; 8];
    rows[BIG_UP as usize] = [0b11110, 0b11110, 0b11110, 0, 0, 0, 0, 0];
    rows[BIG_LO as usize] = [0, 0, 0, 0, 0, 0b11110, 0b11110, 0b11110];
    rows[BIG_BOTH as usize] = [0b11110, 0b11110, 0b11110, 0, 0, 0b11110, 0b11110, 0b11110];
    rows[BIG_BAR as usize] = [0b11110; 8];
    rows[BIG_DOT as usize] = [0, 0, 0b01100, 0b01100, 0b01100, 0, 0, 0];
    rows
}

/// `text` in large digits across two rows: a digit is three cells, a
/// colon or a space one; anything else is a blank digit's width.
pub fn big_text(text: &str) -> [Vec<u8>; 2] {
    use self::{BIG_BAR as B, BIG_BOTH as M, BIG_LO as L, BIG_UP as U, SPACE as S};
    let mut top = Vec::new();
    let mut bottom = Vec::new();
    for c in text.chars() {
        let (t, b): ([u8; 3], [u8; 3]) = match c {
            '0' => ([B, U, B], [B, L, B]),
            '1' => ([U, B, S], [S, B, S]),
            '2' => ([U, M, B], [B, L, L]),
            '3' => ([U, M, B], [L, L, B]),
            '4' => ([B, L, B], [S, S, B]),
            '5' => ([B, M, U], [L, L, B]),
            '6' => ([B, M, U], [B, L, B]),
            '7' => ([U, U, B], [S, S, B]),
            '8' => ([B, M, B], [B, L, B]),
            '9' => ([B, M, B], [S, S, B]),
            ':' => {
                top.push(BIG_DOT);
                bottom.push(BIG_DOT);
                continue;
            }
            ' ' => {
                top.push(S);
                bottom.push(S);
                continue;
            }
            _ => ([S, S, S], [S, S, S]),
        };
        top.extend(t);
        bottom.extend(b);
    }
    [top, bottom]
}

/// The eight glyphs of a style, and the cells it draws with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyphs {
    style: Style,
    rows: [[u8; 8]; 8],
}

impl Glyphs {
    pub fn new(style: Style) -> Glyphs {
        let mut rows = [[0u8; 8]; 8];
        match style {
            Style::Blocks => {
                for (k, g) in rows.iter_mut().enumerate().take(4) {
                    // k + 1 columns lit from the left.
                    *g = [(0x1F << (4 - k)) & 0x1F; 8];
                }
            }
            Style::Lines => {
                rows[0] = [0b10000; 8];
                rows[1] = [0b10100; 8];
                rows[2] = [0b10101; 8];
            }
            Style::Segments => {
                rows[0] = [0b11000; 8];
                rows[1] = [0b11011; 8];
            }
            Style::Needle => {
                for (k, g) in rows.iter_mut().enumerate().take(5) {
                    *g = [0b10000 >> k; 8];
                }
            }
        }
        rows[PEAK as usize] = match style {
            // Beside a needle the marker is dotted, so the two are told apart.
            Style::Needle => [0b00100, 0, 0b00100, 0, 0b00100, 0, 0b00100, 0],
            _ => [0b00100; 8],
        };
        rows[PLAY as usize] = [
            0b10000, 0b11000, 0b11100, 0b11110, 0b11100, 0b11000, 0b10000, 0b00000,
        ];
        rows[PAUSE as usize] = [
            0b11011, 0b11011, 0b11011, 0b11011, 0b11011, 0b11011, 0b11011, 0b00000,
        ];
        Glyphs { style, rows }
    }

    pub fn style(&self) -> Style {
        self.style
    }

    /// The rows of every glyph, by slot.
    pub fn rows(&self) -> &[[u8; 8]; 8] {
        &self.rows
    }

    /// The cell for `lit` steps of the style's steps per cell.
    pub fn cell(&self, lit: u16) -> u8 {
        let per = self.style.steps_per_cell();
        let lit = lit.min(per);
        if lit == 0 {
            return SPACE;
        }
        match self.style {
            Style::Blocks => {
                if lit >= per {
                    FULL
                } else {
                    (lit - 1) as u8
                }
            }
            Style::Lines | Style::Segments => (lit - 1) as u8,
            Style::Needle => SPACE,
        }
    }

    /// A bar of `steps` across `cells` cells, with the peak marker at step
    /// `peak` where it lies beyond the bar.
    pub fn bar(&self, steps: u16, peak: Option<u16>, cells: usize) -> Vec<u8> {
        let per = self.style.steps_per_cell();
        let total = cells as u16 * per;
        let steps = steps.min(total);
        let mut out = Vec::with_capacity(cells);
        for cell in 0..cells as u16 {
            let start = cell * per;
            out.push(self.cell(steps.saturating_sub(start)));
        }
        if self.style == Style::Needle && steps > 0 {
            let at = steps - 1;
            out[(at / per) as usize] = (at % per) as u8;
        }
        if let Some(p) = peak {
            let p = p.min(total.saturating_sub(1));
            let cell = (p / per) as usize;
            if cell < cells && out[cell] == SPACE {
                out[cell] = PEAK;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_fill_cell_by_cell_and_the_peak_sits_past_them() {
        let g = Glyphs::new(Style::Blocks);
        assert_eq!(g.bar(0, None, 3), [SPACE, SPACE, SPACE]);
        assert_eq!(g.bar(7, None, 3), [FULL, 1, SPACE]);
        assert_eq!(g.bar(15, None, 3), [FULL, FULL, FULL]);
        assert_eq!(g.bar(99, None, 3), [FULL, FULL, FULL], "clamped");
        assert_eq!(g.bar(5, Some(12), 3), [FULL, SPACE, PEAK]);
        assert_eq!(
            g.bar(7, Some(6), 3),
            [FULL, 1, SPACE],
            "a peak inside the bar is not drawn over it"
        );
        assert_eq!(g.bar(0, Some(0), 3), [PEAK, SPACE, SPACE]);
        assert_eq!(g.bar(0, Some(100), 3), [SPACE, SPACE, PEAK], "clamped");
        assert_eq!(g.rows()[0], [0b10000; 8]);
        assert_eq!(g.rows()[1], [0b11000; 8]);
        assert_eq!(g.rows()[3], [0b11110; 8]);
    }

    #[test]
    fn the_other_styles_have_their_own_steps() {
        let lines = Glyphs::new(Style::Lines);
        assert_eq!(lines.bar(4, None, 3), [2, 0, SPACE]);
        assert_eq!(lines.bar(9, None, 3), [2, 2, 2]);
        let segments = Glyphs::new(Style::Segments);
        assert_eq!(segments.bar(3, None, 3), [1, 0, SPACE]);
        let needle = Glyphs::new(Style::Needle);
        assert_eq!(needle.bar(0, None, 3), [SPACE, SPACE, SPACE]);
        assert_eq!(needle.bar(1, None, 3), [0, SPACE, SPACE]);
        assert_eq!(needle.bar(7, Some(12), 3), [SPACE, 1, PEAK]);
        assert_eq!(needle.rows()[4], [0b00001; 8]);
        assert_eq!(Style::by_name("needle"), Some(Style::Needle));
        assert_eq!(Style::by_name("x"), None);
    }

    #[test]
    fn every_glyph_fits_five_by_eight() {
        for style in Style::ALL {
            for (i, g) in Glyphs::new(style).rows().iter().enumerate() {
                assert!(
                    g.iter().all(|r| *r < 32),
                    "{style:?} glyph {i} overflows five columns"
                );
            }
        }
        for g in big_digits() {
            assert!(g.iter().all(|r| *r < 32));
        }
    }

    #[test]
    fn large_digits_take_three_cells_and_a_colon_one() {
        let [top, bottom] = big_text("10:45");
        assert_eq!(top.len(), 4 * BIG_DIGIT_CELLS + 1);
        assert_eq!(bottom.len(), top.len());
        assert_eq!(&top[..3], &[BIG_UP, BIG_BAR, SPACE]);
        assert_eq!(&bottom[3..6], &[BIG_BAR, BIG_LO, BIG_BAR]);
        assert_eq!(top[6], BIG_DOT);
        assert_eq!(big_text("x")[0], vec![SPACE; 3]);
        assert_eq!(big_text(" ")[1], vec![SPACE]);
    }
}
