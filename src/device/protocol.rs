//! Register-level protocol of the scanner mouse.
//!
//! Everything goes through the vendor UVC extension unit
//! (GUID `dd880f8a-1cba-4954-8a25-f7875967f0f7`):
//!
//! * selector 1 (20 bytes, read-only) – firmware / protocol information;
//! * selector 2 (5 bytes) – register access `[cmd, addr_lo, addr_hi, val_lo, val_hi]`;
//! * selector 3 (33 bytes) – EEPROM block transfer state machine.
//!
//! Two register spaces exist:
//!
//! * the mouse micro-controller (SCCB id `0x88`), accessed directly;
//! * the image sensor (SCCB id `0x42`), accessed indirectly through MCU
//!   mailbox registers `0xb8` / `0xbd` / `0xbe` while mouse mode is disabled.
//!
//! The sequences below reproduce those of the original Windows software
//! (protocol variant 2), as described in `docs/PROTOCOLE.md`.

#![allow(dead_code)] // complete register map kept for reference and diagnostics

use std::sync::Arc;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;

use super::XuControl;

pub const SEL_INFO: u8 = 1;
pub const SEL_REGISTER: u8 = 2;
pub const SEL_EEPROM: u8 = 3;

/// MCU registers.
pub mod reg {
    /// Mode / command register.
    pub const MODE: u16 = 0xd8;
    /// Scan button state (0/1).
    pub const BUTTON: u16 = 0xbc;
    /// Sensor mailbox: status/command (1 = write, 2 = read, 0 = idle).
    pub const MAILBOX_CMD: u16 = 0xb8;
    pub const MAILBOX_ADDR: u16 = 0xbd;
    pub const MAILBOX_DATA: u16 = 0xbe;
    /// Written with 0x60 before the first scan start.
    pub const SCAN_SETUP: u16 = 0x2d;
    /// LED exposure time (units of 1/49.23 ms).
    pub const EXPOSURE: u16 = 0xe6;
    /// Laser read position (0..=128).
    pub const LASER_READ_POS: u16 = 0xdc;
    /// Security challenge (5 bytes) and response (5 bytes).
    pub const CHALLENGE: u16 = 0xa1;
    pub const RESPONSE: u16 = 0xb1;
}

/// MCU mode commands and the value the MCU reports once the command has been executed.
pub mod mode {
    pub const DISABLE_MOUSE: (u16, u16) = (0x00, 0xff);
    pub const ENABLE_MOUSE: (u16, u16) = (0xe0, 0x11);
    pub const START_SCAN: (u16, u16) = (0xe1, 0x99);
    pub const SECURITY_A: (u16, u16) = (0xed, 0xff);
    pub const SECURITY_B: (u16, u16) = (0xec, 0xff);
}

/// Image sensor registers (OmniVision).
pub mod sensor {
    pub const GAIN: u16 = 0x00;
    pub const BLUE: u16 = 0x01;
    pub const RED: u16 = 0x02;
    pub const GREEN: u16 = 0x03;
    pub const WINDOW_A: u16 = 0x16;
    pub const WINDOW_B: u16 = 0x19;
}

const EXPOSURE_UNITS_PER_MS: f64 = 49.23;

/// Information block exposed by selector 1.
#[derive(Debug, Clone)]
pub struct FirmwareInfo {
    pub raw: Vec<u8>,
    pub protocol: u8,
}

/// Thread-safe protocol handle.
#[derive(Clone)]
pub struct Mcu {
    xu: Arc<dyn XuControl>,
    lock: Arc<Mutex<()>>,
}

impl Mcu {
    pub fn new(xu: Arc<dyn XuControl>) -> Self {
        Self { xu, lock: Arc::new(Mutex::new(())) }
    }

    pub fn firmware_info(&self) -> Result<FirmwareInfo> {
        let len = self.xu.len(SEL_INFO)? as usize;
        let mut buf = vec![0u8; len];
        self.xu.get(SEL_INFO, &mut buf)?;
        let protocol = if len == 3 { 0 } else { buf[0] };
        Ok(FirmwareInfo { raw: buf, protocol })
    }

