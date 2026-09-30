//! The Apple Studio Display, which does not answer DDC/CI at all.
//!
//! It exposes brightness as a USB HID feature report instead. `ddcutil detect`
//! lists it as "Invalid display", which is what sends people looking for a
//! vendor tool; there is no need for one, because the display describes the
//! control itself in its HID report descriptor:
//!
//! ```text
//! 05 80              Usage Page (Monitor)
//! 09 01              Usage (Monitor Control)
//! a1 01              Collection (Application)
//! 85 01                Report ID 1
//! 06 82 00             Usage Page (Monitor Enumerated Values)
//! 09 10                Usage 0x10 — the VESA VCP code for Brightness
//! 16 90 01             Logical Minimum 400
//! 27 60 ea 00 00       Logical Maximum 60000
//! 55 0e                Unit Exponent -2   → 4.00 … 600.00 nits
//! 75 20  95 01         one 32-bit field
//! b1 42                Feature
//! ```
//!
//! So the report is seven bytes — id, a little-endian `u32` of centinits, and a
//! trailing `u16` the display leaves at zero — and the range is the panel's own
//! 600-nit spec rather than an arbitrary scale.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Apple's vendor id and the Studio Display's product id, as hidraw spells them
/// in `HID_ID=0003:000005AC:00001114`.
const HID_ID: &str = "0003:000005AC:00001114";

/// The display reports brightness in hundredths of a nit, and will not accept
/// a value outside this range.
const RAW_MIN: u32 = 400;
const RAW_MAX: u32 = 60_000;

const REPORT_ID: u8 = 1;
const REPORT_LEN: usize = 7;

/// `_IOC(dir, type, nr, size)`, the encoding Linux uses for ioctl numbers.
fn ioc(dir: u32, kind: u8, nr: u8, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u32) << 16) | ((kind as u32) << 8) | nr as u32) as libc::c_ulong
}

const READ_WRITE: u32 = 3;
const HID: u8 = b'H';

fn get_feature(len: usize) -> libc::c_ulong {
    ioc(READ_WRITE, HID, 0x07, len)
}

fn set_feature(len: usize) -> libc::c_ulong {
    ioc(READ_WRITE, HID, 0x06, len)
}

/// A Studio Display, already located.
pub struct StudioDisplay {
    node: PathBuf,
}

impl StudioDisplay {
    /// Finds the one hidraw node that carries the brightness control.
    ///
    /// The display presents five of them and only one answers, so they are told
    /// apart by their report descriptor rather than by number: the node whose
    /// descriptor opens with usage page `Monitor` is the one. Hardcoding
    /// `/dev/hidraw11` works right up until something is replugged.
    pub fn find() -> Result<Option<Self>> {
        let Ok(entries) = std::fs::read_dir("/sys/class/hidraw") else {
            return Ok(None);
        };
        let mut candidates: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let sysfs = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_studio_display(&sysfs) {
                continue;
            }
            if !describes_brightness(&sysfs) {
                continue;
            }
            candidates.push(Path::new("/dev").join(name));
        }
        // Deterministic across runs; several displays would be a different
        // feature, and picking at random would be worse than picking the first.
        candidates.sort();
        Ok(candidates.into_iter().next().map(|node| Self { node }))
    }

    fn open(&self) -> Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.node)
            .with_context(|| {
                format!(
                    "opening {} — logind normally grants the active session \
                     access to it; check you are on the seat that owns the display",
                    self.node.display()
                )
            })
    }

    /// Brightness as a percentage of the panel's usable range.
    pub fn get(&self) -> Result<u8> {
        Ok(to_percent(self.raw()?))
    }

    fn raw(&self) -> Result<u32> {
        let file = self.open()?;
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        let rc = unsafe {
            libc::ioctl(file.as_raw_fd(), get_feature(REPORT_LEN), buf.as_mut_ptr())
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error())
                .context("reading the brightness feature report");
        }
        Ok(u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]))
    }

    pub fn set(&self, percent: u8) -> Result<()> {
        let raw = from_percent(percent);
        let file = self.open()?;
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[1..5].copy_from_slice(&raw.to_le_bytes());
        let rc =
            unsafe { libc::ioctl(file.as_raw_fd(), set_feature(REPORT_LEN), buf.as_ptr()) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error())
                .context("writing the brightness feature report");
        }
        Ok(())
    }

    /// What the panel is actually emitting, which is the number on its spec
    /// sheet rather than a percentage of it.
    pub fn nits(&self) -> Result<f64> {
        Ok(f64::from(self.raw()?) / 100.0)
    }
}

fn is_studio_display(sysfs: &Path) -> bool {
    let Ok(uevent) = std::fs::read_to_string(sysfs.join("device/uevent")) else {
        return false;
    };
    uevent
        .lines()
        .any(|line| line.strip_prefix("HID_ID=").is_some_and(|id| id == HID_ID))
}

/// Whether this interface is the one describing a monitor brightness control.
///
/// Matched on the opening bytes of the report descriptor — usage page `Monitor`
/// (0x80), usage `Monitor Control` — which is what separates the one useful
/// node from the display's four others.
fn describes_brightness(sysfs: &Path) -> bool {
    let Ok(mut file) = File::open(sysfs.join("device/report_descriptor")) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return false;
    }
    bytes.starts_with(&[0x05, 0x80, 0x09, 0x01])
}

fn to_percent(raw: u32) -> u8 {
    let raw = raw.clamp(RAW_MIN, RAW_MAX);
    let span = f64::from(RAW_MAX - RAW_MIN);
    ((f64::from(raw - RAW_MIN) / span) * 100.0).round() as u8
}

fn from_percent(percent: u8) -> u32 {
    let percent = f64::from(percent.min(100)) / 100.0;
    RAW_MIN + (percent * f64::from(RAW_MAX - RAW_MIN)).round() as u32
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ends_of_the_range_are_exact() {
        // 0% must be the display's own minimum rather than off, and 100% its
        // maximum rather than one short of it.
        assert_eq!(from_percent(0), RAW_MIN);
        assert_eq!(from_percent(100), RAW_MAX);
        assert_eq!(to_percent(RAW_MIN), 0);
        assert_eq!(to_percent(RAW_MAX), 100);
    }

    #[test]
    fn a_percentage_survives_the_round_trip() {
        for percent in 0..=100u8 {
            assert_eq!(to_percent(from_percent(percent)), percent, "at {percent}%");
        }
    }

    #[test]
    fn values_outside_the_range_are_pulled_back_in() {
        // The display rejects anything outside it, so sending 0 would fail
        // rather than dim.
        assert_eq!(from_percent(200), RAW_MAX);
        assert_eq!(to_percent(0), 0);
        assert_eq!(to_percent(u32::MAX), 100);
    }

    #[test]
    fn the_ioctl_numbers_match_the_kernels() {
        // HIDIOCGFEATURE(7) and HIDIOCSFEATURE(7) as the kernel defines them;
        // wrong numbers here fail as a confusing EINVAL at runtime.
        assert_eq!(get_feature(7), 0xC0074807);
        assert_eq!(set_feature(7), 0xC0074806);
    }

    #[test]
    fn nits_are_hundredths() {
        assert_eq!(to_percent(30_200), 50);
    }
}
