//! The RGB backlight controllers found on 16x2 modules, each a PWM LED
//! driver on its own I2C address with its own register map, as the
//! Waveshare, Grove and DFRobot reference code drives them.

use std::io;

use crate::i2c::Device;

/// Which driver sits behind the backlight, by the address it answers at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgbChip {
    /// PCA9633 at 0x60 (Waveshare LCD1602 RGB, DFRobot v1) or 0x62
    /// (Grove LCD RGB Backlight v4 and earlier).
    Pca9633,
    /// The driver at 0x30 (Grove v5, DFRobot "0x60>>1"): PWM registers
    /// 6 to 8, a 192 step range.
    Grove5,
    /// The driver at 0x6B (DFRobot v2): PWM registers 4 to 6 with a
    /// global latch.
    Dfr6b,
    /// SGM31323 at 0x2D (DFRobot v2.1 and later): PWM registers 1 to 3.
    Sgm31323,
}

impl RgbChip {
    /// The addresses each chip is known at, in the order they are tried.
    pub const KNOWN: [(u8, RgbChip); 5] = [
        (0x60, RgbChip::Pca9633),
        (0x62, RgbChip::Pca9633),
        (0x2D, RgbChip::Sgm31323),
        (0x6B, RgbChip::Dfr6b),
        (0x30, RgbChip::Grove5),
    ];

    pub fn at(addr: u8) -> Option<RgbChip> {
        Self::KNOWN
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, c)| *c)
    }

    pub fn name(&self) -> &'static str {
        match self {
            RgbChip::Pca9633 => "pca9633",
            RgbChip::Grove5 => "grove5",
            RgbChip::Dfr6b => "dfr6b",
            RgbChip::Sgm31323 => "sgm31323",
        }
    }

    /// The red, green and blue PWM registers.
    fn registers(&self) -> [u8; 3] {
        match self {
            RgbChip::Pca9633 => [0x04, 0x03, 0x02],
            RgbChip::Grove5 => [0x06, 0x07, 0x08],
            RgbChip::Dfr6b => [0x06, 0x05, 0x04],
            RgbChip::Sgm31323 => [0x01, 0x02, 0x03],
        }
    }

    /// Register writes that bring the driver up with all outputs under PWM.
    fn startup(&self) -> &'static [(u8, u8)] {
        match self {
            // MODE1 normal, every LED under its PWM and the group PWM, MODE2 blink mode.
            RgbChip::Pca9633 => &[(0x00, 0x00), (0x08, 0xFF), (0x01, 0x20)],
            RgbChip::Grove5 => &[(0x01, 0x00), (0x02, 0xFF), (0x04, 0x15)],
            RgbChip::Dfr6b => &[
                (0x2F, 0x00),
                (0x00, 0x20),
                (0x01, 0x00),
                (0x02, 0x01),
                (0x03, 0x04),
            ],
            RgbChip::Sgm31323 => &[],
        }
    }
}

/// A backlight driver on its device.
pub struct Backlight {
    dev: Device,
    chip: RgbChip,
}

impl Backlight {
    pub fn new(dev: Device, chip: RgbChip) -> Self {
        Self { dev, chip }
    }

    pub fn chip(&self) -> RgbChip {
        self.chip
    }

    pub fn describe(&self) -> String {
        format!("{} at {:?}", self.chip.name(), self.dev)
    }

    fn reg(&self, reg: u8, value: u8) -> io::Result<()> {
        self.dev.write(&[reg, value])
    }

    pub fn init(&mut self) -> io::Result<()> {
        for (reg, value) in self.chip.startup() {
            self.reg(*reg, *value)?;
        }
        Ok(())
    }

    /// The colour, 0 to 255 per channel.
    pub fn set(&mut self, r: u8, g: u8, b: u8) -> io::Result<()> {
        let [rr, rg, rb] = self.chip.registers();
        let scale = |v: u8| match self.chip {
            RgbChip::Grove5 => (v as u16 * 192 / 255) as u8,
            _ => v,
        };
        self.reg(rr, scale(r))?;
        self.reg(rg, scale(g))?;
        self.reg(rb, scale(b))?;
        if self.chip == RgbChip::Dfr6b {
            // The latch: the three values take effect together.
            self.reg(0x07, 0xFF)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chips_are_known_by_address() {
        assert_eq!(RgbChip::at(0x60), Some(RgbChip::Pca9633));
        assert_eq!(RgbChip::at(0x62), Some(RgbChip::Pca9633));
        assert_eq!(RgbChip::at(0x2D), Some(RgbChip::Sgm31323));
        assert_eq!(RgbChip::at(0x3E), None);
        assert_eq!(RgbChip::Pca9633.registers(), [0x04, 0x03, 0x02]);
    }
}