    // ---------------------------------------------------------------- MCU registers

    fn read_raw(&self, addr: u16) -> Result<u16> {
        let (cmd, a) = if addr < 0x100 { (0x80u8, [addr as u8, 0]) } else { (0xa0u8, addr.to_le_bytes()) };
        let req = [cmd, a[0], a[1], 0, 0];
        self.xu.set(SEL_REGISTER, &req)?;
        let mut resp = [0u8; 5];
        self.xu.get(SEL_REGISTER, &mut resp)?;
        if resp[1] != a[0] || resp[2] != a[1] {
            bail!("register read echo mismatch for {addr:#x}: {resp:02x?}");
        }
        Ok(u16::from_le_bytes([resp[3], resp[4]]))
    }

    fn write_raw(&self, addr: u16, val: u16) -> Result<()> {
        let (cmd, a) = if addr < 0x100 { (0x00u8, [addr as u8, 0]) } else { (0x20u8, addr.to_le_bytes()) };
        let v = val.to_le_bytes();
        self.xu.set(SEL_REGISTER, &[cmd, a[0], a[1], v[0], v[1]])
    }

    pub fn read(&self, addr: u16) -> Result<u16> {
        let _g = self.lock.lock();
        self.read_raw(addr)
    }

    pub fn write(&self, addr: u16, val: u16) -> Result<()> {
        let _g = self.lock.lock();
        self.write_raw(addr, val)
    }

    /// Write and read back, retrying up to 10 times (as the original software does).
    pub fn write_verified(&self, addr: u16, val: u16) -> Result<()> {
        let _g = self.lock.lock();
        self.write_raw(addr, val)?;
        for _ in 0..10 {
            if self.read_raw(addr)? == val {
                return Ok(());
            }
            self.write_raw(addr, val)?;
            sleep(Duration::from_millis(10));
        }
        if self.read_raw(addr)? == val {
            return Ok(());
        }
        bail!("register {addr:#x} did not accept value {val:#x}")
    }

    /// Writes a command to the mode register and polls until the MCU acknowledges it.
    pub fn mode_command(&self, (cmd, ack): (u16, u16)) -> Result<()> {
        let _g = self.lock.lock();
        self.write_raw(reg::MODE, cmd)?;
        let mut last = 0;
        for _ in 0..100 {
            last = self.read_raw(reg::MODE)?;
            if last == ack {
                return Ok(());
            }
        }
        bail!("MCU did not acknowledge mode command {cmd:#x} (expected {ack:#x}, got {last:#x})")
    }

    pub fn current_mode(&self) -> Result<u16> {
        self.read(reg::MODE)
    }

    pub fn disable_mouse(&self) -> Result<()> {
        self.mode_command(mode::DISABLE_MOUSE)
    }

    /// The original driver always sends the enable command twice.
    pub fn enable_mouse(&self) -> Result<()> {
        self.mode_command(mode::ENABLE_MOUSE)?;
        self.mode_command(mode::ENABLE_MOUSE)
    }

    pub fn start_scan_mode(&self) -> Result<()> {
        self.mode_command(mode::START_SCAN).context("starting scan mode in mouse MCU failed")
    }

    pub fn button_pressed(&self) -> Result<bool> {
        Ok(self.read(reg::BUTTON)? != 0)
    }

    // ---------------------------------------------------------------- sensor (indirect)

    fn mailbox_wait_idle(&self, sleep_between: bool) -> Result<()> {
        for _ in 0..100 {
            if self.read_raw(reg::MAILBOX_CMD)? == 0 {
                return Ok(());
            }
            if sleep_between {
                sleep(Duration::from_millis(10));
            }
        }
        bail!("sensor mailbox busy")
    }

    /// Reads an image-sensor register. Mouse mode must be disabled.
    pub fn sensor_read(&self, addr: u16) -> Result<u16> {
        let _g = self.lock.lock();
        self.mailbox_wait_idle(true)?;
        self.write_raw(reg::MAILBOX_ADDR, addr)?;
        self.write_raw(reg::MAILBOX_CMD, 2)?;
        self.mailbox_wait_idle(false)?;
        self.read_raw(reg::MAILBOX_DATA)
    }

