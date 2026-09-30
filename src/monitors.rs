//! Which screens exist, what each one is, and which one you are looking at.
//!
//! The ordering here is the whole performance story. Asking the compositor
//! which monitor is focused costs about 7ms; asking ddcutil which I2C bus a
//! monitor answers on costs about 19 seconds on this hardware. So nothing
//! touches ddcutil until something actually needs a bus, and a Studio Display
//! never does.

use crate::percent::Percent;
use crate::{cache, ddc::DdcDisplay, hid::StudioDisplay};
use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

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
    pub backend: Backend,
}

impl Display {
    /// Reads the hardware. Slow over DDC — about a second — and a few
    /// milliseconds over USB HID.
    pub fn get(&self) -> Result<Percent> {
        match &self.backend {
            Backend::Hid(d) => d.get(),
            Backend::Ddc(d) => d.get(),
        }
        .with_context(|| format!("reading the brightness of {}", self.connector))
    }

    /// Writes the hardware. Does not touch the cache: the caller holds the lock
    /// and records the level, so that the read, the write and the remembering
    /// are one operation rather than three racing ones.
    pub fn apply(&self, percent: Percent) -> Result<()> {
        match &self.backend {
            Backend::Hid(d) => d.set(percent),
            Backend::Ddc(d) => d.set(percent),
        }
        .with_context(|| format!("setting the brightness of {}", self.connector))
    }

    /// Whether reading this display is cheap enough to always prefer to a
    /// remembered value.
    pub const fn reads_cheaply(&self) -> bool {
        matches!(self.backend, Backend::Hid(_))
    }

    /// Sets the level and records it, taking the lock for both.
    pub fn set(&self, percent: Percent) -> Result<()> {
        cache::update(|cache| {
            self.apply(percent)?;
            cache.levels.insert(self.connector.clone(), percent.get());
            Ok(())
        })
    }

    /// Moves by `delta`, clamped into 0..=100, and reports where it landed.
    ///
    /// The read, the write and the record all happen under one lock. Without
    /// that, every notch of a held scroll wheel reads the same level, computes
    /// the same target, and the brightness moves one step no matter how long
    /// you scroll.
    pub fn nudge(&self, delta: i16) -> Result<Percent> {
        cache::update(|cache| {
            let now = if self.reads_cheaply() {
                self.get()?
            } else {
                // A DDC read is a second, which is the difference between a
                // brightness key that responds and one that does not. The cost
                // is that a level changed on the monitor's own buttons is not
                // noticed until something reads it again.
                match cache.levels.get(&self.connector).copied() {
                    Some(level) => Percent::try_from(level).unwrap_or(Percent::MAX),
                    None => self.get()?,
                }
            };
            let next = now.stepped(delta);
            self.apply(next)?;
            cache.levels.insert(self.connector.clone(), next.get());
            Ok(next)
        })
    }

