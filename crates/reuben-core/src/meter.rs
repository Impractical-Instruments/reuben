//! Level metering: a block's fold into a peak and a sum of squares, and the ballistics that turn a
//! stream of those into a held peak and an averaged square.
//!
//! The ballistics run where the signal is produced, rather than in whatever reads them: the reader
//! sees only the latest value, so a peak between two of its reads would otherwise be lost, and every
//! client would apply its own decay at its own poll rate. A [`Level`] costs, per commit, two relaxed
//! stores; a [`Ballistics`] costs two `exp`s per commit.

use core::sync::atomic::{AtomicU32, Ordering};

/// How fast a held peak falls back once the signal drops, in dB per second.
pub const PEAK_FALL_DB_PER_SEC: f32 = 12.0;
/// The time constant of the averaged square, in seconds.
pub const RMS_WINDOW_SECS: f32 = 0.3;

/// The most a block is metered at: +60 dBFS peak, +60 dBFS RMS, the reported ceiling. A channel
/// blowing up passes through huge finite values on its way to infinity or NaN; held at those, its
/// meter would take a minute to fall back once it recovers, and held at infinity, forever.
const BLOWN_UP_PEAK: f32 = 1_000.0;
const BLOWN_UP_MEAN_SQUARE: f32 = BLOWN_UP_PEAK * BLOWN_UP_PEAK;

/// Below this a held value is flushed to zero, so a channel falling silent leaves the held state at
/// exactly zero rather than decaying through denormals, which are slow on some CPUs. About
/// -200 dBFS, far under the reported floor. The samples folded in are not flushed: a block of
/// denormal samples costs the engine that produced it more than the meter.
const FLUSH_BELOW: f32 = 1e-10;

/// The per-frame decay rates at one sample rate, as `ln` factors.
#[derive(Debug, Clone, Copy)]
pub struct Ballistics {
    /// The peak's fall per frame, as a log gain.
    peak_fall_per_frame: f32,
    /// The RMS average's per-frame decay: `-1 / (window × sample rate)`.
    rms_decay_per_frame: f32,
}

impl Ballistics {
    /// Ballistics at `sample_rate`, floored at 1 Hz.
    pub fn new(sample_rate: f32) -> Self {
        let rate = sample_rate.max(1.0);
        Self {
            peak_fall_per_frame: -PEAK_FALL_DB_PER_SEC / 20.0 * core::f32::consts::LN_10 / rate,
            rms_decay_per_frame: -1.0 / (RMS_WINDOW_SECS * rate),
        }
    }

    /// The factors a held peak and an averaged square keep over `frames` frames.
    fn keep(&self, frames: usize) -> (f32, f32) {
        (
            num_traits::Float::exp(self.peak_fall_per_frame * frames as f32),
            num_traits::Float::exp(self.rms_decay_per_frame * frames as f32),
        )
    }
}

/// One block's samples folded to the two numbers a [`Level`] needs.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Block {
    pub peak: f32,
    pub sum_squares: f32,
}

impl Block {
    /// Fold `samples` into their peak magnitude and sum of squares.
    pub fn measure(samples: impl Iterator<Item = f32>) -> Self {
        let mut block = Self::default();
        for sample in samples {
            // `max` passes over a NaN sample; the sum of squares does not, and is caught on commit.
            block.peak = block.peak.max(sample.abs());
            block.sum_squares += sample * sample;
        }
        block
    }

    /// Fold `other` in as more samples of the same block: the louder peak, the summed squares.
    pub fn merge(&mut self, other: Block) {
        self.peak = self.peak.max(other.peak);
        self.sum_squares += other.sum_squares;
    }
}

/// One channel's held peak and averaged square, as `f32` bits in atomics.
///
/// Only the one thread that commits to a level writes it, so it reads its own last stores back as
/// its state; `Relaxed` throughout, since a reader wants the latest value of each and nothing
/// ordered between them.
#[derive(Debug, Default)]
pub struct Level {
    peak: AtomicU32,
    mean_square: AtomicU32,
}

