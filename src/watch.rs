//! The status bar module: one JSON line whenever what the bar shows changes.
//!
//! Three things make this more than a poll loop.
//!
//! Finding out which displays answer DDC probes every I²C bus and costs the
//! best part of a second, so the set of displays is worked out once and kept.
//! Reading a level over DDC is slow too, so a level once read is remembered.
//! And a level this tool just set is written to a small file, so scrolling the
//! wheel shows up at once rather than at the next hardware read.

use crate::monitors::{self, Display};
use crate::percent::Percent;
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// How often to notice that focus moved to another screen. One IPC call to the
/// compositor, no display traffic, so this can be brisk.
const FOCUS_POLL: Duration = Duration::from_millis(250);

/// How often to ask a display again, for changes made by something else — the
/// buttons on the monitor, another tool.
///
/// A DDC read is about a second and blocks this loop while it happens, so this
/// is deliberately long: anything this tool does is published through the note
/// file and seen within 250ms regardless. What this interval actually costs is
/// how long a change made on the monitor's own buttons goes unnoticed.
const REREAD: Duration = Duration::from_secs(60);

/// How often to look for displays being plugged in or unplugged. This is the
/// expensive one, which is why it is not on the main beat.
const RESCAN: Duration = Duration::from_secs(30);

#[derive(Serialize)]
struct Payload {
    text: String,
    alt: String,
    class: String,
    tooltip: String,
    percent: u8,
    display: String,
}

fn note_path() -> Result<PathBuf> {
    Ok(crate::cache::dir()?.join("studio-display-brightness.state"))
}

/// Records a level just set, so the bar reflects it without waiting for a read.
pub fn note(connector: &str, percent: Percent) -> Result<()> {
    // Atomically, for the same reason the cache is: a reader polling four
    // times a second can otherwise catch this file truncated between the
    // create and the write.
    crate::cache::write_atomically(&note_path()?, format!("{connector} {percent}\n").as_bytes())
}

/// Reads a note back.
///
/// `File::create` truncates before writing, so a reader can catch the file
/// empty or half-written; half a line must not become a brightness.
fn parse_note(text: &str) -> Option<(&str, Percent)> {
    let (connector, percent) = text.trim().split_once(' ')?;
    if connector.is_empty() {
        return None;
    }
    Some((
        connector,
        Percent::try_from(percent.trim().parse::<u8>().ok()?).ok()?,
    ))
}

fn read_note() -> Option<(String, Percent, SystemTime)> {
    let path = note_path().ok()?;
    let when = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let (connector, percent) = parse_note(&text)?;
    Some((connector.to_string(), percent, when))
}

pub fn run() -> Result<()> {
    let mut displays: Vec<Display> = monitors::all().unwrap_or_default();
    let mut scanned = Instant::now();
    let mut levels: HashMap<String, (Percent, Instant)> = HashMap::new();
    let mut last_line = String::new();
    let mut last_note: Option<SystemTime> = None;

    loop {
        // A level this tool just set beats anything cached: it is newer, and it
        // cost nothing to learn.
        if let Some((connector, percent, when)) = read_note() {
            if last_note != Some(when) {
                last_note = Some(when);
                levels.insert(connector, (percent, Instant::now()));
            }
        }

        let focused = monitors::focused_name().ok();

        // Rescan when the clock says so, or straight away if focus moved to a
        // screen we have never heard of — that is a display being plugged in,
        // and waiting half a minute to notice would look broken.
        let unknown = focused
            .as_ref()
            .is_some_and(|name| !displays.iter().any(|d| &d.connector == name));
        if scanned.elapsed() > RESCAN || (unknown && scanned.elapsed() > Duration::from_secs(2)) {
            displays = monitors::all().unwrap_or(displays);
            scanned = Instant::now();
        }

        if let Some(display) = focused
            .as_ref()
            .and_then(|name| displays.iter().find(|d| &d.connector == name))
        {
            let stale = levels
                .get(&display.connector)
                .is_none_or(|(_, at)| at.elapsed() > REREAD);
            if stale {
                if let Ok(percent) = display.get() {
                    levels.insert(display.connector.clone(), (percent, Instant::now()));
                }
            }

            if let Some((percent, _)) = levels.get(&display.connector).copied() {
                let line = serde_json::to_string(&payload(display, percent))?;
                if line != last_line {
                    println!("{line}");
                    // The bar reads lines as they arrive; without this the
                    // module sits blank until the buffer happens to fill.
                    std::io::stdout().flush()?;
                    last_line = line;
                }
            }
        }

        std::thread::sleep(FOCUS_POLL);
    }
}

/// The CSS class the bar styles on, and the key an icon-map is keyed by.
const fn class_for(percent: u8) -> &'static str {
    match percent {
        0..=20 => "dim",
        21..=79 => "mid",
        _ => "bright",
    }
}

fn payload(display: &Display, percent: Percent) -> Payload {
    let name = display.short_name();
    let class = class_for(percent.get());
    let mut tooltip = format!("{name} — {percent}%");
    if let monitors::Backend::Hid(studio) = &display.backend {
        // The Studio Display reports what it is actually emitting, which is
        // more use than a percentage of an unstated maximum.
        if let Ok(nits) = studio.nits() {
            tooltip.push_str(&format!(" ({nits:.0} nits)"));
        }
    }
    tooltip.push_str("\n\nScroll: adjust · Left click: 25/50/75/100 · Right click: full");
    Payload {
        text: format!("{percent}%"),
        alt: class.to_string(),
        class: class.to_string(),
        tooltip,
        percent: percent.get(),
        display: display.connector.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_class_follows_the_level() {
        // The bar styles on this, and the thresholds are the contract: a
        // change here silently restyles somebody's bar.
        assert_eq!(class_for(0), "dim");
        assert_eq!(class_for(20), "dim");
        assert_eq!(class_for(21), "mid");
        assert_eq!(class_for(79), "mid");
        assert_eq!(class_for(80), "bright");
        assert_eq!(class_for(100), "bright");
    }

    #[test]
    fn the_class_only_ever_changes_at_the_documented_boundaries() {
        // A bar's icon-map is keyed on these three names and fails silently on
        // a fourth, so the set is a contract. Walking the range also pins where
        // the steps are, which a wildcard arm cannot guarantee on its own.
        let changes: Vec<(u8, &str)> = (1..=100u8)
            .filter(|p| class_for(*p) != class_for(p - 1))
            .map(|p| (p, class_for(p)))
            .collect();
        assert_eq!(changes, vec![(21, "mid"), (80, "bright")]);
    }

    #[test]
    fn a_note_reads_back_as_what_was_written() {
        assert_eq!(
            parse_note("DP-1 65\n").map(|(c, p)| (c, p.get())),
            Some(("DP-1", 65))
        );
        assert_eq!(
            parse_note("DP-1 0\n").map(|(c, p)| (c, p.get())),
            Some(("DP-1", 0))
        );
        assert_eq!(
            parse_note("HDMI-A-1 100\n").map(|(c, p)| (c, p.get())),
            Some(("HDMI-A-1", 100))
        );
    }

    #[test]
    fn a_truncated_note_is_ignored_rather_than_misread() {
        // The real parser, not a copy of it: a reader can catch this file
        // empty or half-written, and half a line must not become a brightness.
        for bad in ["", "   ", "DP-1", "DP-1 ", " 65", "DP-1 abc", "DP-1 999"] {
            assert!(parse_note(bad).is_none(), "{bad:?} should not parse");
        }
    }
}
