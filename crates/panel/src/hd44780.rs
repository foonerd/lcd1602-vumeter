//! The HD44780 command set, the controllers that speak it over I2C, and
//! how each is brought up. The controller does not know the wire: a
//! [`Bus`] carries commands and data, as the native I2C controllers take
//! them (a control byte, then the byte) or as a PCF8574 backpack toggles
//! them into a parallel HD44780 four bits at a time.

use std::io;
use std::thread::sleep;
use std::time::Duration;

/// The wire to the controller.
pub trait Bus {
    /// One instruction byte, RS low.
    fn command(&mut self, cmd: u8) -> io::Result<()>;
    /// Data bytes, RS high, to the address the controller holds.
    fn data(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// One bare nibble, RS low, on a four-bit wire: the interface sync of
    /// a start-up. A native wire has no such thing.
    fn nibble(&mut self, _n: u8) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no four-bit wire",
        ))
    }
    /// The backlight where the wire itself switches it (a backpack's pin);
    /// `false` where it does not.
    fn backlight(&mut self, _on: bool) -> io::Result<bool> {
        Ok(false)
    }
    /// Said once in the log.
    fn describe(&self) -> String;
}

pub const CLEAR: u8 = 0x01;
pub const HOME: u8 = 0x02;
pub const ENTRY_MODE: u8 = 0x04;
pub const DISPLAY_CONTROL: u8 = 0x08;
pub const FUNCTION_SET: u8 = 0x20;
pub const SET_CGRAM: u8 = 0x40;
pub const SET_DDRAM: u8 = 0x80;

pub const ENTRY_LEFT: u8 = 0x02;
pub const DISPLAY_ON: u8 = 0x04;
pub const CURSOR_ON: u8 = 0x02;
pub const BLINK_ON: u8 = 0x01;
pub const TWO_LINES: u8 = 0x08;
pub const EIGHT_BIT: u8 = 0x10;

/// Where each row starts in display memory, on every controller here.
pub const ROW_ADDRESS: [u8; 2] = [0x00, 0x40];

/// The controller behind the wire, which decides the start-up sequence
/// and what else it can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Controller {
    /// AiP31068L: Waveshare LCD1602 I2C and RGB modules, Grove LCD RGB,
    /// DFRobot RGB LCD1602. Native I2C, the HD44780 set as is.
    Aip31068,
    /// Sitronix ST7032i: Midas, Newhaven C0216CiZ and the like. Native
    /// I2C with an extended set that holds the contrast.
    St7032 { contrast: u8 },
    /// US2066 (SSD1311): Winstar WEO/WEA and Newhaven slim character
    /// OLEDs over I2C. Its own start-up, then the HD44780 set.
    Us2066,
    /// A parallel HD44780 behind a PCF8574 backpack, in four-bit mode.
    Hd44780,
    /// Winstar WS0010 character OLED behind a backpack, in four-bit mode.
    Ws0010,
}

impl Controller {
    pub fn name(&self) -> &'static str {
        match self {
            Controller::Aip31068 => "aip31068",
            Controller::St7032 { .. } => "st7032",
            Controller::Us2066 => "us2066",
            Controller::Hd44780 => "hd44780",
            Controller::Ws0010 => "ws0010",
        }
    }

    /// Whether the bus must sync the four-bit interface before commands.
    pub fn four_bit(&self) -> bool {
        matches!(self, Controller::Hd44780 | Controller::Ws0010)
    }

    /// How long a clear or home takes on this controller.
    fn settle(&self) -> Duration {
        match self {
            Controller::Ws0010 => Duration::from_millis(7),
            _ => Duration::from_millis(3),
        }
    }
}

/// One step of a start-up sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Cmd(u8),
    Data(u8),
    /// A bare nibble on a four-bit wire, for the interface sync.
    Nibble(u8),
    Wait(u16),
}

