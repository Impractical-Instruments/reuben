//! Level meters at the device: each audio callback folds its block into one [`Bank`], and the
//! engine-channel server reads the banks as a [`MetersReport`].
//!
//! The fold and ballistics are [`reuben_api::meter`]'s; they run here, on the audio thread, so the
//! reader sees a held value rather than whatever its own poll rate happens to catch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use reuben_api::engine::{ChannelLevel, MetersReport};
use reuben_api::meter::{Ballistics, Block, Level};

/// One direction's channels: the output device's, or the input device's.
///
/// Only the one callback that meters this bank writes its levels; see [`Level`].
#[derive(Debug)]
pub struct Bank {
    channels: Box<[Level]>,
    frames: AtomicU64,
    ballistics: Ballistics,
}

impl Bank {
    /// `channels` channels, metered at `sample_rate`.
    pub fn new(channels: usize, sample_rate: f32) -> Arc<Self> {
        Arc::new(Self {
            channels: (0..channels).map(|_| Level::default()).collect(),
            frames: AtomicU64::new(0),
            ballistics: Ballistics::new(sample_rate),
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
        for (channel, level) in self.channels.iter().enumerate() {
            let samples = block.iter().skip(channel).step_by(width).copied();
            level.commit(Block::measure(samples), frames, &self.ballistics);
        }
        self.frames.fetch_add(frames as u64, Ordering::Relaxed);
    }

    /// Each channel's level as of the last block metered.
    pub fn levels(&self) -> Vec<ChannelLevel> {
        self.channels
            .iter()
            .map(|level| {
                let (peak, mean_square) = level.read();
                ChannelLevel::from_linear(peak, mean_square)
            })
            .collect()
    }

    /// Frames metered since the bank was made.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// Both directions' banks, as the engine-channel server reads them.
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
            probes: Vec::new(),
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
