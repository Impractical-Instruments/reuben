//! Level meters at the device: each audio callback folds its block into one [`Bank`], and the
//! structure server reads the banks as a [`MetersReport`].
//!
//! The ballistics run here, on the audio thread, rather than in whatever reads them: the reader
//! sees only the latest value, so a peak between two of its reads would otherwise be lost, and
//! every client would apply its own decay at its own poll rate. Per callback that costs one `exp`
//! and, per channel, a fold over its samples and two relaxed stores.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use reuben_api::engine::{ChannelLevel, MetersReport, PEAK_FALL_DB_PER_SEC, RMS_WINDOW_SECS};

/// What a block reporting a non-finite level is metered as instead: +60 dBFS peak, +60 dBFS RMS.
/// Finite, so the held value falls back once the channel recovers rather than sticking at
/// infinity, and the reader still clamps it to the ceiling.
const BLOWN_UP_PEAK: f32 = 1_000.0;
const BLOWN_UP_MEAN_SQUARE: f32 = BLOWN_UP_PEAK * BLOWN_UP_PEAK;

/// Below this a held value is flushed to zero, so a channel falling silent decays to exactly zero
/// rather than through denormals, which are slow on some CPUs. About -200 dBFS, far under the
/// reported floor.
const FLUSH_BELOW: f32 = 1e-10;

/// One direction's channels: the output device's, or the input device's.
///
/// Each channel's held peak and averaged square live in atomics as `f32` bits. Only the one
/// callback that meters this bank writes them, so it reads its own last stores back as its state;
/// `Relaxed` throughout, since a reader wants the latest value of each and nothing ordered
/// between them.
#[derive(Debug)]
pub struct Bank {
    channels: Box<[Level]>,
    frames: AtomicU64,
    /// The peak's fall per frame, as a log gain: `ln` of the factor it is multiplied by each frame.
    peak_fall_per_frame: f32,
    /// The RMS average's per-frame `ln` decay: `-1 / (window × sample rate)`.
    rms_decay_per_frame: f32,
}

#[derive(Debug, Default)]
struct Level {
    peak: AtomicU32,
    mean_square: AtomicU32,
}

impl Bank {
    /// `channels` channels, metered at `sample_rate`.
    pub fn new(channels: usize, sample_rate: f32) -> Arc<Self> {
        let rate = sample_rate.max(1.0);
        Arc::new(Self {
            channels: (0..channels).map(|_| Level::default()).collect(),
            frames: AtomicU64::new(0),
            peak_fall_per_frame: -PEAK_FALL_DB_PER_SEC / 20.0 * std::f32::consts::LN_10 / rate,
            rms_decay_per_frame: -1.0 / (RMS_WINDOW_SECS * rate),
        })
    }

