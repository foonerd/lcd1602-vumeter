//! A 16x2 character display over I2C: the controller behind it, the wire
//! it hangs on, the RGB backlight beside it where there is one, and the
//! glyphs the meter draws with. The [`Panel`] holds what the display
//! shows and writes only the cells that changed.

pub mod backlight;
pub mod font;
pub mod glyphs;
pub mod hd44780;
pub mod i2c;
pub mod render;
pub mod sim;
pub mod transport;

use std::fmt;
use std::io;

use backlight::{Backlight, RgbChip};
use glyphs::{Glyphs, Style};
use hd44780::{Controller, Lcd};
use i2c::Device;
use transport::{Native, Pcf8574, PinMap};

pub const COLUMNS: usize = 16;
pub const ROWS: usize = 2;

/// Which display to open: everything the settings page asks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub bus: u8,
    pub controller: Controller,
    pub address: u8,
    /// The backpack's pin layout, where the controller hangs on one.
    pub pins: PinMap,
    /// The RGB driver and its address, where the module has one.
    pub rgb: Option<(u8, RgbChip)>,
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "i2c-{} {} at 0x{:02X}",
            self.bus,
            self.controller.name(),
            self.address
        )?;
        match self.rgb {
            Some((addr, chip)) => write!(f, ", RGB {} at 0x{:02X}", chip.name(), addr),
            None => write!(f, ", no RGB"),
        }
    }
}

impl Spec {
    /// Look for a display on `bus` at the addresses the known modules
    /// use, and for an RGB driver beside it. A controller the probe cannot
    /// tell from another (ST7032 answers at the AiP31068's address) is
    /// taken for the common one; the settings page can say otherwise.
    pub fn detect(bus: u8) -> Option<Spec> {
        let found = [
            (Native::LCD_ADDRESS, Controller::Aip31068),
            (Pcf8574::ADDRESSES[0], Controller::Hd44780),
            (Pcf8574::ADDRESSES[1], Controller::Hd44780),
            (Native::OLED_ADDRESSES[0], Controller::Us2066),
            (Native::OLED_ADDRESSES[1], Controller::Us2066),
        ]
        .into_iter()
        .find(|(addr, _)| Device::present(bus, *addr))?;
        Some(Spec {
            bus,
            controller: found.1,
            address: found.0,
            pins: PinMap::STANDARD,
            rgb: Self::detect_rgb(bus),
        })
    }

    /// The RGB driver on `bus`, if one answers at a known address.
    pub fn detect_rgb(bus: u8) -> Option<(u8, RgbChip)> {
        RgbChip::KNOWN
            .into_iter()
            .find(|(addr, _)| Device::present(bus, *addr))
    }
}

/// What the eight glyph slots hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bank {
    /// The bar cells of a style, the peak marker, play and pause.
    Bars(Style),
    /// The large digits for the standby clock.
    Digits,
}

/// The display, with what it shows.
pub struct Panel {
    lcd: Lcd,
    backlight: Option<Backlight>,
    spec: Spec,
    glyphs: Glyphs,
    bank: Bank,
    shown: [[u8; COLUMNS]; ROWS],
    want: [[u8; COLUMNS]; ROWS],
    /// Nothing is known to be on the glass yet: the first flush writes all.
    fresh: bool,
    rgb: Option<(u8, u8, u8)>,
    display_on: bool,
}

impl Panel {
    /// Open the display `spec` names, bring it up and load the glyphs of
    /// `style`.
    pub fn open(spec: &Spec, style: Style) -> io::Result<Panel> {
        let dev = Device::open(spec.bus, spec.address)?;
        let bus: Box<dyn hd44780::Bus> = if spec.controller.four_bit() {
            Box::new(Pcf8574::new(dev, spec.pins))
        } else {
            Box::new(Native::new(dev))
        };
        let backlight = match spec.rgb {
            Some((addr, chip)) => {
                let mut b = Backlight::new(Device::open(spec.bus, addr)?, chip);
                b.init()?;
                Some(b)
            }
            None => None,
        };
        Self::with_bus(bus, spec.clone(), backlight, style)
    }

