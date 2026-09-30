//! Everything that is not a Studio Display, over DDC/CI.
//!
//! This drives `ddcutil` rather than speaking I²C directly. The protocol is
//! simple; the monitors are not, and ddcutil carries years of per-model quirks,
//! retries and timing workarounds that would otherwise have to be rediscovered
//! one display at a time.
//!
//! A read costs roughly a third of a second, which is why nothing here is
//! called on a timer — see `cache`.

use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long any one ddcutil call may take before it is given up on.
///
/// A wedged I2C bus makes ddcutil block rather than fail, and `watch` calls
/// this from its only thread: without a bound, one sulking monitor stops the
/// bar updating for every display. Generous enough that a slow `detect` on a
/// machine with many buses still completes.
const PATIENCE: Duration = Duration::from_secs(30);

/// A monitor reachable over DDC, identified by the I²C bus it answers on.
pub struct DdcDisplay {
    bus: u32,
}

/// VESA VCP feature code for brightness. Standard across every monitor that
/// implements DDC/CI at all.
const BRIGHTNESS: &str = "10";

impl DdcDisplay {
    pub const fn on_bus(bus: u32) -> Self {
        Self { bus }
    }

    pub fn get(&self) -> Result<u8> {
        let out = ddcutil(&["--bus", &self.bus.to_string(), "getvcp", BRIGHTNESS])?;
        parse_current(&out)
            .ok_or_else(|| anyhow!("could not read a brightness value out of: {}", out.trim()))
    }

    pub fn set(&self, percent: u8) -> Result<()> {
        // DDC brightness is already a percentage on every display that reports
        // a maximum of 100, which is all of them in practice.
        ddcutil(&[
            "--bus",
            &self.bus.to_string(),
            "setvcp",
            BRIGHTNESS,
            &percent.min(100).to_string(),
        ])?;
        Ok(())
    }
}

fn ddcutil(args: &[&str]) -> Result<String> {
    let mut child = Command::new("ddcutil")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running ddcutil — it is what talks to DDC monitors, and must be installed")?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < PATIENCE => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                // Killed rather than waited on: the caller is a keypress or a
                // status bar, and neither can afford to block indefinitely on a
                // monitor that has stopped answering.
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "ddcutil {} gave no answer in {}s — the monitor may have DDC/CI \
                     switched off, or its I2C bus may be wedged",
                    args.join(" "),
                    PATIENCE.as_secs()
                ));
            }
            Err(e) => return Err(e).context("waiting for ddcutil"),
        }
    }

    let out = child
        .wait_with_output()
        .context("collecting ddcutil's output")?;
    if !out.status.success() {
        return Err(anyhow!(
            "ddcutil {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pulls the number out of ddcutil's `getvcp` line.
///
/// Its output is meant for people:
/// `VCP code 0x10 (Brightness    ): current value =    60, max value =   100`
fn parse_current(text: &str) -> Option<u8> {
    let (_, rest) = text.split_once("current value =")?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Which I²C bus each DRM connector answers on, from `ddcutil detect`.
///
/// The connector name is the link back to the compositor: Hyprland calls the
/// monitor `DP-4` and ddcutil calls it `card1-DP-4`, and matching on that is
/// what lets a scroll on one screen change that screen rather than a guess.
pub fn buses_by_connector() -> Result<Vec<(String, u32)>> {
    let text = ddcutil(&["detect", "--brief"])?;
    let mut found = Vec::new();
    let mut bus: Option<u32> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(path) = line.strip_prefix("I2C bus:") {
            bus = path.trim().rsplit('-').next().and_then(|n| n.parse().ok());
        } else if let Some(connector) = line.strip_prefix("DRM connector:") {
            // "card1-DP-4" — the card prefix is the GPU, which is not ours to
            // care about; the connector after it is what Hyprland names.
            let connector = connector.trim();
            let short = connector
                .split_once('-')
                .map_or_else(|| connector.to_string(), |(_, rest)| rest.to_string());
            if let Some(bus) = bus.take() {
                found.push((short, bus));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_getvcp_line_yields_its_number() {
        let line = "VCP code 0x10 (Brightness                    ): \
                    current value =    60, max value =   100";
        assert_eq!(parse_current(line), Some(60));
    }

    #[test]
    fn the_maximum_is_not_mistaken_for_the_current_value() {
        // Both numbers are on the line and the wrong one is always 100, which
        // would look plausible and be wrong.
        let line = "current value =     0, max value =   100";
        assert_eq!(parse_current(line), Some(0));
    }

    #[test]
    fn output_without_a_value_is_refused_rather_than_guessed() {
        assert_eq!(parse_current("Display not found"), None);
        assert_eq!(parse_current(""), None);
        assert_eq!(parse_current("current value = "), None);
    }
}
