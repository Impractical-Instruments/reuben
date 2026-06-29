//! Reusable single-cycle wavetable with linear interpolation.
//!
//! A [`Wavetable`] stores one cycle of a periodic waveform sampled into a fixed table and read by
//! phase (turns in `[0, 1)`) with linear interpolation between the two neighbouring samples. It
//! owns its data and is built on the **cold** path (operator constructors / resource binding), so
//! the audio render thread only ever reads it — [`Wavetable::lookup`] does no allocation, no trig
//! call, and no branch on the phase wrap. This is the shared primitive behind the [`Oscillator`]'s
//! sine (via [`shared_sine`]) and, later, a wavetable oscillator that loads arbitrary band-limited
//! cycles from a resource.
//!
//! [`Oscillator`]: crate::operators::oscillator

use std::sync::OnceLock;

/// Samples per cycle in the built-in tables.
///
/// Reading a single-cycle sine by phase with linear interpolation introduces a periodic error whose
/// peak magnitude is bounded by `(Δφ)²/8 · max|f''|`, where `Δφ = 2π/N` is the inter-sample phase
/// step. For a unit sine that is `≈ π²/(2·N²)`. At `N = 4096` the worst-case error is `≈ 2.9e-7`
/// (about **-130 dBFS**), so the harmonics linear interpolation folds back in sit far below
/// audibility across the whole band — including low notes, where many cycles are read per second and
/// any table artefact would be most exposed. 4096 also leaves generous headroom for the richer
/// band-limited cycles a wavetable oscillator will store, not just a pure sine.
pub const TABLE_SIZE: usize = 4096;

/// A single cycle of a periodic waveform, sampled into a fixed table and read by phase with linear
/// interpolation. Owns its samples (built cold), so it serves both the built-in sine and arbitrary
/// cycles a future wavetable oscillator loads.
#[derive(Clone, Debug)]
pub struct Wavetable {
    /// One cycle: `size` real samples plus a trailing **guard** sample equal to the first
    /// (`data[size] == data[0]`). The guard lets a lookup in the last cell interpolate toward the
    /// wrap without a modulo or branch on the hot path.
    data: Vec<f32>,
    /// `data.len() - 1` — the number of real samples per cycle.
    size: usize,
    /// `size` as `f32`, precomputed for the per-sample phase→index map.
    size_f: f32,
}

impl Wavetable {
    /// Build a `size`-sample table from a closure mapping phase ∈ `[0, 1)` to amplitude. Allocates,
    /// so call it from a cold path only. Panics if `size < 2` (a table needs at least two points to
    /// interpolate between).
    pub fn from_phase_fn(size: usize, mut f: impl FnMut(f32) -> f32) -> Self {
        assert!(size >= 2, "wavetable needs at least 2 samples, got {size}");
        let mut data = Vec::with_capacity(size + 1);
        for i in 0..size {
            data.push(f(i as f32 / size as f32));
        }
        // Guard sample closes the cycle so `lookup` never has to wrap `i + 1` back to 0.
        data.push(data[0]);
        Self {
            data,
            size,
            size_f: size as f32,
        }
    }

    /// A single cycle of `sin(2π·phase)` sampled into `size` points.
    pub fn sine(size: usize) -> Self {
        Self::from_phase_fn(size, |p| (core::f32::consts::TAU * p).sin())
    }

    /// Samples per cycle (excludes the guard sample).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Read the table at `phase` (turns) with linear interpolation. Hot-path safe: no allocation, no
    /// trig, no branch on the wrap. The contract is `phase ∈ [0, 1)` (a `debug_assert` checks it);
    /// the index is clamped so any input stays memory-safe in release, but values outside `[0, 1)`
    /// extrapolate rather than wrap.
    #[inline]
    pub fn lookup(&self, phase: f32) -> f32 {
        debug_assert!(
            phase.is_finite() && (0.0..1.0).contains(&phase),
            "wavetable phase {phase} must be in [0, 1)"
        );
        let x = phase * self.size_f; // position in [0, size)
        let i = (x as usize).min(self.size - 1); // floor, clamped off the guard cell
        let frac = x - i as f32;
        let a = self.data[i];
        let b = self.data[i + 1]; // i + 1 ≤ size — the guard sample makes this always valid
        a + (b - a) * frac
    }
}