    /// A bank with no channels, for a direction with no device.
    pub fn none() -> Arc<Self> {
        Self::new(0, 1.0)
    }

    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Fold one block of interleaved samples, `channels()` wide, into the levels.
    ///
    /// RT-safe: no allocation, lock or syscall. A ragged tail is ignored. An empty or short block
    /// still advances the ballistics by `frames`, metered as silence past the samples it has, so a
    /// meter whose callback is fed nothing decays rather than freezing.
    pub fn meter(&self, block: &[f32], frames: usize) {
        let width = self.channels.len();
        if width == 0 || frames == 0 {
            return;
        }
        let whole = (block.len() / width).min(frames);
        let block = &block[..whole * width];
        let peak_fall = (self.peak_fall_per_frame * frames as f32).exp();
        let rms_keep = (self.rms_decay_per_frame * frames as f32).exp();
        for (channel, level) in self.channels.iter().enumerate() {
            let mut peak = 0.0f32;
            let mut sum_squares = 0.0f32;
            for &sample in block.iter().skip(channel).step_by(width) {
                // `max` passes over a NaN sample; the sum of squares does not, and is caught below.
                peak = peak.max(sample.abs());
                sum_squares += sample * sample;
            }
            let mut mean_square = sum_squares / frames as f32;
            if !peak.is_finite() {
                peak = BLOWN_UP_PEAK;
            }
            if !mean_square.is_finite() {
                mean_square = BLOWN_UP_MEAN_SQUARE;
                peak = peak.max(BLOWN_UP_PEAK);
            }

            let held = f32::from_bits(level.peak.load(Ordering::Relaxed)) * peak_fall;
            let held = flush(peak.max(held));
            let averaged = f32::from_bits(level.mean_square.load(Ordering::Relaxed));
            let averaged = flush(mean_square + (averaged - mean_square) * rms_keep);
            level.peak.store(held.to_bits(), Ordering::Relaxed);
            level
                .mean_square
                .store(averaged.to_bits(), Ordering::Relaxed);
        }
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    /// Each channel's level as of the last block metered.
    pub fn levels(&self) -> Vec<ChannelLevel> {
        self.channels
            .iter()
            .map(|level| {
                ChannelLevel::from_linear(
                    f32::from_bits(level.peak.load(Ordering::Relaxed)),
                    f32::from_bits(level.mean_square.load(Ordering::Relaxed)),
                )
            })
            .collect()
    }

    /// Frames metered since the bank was made.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

fn flush(value: f32) -> f32 {
    if value < FLUSH_BELOW {
        0.0
    } else {
        value
    }
}

/// Both directions' banks, as the structure server reads them.
#[derive(Debug, Clone)]
pub struct Meters {
    pub outputs: Arc<Bank>,
    pub inputs: Arc<Bank>,
}

impl Meters {
    /// No channels either way: a host with no device.
    pub fn none() -> Self {
        Self {
            outputs: Bank::none(),
            inputs: Bank::none(),
        }
    }

    pub fn report(&self) -> MetersReport {
        MetersReport {
            outputs: self.outputs.levels(),
            inputs: self.inputs.levels(),
            frames: self.outputs.frames(),
        }
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::engine::{METER_CEILING_DBFS, METER_FLOOR_DBFS};

    use super::*;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 128;

    /// Feed `seconds` of `signal(frame, channel)` through `bank` in callback-sized blocks.
    fn drive(bank: &Bank, seconds: f32, signal: impl Fn(usize, usize) -> f32) {
        let width = bank.channels();
        let mut block = vec![0.0; BLOCK * width];
        let blocks = (seconds * RATE) as usize / BLOCK;
        for b in 0..blocks {
            for frame in 0..BLOCK {
                for channel in 0..width {
                    block[frame * width + channel] = signal(b * BLOCK + frame, channel);
                }
            }
            bank.meter(&block, BLOCK);
        }
    }

    fn sine(frame: usize, hz: f32, amplitude: f32) -> f32 {
        amplitude * (std::f32::consts::TAU * hz * frame as f32 / RATE).sin()
    }

    fn close(got: f32, want: f32, tolerance: f32) -> bool {
        (got - want).abs() <= tolerance
    }

    #[test]
    fn a_full_scale_sine_silence_and_dc_read_their_known_levels() {
        let bank = Bank::new(3, RATE);
        // 997 Hz, so the sampled peak reaches full scale within a block rather than landing on the
        // same few phases.
        drive(&bank, 2.0, |frame, channel| match channel {
            0 => sine(frame, 997.0, 1.0),
            1 => 0.0,
            _ => 0.5,
        });
        let levels = bank.levels();

        assert!(close(levels[0].peak_dbfs, 0.0, 0.05), "{:?}", levels[0]);
        assert!(close(levels[0].rms_dbfs, -3.01, 0.05), "{:?}", levels[0]);

        assert_eq!(levels[1].peak_dbfs, METER_FLOOR_DBFS);
        assert_eq!(levels[1].rms_dbfs, METER_FLOOR_DBFS);

        assert!(close(levels[2].peak_dbfs, -6.02, 0.01), "{:?}", levels[2]);
        assert!(close(levels[2].rms_dbfs, -6.02, 0.01), "{:?}", levels[2]);

        assert_eq!(
            bank.frames(),
            (2.0 * RATE) as u64 / BLOCK as u64 * BLOCK as u64
        );
    }

    #[test]
    fn a_peak_between_two_reads_is_held_and_falls_back_at_the_published_rate() {
        let bank = Bank::new(1, RATE);
        // One full-scale sample, then a second of silence.
        drive(&bank, 1.0, |frame, _| if frame == 0 { 1.0 } else { 0.0 });
        let level = bank.levels()[0];
        let elapsed = (RATE as usize / BLOCK * BLOCK - 1) as f32 / RATE;
        assert!(
            close(level.peak_dbfs, -PEAK_FALL_DB_PER_SEC * elapsed, 0.1),
            "{level:?}"
        );
    }

    #[test]
    fn rms_settles_with_the_published_time_constant() {
        let bank = Bank::new(1, RATE);
        // Ten time constants of full-scale DC: settled to within 0.01 dB.
        drive(&bank, 10.0 * RMS_WINDOW_SECS, |_, _| 1.0);
        let settled = bank.levels()[0];
        assert!(close(settled.rms_dbfs, 0.0, 0.01), "{settled:?}");
        // Whole blocks only, so the silence lasts a little under one time constant.
        let blocks = (RMS_WINDOW_SECS * RATE) as usize / BLOCK;
        drive(&bank, RMS_WINDOW_SECS, |_, _| 0.0);
        let silent_for = (blocks * BLOCK) as f32 / RATE;
        let want = 10.0 * (-silent_for / RMS_WINDOW_SECS).exp().log10();
        let level = bank.levels()[0];
        assert!(close(level.rms_dbfs, want, 0.01), "{level:?}");
    }

    #[test]
    fn a_blown_up_channel_reads_the_ceiling_then_recovers() {
        let bank = Bank::new(2, RATE);
        let mut block = vec![0.0; BLOCK * 2];
        block[0] = f32::INFINITY;
        block[1] = f32::NAN;
        bank.meter(&block, BLOCK);
        for level in bank.levels() {
            assert_eq!(level.peak_dbfs, METER_CEILING_DBFS, "{level:?}");
            // An average, so one block moves it only part of the way to the ceiling.
            assert!(level.rms_dbfs > 30.0, "{level:?}");
        }
        drive(&bank, 30.0, |_, _| 0.0);
        for level in bank.levels() {
            assert_eq!(level.peak_dbfs, METER_FLOOR_DBFS, "{level:?}");
            assert_eq!(level.rms_dbfs, METER_FLOOR_DBFS, "{level:?}");
        }
    }

    #[test]
    fn a_block_shorter_than_its_frames_meters_the_rest_as_silence() {
        let bank = Bank::new(1, RATE);
        drive(&bank, 1.0, |_, _| 1.0);
        let before = bank.levels()[0];
        bank.meter(&[], RATE as usize);
        let after = bank.levels()[0];
        assert!(after.peak_dbfs < before.peak_dbfs - 10.0, "{after:?}");
        assert!(after.rms_dbfs < before.rms_dbfs - 10.0, "{after:?}");
    }

    #[test]
    fn a_bank_with_no_channels_reports_none() {
        let meters = Meters::none();
        meters.outputs.meter(&[0.5; 4], 4);
        let report = meters.report();
        assert!(report.outputs.is_empty() && report.inputs.is_empty());
        assert_eq!(report.frames, 0);
    }
}