/// The start-up sequence of a controller, from its data sheet, ending
/// with the display on, cleared, cursor hidden, writing left to right.
pub fn startup(controller: Controller) -> Vec<Step> {
    use Step::*;
    match controller {
        Controller::Aip31068 => vec![
            Wait(50),
            Cmd(FUNCTION_SET | TWO_LINES),
            Wait(5),
            Cmd(FUNCTION_SET | TWO_LINES),
            Wait(5),
            Cmd(FUNCTION_SET | TWO_LINES),
            Cmd(FUNCTION_SET | TWO_LINES),
            Cmd(DISPLAY_CONTROL | DISPLAY_ON),
            Cmd(CLEAR),
            Wait(3),
            Cmd(ENTRY_MODE | ENTRY_LEFT),
        ],
        Controller::St7032 { contrast } => {
            let c = contrast.min(63);
            vec![
                Wait(50),
                Cmd(FUNCTION_SET | EIGHT_BIT | TWO_LINES),
                Cmd(FUNCTION_SET | EIGHT_BIT | TWO_LINES | 0x01),
                // Internal oscillator, 1/5 bias.
                Cmd(0x14),
                // Contrast low nibble; booster on with the high bits.
                Cmd(0x70 | (c & 0x0F)),
                Cmd(0x5C | ((c >> 4) & 0x03)),
                // Follower on, amplification ratio 4.
                Cmd(0x6C),
                Wait(200),
                Cmd(FUNCTION_SET | EIGHT_BIT | TWO_LINES),
                Cmd(DISPLAY_CONTROL | DISPLAY_ON),
                Cmd(CLEAR),
                Wait(3),
                Cmd(ENTRY_MODE | ENTRY_LEFT),
            ]
        }
        Controller::Us2066 => vec![
            Wait(100),
            // Extended command set: internal regulator off for 3.3 V logic.
            Cmd(0x2A),
            Cmd(0x71),
            Data(0x00),
            Cmd(0x28),
            Cmd(DISPLAY_CONTROL),
            // OLED command set: clock, then back.
            Cmd(0x2A),
            Cmd(0x79),
            Cmd(0xD5),
            Cmd(0x70),
            Cmd(0x78),
            // Two lines, COM/SEG direction, ROM A with eight CGRAM glyphs.
            Cmd(0x08),
            Cmd(0x06),
            Cmd(0x72),
            Data(0x00),
            // OLED command set: segment pins, VSL, contrast, phase, VCOMH.
            Cmd(0x2A),
            Cmd(0x79),
            Cmd(0xDA),
            Cmd(0x10),
            Cmd(0xDC),
            Cmd(0x00),
            Cmd(0x81),
            Cmd(0x7F),
            Cmd(0xD9),
            Cmd(0xF1),
            Cmd(0xDB),
            Cmd(0x40),
            Cmd(0x78),
            Cmd(0x28),
            Cmd(CLEAR),
            Wait(3),
            Cmd(SET_DDRAM),
            Cmd(ENTRY_MODE | ENTRY_LEFT),
            Cmd(DISPLAY_CONTROL | DISPLAY_ON),
            Wait(100),
        ],
        Controller::Hd44780 => vec![
            Wait(50),
            Nibble(0x03),
            Wait(5),
            Nibble(0x03),
            Wait(1),
            Nibble(0x03),
            Wait(1),
            Nibble(0x02),
            Cmd(FUNCTION_SET | TWO_LINES),
            Cmd(DISPLAY_CONTROL),
            Cmd(CLEAR),
            Wait(3),
            Cmd(ENTRY_MODE | ENTRY_LEFT),
            Cmd(DISPLAY_CONTROL | DISPLAY_ON),
        ],
        Controller::Ws0010 => vec![
            Wait(500),
            // Five zero nibbles put the interface in a known state.
            Nibble(0x00),
            Nibble(0x00),
            Nibble(0x00),
            Nibble(0x00),
            Nibble(0x00),
            Nibble(0x02),
            // Four bits, two lines, the English/Japanese font table.
            Cmd(FUNCTION_SET | TWO_LINES),
            Cmd(DISPLAY_CONTROL),
            Cmd(CLEAR),
            Wait(7),
            Cmd(ENTRY_MODE | ENTRY_LEFT),
            Cmd(HOME),
            Wait(7),
            Cmd(DISPLAY_CONTROL | DISPLAY_ON),
        ],
    }
}

/// A controller on its wire.
pub struct Lcd {
    bus: Box<dyn Bus>,
    controller: Controller,
    control: u8,
}

impl Lcd {
    pub fn new(bus: Box<dyn Bus>, controller: Controller) -> Self {
        Self {
            bus,
            controller,
            control: DISPLAY_CONTROL | DISPLAY_ON,
        }
    }

    pub fn controller(&self) -> Controller {
        self.controller
    }

    pub fn describe(&self) -> String {
        format!("{} on {}", self.controller.name(), self.bus.describe())
    }

    /// Bring the controller up as its data sheet says.
    pub fn init(&mut self) -> io::Result<()> {
        for step in startup(self.controller) {
            match step {
                Step::Cmd(c) => self.bus.command(c)?,
                Step::Data(d) => self.bus.data(&[d])?,
                Step::Nibble(n) => self.bus.nibble(n)?,
                Step::Wait(ms) => sleep(Duration::from_millis(ms as u64)),
            }
        }
        self.control = DISPLAY_CONTROL | DISPLAY_ON;
        Ok(())
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.bus.command(CLEAR)?;
        sleep(self.controller.settle());
        Ok(())
    }

