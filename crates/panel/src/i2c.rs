//! One device on an I2C bus, through the kernel's `/dev/i2c-N` interface.

use std::ffi::CString;
use std::fmt;
use std::io;

/// `ioctl` requests of the i2c-dev driver.
const I2C_SLAVE: libc::c_ulong = 0x0703;

/// A device at one address on one bus.
pub struct Device {
    fd: libc::c_int,
    bus: u8,
    addr: u8,
}

impl fmt::Debug for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i2c-{} 0x{:02X}", self.bus, self.addr)
    }
}

impl Device {
    /// Open `/dev/i2c-<bus>` and address `addr` (7-bit).
    pub fn open(bus: u8, addr: u8) -> io::Result<Self> {
        let path = CString::new(format!("/dev/i2c-{bus}")).unwrap();
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::ioctl(fd, I2C_SLAVE, addr as libc::c_ulong) } < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }
        Ok(Self { fd, bus, addr })
    }

    pub fn bus(&self) -> u8 {
        self.bus
    }

    pub fn addr(&self) -> u8 {
        self.addr
    }

    /// One write transaction: start, address, `bytes`, stop.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let n = unsafe { libc::write(self.fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n as usize != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("wrote {n} of {} bytes", bytes.len()),
            ));
        }
        Ok(())
    }

    /// Whether a device answers at `addr` on `bus`: it acknowledges a
    /// one-byte read, as `i2cdetect` asks. A bus that cannot be opened
    /// reads as no device.
    pub fn present(bus: u8, addr: u8) -> bool {
        let Ok(dev) = Self::open(bus, addr) else {
            return false;
        };
        let mut byte = 0u8;
        let n = unsafe { libc::read(dev.fd, &mut byte as *mut u8 as *mut libc::c_void, 1) };
        n == 1
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// Whether `/dev/i2c-<bus>` exists.
pub fn bus_exists(bus: u8) -> bool {
    std::path::Path::new(&format!("/dev/i2c-{bus}")).exists()
}
