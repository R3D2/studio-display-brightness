//! Everything that is not a Studio Display, over DDC/CI.
//!
//! This drives `ddcutil` rather than speaking I²C directly. The protocol is
//! simple; the monitors are not, and ddcutil carries years of per-model quirks,
//! retries and timing workarounds that would otherwise have to be rediscovered
//! one display at a time.
//!
//! A read costs roughly a third of a second, which is why nothing here is
//! called on a timer — see `cache`.

use crate::percent::Percent;
use anyhow::{anyhow, Context, Result};
use std::process::{Command, Stdio};
use std::time::Duration;

/// How long any one ddcutil call may take before it is given up on.
///
/// A wedged I2C bus makes ddcutil block rather than fail, and `watch` calls
/// this from its only thread: without a bound, one sulking monitor stops the
/// bar updating for every display. Generous enough that a slow `detect` on a
/// machine with many buses still completes.
const PATIENCE: Duration = Duration::from_secs(5);

/// `detect` probes every bus on the machine and is legitimately slow — it
/// measured nineteen seconds here — so it gets its own, far longer bound.
const PATIENCE_DETECT: Duration = Duration::from_secs(45);

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

    pub fn get(&self) -> Result<Percent> {
        let out = ddcutil(
            &["--bus", &self.bus.to_string(), "getvcp", BRIGHTNESS],
            PATIENCE,
        )?;
        parse_current(&out)
            .ok_or_else(|| anyhow!("could not read a brightness value out of: {}", out.trim()))
    }

    pub fn set(&self, percent: Percent) -> Result<()> {
        // DDC brightness is already a percentage on every display that reports
        // a maximum of 100, which is all of them in practice.
        ddcutil(
            &[
                "--bus",
                &self.bus.to_string(),
                "setvcp",
                BRIGHTNESS,
                &percent.to_string(),
            ],
            PATIENCE,
        )?;
        Ok(())
    }
}