    pub fn set_cursor(&mut self, col: u8, row: u8) -> io::Result<()> {
        let row = (row as usize).min(ROW_ADDRESS.len() - 1);
        self.bus
            .command(SET_DDRAM | (ROW_ADDRESS[row] + col.min(39)))
    }

    pub fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.bus.data(bytes)
    }

    /// One of the eight user glyphs, slot 0 to 7, eight rows of five bits.
    /// The cursor is left in glyph memory; set it before writing text.
    pub fn define_glyph(&mut self, slot: u8, rows: &[u8; 8]) -> io::Result<()> {
        self.bus.command(SET_CGRAM | ((slot & 0x07) << 3))?;
        let rows: Vec<u8> = rows.iter().map(|r| r & 0x1F).collect();
        self.bus.data(&rows)
    }

    pub fn display(&mut self, on: bool) -> io::Result<()> {
        if on {
            self.control |= DISPLAY_ON;
        } else {
            self.control &= !DISPLAY_ON;
        }
        self.bus.command(self.control)
    }

    /// The backlight where the wire switches it; `false` when it does not.
    pub fn backlight(&mut self, on: bool) -> io::Result<bool> {
        self.bus.backlight(on)
    }

    /// The contrast on a controller that holds one; nothing on the others.
    pub fn contrast(&mut self, contrast: u8) -> io::Result<()> {
        match self.controller {
            Controller::St7032 { .. } => {
                let c = contrast.min(63);
                self.bus
                    .command(FUNCTION_SET | EIGHT_BIT | TWO_LINES | 0x01)?;
                self.bus.command(0x70 | (c & 0x0F))?;
                self.bus.command(0x5C | ((c >> 4) & 0x03))?;
                self.bus.command(FUNCTION_SET | EIGHT_BIT | TWO_LINES)?;
                self.controller = Controller::St7032 { contrast: c };
                Ok(())
            }
            Controller::Us2066 => {
                self.bus.command(0x2A)?;
                self.bus.command(0x79)?;
                self.bus.command(0x81)?;
                self.bus.command(contrast)?;
                self.bus.command(0x78)?;
                self.bus.command(0x28)
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A wire that remembers what went over it.
    pub struct Recorder(pub Rc<RefCell<Vec<(bool, u8)>>>);

    impl Bus for Recorder {
        fn command(&mut self, cmd: u8) -> io::Result<()> {
            self.0.borrow_mut().push((false, cmd));
            Ok(())
        }
        fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
            for b in bytes {
                self.0.borrow_mut().push((true, *b));
            }
            Ok(())
        }
        fn nibble(&mut self, n: u8) -> io::Result<()> {
            self.0.borrow_mut().push((false, 0xF0 | n));
            Ok(())
        }
        fn describe(&self) -> String {
            "recorder".into()
        }
    }

    #[test]
    fn the_cursor_and_glyphs_go_to_the_right_addresses() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut lcd = Lcd::new(Box::new(Recorder(log.clone())), Controller::Aip31068);
        lcd.set_cursor(3, 1).unwrap();
        lcd.write(b"ab").unwrap();
        lcd.define_glyph(2, &[0x1F; 8]).unwrap();
        lcd.display(false).unwrap();
        let got = log.borrow().clone();
        assert_eq!(got[0], (false, 0x80 | 0x40 | 3));
        assert_eq!(&got[1..3], &[(true, b'a'), (true, b'b')]);
        assert_eq!(got[3], (false, 0x40 | (2 << 3)));
        assert_eq!(got.len(), 3 + 1 + 8 + 1);
        assert_eq!(got[12], (false, DISPLAY_CONTROL));
    }

    #[test]
    fn every_startup_ends_with_the_display_on_and_entry_left() {
        for c in [
            Controller::Aip31068,
            Controller::St7032 { contrast: 40 },
            Controller::Us2066,
            Controller::Hd44780,
            Controller::Ws0010,
        ] {
            let steps = startup(c);
            assert!(
                steps.contains(&Step::Cmd(DISPLAY_CONTROL | DISPLAY_ON)),
                "{c:?} turns the display on"
            );
            assert!(
                steps.contains(&Step::Cmd(ENTRY_MODE | ENTRY_LEFT)),
                "{c:?} writes left to right"
            );
            assert!(steps.contains(&Step::Cmd(CLEAR)), "{c:?} clears");
            assert_eq!(
                steps.iter().any(|s| matches!(s, Step::Nibble(_))),
                c.four_bit(),
                "{c:?} syncs the four-bit wire only where there is one"
            );
        }
        // The ST7032 carries its contrast in two commands.
        let steps = startup(Controller::St7032 { contrast: 0x28 });
        assert!(steps.contains(&Step::Cmd(0x78)));
        assert!(steps.contains(&Step::Cmd(0x5E)));
    }
}
