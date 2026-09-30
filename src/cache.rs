//! What this tool would otherwise pay for twice, and the lock that makes
//! sharing it safe.
//!
//! Two things here are expensive and neither changes often:
//!
//! - Finding which I2C bus a monitor answers DDC on means `ddcutil detect`,
//!   which probes every bus on the machine. On a box with an NVIDIA card whose
//!   connectors do not publish a `ddc` link in sysfs, that measured **19.6
//!   seconds**. Doing it per keypress made the brightness keys unusable.
//! - Reading a level back over DDC is about a second.
//!
//! So both are remembered, keyed on the set of monitors attached: if that set
//! is unchanged, so are the buses.
//!
//! # Why there is a lock
//!
//! Holding the scroll wheel starts a process per notch, several overlapping.
//! Every one of them reads this file, decides what to do, spends up to a second
//! in `ddcutil`, and writes back — a read-modify-write with a very long middle.
//! Writing the file atomically is not enough, because the lost update happens
//! between the read and the write, not during it:
//!
//! - ten notches that all read 60 all compute 65, and the brightness moves one
//!   step however long you scroll;
//! - a process that read the cache before a nineteen-second `detect` can save
//!   afterwards and wipe the bus map that probe just paid for.
//!
//! So the whole read-modify-write is done under an exclusive file lock, held
//! across the hardware call as well. That serialises the ddcutil calls too,
//! which they need anyway.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Cache {
    /// The monitors this was learned from. A different set means the bus
    /// numbers cannot be trusted and are worth paying for again.
    #[serde(default)]
    pub fingerprint: String,
    /// Connector name to I2C bus, for the displays that speak DDC.
    #[serde(default)]
    pub buses: HashMap<String, u32>,
    /// Connectors that a completed probe did not find on any bus, so a second
    /// probe would cost nineteen seconds to learn the same thing.
    #[serde(default)]
    pub without_ddc: Vec<String>,
    /// Last known level per connector. Only worth keeping for DDC displays;
    /// reading a Studio Display is a few milliseconds and always exact.
    #[serde(default)]
    pub levels: HashMap<String, u8>,
}

impl Cache {
    /// Reads a cache, treating anything unreadable as empty.
    ///
    /// A damaged cache must cost a slow lookup and nothing else, so this never
    /// fails: an unparseable file means "nothing is known", which is true.
    pub fn parse(text: &str) -> Self {
        serde_json::from_str(text).unwrap_or_default()
    }
}

/// Where per-boot state lives.
///
/// `XDG_RUNTIME_DIR` is required rather than falling back to `/tmp`: the
/// fallback would put a predictable name in a directory every local user can
/// write, and this file decides which I2C bus gets written to. logind always
/// sets it for a real session.
pub fn dir() -> Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .context(
            "XDG_RUNTIME_DIR is not set — this expects a logind session, and will not \
         fall back to a world-writable /tmp for state that decides which device to write to",
        )
}

fn path() -> Result<PathBuf> {
    Ok(dir()?.join("studio-display-brightness.cache.json"))
}

fn lock_path() -> Result<PathBuf> {
    Ok(dir()?.join("studio-display-brightness.lock"))
}

/// Writes a file so that a reader never sees it half-written.
///
/// Rename within a directory is atomic on every filesystem Linux ships, so a
/// reader sees either the old contents or the new, never a mixture.
pub fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    // The pid keeps two processes from choosing the same temporary name and
    // each truncating the other's file before either renames.
    let temp = parent.join(format!(".{}.{}.tmp", file_stem(path), std::process::id()));
    if let Err(e) = std::fs::write(&temp, bytes) {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("writing {}", temp.display()));
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        // Otherwise a failed rename leaves the temporary file behind for good.
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

fn file_stem(path: &std::path::Path) -> String {
    path.file_name()
        .map_or_else(|| "state".to_string(), |n| n.to_string_lossy().into_owned())
}