impl Level {
    /// Fold one block, `frames` frames long, into the held state. Samples a block lacks are
    /// metered as silence, so a level committed nothing decays rather than freezing. A zero-frame
    /// commit is no time passing and leaves the level as it was.
    ///
    /// RT-safe: no allocation, lock or syscall.
    pub fn commit(&self, block: Block, frames: usize, ballistics: &Ballistics) {
        // The mean square divides by `frames`: zero frames would make it NaN, which reads as a
        // blown-up block and pins the level at the ceiling.
        if frames == 0 {
            return;
        }
        let (peak_fall, rms_keep) = ballistics.keep(frames);
        let mut peak = block.peak;
        let mut mean_square = block.sum_squares / frames as f32;
        // A mean square this large, or NaN, means a sample at least this large, so the peak goes
        // to the ceiling with it.
        if mean_square.is_nan() || mean_square > BLOWN_UP_MEAN_SQUARE {
            mean_square = BLOWN_UP_MEAN_SQUARE;
            peak = BLOWN_UP_PEAK;
        }
        let peak = peak.min(BLOWN_UP_PEAK);

        let held = f32::from_bits(self.peak.load(Ordering::Relaxed)) * peak_fall;
        let held = flush(peak.max(held));
        let averaged = f32::from_bits(self.mean_square.load(Ordering::Relaxed));
        let averaged = flush(mean_square + (averaged - mean_square) * rms_keep);
        self.peak.store(held.to_bits(), Ordering::Relaxed);
        self.mean_square
            .store(averaged.to_bits(), Ordering::Relaxed);
    }

    /// The linear held peak and averaged square as of the last commit.
    pub fn read(&self) -> (f32, f32) {
        (
            f32::from_bits(self.peak.load(Ordering::Relaxed)),
            f32::from_bits(self.mean_square.load(Ordering::Relaxed)),
        )
    }

    /// Back to silence.
    pub fn reset(&self) {
        self.peak.store(0, Ordering::Relaxed);
        self.mean_square.store(0, Ordering::Relaxed);
    }
}

