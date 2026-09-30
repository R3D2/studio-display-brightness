//! The on-screen notification a brightness key is expected to produce.
//!
//! Only for the key bindings and the CLI: the bar module shows the level in the
//! bar already, and a notification for something you can see would be noise.

use std::path::PathBuf;
use std::process::Command;

fn id_path() -> Option<PathBuf> {
    crate::cache::dir()
        .ok()
        .map(|dir| dir.join("studio-display-brightness.notify"))
}

/// Shows the level, replacing the previous notification rather than stacking.
///
/// `replaces_id` is the mechanism the desktop notification spec gives for this,
/// and the daemon hands the id back on stdout when asked with `-p`. The
/// `x-canonical-private-synchronous` hint is the other common answer and is not
/// honoured everywhere, so three presses become three notifications.
///
/// Best effort throughout: a missing notification daemon must not stop the
/// brightness from changing, which has already happened by the time this runs.
pub fn show(label: &str, percent: u8) {
    let path = id_path();
    let previous = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0);

    let output = Command::new("notify-send")
        .arg("-p")
        .args(["-r", &previous.to_string()])
        // The progress hint is what turns this into a bar rather than a line
        // of text.
        .args(["-h", &format!("int:value:{percent}")])
        .args(["-h", "string:x-dunst-stack-tag:brightness"])
        .arg("Brightness")
        .arg(format!("{label} {percent}%"))
        .output();

    if let (Ok(out), Some(path)) = (output, path) {
        if out.status.success() {
            let _ = crate::cache::write_atomically(&path, out.stdout.trim_ascii());
        }
    }
}