    /// Writes an image-sensor register. Mouse mode must be disabled.
    pub fn sensor_write(&self, addr: u16, val: u16) -> Result<()> {
        let _g = self.lock.lock();
        self.mailbox_wait_idle(true)?;
        self.write_raw(reg::MAILBOX_ADDR, addr)?;
        self.write_raw(reg::MAILBOX_DATA, val)?;
        self.write_raw(reg::MAILBOX_CMD, 1)
    }

    pub fn sensor_write_verified(&self, addr: u16, val: u16) -> Result<()> {
        self.sensor_write(addr, val)?;
        for _ in 0..10 {
            if self.sensor_read(addr)? == val {
                return Ok(());
            }
            sleep(Duration::from_millis(10));
            self.sensor_write(addr, val)?;
            sleep(Duration::from_millis(10));
        }
        bail!("sensor register {addr:#x} did not accept value {val:#x}")
    }

    // ---------------------------------------------------------------- imaging parameters

    /// LED exposure time in milliseconds.
    pub fn set_exposure_ms(&self, ms: f64) -> Result<()> {
        let v = (ms.max(0.0) * EXPOSURE_UNITS_PER_MS + 0.5).floor().clamp(0.0, 255.0) as u16;
        self.write_verified(reg::EXPOSURE, v)
    }

    pub fn exposure_ms(&self) -> Result<f64> {
        Ok(self.read(reg::EXPOSURE)? as f64 / EXPOSURE_UNITS_PER_MS)
    }

    /// Laser read position, 0.0..=1.0 (only valid outside scan mode).
    pub fn set_laser_read_position(&self, pos: f64) -> Result<()> {
        let v = (pos.clamp(0.0, 1.0) * 128.0 + 0.5) as u16;
        self.write_verified(reg::LASER_READ_POS, v)
    }

    /// Analog gain (1.0 ..= 32.0) in OmniVision register format.
    pub fn set_gain(&self, gain: f64) -> Result<()> {
        self.sensor_write_verified(sensor::GAIN, encode_gain(gain))
    }

    /// White-balance channel gains (raw sensor values 0..=255).
    pub fn set_white_balance(&self, blue: u8, red: u8, green: u8) -> Result<()> {
        self.sensor_write_verified(sensor::BLUE, blue as u16)?;
        self.sensor_write_verified(sensor::RED, red as u16)?;
        self.sensor_write_verified(sensor::GREEN, green as u16)
    }

    pub fn white_balance(&self) -> Result<(u8, u8, u8)> {
        Ok((
            self.sensor_read(sensor::BLUE)? as u8,
            self.sensor_read(sensor::RED)? as u8,
            self.sensor_read(sensor::GREEN)? as u8,
        ))
    }

    // ---------------------------------------------------------------- EEPROM

