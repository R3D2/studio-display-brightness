//! What this tool would otherwise pay for twice.
//!
//! Two things here are expensive and neither changes often:
//!
//! - Finding which I2C bus a monitor answers DDC on means `ddcutil detect`,
//!   which probes every bus on the machine. On a box with an NVIDIA card whose
//!   connectors do not publish a `ddc` link in sysfs, that measured **19.6
//!   seconds**. Doing it per keypress made the brightness keys unusable.
//! - Reading a level back over DDC is about a second.
//!
//! So both are remembered, keyed on the set of monitors currently attached: if
//! that set is unchanged, so are the buses. The file lives under
//! `XDG_RUNTIME_DIR`, so it is rebuilt once per boot and cannot outlive a
//! hardware change that a reboot would fix.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Last known level per connector. Only worth keeping for DDC displays;
    /// reading a Studio Display is a few milliseconds and always exact.
    #[serde(default)]
    pub levels: HashMap<String, u8>,
}

fn dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

pub fn path() -> PathBuf {
    dir().join("studio-display-brightness.cache.json")
}

pub fn load() -> Cache {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes the cache so that a reader never sees a half-written one.
///
/// This matters more than it looks. Holding the scroll wheel starts a new
/// process per notch, several of them overlapping, and a torn file does not
/// fail safe: it fails to parse, `load` hands back a default, the bus map is
/// treated as unknown and the next change pays nineteen seconds for
/// `ddcutil detect` again. Writing a temporary file and renaming it means a
/// reader sees either the old cache or the new one, never a mixture — rename
/// within a directory is atomic on every filesystem Linux ships.
///
/// Best effort otherwise: a cache that cannot be written costs speed, not
/// correctness.
pub fn save(cache: &Cache) {
    let Ok(text) = serde_json::to_string(cache) else {
        return;
    };
    // The pid keeps two processes from picking the same temporary name and
    // each truncating the other's file before either renames.
    let temp = dir().join(format!(
        "studio-display-brightness.cache.{}.tmp",
        std::process::id()
    ));
    if std::fs::write(&temp, text).is_ok() && std::fs::rename(&temp, path()).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
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
        let a = fingerprint(&["DP-1".into(), "DP-4".into()]);
        let b = fingerprint(&["DP-4".into(), "DP-1".into()]);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_set_of_monitors_is_a_different_fingerprint() {
        let two = fingerprint(&["DP-1".into(), "DP-4".into()]);
        let one = fingerprint(&["DP-1".into()]);
        let other = fingerprint(&["DP-1".into(), "HDMI-A-1".into()]);
        assert_ne!(two, one, "unplugging a monitor must invalidate the buses");
        assert_ne!(two, other, "swapping one must invalidate them too");
    }

    #[test]
    fn nothing_attached_is_still_a_stable_fingerprint() {
        assert_eq!(fingerprint(&[]), fingerprint(&[]));
    }

    #[test]
    fn a_cache_file_that_is_not_json_reads_as_empty_rather_than_failing() {
        // The point of the default is that a damaged cache costs a slow lookup
        // and nothing else.
        let torn: Result<Cache, _> = serde_json::from_str("{\"buses\": {\"DP-4\": 1");
        assert!(torn.is_err());
        let empty = Cache::default();
        assert!(empty.buses.is_empty() && empty.levels.is_empty());
    }

    #[test]
    fn fields_added_later_do_not_break_an_older_cache() {
        // Every field carries serde(default), so a file written by an older
        // build still loads instead of being thrown away.
        let old: Cache = serde_json::from_str("{}").expect("an empty object loads");
        assert_eq!(old.fingerprint, "");
    }
}
