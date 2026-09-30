//! Which screens exist, what each one is, and which one you are looking at.

use crate::{ddc::DdcDisplay, hid::StudioDisplay};
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

    pub fn set(&self, percent: u8) -> Result<()> {
        match &self.backend {
            Backend::Hid(d) => d.set(percent),
            Backend::Ddc(d) => d.set(percent),
        }
        .with_context(|| format!("setting the brightness of {}", self.connector))
    }

    /// A short name for a tooltip: "Studio Display", not the full EDID string.
    pub fn short_name(&self) -> String {
        if self.is_studio_display() {
            return "Studio Display".to_string();
        }
        // "ASUSTek COMPUTER INC PG27UCDM T1LMAS012351" — the model is the part
        // worth showing; the maker is noise and the serial is not for people.
        self.description
            .split_whitespace()
            .find(|word| word.len() > 3 && word.chars().any(|c| c.is_ascii_digit()))
            .unwrap_or(&self.description)
            .to_string()
    }

    fn is_studio_display(&self) -> bool {
        matches!(self.backend, Backend::Hid(_))
    }
}

/// Every display we can actually change, newest state each call.
pub fn all() -> Result<Vec<Display>> {
    let monitors = hyprland_monitors()?;
    // One lookup for the whole set: `ddcutil detect` probes every bus and is
    // far too slow to call per display.
    let buses = crate::ddc::buses_by_connector().unwrap_or_default();
    let studio = StudioDisplay::find()?;
    let mut studio = studio;

    let mut displays = Vec::new();
    for m in monitors {
        let apple = m.description.contains("StudioDisplay")
            || (m.description.contains("Apple") && m.description.contains("Studio"));
        let backend = if apple {
            // Taken rather than cloned: with one Studio Display attached, the
            // second would otherwise silently drive the first.
            match studio.take() {
                Some(d) => Backend::Hid(d),
                None => continue,
            }
        } else {
            match buses.iter().find(|(c, _)| *c == m.name) {
                Some((_, bus)) => Backend::Ddc(DdcDisplay::on_bus(*bus)),
                // A laptop panel or anything that does not answer DDC. Listing
                // it with no way to change it would only be confusing.
                None => continue,
            }
        };
        displays.push(Display {
            connector: m.name,
            description: m.description,
            focused: m.focused,
            backend,
        });
    }
    Ok(displays)
}

/// The display you are looking at.
///
/// With focus-follows-mouse — the default across the wlroots family — the
/// focused monitor is the one under the pointer, which is the one whose bar you
/// just scrolled on. That is what makes an unqualified `up` do the obvious
/// thing rather than needing to name a screen.
pub fn focused() -> Result<Display> {
    let mut displays = all()?;
    if let Some(position) = displays.iter().position(|d| d.focused) {
        return Ok(displays.swap_remove(position));
    }
    displays
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no display with a brightness control was found"))
}

pub fn by_name(name: &str) -> Result<Display> {
    let displays = all()?;
    let known: Vec<String> = displays.iter().map(|d| d.connector.clone()).collect();
    displays
        .into_iter()
        .find(|d| d.connector.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("no display called {name}; there is {}", known.join(", ")))
}

struct Monitor {
    name: String,
    description: String,
    focused: bool,
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

/// Just the name of the focused screen.
///
/// Separate from `all` on purpose: `all` probes every I²C bus to find out what
/// answers DDC, which costs the best part of a second. Knowing where you are
/// looking is one IPC call to the compositor and is safe to ask constantly.
pub fn focused_name() -> Result<String> {
    hyprland_monitors()?
        .into_iter()
        .find(|m| m.focused)
        .map(|m| m.name)
        .ok_or_else(|| anyhow!("the compositor reports no focused monitor"))
}
