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
use anyhow::{Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// How often to notice that focus moved to another screen. One IPC call to the
/// compositor, no display traffic, so this can be brisk.
const FOCUS_POLL: Duration = Duration::from_millis(250);

/// How often to ask a display again, for changes made by something else — the
/// buttons on the monitor, another tool. Rare, and slow over DDC.
const REREAD: Duration = Duration::from_secs(10);

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

fn note_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("display-brightness.state")
}

/// Records a level just set, so the bar reflects it without waiting for a read.
pub fn note(connector: &str, percent: u8) -> Result<()> {
    let path = note_path();
    let mut file =
        std::fs::File::create(&path).with_context(|| format!("writing {}", path.display()))?;
    writeln!(file, "{connector} {percent}")?;
    Ok(())
}

fn read_note() -> Option<(String, u8, SystemTime)> {
    let path = note_path();
    let when = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let text = std::fs::read_to_string(&path).ok()?;
    let (connector, percent) = text.trim().split_once(' ')?;
    Some((connector.to_string(), percent.parse().ok()?, when))
}

pub fn run() -> Result<()> {
    let mut displays: Vec<Display> = monitors::all().unwrap_or_default();
    let mut scanned = Instant::now();
    let mut levels: HashMap<String, (u8, Instant)> = HashMap::new();
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

fn payload(display: &Display, percent: u8) -> Payload {
    let name = display.short_name();
    let class = match percent {
        0..=20 => "dim",
        21..=79 => "mid",
        _ => "bright",
    };
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
        percent,
        display: display.connector.clone(),
    }
}