fn flush(value: f32) -> f32 {
    if value < FLUSH_BELOW {
        0.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 128;

    /// Feed `seconds` of `signal(frame)` through `level` in block-sized commits.
    fn drive(level: &Level, seconds: f32, signal: impl Fn(usize) -> f32) {
        let ballistics = Ballistics::new(RATE);
        let blocks = (seconds * RATE) as usize / BLOCK;
        for b in 0..blocks {
            let block = Block::measure((0..BLOCK).map(|frame| signal(b * BLOCK + frame)));
            level.commit(block, BLOCK, &ballistics);
        }
    }

    fn peak_db(level: &Level) -> f32 {
        20.0 * level.read().0.log10()
    }

    fn rms_db(level: &Level) -> f32 {
        10.0 * level.read().1.log10()
    }

    fn sine(frame: usize, hz: f32, amplitude: f32) -> f32 {
        amplitude * (core::f32::consts::TAU * hz * frame as f32 / RATE).sin()
    }

    fn close(got: f32, want: f32, tolerance: f32) -> bool {
        (got - want).abs() <= tolerance
    }

    #[test]
    fn a_full_scale_sine_silence_and_dc_read_their_known_levels() {
        let (sine_level, silence, dc) = (Level::default(), Level::default(), Level::default());
        // 997 Hz, so the sampled peak reaches full scale within a block rather than landing on the
        // same few phases.
        drive(&sine_level, 2.0, |frame| sine(frame, 997.0, 1.0));
        drive(&silence, 2.0, |_| 0.0);
        drive(&dc, 2.0, |_| 0.5);

        assert!(close(peak_db(&sine_level), 0.0, 0.05));
        assert!(close(rms_db(&sine_level), -3.01, 0.05));
        assert_eq!(silence.read(), (0.0, 0.0));
        assert!(close(peak_db(&dc), -6.02, 0.01));
        assert!(close(rms_db(&dc), -6.02, 0.01));
    }

    #[test]
    fn a_peak_between_two_reads_is_held_and_falls_back_at_the_published_rate() {
        let level = Level::default();
        // One full-scale sample, then a second of silence.
        drive(&level, 1.0, |frame| if frame == 0 { 1.0 } else { 0.0 });
        // A block's own peak is stored undecayed, so the fall runs over every block but the first.
        let blocks = RATE as usize / BLOCK;
        let elapsed = ((blocks - 1) * BLOCK) as f32 / RATE;
        assert!(close(
            peak_db(&level),
            -PEAK_FALL_DB_PER_SEC * elapsed,
            0.01
        ));
    }

    #[test]
    fn rms_settles_with_the_published_time_constant() {
        let level = Level::default();
        // Ten time constants of full-scale DC: settled to within 0.01 dB.
        drive(&level, 10.0 * RMS_WINDOW_SECS, |_| 1.0);
        assert!(close(rms_db(&level), 0.0, 0.01));
        // Whole blocks only, so the silence lasts a little under one time constant.
        let blocks = (RMS_WINDOW_SECS * RATE) as usize / BLOCK;
        drive(&level, RMS_WINDOW_SECS, |_| 0.0);
        let silent_for = (blocks * BLOCK) as f32 / RATE;
        let want = 10.0 * (-silent_for / RMS_WINDOW_SECS).exp().log10();
        assert!(close(rms_db(&level), want, 0.01));
    }

    #[test]
    fn a_blown_up_block_reads_the_ceiling_then_recovers() {
        for bad in [f32::INFINITY, f32::NAN] {
            let level = Level::default();
            let mut samples = [0.0; BLOCK];
            samples[0] = bad;
            let ballistics = Ballistics::new(RATE);
            level.commit(Block::measure(samples.into_iter()), BLOCK, &ballistics);
            assert_eq!(peak_db(&level), 60.0);
            // An average, so one block moves it only part of the way to the ceiling.
            assert!(rms_db(&level) > 30.0);
            drive(&level, 30.0, |_| 0.0);
            assert_eq!(level.read(), (0.0, 0.0));
        }
    }

    #[test]
    fn a_huge_finite_sample_is_held_at_the_ceiling_so_it_falls_back_in_seconds() {
        let level = Level::default();
        let mut samples = [0.0; BLOCK];
        samples[0] = 1e30;
        level.commit(
            Block::measure(samples.into_iter()),
            BLOCK,
            &Ballistics::new(RATE),
        );
        assert_eq!(peak_db(&level), 60.0);
        // From the ceiling, 180 dB at 12 dB a second is 15 s to the reported floor.
        drive(&level, 16.0, |_| 0.0);
        assert!(peak_db(&level) <= -120.0 && rms_db(&level) <= -120.0);
    }

    #[test]
    fn an_empty_block_still_decays_the_level() {
        let level = Level::default();
        drive(&level, 1.0, |_| 1.0);
        let before = (peak_db(&level), rms_db(&level));
        level.commit(
            Block::measure(core::iter::empty()),
            RATE as usize,
            &Ballistics::new(RATE),
        );
        assert!(peak_db(&level) < before.0 - 10.0);
        assert!(rms_db(&level) < before.1 - 10.0);
    }

    #[test]
    fn a_zero_frame_commit_leaves_the_level_alone() {
        let level = Level::default();
        drive(&level, 0.1, |_| 0.5);
        let before = level.read();
        level.commit(Block::default(), 0, &Ballistics::new(RATE));
        assert_eq!(level.read(), before);
    }

    #[test]
    fn reset_returns_the_level_to_silence() {
        let level = Level::default();
        drive(&level, 0.1, |_| 1.0);
        level.reset();
        assert_eq!(level.read(), (0.0, 0.0));
    }

    #[test]
    fn merged_blocks_take_the_loudest_peak_and_sum_their_squares() {
        let mut block = Block {
            peak: 0.5,
            sum_squares: 2.0,
        };
        block.merge(Block {
            peak: 0.25,
            sum_squares: 1.0,
        });
        assert_eq!(block.peak, 0.5);
        assert_eq!(block.sum_squares, 3.0);
    }
}
