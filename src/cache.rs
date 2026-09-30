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
//! `XDG_RUNTIME_DIR`, which means it is rebuilt once per boot and can never go
//! stale across a hardware change that a reboot would fix.

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

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("studio-display-brightness.cache.json")
}

pub fn load() -> Cache {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Best effort: a cache that cannot be written costs speed, not correctness.
pub fn save(cache: &Cache) {
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path(), text);
    }
}

/// Identifies the current set of monitors, so a cache learned from a different
/// one is discarded rather than believed.
pub fn fingerprint(names: &[String]) -> String {
    let mut names = names.to_vec();
    names.sort();
    names.join("|")
}
