mod cache;
mod ddc;
mod hid;
mod monitors;
mod osd;
mod percent;
mod watch;

use crate::percent::Percent;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

/// How far one notch of the scroll wheel moves it.
const STEP: u8 = 5;

/// What clicking cycles through. Ends at full so a click always gets you back
/// to a known place rather than wherever the cycle happened to be.
const PRESETS: [u8; 4] = [25, 50, 75, 100];

/// The presets as levels. `expect` rather than a fallback: these are literals
/// in this file, so one outside the range is a typo to catch at startup.
fn presets() -> [Percent; 4] {
    PRESETS.map(|p| Percent::try_from(p).expect("a preset must be a percentage"))
}

#[derive(Parser)]
#[command(
    name = "studio-display-brightness",
    about = "Brightness for external displays: Apple Studio Display over USB HID, the rest over DDC/CI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the brightness of a display.
    Get(Target),
    /// Set it to a percentage.
    Set {
        /// 0 to 100
        ///
        /// Rejected rather than clamped: `set 150` is a mistake, and silently
        /// doing something else is how a mistake goes unnoticed.
        #[arg(value_parser = parse_percent)]
        percent: Percent,
        #[command(flatten)]
        target: Target,
    },
    /// Raise it by one step.
    Up {
        #[arg(long, default_value_t = STEP)]
        step: u8,
        #[command(flatten)]
        target: Target,
    },
    /// Lower it by one step.
    Down {
        #[arg(long, default_value_t = STEP)]
        step: u8,
        #[command(flatten)]
        target: Target,
    },
    /// Step to the next preset: 25, 50, 75, 100.
    Cycle(Target),
    /// List the displays that can be controlled, and their current level.
    List,
    /// Stream JSON for the status bar, one line per change.
    Watch,
}

#[derive(clap::Args)]
struct Target {
    /// Which display, by connector name (DP-1, DP-4). Defaults to the one you
    /// are looking at.
    #[arg(long)]
    display: Option<String>,

    /// Show the new level as a notification. For a key binding, where there is
    /// nothing else on screen to say what happened.
    #[arg(long)]
    notify: bool,
}

impl Target {
    fn resolve(&self) -> Result<monitors::Display> {
        self.display
            .as_deref()
            .map_or_else(monitors::focused, monitors::by_name)
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("studio-display-brightness: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::List) {
        Command::Get(target) => {
            let display = target.resolve()?;
            println!("{}", display.get()?);
            Ok(())
        }
        Command::Set { percent, target } => {
            let display = target.resolve()?;
            display.set(percent)?;
            report(&display, percent, target.notify)
        }
        Command::Up { step, target } => nudge(&target, i16::from(step)),
        Command::Down { step, target } => nudge(&target, -i16::from(step)),
        Command::Cycle(target) => {
            let display = target.resolve()?;
            let now = display.get()?;
            // The next preset above where it is now, wrapping round. Reading
            // first means a brightness set by anything else still lands
            // somewhere sensible.
            let all = presets();
            let next = all.iter().copied().find(|p| *p > now).unwrap_or(all[0]);
            display.set(next)?;
            report(&display, next, target.notify)
        }
        Command::List => {
            for display in monitors::all()? {
                let level = display
                    .get()
                    .map_or_else(|_| "  ?".into(), |p| format!("{p:>3}%"));
                let how = match display.backend {
                    monitors::Backend::Hid(_) => "USB HID",
                    monitors::Backend::Ddc(_) => "DDC/CI",
                };
                println!(
                    "{:<6} {level}  {:<8} {}",
                    display.connector, how, display.description
                );
            }
            Ok(())
        }
        Command::Watch => watch::run(),
    }
}

/// Moves by a step, clamped at both ends rather than wrapping: scrolling past
/// the bottom should stop, not jump to full brightness.
fn nudge(target: &Target, delta: i16) -> Result<()> {
    let display = target.resolve()?;
    let next = display.nudge(delta)?;
    report(&display, next, target.notify)
}

/// Reads a level a person typed, refusing anything that is not a percentage.
fn parse_percent(raw: &str) -> Result<Percent, String> {
    let value: u8 = raw.parse().map_err(|_| format!("{raw} is not a number"))?;
    Percent::try_from(value).map_err(|e| e.to_string())
}

fn report(display: &monitors::Display, percent: Percent, notify: bool) -> Result<()> {
    // Written where the bar's `watch` can see it, so scrolling shows up
    // immediately instead of at the next poll.
    watch::note(&display.connector, percent).context("recording the new level")?;
    if notify {
        osd::show(&display.short_name(), percent);
    }
    println!("{percent}");
    Ok(())
}