fn ddcutil(args: &[&str], patience: Duration) -> Result<String> {
    // `timeout` rather than a hand-rolled spawn/poll/kill loop. The hand-rolled
    // version has to leave stdout and stderr as pipes and cannot drain them
    // until the child exits — so a child that writes more than a pipe buffer
    // (64 KiB) blocks in write(), never exits, and gets killed and reported as
    // a wedged bus. ddcutil with tracing turned on through its config file or
    // DDCUTIL_* environment reaches that easily.
    //
    // -k gives it a couple of seconds to leave the I2C transaction tidy after
    // SIGTERM before SIGKILL lands.
    let seconds = patience.as_secs().to_string();
    let mut command = vec!["-k", "2", seconds.as_str(), "ddcutil"];
    command.extend_from_slice(args);

    let out = Command::new("timeout")
        .args(&command)
        .stdin(Stdio::null())
        .output()
        .context("running ddcutil — it is what talks to DDC monitors, and must be installed")?;

    // The exit status `timeout` reports when it had to step in.
    if out.status.code() == Some(124) {
        return Err(anyhow!(
            "ddcutil {} gave no answer in {seconds}s — the monitor may have DDC/CI \
             switched off, or its I2C bus may be wedged",
            args.join(" ")
        ));
    }
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
fn parse_current(text: &str) -> Option<Percent> {
    let (_, rest) = text.split_once("current value =")?;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    // A monitor reporting outside 0..=100 is reporting on a scale this does
    // not understand, which is worth refusing rather than guessing at.
    Percent::try_from(digits.parse::<u8>().ok()?).ok()
}

/// Which I²C bus each DRM connector answers on, from `ddcutil detect`.
///
/// The connector name is the link back to the compositor: Hyprland calls the
/// monitor `DP-4` and ddcutil calls it `card1-DP-4`, and matching on that is
/// what lets a scroll on one screen change that screen rather than a guess.
pub fn buses_by_connector() -> Result<Vec<(String, u32)>> {
    Ok(parse_detect(&ddcutil(
        &["detect", "--brief"],
        PATIENCE_DETECT,
    )?))
}

/// Reads `ddcutil detect --brief` into connector-to-bus pairs.
///
/// Only blocks headed `Display N` count. ddcutil prints an `I2C bus:` and a
/// `DRM connector:` line under `Invalid display` too — which is exactly what it
/// calls an Apple Studio Display — and taking those at face value hands a bus
/// to a panel that cannot answer on it. The Studio Display gets away with it
/// because it is recognised earlier; a laptop's eDP would be listed as
/// DDC-capable and then spend a second failing on every keypress.
fn parse_detect(text: &str) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let mut bus: Option<u32> = None;
    let mut usable = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Block headers sit at the left margin; their fields are indented.
        if !line.starts_with(char::is_whitespace) {
            usable = trimmed.starts_with("Display ");
            bus = None;
            continue;
        }
        if let Some(path) = trimmed.strip_prefix("I2C bus:") {
            bus = path.trim().rsplit('-').next().and_then(|n| n.parse().ok());
        } else if let Some(connector) = trimmed.strip_prefix("DRM connector:") {
            // "card1-DP-4" — the card prefix is the GPU, which is not ours to
            // care about; the connector after it is what the compositor names.
            let connector = connector.trim();
            let short = connector
                .split_once('-')
                .map_or_else(|| connector.to_string(), |(_, rest)| rest.to_string());
            if let (Some(bus), true) = (bus.take(), usable) {
                found.push((short, bus));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_getvcp_line_yields_its_number() {
        let line = "VCP code 0x10 (Brightness                    ): \
                    current value =    60, max value =   100";
        assert_eq!(parse_current(line).map(Percent::get), Some(60));
    }

    #[test]
    fn the_maximum_is_not_mistaken_for_the_current_value() {
        // Both numbers are on the line and the wrong one is always 100, which
        // would look plausible and be wrong.
        let line = "current value =     0, max value =   100";
        assert_eq!(parse_current(line).map(Percent::get), Some(0));
    }

    #[test]
    fn output_without_a_value_is_refused_rather_than_guessed() {
        assert_eq!(parse_current("Display not found"), None);
        assert_eq!(parse_current(""), None);
        assert_eq!(parse_current("current value = "), None);
    }
}

#[cfg(test)]
mod detect_tests {
    use super::*;

    /// Real `ddcutil detect --brief` output from a machine with an ASUS on
    /// DDC and an Apple Studio Display, which ddcutil cannot drive and files
    /// under "Invalid display" while still printing both of its lines.
    const SAMPLE: &str = "\
Display 1
   I2C bus:          /dev/i2c-11
   DRM connector:    card1-DP-4
   drm_connector_id: 0
   Monitor:          AUS:PG27UCDM:T1LMAS012351

Invalid display
   I2C bus:          /dev/i2c-12
   DRM connector:    card2-DP-1
   drm_connector_id: 103
   Monitor:          APP:StudioDisplay:
";

    #[test]
    fn an_invalid_display_is_not_given_a_bus() {
        // The bug this exists for: taking every I2C-bus line at face value
        // marked the Studio Display as DDC-capable. Harmless there because it
        // is recognised as Apple first — but a laptop's eDP would then spend a
        // second failing in ddcutil on every keypress.
        let found = parse_detect(SAMPLE);
        assert_eq!(found, vec![("DP-4".to_string(), 11)]);
        assert!(!found.iter().any(|(name, _)| name == "DP-1"));
    }

    #[test]
    fn the_card_prefix_is_dropped_so_the_name_matches_the_compositor() {
        // ddcutil says "card1-DP-4"; the compositor says "DP-4", and the whole
        // lookup is keyed on the latter.
        assert_eq!(
            parse_detect(SAMPLE).first().map(|(n, _)| n.as_str()),
            Some("DP-4")
        );
    }

    #[test]
    fn nothing_detected_is_an_empty_list_rather_than_a_guess() {
        assert!(parse_detect("").is_empty());
        assert!(parse_detect(
            "Invalid display\n   I2C bus: /dev/i2c-3\n   DRM connector: card0-eDP-1\n"
        )
        .is_empty());
    }

    #[test]
    fn a_block_without_a_connector_line_contributes_nothing() {
        // Some drivers report a bus with no DRM connector at all; there is no
        // name to key it on, so it cannot be used.
        let text = "Display 1\n   I2C bus:          /dev/i2c-7\n   Monitor:          FOO:BAR:1\n";
        assert!(parse_detect(text).is_empty());
    }
}
