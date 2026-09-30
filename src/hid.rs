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

use crate::percent::Percent;
use anyhow::{anyhow, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// Apple's vendor id, as hidraw spells it in `HID_ID=0003:000005AC:00001114`.
const APPLE: &str = "0003:000005AC:";

/// The displays that carry this control: the Studio Display and the two Pro
/// Display XDR variants. The udev rule ships all three, so recognising only
/// the first would grant a Pro Display XDR access it then never uses.
const DISPLAY_PRODUCTS: [&str; 3] = ["00001114", "00001116", "00001118"];

/// The display reports brightness in hundredths of a nit, and will not accept
/// a value outside this range.
const RAW_MIN: u32 = 400;
const RAW_MAX: u32 = 60_000;

/// Usage Page (Monitor), Usage (Monitor Control) -- the opening of the one
/// interface that carries the brightness control.
const BRIGHTNESS_DESCRIPTOR_PREFIX: [u8; 4] = [0x05, 0x80, 0x09, 0x01];

const REPORT_ID: u8 = 1;
const REPORT_LEN: usize = 7;

/// `HIDIOCGFEATURE(7)` and `HIDIOCSFEATURE(7)`.
///
/// Built with libc's own `_IOWR` rather than by hand. The bit layout of an
/// ioctl request is not the same on every architecture — the direction bits
/// and the size field move on MIPS, PowerPC and SPARC — and the request type
/// is `c_ulong` against glibc but `c_int` against musl, so a hand-rolled
/// `c_ulong` would not even compile for a static musl build.
const HID: u32 = b'H' as u32;
const GET_FEATURE: libc::Ioctl = libc::_IOWR::<[u8; REPORT_LEN]>(HID, 0x07);
const SET_FEATURE: libc::Ioctl = libc::_IOWR::<[u8; REPORT_LEN]>(HID, 0x06);

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
    pub fn find() -> Option<Self> {
        let entries = std::fs::read_dir("/sys/class/hidraw").ok()?;
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
        candidates.into_iter().next().map(|node| Self { node })
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
    pub fn get(&self) -> Result<Percent> {
        Ok(Percent::scaled_from(self.raw()?, RAW_MIN, RAW_MAX))
    }

    fn raw(&self) -> Result<u32> {
        let file = self.open()?;
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        // SAFETY: `file` is an open hidraw node, so the descriptor is valid for
        // the duration of the call. HIDIOCGFEATURE writes back at most the
        // number of bytes encoded in the request — REPORT_LEN, the length of
        // `buf` — and `buf` is a live, uniquely borrowed, correctly aligned
        // array of that many bytes. Nothing here retains the pointer.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), GET_FEATURE, buf.as_mut_ptr()) };
        if rc < 0 {
            return Err(std::io::Error::last_os_error())
                .context("reading the brightness feature report");
        }
        // hidraw copies back however many bytes the report actually had. A
        // short one leaves the brightness field zeroed, and reporting 0% is
        // worse than reporting that the read went wrong.
        if rc < 5 {
            return Err(anyhow!(
                "the display answered with {rc} bytes; the brightness field needs 5"
            ));
        }
        Ok(u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]))
    }

    pub fn set(&self, percent: Percent) -> Result<()> {
        let raw = percent.scaled_into(RAW_MIN, RAW_MAX);
        let file = self.open()?;
        let mut buf = [0u8; REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[1..5].copy_from_slice(&raw.to_le_bytes());
        // SAFETY: as in `raw`. The pointer is `as_mut_ptr` rather than
        // `as_ptr` because HIDIOCSFEATURE is encoded `_IOC_READ|_IOC_WRITE`:
        // today's hidraw only reads from the buffer, but the request says the
        // kernel may write to it, and handing a write-permitted ioctl a
        // pointer derived from a shared borrow is not a promise worth making.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), SET_FEATURE, buf.as_mut_ptr()) };
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
    uevent.lines().any(|line| {
        line.strip_prefix("HID_ID=").is_some_and(|id| {
            id.strip_prefix(APPLE)
                .is_some_and(|product| DISPLAY_PRODUCTS.contains(&product))
        })
    })
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
    bytes.starts_with(&BRIGHTNESS_DESCRIPTOR_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ioctl_numbers_match_the_kernels() {
        // HIDIOCGFEATURE(7) and HIDIOCSFEATURE(7) as the kernel defines them.
        // Wrong numbers here are a bare EINVAL at runtime with nothing to
        // explain it, and libc builds these per architecture, so this pins the
        // value on the one that matters here.
        assert_eq!(GET_FEATURE, 0xC007_4807);
        assert_eq!(SET_FEATURE, 0xC007_4806);
    }

    #[test]
    fn the_report_is_the_length_the_descriptor_says() {
        // One byte of report id, a 32-bit brightness, and the 16-bit field the
        // display leaves at zero. The ioctl numbers above encode this length,
        // so the two must not drift apart.
        assert_eq!(REPORT_LEN, 1 + 4 + 2);
    }

    #[test]
    fn only_apple_displays_that_carry_this_control_are_recognised() {
        // The vendor prefix alone is not enough: a keyboard is 05ac too.
        let apple_display = format!("{APPLE}00001114");
        assert!(apple_display.starts_with(APPLE));
        assert!(DISPLAY_PRODUCTS.contains(&"00001114"));
        assert!(DISPLAY_PRODUCTS.contains(&"00001116"));
        assert!(DISPLAY_PRODUCTS.contains(&"00001118"));
        assert!(!DISPLAY_PRODUCTS.contains(&"00000250"));
    }

    #[test]
    fn the_descriptor_prefix_is_the_monitor_usage_page() {
        // 05 80 = Usage Page (Monitor), 09 01 = Usage (Monitor Control). This
        // is what separates the one node that answers from the display's four
        // others, so it is worth stating rather than leaving in a literal.
        assert_eq!(BRIGHTNESS_DESCRIPTOR_PREFIX, [0x05, 0x80, 0x09, 0x01]);
    }
}