/// The process-wide single-cycle sine table, shared by every sine consumer so the table is built
/// once rather than per voice. Built lazily on first call; call it from a cold path (an operator's
/// `new`/`spawn`) so the audio thread only ever reads the already-built table.
pub fn shared_sine() -> &'static Wavetable {
    static SINE: OnceLock<Wavetable> = OnceLock::new();
    SINE.get_or_init(|| Wavetable::sine(TABLE_SIZE))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linear interpolation of the sine table tracks the true sine far below audibility. We assert a
    /// generous `1e-5` (≈ -100 dBFS) ceiling; the analytic bound at `TABLE_SIZE` is ~`2.9e-7`, so the
    /// real margin is ~30×. This is the test behind the "no audible harmonics, even low" claim.
    #[test]
    fn sine_lookup_tracks_true_sine_below_audibility() {
        let table = Wavetable::sine(TABLE_SIZE);
        // Dense sweep that deliberately lands between table points (not on integer indices).
        let probes = 200_003usize;
        let mut max_err = 0.0f32;
        for k in 0..probes {
            let phase = k as f32 / probes as f32; // [0, 1)
            let reference = (core::f32::consts::TAU * phase).sin();
            max_err = max_err.max((table.lookup(phase) - reference).abs());
        }
        assert!(
            max_err < 1e-5,
            "interpolated sine error {max_err} exceeds the inaudible ceiling 1e-5"
        );
    }

    /// Endpoints: phase 0 reads the first sample exactly (`sin 0 == 0`); a quarter turn reads the
    /// peak (`sin(π/2) == 1`), which lands exactly on a sample at this power-of-two size.
    #[test]
    fn sine_lookup_hits_known_points() {
        let table = Wavetable::sine(TABLE_SIZE);
        assert!(table.lookup(0.0).abs() < 1e-6, "sin(0) should be 0");
        assert!(
            (table.lookup(0.25) - 1.0).abs() < 1e-6,
            "sin(2π·0.25) should be 1"
        );
    }

    /// The guard sample makes the final cell interpolate toward the wrap (back to sample 0) instead
    /// of reading out of bounds: a phase just below 1.0 stays finite and near `sin(2π) == 0`.
    #[test]
    fn lookup_in_last_cell_wraps_via_guard() {
        let table = Wavetable::sine(TABLE_SIZE);
        let phase = (TABLE_SIZE as f32 - 0.5) / TABLE_SIZE as f32; // mid of the last cell
        let v = table.lookup(phase);
        assert!(v.is_finite(), "last-cell lookup must stay finite, got {v}");
        assert!(
            v.abs() < 1e-2,
            "near the wrap the sine should be ≈ 0, got {v}"
        );
    }

    /// A constant generator yields that constant at every phase — exercises `from_phase_fn` and shows
    /// interpolation is exact when neighbouring samples are equal.
    #[test]
    fn constant_table_reads_back_constant() {
        let table = Wavetable::from_phase_fn(8, |_| 0.7);
        assert_eq!(table.size(), 8);
        for k in 0..1000 {
            let phase = k as f32 / 1000.0;
            assert!((table.lookup(phase) - 0.7).abs() < 1e-6);
        }
    }

    /// The shared sine table is the same instance on every call (one allocation, shared by all sine
    /// consumers) and is a real sine.
    #[test]
    fn shared_sine_is_a_single_shared_instance() {
        let a = shared_sine();
        let b = shared_sine();
        assert!(
            std::ptr::eq(a, b),
            "shared_sine must hand back one instance"
        );
        assert_eq!(a.size(), TABLE_SIZE);
        assert!((a.lookup(0.25) - 1.0).abs() < 1e-6);
    }

    #[test]
    #[should_panic(expected = "at least 2 samples")]
    fn too_small_table_panics() {
        let _ = Wavetable::from_phase_fn(1, |_| 0.0);
    }
}