    /// A short name for a tooltip: "Studio Display", not the full EDID string.
    pub fn short_name(&self) -> String {
        if self.reads_cheaply() {
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

#[derive(serde::Deserialize)]
struct Monitor {
    name: String,
    description: String,
    #[serde(default)]
    focused: bool,
}

fn is_apple_display(description: &str) -> bool {
    description.contains("StudioDisplay")
        || (description.contains("Apple") && description.contains("Studio"))
}

/// Builds one display, paying for a bus lookup only if this one needs it.
fn build(monitor: &Monitor, all_names: &[String]) -> Option<Display> {
    let backend = if is_apple_display(&monitor.description) {
        Backend::Hid(StudioDisplay::find()?)
    } else {
        // Not a Studio Display and not answering DDC: a laptop panel, or one
        // with DDC/CI switched off in its menu. Listing it with no way to
        // change it would only be confusing.
        Backend::Ddc(DdcDisplay::on_bus(bus_for(&monitor.name, all_names)?))
    };
    Some(Display {
        connector: monitor.name.clone(),
        description: monitor.description.clone(),
        backend,
    })
}

/// Which I2C bus a connector answers on, from the cache where possible.
///
/// The slow path runs `ddcutil detect` once and remembers the answer for every
/// connector it found, so the cost is paid on the first DDC change after a boot
/// rather than on every keypress.
fn bus_for(connector: &str, all_names: &[String]) -> Option<u32> {
    let print = cache::fingerprint(all_names);
    let known = cache::read();
    if known.fingerprint == print {
        if let Some(bus) = known.buses.get(connector).copied() {
            return Some(bus);
        }
        // A completed probe that did not find it will not find it now either,
        // and looking again costs nineteen seconds to learn the same thing.
        if known.without_ddc.iter().any(|name| name == connector) {
            return None;
        }
    }

    // The slow path, under the lock so two processes do not probe at once and
    // so the result cannot be overwritten by a process that loaded the cache
    // before the probe started.
    cache::update(|cache| {
        // Another process may have probed while this one waited for the lock.
        if cache.fingerprint == print {
            if let Some(bus) = cache.buses.get(connector).copied() {
                return Ok(Some(bus));
            }
            if cache.without_ddc.iter().any(|name| name == connector) {
                return Ok(None);
            }
        }

        let found = crate::ddc::buses_by_connector()?;
        cache.fingerprint.clone_from(&print);
        cache.buses = found.iter().cloned().collect();
        // Only a probe that completed can say a connector has no DDC. One that
        // failed leaves the question open rather than pinning a wrong answer
        // until the monitors change.
        cache.without_ddc = all_names
            .iter()
            .filter(|name| !cache.buses.contains_key(*name))
            .cloned()
            .collect();
        Ok(cache.buses.get(connector).copied())
    })
    .unwrap_or(None)
}

/// Every display we can actually change.
pub fn all() -> Result<Vec<Display>> {
    let monitors = hyprland_monitors()?;
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let mut displays = Vec::new();
    for monitor in &monitors {
        if let Some(display) = build(monitor, &names) {
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
    build(monitor, &names).ok_or_else(|| anyhow!("{} has no brightness control", monitor.name))
}

pub fn by_name(name: &str) -> Result<Display> {
    let monitors = hyprland_monitors()?;
    let names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    let monitor = monitors
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("no display called {name}; there is {}", names.join(", ")))?;
    build(monitor, &names).ok_or_else(|| anyhow!("{} has no brightness control", monitor.name))
}

/// Just the name of the focused screen.
pub fn focused_name() -> Result<String> {
    hyprland_monitors()?
        .into_iter()
        .find(|m| m.focused)
        .map(|m| m.name)
        .ok_or_else(|| anyhow!("the compositor reports no focused monitor"))
}

/// Asks the compositor what is attached.
///
/// Everything here needs this, including `--display`: the connector names and
/// the descriptions that decide USB HID against DDC both come from it. There is
/// no mode that works without a compositor to ask.
fn hyprland_monitors() -> Result<Vec<Monitor>> {
    // Hyprland's own IPC socket first. `watch` asks this four times a second
    // forever, and a Unix socket round trip is a fraction of the cost of
    // forking hyprctl to do exactly the same thing.
    // The socket layout is Hyprland's to change, and hyprctl knows how to find
    // it whatever it becomes. Falling back costs a fork on a machine where the
    // fast path stopped working, rather than the tool stopping.
    ask_hyprland("j/monitors").map_or_else(
        |_| hyprctl_monitors(),
        |json| serde_json::from_slice(&json).context("Hyprland did not answer with a monitor list"),
    )
}

/// The path Hyprland listens on for this session.
fn hyprland_socket() -> Result<PathBuf> {
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
        .context("HYPRLAND_INSTANCE_SIGNATURE is not set")?;
    Ok(crate::cache::dir()?
        .join("hypr")
        .join(signature)
        .join(".socket.sock"))
}

/// Sends one command and reads the whole answer.
///
/// The protocol is as simple as it looks: write the request, read until the
/// compositor closes its side.
fn ask_hyprland(request: &str) -> Result<Vec<u8>> {
    let mut socket = UnixStream::connect(hyprland_socket()?).context("connecting to Hyprland")?;
    // Neither side of this should ever block for long, and `watch` has one
    // thread: a compositor that has stopped answering must not take the bar
    // down with it.
    let limit = Duration::from_secs(2);
    socket.set_read_timeout(Some(limit))?;
    socket.set_write_timeout(Some(limit))?;
    socket
        .write_all(request.as_bytes())
        .context("asking Hyprland")?;
    socket.flush()?;
    let mut answer = Vec::new();
    socket
        .read_to_end(&mut answer)
        .context("reading Hyprland's answer")?;
    Ok(answer)
}

fn hyprctl_monitors() -> Result<Vec<Monitor>> {
    let out = Command::new("hyprctl")
        .args(["-j", "monitors"])
        .output()
        .context("running hyprctl — this reads the monitor list from Hyprland")?;
    if !out.status.success() {
        // hyprctl says nothing on stderr when it cannot find the compositor,
        // so a bare "hyprctl failed:" would be the whole message.
        let complaint = String::from_utf8_lossy(&out.stderr);
        let complaint = complaint.trim();
        return Err(anyhow!(
            "hyprctl {}{}{}",
            out.status,
            if complaint.is_empty() { "" } else { ": " },
            complaint
        ));
    }
    serde_json::from_slice(&out.stdout).context("hyprctl did not answer with a monitor list")
}
