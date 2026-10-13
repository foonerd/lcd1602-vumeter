//! A controller in software: the same commands and data as the glass
//! takes, kept as display and glyph memory. The renders for the wiki and
//! the tests draw on it.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;

use crate::hd44780::{self, Bus, ROW_ADDRESS};

/// Display memory holds 40 cells a row; the glass shows the first 16.
pub const DDRAM_ROW: usize = 40;

/// What the controller holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub ddram: [[u8; DDRAM_ROW]; 2],
    pub cgram: [[u8; 8]; 8],
    pub display_on: bool,
    pub backlight: bool,
    /// Every command as it came, for the tests.
    pub commands: Vec<u8>,
    address: u8,
    in_cgram: bool,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            ddram: [[b' '; DDRAM_ROW]; 2],
            cgram: [[0; 8]; 8],
            display_on: false,
            backlight: true,
            commands: Vec::new(),
            address: 0,
            in_cgram: false,
        }
    }
}

impl Memory {
    /// The first `cols` cells of each row, as shown.
    pub fn rows(&self, cols: usize) -> [Vec<u8>; 2] {
        [
            self.ddram[0][..cols.min(DDRAM_ROW)].to_vec(),
            self.ddram[1][..cols.min(DDRAM_ROW)].to_vec(),
        ]
    }

    /// A row as text, user glyphs as `#`, the block as `@`.
    pub fn text(&self, row: usize, cols: usize) -> String {
        self.ddram[row.min(1)][..cols.min(DDRAM_ROW)]
            .iter()
            .map(|c| match *c {
                0..=7 => '#',
                0xFF => '@',
                0x20..=0x7E => *c as char,
                _ => '?',
            })
            .collect()
    }

    fn command(&mut self, cmd: u8) {
        self.commands.push(cmd);
        if cmd & hd44780::SET_DDRAM != 0 {
            self.address = cmd & 0x7F;
            self.in_cgram = false;
        } else if cmd & hd44780::SET_CGRAM != 0 {
            self.address = cmd & 0x3F;
            self.in_cgram = true;
        } else if cmd & hd44780::DISPLAY_CONTROL != 0 && cmd & 0xF0 == 0 {
            self.display_on = cmd & hd44780::DISPLAY_ON != 0;
        } else if cmd == hd44780::CLEAR {
            self.ddram = [[b' '; DDRAM_ROW]; 2];
            self.address = 0;
            self.in_cgram = false;
        } else if cmd == hd44780::HOME {
            self.address = 0;
            self.in_cgram = false;
        }
    }

    fn data(&mut self, byte: u8) {
        if self.in_cgram {
            let slot = (self.address >> 3) as usize & 7;
            let row = (self.address & 7) as usize;
            self.cgram[slot][row] = byte & 0x1F;
            self.address = (self.address + 1) & 0x3F;
            return;
        }
        let (row, col) = if self.address >= ROW_ADDRESS[1] {
            (1, (self.address - ROW_ADDRESS[1]) as usize)
        } else {
            (0, self.address as usize)
        };
        if col < DDRAM_ROW {
            self.ddram[row][col] = byte;
        }
        // The address counter runs on; past a row's end it reaches the other row.
        self.address = match self.address {
            0x27 => 0x40,
            0x67 => 0x00,
            a => a + 1,
        };
    }
}

/// The wire into a [`Memory`] shared with whoever looks at it.
pub struct Simulator {
    memory: Rc<RefCell<Memory>>,
}

impl Simulator {
    pub fn new() -> (Simulator, Rc<RefCell<Memory>>) {
        let memory = Rc::new(RefCell::new(Memory::default()));
        (
            Simulator {
                memory: memory.clone(),
            },
            memory,
        )
    }
}

impl Bus for Simulator {
    fn command(&mut self, cmd: u8) -> io::Result<()> {
        self.memory.borrow_mut().command(cmd);
        Ok(())
    }

    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut m = self.memory.borrow_mut();
        for b in bytes {
            m.data(*b);
        }
        Ok(())
    }

    fn nibble(&mut self, _n: u8) -> io::Result<()> {
        Ok(())
    }

    fn backlight(&mut self, on: bool) -> io::Result<bool> {
        self.memory.borrow_mut().backlight = on;
        Ok(true)
    }

    fn describe(&self) -> String {
        "simulator".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hd44780::{Controller, Lcd};

    #[test]
    fn what_is_written_is_what_the_memory_holds() {
        let (sim, memory) = Simulator::new();
        let mut lcd = Lcd::new(Box::new(sim), Controller::Hd44780);
        lcd.init().unwrap();
        assert!(memory.borrow().display_on);
        lcd.define_glyph(3, &[0x1F, 0, 0x1F, 0, 0x1F, 0, 0x1F, 0])
            .unwrap();
        lcd.set_cursor(14, 0).unwrap();
        lcd.write(b"abcd").unwrap();
        lcd.set_cursor(0, 1).unwrap();
        lcd.write(&[3, 0xFF]).unwrap();
        let m = memory.borrow();
        assert_eq!(m.cgram[3][0], 0x1F);
        assert_eq!(m.cgram[3][1], 0);
        assert_eq!(&m.ddram[0][14..16], b"ab");
        assert_eq!(&m.ddram[0][16..18], b"cd", "the row runs on past the glass");
        assert_eq!(m.text(1, 16), "#@              ");
        drop(m);
        lcd.clear().unwrap();
        assert_eq!(memory.borrow().text(0, 16), " ".repeat(16));
    }
}
