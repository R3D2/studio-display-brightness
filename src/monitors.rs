//! Which screens exist, what each one is, and which one you are looking at.
//!
//! The ordering here is the whole performance story. Asking the compositor
//! which monitor is focused costs about 7ms; asking ddcutil which I2C bus a
//! monitor answers on costs about 19 seconds on this hardware. So nothing
//! touches ddcutil until something actually needs a bus, and a Studio Display
//! never does.

use crate::{cache, ddc::DdcDisplay, hid::StudioDisplay};
use anyhow::{anyhow, Context, Result};
use std::process::Command;

/// How a display's brightness is reached.
pub enum Backend {
    /// A Studio Display, over its USB HID feature report.
    Hid(StudioDisplay),
    /// Anything that answers DDC/CI.
    Ddc(DdcDisplay),
}

pub struct Display {
    /// The compositor's name for it: `DP-1`, `DP-4`.
    pub connector: String,
    /// What it calls itself, for the tooltip.
    pub description: String,
    pub focused: bool,
    pub backend: Backend,
}

impl Display {
    pub fn get(&self) -> Result<u8> {
        match &self.backend {
            Backend::Hid(d) => d.get(),
            Backend::Ddc(d) => d.get(),
        }
        .with_context(|| format!("reading the brightness of {}", self.connector))
    }

    /// The level, preferring what was last recorded when reading is expensive.
    ///
    /// A Studio Display is read outright: it takes a few milliseconds and is
    /// always right. A DDC monitor takes about a second, which is the
    /// difference between a brightness key that responds and one that does not,
    /// so a remembered level wins there. The cost is that a level changed with
    /// the monitor's own buttons is not noticed until something reads it again.
    pub fn get_cached(&self) -> Result<u8> {
        if matches!(self.backend, Backend::Hid(_)) {
            return self.get();
        }
        if let Some(level) = cache::load().levels.get(&self.connector).copied() {
            return Ok(level);
        }
        self.get()
    }

    pub fn set(&self, percent: u8) -> Result<()> {
        match &self.backend {
            Backend::Hid(d) => d.set(percent),
            Backend::Ddc(d) => d.set(percent),
        }
        .with_context(|| format!("setting the brightness of {}", self.connector))?;

        let mut cache = cache::load();
        cache.levels.insert(self.connector.clone(), percent);
        cache::save(&cache);
        Ok(())
    }

    /// A short name for a tooltip: "Studio Display", not the full EDID string.
    pub fn short_name(&self) -> String {
        if matches!(self.backend, Backend::Hid(_)) {
            return "Studio Display".to_string();
        }
        // EDID descriptions read "Dell Inc. U2720Q CFV9N13" — the model is the
        // part worth showing; the maker is noise and the serial is not for
        // people.
        self.description
            .split_whitespace()
            .find(|word| word.len() > 3 && word.chars().any(|c| c.is_ascii_digit()))
            .unwrap_or(&self.description)
            .to_string()
    }
}

struct Monitor {
    name: String,
    description: String,
    focused: bool,
}

fn is_apple_display(description: &str) -> bool {
    description.contains("StudioDisplay")
        || (description.contains("Apple") && description.contains("Studio"))
}

/// Builds one display, paying for a bus lookup only if this one needs it.
fn build(monitor: &Monitor, all_names: &[String]) -> Result<Option<Display>> {
    let backend = if is_apple_display(&monitor.description) {
        match StudioDisplay::find()? {
            Some(d) => Backend::Hid(d),
            None => return Ok(None),
        }
    } else {
        match bus_for(&monitor.name, all_names) {
            Some(bus) => Backend::Ddc(DdcDisplay::on_bus(bus)),
            // Not a Studio Display and not answering DDC: a laptop panel, or a
            // monitor with DDC/CI switched off in its menu. Listing it with no
            // way to change it would only be confusing.
            None => return Ok(None),
        }
    };
    Ok(Some(Display {
        connector: monitor.name.clone(),
        description: monitor.description.clone(),
        focused: monitor.focused,
        backend,
    }))
}

/// Which I2C bus a connector answers on, from the cache where possible.
///
/// The slow path runs `ddcutil detect` once and remembers the answer for every
/// connector it found, so the cost is paid on the first DDC change after a boot
/// rather than on every keypress.
fn bus_for(connector: &str, all_names: &[String]) -> Option<u32> {
    let print = cache::fingerprint(all_names);
    let mut cached = cache::load();
    if cached.fingerprint == print {
        if let Some(bus) = cached.buses.get(connector).copied() {
            return Some(bus);
        }
        // A connector known to be absent from the last detect is absent still;
        // re-probing every time would cost 19 seconds to learn nothing.
        if !cached.buses.is_empty() {
            return None;
        }
    }

    let found = crate::ddc::buses_by_connector().unwrap_or_default();
    cached.fingerprint = print;
    cached.buses = found.iter().cloned().collect();
    cache::save(&cached);
    found
        .into_iter()
        .find(|(name, _)| name == connector)
        .map(|(_, bus)| bus)
}

/// Every display we can actually change.
pub fn all() -> Result<Vec<Display>> {
    let monitors = hyprland_monitors()?;
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let mut displays = Vec::new();
    for monitor in &monitors {
        if let Some(display) = build(monitor, &names)? {
            displays.push(display);
        }
    }
    Ok(displays)
}

/// The display you are looking at.
///
/// With focus-follows-mouse — the Hyprland default — the focused monitor is the
/// one under the pointer, which is the one whose bar you just scrolled on. That
/// is what makes an unqualified `up` do the obvious thing.
///
/// Only the focused monitor is built, so scrolling a Studio Display's bar never
/// waits on ddcutil for a monitor it is not touching.
pub fn focused() -> Result<Display> {
    let monitors = hyprland_monitors()?;
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let monitor = monitors
        .iter()
        .find(|m| m.focused)
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow!("the compositor reports no monitors"))?;
    build(monitor, &names)?
        .ok_or_else(|| anyhow!("{} has no brightness control", monitor.name))
}

pub fn by_name(name: &str) -> Result<Display> {
    let monitors = hyprland_monitors()?;
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let monitor = monitors
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("no display called {name}; there is {}", names.join(", ")))?;
    build(monitor, &names)?
        .ok_or_else(|| anyhow!("{} has no brightness control", monitor.name))
}

/// Just the name of the focused screen.
pub fn focused_name() -> Result<String> {
    hyprland_monitors()?
        .into_iter()
        .find(|m| m.focused)
        .map(|m| m.name)
        .ok_or_else(|| anyhow!("the compositor reports no focused monitor"))
}

fn hyprland_monitors() -> Result<Vec<Monitor>> {
    let out = Command::new("hyprctl")
        .args(["-j", "monitors"])
        .output()
        .context("asking hyprctl which monitors exist")?;
    let parsed: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("hyprctl did not answer with the monitor list")?;
    Ok(parsed
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|m| Monitor {
                    name: m["name"].as_str().unwrap_or_default().to_string(),
                    description: m["description"].as_str().unwrap_or_default().to_string(),
                    focused: m["focused"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default())
}