    /// Reads `len` bytes of the EEPROM starting at `addr` (selector 3 state
    /// machine). Mouse mode should be disabled.
    pub fn eeprom_read(&self, addr: u16, len: usize) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let _g = self.lock.lock();
        let size = self.xu.len(SEL_EEPROM)? as usize;
        if size < 2 {
            bail!("unexpected EEPROM selector size {size}");
        }
        let end = addr.wrapping_add(len as u16 - 1);
        let mut b = vec![0u8; size];
        b[0] = 1;
        b[1..3].copy_from_slice(&addr.to_le_bytes());
        b[3..5].copy_from_slice(&end.to_le_bytes());
        self.xu.set(SEL_EEPROM, &b)?;
        self.xu.get(SEL_EEPROM, &mut b)?;
        let cmd = |c: u8| -> Result<()> {
            let mut b = vec![0u8; size];
            b[0] = c;
            self.xu.set(SEL_EEPROM, &b)
        };
        cmd(2)?;
        cmd(3)?;
        let mut out = Vec::with_capacity(len + size);
        let result = (|| -> Result<()> {
            while out.len() < len {
                let mut chunk = vec![0u8; size];
                self.xu.get(SEL_EEPROM, &mut chunk)?;
                out.extend_from_slice(&chunk[1..]);
            }
            Ok(())
        })();
        let close = cmd(5);
        result?;
        close?;
        out.truncate(len);
        Ok(out)
    }

    /// Reads the calibration blob stored by the original software.
    pub fn read_user_data(&self) -> Result<Option<Vec<u8>>> {
        let hdr = self.eeprom_read(0x3000, 2)?;
        let n = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
        if n == 0 || n > 20_000 {
            return Ok(None);
        }
        Ok(Some(self.eeprom_read(0x3006, n)?))
    }

    pub fn read_mouse_id(&self) -> Result<Vec<u8>> {
        self.eeprom_read(0x7eb5, 0x40)
    }

    // ---------------------------------------------------------------- security check

    /// Challenge/response authentication of the mouse (`DaPerformSecurityCheck`).
    /// Returns `true` when the mouse answered correctly. Leaves the mouse with
    /// mouse mode enabled.
    pub fn security_check(&self, seed: u64) -> Result<bool> {
        let mut rng = seed | 1;
        let mut challenge = [0u8; 5];
        for c in &mut challenge {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            *c = rng as u8;
        }
        for (i, &c) in challenge.iter().enumerate() {
            self.write_verified(reg::CHALLENGE + i as u16, c as u16)?;
        }
        self.mode_command(mode::SECURITY_A)?;
        sleep(Duration::from_millis(100));
        let expected = security_transform(&challenge);
        self.mode_command(mode::SECURITY_B)?;
        let mut response = [0u8; 5];
        for (i, r) in response.iter_mut().enumerate() {
            *r = self.read(reg::RESPONSE + i as u16)? as u8;
        }
        let ok = response == expected;
        let confirm = security_transform(&expected);
        for (i, &c) in confirm.iter().enumerate() {
            self.write_verified(reg::CHALLENGE + i as u16, c as u16)?;
        }
        self.mode_command(mode::SECURITY_A)?;
        sleep(Duration::from_millis(100));
        self.enable_mouse()?;
        Ok(ok)
    }
}

/// Gain encoding of the image sensor (OmniVision AGC format).
pub fn encode_gain(gain: f64) -> u16 {
    let mut g = gain;
    let mut bits: u16 = 0;
    if g > 0.0 {
        let reference = if g < 16.0 { g } else { 16.0 };
        if g >= 16.0 {
            bits = 0x80;
            g *= 0.5;
        }
        if reference >= 8.0 {
            bits |= 0x40;
            g *= 0.5;
        }
        if reference >= 4.0 {
            bits |= 0x20;
            g *= 0.5;
        }
        if reference >= 2.0 {
            bits |= 0x10;
            g *= 0.5;
        }
    }
    if g < 2.0 {
        let frac = (g - 1.0).max(0.0);
        bits |= (frac * 16.0) as u16;
    } else {
        bits |= 0x0f;
    }
    bits
}

#[inline]
fn rol(v: u8, n: u32) -> u8 {
    v.rotate_left(n)
}

/// Response function of the security handshake (from `DaPerformSecurityCheck`).
pub fn security_transform(c: &[u8; 5]) -> [u8; 5] {
    [
        rol(c[4], 4) ^ rol(c[3], 2) ^ rol(c[0], 1) ^ c[1],
        rol(c[3], 6) ^ rol(c[1], 6) ^ rol(c[0], 3) ^ c[4],
        rol(c[2], 4),
        rol(c[3], 2) ^ rol(c[0], 1) ^ c[4] ^ c[1],
        rol(c[4], 4) ^ rol(c[1], 7) ^ c[3] ^ c[0],
    ]
}

/// Convenience: an error used when the mouse is not plugged in.
pub fn not_found() -> anyhow::Error {
    anyhow!("Scanner Mouse (USB 274f:c001) not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_encoding() {
        assert_eq!(encode_gain(1.0), 0x00);
        assert_eq!(encode_gain(1.5), 0x08);
        assert_eq!(encode_gain(2.0), 0x10);
        assert_eq!(encode_gain(4.0), 0x30);
        assert_eq!(encode_gain(16.0), 0xf0);
        assert_eq!(encode_gain(32.0), 0xff);
    }
}
