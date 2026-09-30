//! A brightness level, which is a percentage and cannot be anything else.
//!
//! This exists because `u8` does not carry the invariant, and every place that
//! took a `u8` had to remember it: `.min(100)` in both backends, a clamp in the
//! stepping, a range check in the argument parser, and a test asserting that
//! passing 200 does something sensible. One type makes all of those either
//! unnecessary or impossible, and the compiler enforces it at every boundary
//! rather than each caller remembering.

use std::fmt;

/// A brightness level: `0..=100`, and never anything else.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Percent(u8);

impl Percent {
    pub const MIN: Self = Self(0);
    pub const MAX: Self = Self(100);

    /// Clamps rather than refusing, for callers computing a level rather than
    /// reading one a person typed.
    #[must_use]
    pub const fn saturating(value: i32) -> Self {
        // `clamp` is not const for i32 on this edition, so this is written out.
        if value <= 0 {
            Self::MIN
        } else if value >= 100 {
            Self::MAX
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Self(value as u8)
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Moves by `delta`, stopping at either end.
    ///
    /// Clamped rather than wrapped on purpose: scrolling past the bottom should
    /// stop, not jump to full brightness in a dark room.
    #[must_use]
    pub const fn stepped(self, delta: i16) -> Self {
        Self::saturating(self.0 as i32 + delta as i32)
    }

    /// Maps onto an arbitrary hardware range, rounding to nearest.
    ///
    /// Integer arithmetic throughout: `(a + b/2) / b` rounds without floats,
    /// which keeps the mapping reproducible and every conversion total.
    #[must_use]
    pub fn scaled_into(self, min: u32, max: u32) -> u32 {
        let span = u64::from(max - min);
        let offset = (u64::from(self.0) * span + 50) / 100;
        min + u32::try_from(offset).unwrap_or(max - min)
    }

    /// The inverse of `scaled_into`, for a reading that came off the hardware.
    #[must_use]
    pub fn scaled_from(raw: u32, min: u32, max: u32) -> Self {
        let raw = raw.clamp(min, max);
        let span = u64::from(max - min);
        let scaled = u64::from(raw - min) * 100;
        Self::saturating(i32::try_from((scaled + span / 2) / span).unwrap_or(100))
    }
}

impl TryFrom<u8> for Percent {
    type Error = OutOfRange;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 100 {
            Ok(Self(value))
        } else {
            Err(OutOfRange(value))
        }
    }
}

impl fmt::Display for Percent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A level someone typed that is not a percentage.
#[derive(Debug)]
pub struct OutOfRange(u8);

impl fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not a percentage; expected 0 to 100", self.0)
    }
}

impl std::error::Error for OutOfRange {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_outside_the_range_is_refused_rather_than_clamped() {
        // For something a person typed, silently doing a different thing is
        // how a mistake goes unnoticed.
        assert!(Percent::try_from(101).is_err());
        assert!(Percent::try_from(255).is_err());
        assert_eq!(Percent::try_from(100).map(Percent::get).ok(), Some(100));
        assert_eq!(Percent::try_from(0).map(Percent::get).ok(), Some(0));
    }

    #[test]
    fn stepping_stops_at_both_ends_rather_than_wrapping() {
        // Wrapping at the bottom means a dark room suddenly at full brightness.
        assert_eq!(Percent::MIN.stepped(-5), Percent::MIN);
        assert_eq!(Percent::MAX.stepped(5), Percent::MAX);
        assert_eq!(Percent::MIN.stepped(-30_000), Percent::MIN);
        assert_eq!(Percent::MAX.stepped(30_000), Percent::MAX);
    }

    #[test]
    fn stepping_moves_by_exactly_the_delta_in_between() {
        let half = Percent::try_from(50).expect("in range");
        assert_eq!(half.stepped(5).get(), 55);
        assert_eq!(half.stepped(-5).get(), 45);
        assert_eq!(half.stepped(0).get(), 50);
    }

    #[test]
    fn a_level_survives_the_trip_through_the_studio_displays_range() {
        // 400..60000 centinits, the panel's own 600-nit spec. Every level has
        // to come back as itself or the brightness keys drift.
        for value in 0..=100u8 {
            let percent = Percent::try_from(value).expect("in range");
            let raw = percent.scaled_into(400, 60_000);
            assert_eq!(
                Percent::scaled_from(raw, 400, 60_000),
                percent,
                "round trip at {value}%"
            );
        }
    }

    #[test]
    fn the_ends_of_the_hardware_range_are_exact() {
        // 0% must be the display's own minimum rather than off, and 100% its
        // maximum rather than one short of it.
        assert_eq!(Percent::MIN.scaled_into(400, 60_000), 400);
        assert_eq!(Percent::MAX.scaled_into(400, 60_000), 60_000);
        assert_eq!(Percent::scaled_from(400, 400, 60_000), Percent::MIN);
        assert_eq!(Percent::scaled_from(60_000, 400, 60_000), Percent::MAX);
    }

    #[test]
    fn a_reading_outside_the_hardware_range_is_pulled_back_in() {
        assert_eq!(Percent::scaled_from(0, 400, 60_000), Percent::MIN);
        assert_eq!(Percent::scaled_from(u32::MAX, 400, 60_000), Percent::MAX);
    }

    #[test]
    fn the_midpoint_is_the_midpoint() {
        assert_eq!(Percent::scaled_from(30_200, 400, 60_000).get(), 50);
    }
}
