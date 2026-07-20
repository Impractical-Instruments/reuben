//! Gate/clock **edge detection** — the shared primitive behind every clock-driven operator.
//!
//! `clock`/`gate` are held Values: the engine block-slices at every change, so an
//! operator sees one constant level per (sub)block and detects an edge by comparing that level to
//! the one held across the previous slice — the slice's frame 0 *is* the change frame, so emitting
//! there is sample-accurate. That "compare the held level to the previous, fire on the crossing"
//! logic was copy-pasted (with its bare `0.5` threshold and a `prev_clock`/`prev_gate` latch field)
//! into every clock-driven operator. [`EdgeDetector`] captures it once.
//!
//! It is a `Copy` latch of a single `f32` — allocation-free and trivially cheap, so it is fine to
//! run on the render hot path.

/// Threshold at or above which a held gate/clock level counts as **on**.
///
/// Gate sources emit `0.0`/`1.0`, so the exact cutoff is immaterial in practice; centralizing it
/// keeps every operator's notion of "gate on" identical and changeable in one place.
pub const GATE_ON: f32 = 0.5;

/// Whether a held level reads as an on (high) gate.
#[inline]
pub fn is_on(level: f32) -> bool {
    level >= GATE_ON
}

/// The transition between two successive held gate levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// No crossing — both levels sit on the same side of [`GATE_ON`].
    None,
    /// Off → on: the start of a clock/gate pulse.
    Rising,
    /// On → off: the end of a clock/gate pulse.
    Falling,
}

/// A one-sample latch that reports the [`Edge`] each time it is fed the current held level.
///
/// Carries the previous level across slices and blocks, exactly like the hand-rolled
/// `prev_clock`/`prev_gate` fields it replaces. The default (and [`new`](Self::new)) start low
/// (`0.0`), so a clock that begins already-high fires its first [`Edge::Rising`] at frame 0.
#[derive(Clone, Copy, Debug, Default)]
pub struct EdgeDetector {
    prev: f32,
}

impl EdgeDetector {
    /// A detector latched low (gate off).
    pub const fn new() -> Self {
        Self { prev: 0.0 }
    }

    /// Feed the current held `level`; returns its [`Edge`] relative to the previous level and
    /// latches `level` for the next call.
    #[inline]
    pub fn detect(&mut self, level: f32) -> Edge {
        let edge = match (is_on(self.prev), is_on(level)) {
            (false, true) => Edge::Rising,
            (true, false) => Edge::Falling,
            _ => Edge::None,
        };
        self.prev = level;
        edge
    }

    /// The level held from the last [`detect`](Self::detect) (or `0.0` if never fed).
    #[inline]
    pub fn level(&self) -> f32 {
        self.prev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_high_level_is_a_rising_edge() {
        let mut e = EdgeDetector::new();
        assert_eq!(e.detect(1.0), Edge::Rising);
    }

    #[test]
    fn held_level_reports_no_edge() {
        let mut e = EdgeDetector::new();
        e.detect(1.0);
        assert_eq!(e.detect(1.0), Edge::None);
        assert_eq!(e.detect(0.0), Edge::Falling);
        assert_eq!(e.detect(0.0), Edge::None);
    }

    #[test]
    fn threshold_is_gate_on() {
        let mut e = EdgeDetector::new();
        // Just below the threshold is still off.
        assert_eq!(e.detect(GATE_ON - f32::EPSILON), Edge::None);
        // At the threshold flips on.
        assert_eq!(e.detect(GATE_ON), Edge::Rising);
    }

    #[test]
    fn matches_the_open_coded_clock_idiom() {
        // The exact predicate every clock-driven operator used before this primitive:
        // rising = `prev < 0.5 && g >= 0.5`, falling = `prev >= 0.5 && g < 0.5`.
        let levels = [0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];
        let mut prev = 0.0f32;
        let mut detector = EdgeDetector::new();
        for &g in &levels {
            let want = if prev < 0.5 && g >= 0.5 {
                Edge::Rising
            } else if prev >= 0.5 && g < 0.5 {
                Edge::Falling
            } else {
                Edge::None
            };
            assert_eq!(detector.detect(g), want, "level {g}, prev {prev}");
            prev = g;
        }
    }
}
