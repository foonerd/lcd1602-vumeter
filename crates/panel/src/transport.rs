//! The two wires a 16x2 module comes with: a native I2C controller that
//! takes a control byte before each command or a run of data, and a
//! PCF8574 port expander that drives a parallel HD44780 four bits at a
//! time.

use std::io;

use crate::hd44780::Bus;
use crate::i2c::Device;

/// A controller with I2C of its own: AiP31068, ST7032, US2066. A write is
/// a control byte, `0x80` for one command or `0x40` for data, then the
/// bytes.
pub struct Native {
    dev: Device,
}

impl Native {
    pub fn new(dev: Device) -> Self {
        Self { dev }
    }

    /// The common addresses: the LCD controllers at 0x3E, the OLED
    /// controllers at 0x3C and 0x3D.
    pub const LCD_ADDRESS: u8 = 0x3E;
    pub const OLED_ADDRESSES: [u8; 2] = [0x3C, 0x3D];
}

/// Data bytes per transaction; well under what any bus master takes.
const CHUNK: usize = 16;

impl Bus for Native {
    fn command(&mut self, cmd: u8) -> io::Result<()> {
        self.dev.write(&[0x80, cmd])
    }

    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut buf = [0u8; CHUNK + 1];
        buf[0] = 0x40;
        for chunk in bytes.chunks(CHUNK) {
            buf[1..1 + chunk.len()].copy_from_slice(chunk);
            self.dev.write(&buf[..1 + chunk.len()])?;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!("{:?} (native)", self.dev)
    }
}

/// Which expander pin drives which controller line. Bit numbers 0 to 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinMap {
    pub rs: u8,
    pub rw: u8,
    pub e: u8,
    pub backlight: u8,
    pub d4: u8,
    pub d5: u8,
    pub d6: u8,
    pub d7: u8,
}

impl PinMap {
    /// The layout of nearly every backpack sold: RS, RW, E, backlight on
    /// P0 to P3, the data on P4 to P7.
    pub const STANDARD: PinMap = PinMap {
        rs: 0,
        rw: 1,
        e: 2,
        backlight: 3,
        d4: 4,
        d5: 5,
        d6: 6,
        d7: 7,
    };
    /// The other layout seen (mjkdz and the like): data on P0 to P3,
    /// E, RW, RS, backlight on P4 to P7.
    pub const ALTERNATE: PinMap = PinMap {
        rs: 6,
        rw: 5,
        e: 4,
        backlight: 7,
        d4: 0,
        d5: 1,
        d6: 2,
        d7: 3,
    };

    pub fn by_name(name: &str) -> Option<PinMap> {
        match name {
            "standard" => Some(PinMap::STANDARD),
            "alternate" => Some(PinMap::ALTERNATE),
            _ => None,
        }
    }

    /// The port byte for one nibble on the data lines.
    fn data_bits(&self, nibble: u8) -> u8 {
        let mut b = 0;
        for (i, pin) in [self.d4, self.d5, self.d6, self.d7].iter().enumerate() {
            if nibble & (1 << i) != 0 {
                b |= 1 << pin;
            }
        }
        b
    }
}

/// A PCF8574 or PCF8574A backpack. Every write to the expander sets all
/// eight pins; a nibble goes out with E high then with E low, and the
/// backlight pin rides along on every write.
pub struct Pcf8574 {
    dev: Device,
    map: PinMap,
    backlight: bool,
}

impl Pcf8574 {
    pub fn new(dev: Device, map: PinMap) -> Self {
        Self {
            dev,
            map,
            backlight: true,
        }
    }

    /// Where the backpacks sit: 0x27 (PCF8574) and 0x3F (PCF8574A) with
    /// the address pins open.
    pub const ADDRESSES: [u8; 2] = [0x27, 0x3F];

    fn port(&self, nibble: u8, rs: bool) -> u8 {
        let mut b = self.map.data_bits(nibble);
        if rs {
            b |= 1 << self.map.rs;
        }
        if self.backlight {
            b |= 1 << self.map.backlight;
        }
        b
    }

    /// One nibble, strobed in on the falling edge of E. Each I2C write
    /// takes longer than the controller's 37 µs, so no wait between them.
    fn strobe(&mut self, nibble: u8, rs: bool) -> io::Result<()> {
        let b = self.port(nibble, rs);
        let e = 1 << self.map.e;
        self.dev.write(&[b | e, b])
    }

    fn byte(&mut self, value: u8, rs: bool) -> io::Result<()> {
        self.strobe(value >> 4, rs)?;
        self.strobe(value & 0x0F, rs)
    }
}

impl Bus for Pcf8574 {
    fn command(&mut self, cmd: u8) -> io::Result<()> {
        self.byte(cmd, false)
    }

    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        for b in bytes {
            self.byte(*b, true)?;
        }
        Ok(())
    }

    fn nibble(&mut self, n: u8) -> io::Result<()> {
        self.strobe(n & 0x0F, false)
    }

    fn backlight(&mut self, on: bool) -> io::Result<bool> {
        self.backlight = on;
        let b = self.port(0, false);
        self.dev.write(&[b])?;
        Ok(true)
    }

    fn describe(&self) -> String {
        format!(
            "{:?} (PCF8574 backpack, {} pins)",
            self.dev,
            if self.map == PinMap::STANDARD {
                "standard"
            } else if self.map == PinMap::ALTERNATE {
                "alternate"
            } else {
                "custom"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pin_maps_place_each_nibble_bit_on_its_pin() {
        assert_eq!(PinMap::STANDARD.data_bits(0b1010), 0b1010_0000);
        assert_eq!(PinMap::STANDARD.data_bits(0b0001), 0b0001_0000);
        assert_eq!(PinMap::ALTERNATE.data_bits(0b1010), 0b0000_1010);
        assert_eq!(PinMap::ALTERNATE.data_bits(0b1000), 0b0000_1000);
        assert_eq!(PinMap::by_name("standard"), Some(PinMap::STANDARD));
        assert_eq!(PinMap::by_name("alternate"), Some(PinMap::ALTERNATE));
        assert_eq!(PinMap::by_name("other"), None);
    }
}