    /// A display on a wire of the caller's, the simulator among them.
    pub fn with_bus(
        bus: Box<dyn hd44780::Bus>,
        spec: Spec,
        backlight: Option<Backlight>,
        style: Style,
    ) -> io::Result<Panel> {
        let mut lcd = Lcd::new(bus, spec.controller);
        lcd.init()?;
        let glyphs = Glyphs::new(style);
        for (slot, rows) in glyphs.rows().iter().enumerate() {
            lcd.define_glyph(slot as u8, rows)?;
        }
        lcd.set_cursor(0, 0)?;
        Ok(Panel {
            lcd,
            backlight,
            spec,
            glyphs,
            bank: Bank::Bars(style),
            shown: [[glyphs::SPACE; COLUMNS]; ROWS],
            want: [[glyphs::SPACE; COLUMNS]; ROWS],
            fresh: true,
            rgb: None,
            display_on: true,
        })
    }

    /// A display in software, for renders and tests: the panel and the
    /// memory behind it.
    pub fn simulated(style: Style) -> (Panel, std::rc::Rc<std::cell::RefCell<sim::Memory>>) {
        let (bus, memory) = sim::Simulator::new();
        let spec = Spec {
            bus: 0,
            controller: Controller::Hd44780,
            address: 0,
            pins: PinMap::STANDARD,
            rgb: None,
        };
        // The simulator's wire never fails.
        let panel = Self::with_bus(Box::new(bus), spec, None, style).unwrap();
        (panel, memory)
    }

    /// The bar glyphs, of the style last asked for.
    pub fn glyphs(&self) -> &Glyphs {
        &self.glyphs
    }

    pub fn bank(&self) -> Bank {
        self.bank
    }

    /// Load another set of glyphs; the cells shown are drawn again, so
    /// what is on the glass is drawn with the glyphs it was meant for.
    pub fn set_bank(&mut self, bank: Bank) -> io::Result<()> {
        if self.bank == bank {
            return Ok(());
        }
        let rows = match bank {
            Bank::Bars(style) => {
                if self.glyphs.style() != style {
                    self.glyphs = Glyphs::new(style);
                }
                *self.glyphs.rows()
            }
            Bank::Digits => glyphs::big_digits(),
        };
        for (slot, glyph) in rows.iter().enumerate() {
            self.lcd.define_glyph(slot as u8, glyph)?;
        }
        self.bank = bank;
        self.fresh = true;
        Ok(())
    }