/// Runs `change` with the cache, under an exclusive lock held throughout.
///
/// The lock covers loading, whatever `change` does — including a slow hardware
/// call — and saving. That is the point: the expensive part sits between the
/// read and the write, and it is where the lost updates happen.
///
/// The lock file is separate from the cache file so that replacing the cache by
/// rename cannot pull the locked inode out from under another waiter.
pub fn update<T>(change: impl FnOnce(&mut Cache) -> Result<T>) -> Result<T> {
    let guard = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path()?)
        .context("opening the cache lock")?;
    guard.lock().context("waiting for the cache lock")?;

    let path = path()?;
    let mut cache = std::fs::read_to_string(&path)
        .map(|text| Cache::parse(&text))
        .unwrap_or_default();

    let outcome = change(&mut cache);

    // Saved even when `change` failed: it may have got as far as setting the
    // hardware, and a level we know about is worth keeping either way.
    if let Ok(text) = serde_json::to_string(&cache) {
        let _ = write_atomically(&path, text.as_bytes());
    }
    // The lock releases when `guard` drops, after the save.
    drop(guard);
    outcome
}

/// Reads the cache without taking the lock, for callers that only look.
pub fn read() -> Cache {
    path()
        .and_then(|p| Ok(std::fs::read_to_string(p)?))
        .map(|text| Cache::parse(&text))
        .unwrap_or_default()
}

/// Identifies the current set of monitors, so a cache learned from a different
/// one is discarded rather than believed.
pub fn fingerprint(names: &[String]) -> String {
    let mut names = names.to_vec();
    names.sort();
    names.join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_ignores_the_order_monitors_are_listed_in() {
        // hyprctl does not promise an order, and a reshuffle is not a hardware
        // change: treating it as one would re-run a nineteen-second probe.
        assert_eq!(
            fingerprint(&["DP-1".into(), "DP-4".into()]),
            fingerprint(&["DP-4".into(), "DP-1".into()])
        );
    }

    #[test]
    fn a_different_set_of_monitors_is_a_different_fingerprint() {
        let two = fingerprint(&["DP-1".into(), "DP-4".into()]);
        assert_ne!(
            two,
            fingerprint(&["DP-1".into()]),
            "unplugging must invalidate"
        );
        assert_ne!(
            two,
            fingerprint(&["DP-1".into(), "HDMI-A-1".into()]),
            "swapping one must invalidate too"
        );
    }

    #[test]
    fn a_torn_cache_parses_as_empty_rather_than_failing() {
        // Exercises the real entry point: a half-written file must cost a slow
        // lookup, not an error.
        let torn = Cache::parse("{\"buses\": {\"DP-4\": 1");
        assert!(torn.buses.is_empty());
        assert!(torn.levels.is_empty());
        assert_eq!(torn.fingerprint, "");
    }

    #[test]
    fn a_cache_written_by_an_older_build_still_loads() {
        // Every field carries serde(default), so a missing one is not a reason
        // to throw the rest away.
        let old = Cache::parse("{\"buses\":{\"DP-4\":11}}");
        assert_eq!(old.buses.get("DP-4"), Some(&11));
        assert!(old.without_ddc.is_empty());
    }

    #[test]
    fn a_full_cache_round_trips() {
        let cache = Cache {
            fingerprint: "DP-1|DP-4".into(),
            buses: HashMap::from([("DP-4".to_string(), 11)]),
            without_ddc: vec!["eDP-1".into()],
            levels: HashMap::from([("DP-4".to_string(), 60)]),
        };
        let back = Cache::parse(&serde_json::to_string(&cache).expect("serialises"));
        assert_eq!(back.fingerprint, "DP-1|DP-4");
        assert_eq!(back.buses.get("DP-4"), Some(&11));
        assert_eq!(back.without_ddc, vec!["eDP-1".to_string()]);
        assert_eq!(back.levels.get("DP-4"), Some(&60));
    }
}