    /// Load the bar glyphs of another style.
    pub fn set_style(&mut self, style: Style) -> io::Result<()> {
        self.set_bank(Bank::Bars(style))
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    pub fn describe(&self) -> String {
        match &self.backlight {
            Some(b) => format!("{}; backlight {}", self.lcd.describe(), b.describe()),
            None => self.lcd.describe(),
        }
    }

    /// Whether the backlight takes a colour.
    pub fn has_rgb(&self) -> bool {
        self.backlight.is_some()
    }

    /// A row's cells, as bytes of the controller's character set; shorter
    /// rows are padded with spaces, longer ones cut.
    pub fn set_row(&mut self, row: usize, cells: &[u8]) {
        let Some(target) = self.want.get_mut(row) else {
            return;
        };
        for (i, cell) in target.iter_mut().enumerate() {
            *cell = cells.get(i).copied().unwrap_or(glyphs::SPACE);
        }
    }

    /// A row of text, ASCII as is, anything else as `?`.
    pub fn set_text(&mut self, row: usize, text: &str) {
        let cells: Vec<u8> = text
            .chars()
            .map(|c| {
                if c.is_ascii() && !c.is_ascii_control() {
                    c as u8
                } else {
                    b'?'
                }
            })
            .collect();
        self.set_row(row, &cells);
    }

    /// Text centred in its row.
    pub fn set_centered(&mut self, row: usize, text: &str) {
        let n = text.chars().count().min(COLUMNS);
        let pad = (COLUMNS - n) / 2;
        let line: String = " ".repeat(pad) + &text.chars().take(n).collect::<String>();
        self.set_text(row, &line);
    }

    pub fn clear(&mut self) {
        self.want = [[glyphs::SPACE; COLUMNS]; ROWS];
    }

    /// Write the cells that changed since the last flush, as runs.
    pub fn flush(&mut self) -> io::Result<()> {
        for row in 0..ROWS {
            let mut col = 0;
            while col < COLUMNS {
                if !self.fresh && self.want[row][col] == self.shown[row][col] {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < COLUMNS && (self.fresh || self.want[row][col] != self.shown[row][col]) {
                    col += 1;
                }
                self.lcd.set_cursor(start as u8, row as u8)?;
                self.lcd.write(&self.want[row][start..col])?;
                self.shown[row][start..col].copy_from_slice(&self.want[row][start..col]);
            }
        }
        self.fresh = false;
        Ok(())
    }

    /// Forget what is on the glass: the next flush rewrites every cell,
    /// for a display that was powered off and on behind the daemon.
    pub fn redraw(&mut self) {
        self.fresh = true;
    }

    /// The backlight colour. Without an RGB driver, any light is on and
    /// black is off, where the wire can switch it at all.
    pub fn set_rgb(&mut self, r: u8, g: u8, b: u8) -> io::Result<()> {
        if self.rgb == Some((r, g, b)) {
            return Ok(());
        }
        match self.backlight.as_mut() {
            Some(bl) => bl.set(r, g, b)?,
            None => {
                self.lcd.backlight(r > 0 || g > 0 || b > 0)?;
            }
        }
        self.rgb = Some((r, g, b));
        Ok(())
    }

    pub fn display(&mut self, on: bool) -> io::Result<()> {
        if self.display_on == on {
            return Ok(());
        }
        self.lcd.display(on)?;
        self.display_on = on;
        Ok(())
    }

    pub fn contrast(&mut self, contrast: u8) -> io::Result<()> {
        self.lcd.contrast(contrast)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_describes_itself() {
        let spec = Spec {
            bus: 1,
            controller: Controller::Aip31068,
            address: 0x3E,
            pins: PinMap::STANDARD,
            rgb: Some((0x60, RgbChip::Pca9633)),
        };
        assert_eq!(
            spec.to_string(),
            "i2c-1 aip31068 at 0x3E, RGB pca9633 at 0x60"
        );
        assert_eq!(
            Spec { rgb: None, ..spec }.to_string(),
            "i2c-1 aip31068 at 0x3E, no RGB"
        );
    }

    #[test]
    fn a_flush_writes_only_the_cells_that_changed() {
        let (mut panel, memory) = Panel::simulated(Style::Blocks);
        panel.set_text(0, "Volumio");
        panel.set_centered(1, "ready");
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(0, 16), "Volumio         ");
        assert_eq!(memory.borrow().text(1, 16), "     ready      ");
        assert_eq!(
            memory.borrow().cgram[0],
            [0b10000; 8],
            "the style's glyphs are loaded"
        );
        let before = memory.borrow().commands.len();
        panel.flush().unwrap();
        assert_eq!(
            memory.borrow().commands.len(),
            before,
            "nothing changed, nothing written"
        );
        panel.set_text(0, "Volumio!");
        panel.flush().unwrap();
        assert_eq!(
            memory.borrow().commands.len(),
            before + 1,
            "one cursor move for the one cell that changed"
        );
        assert_eq!(memory.borrow().text(0, 16), "Volumio!        ");
        panel.set_style(Style::Needle).unwrap();
        assert_eq!(memory.borrow().cgram[4], [0b00001; 8]);
        let bar = panel.glyphs().bar(6, None, 3);
        panel.set_row(1, &bar);
        panel.flush().unwrap();
        assert_eq!(memory.borrow().text(1, 16), " #              ");
        panel.set_bank(Bank::Digits).unwrap();
        assert_eq!(memory.borrow().cgram[3], [0b11110; 8], "the digits' stroke");
        panel.set_bank(Bank::Bars(Style::Needle)).unwrap();
        assert_eq!(memory.borrow().cgram[4], [0b00001; 8], "the needle is back");
    }
}
