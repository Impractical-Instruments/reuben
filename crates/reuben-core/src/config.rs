//! Audio configuration, fixed for the life of a [`crate::plan::Plan`].

/// Fixed audio parameters a Plan is instantiated against.
///
/// Sample rate and block size do not change while a Plan runs; changing either is a
/// re-Instantiate (a Swap), not a Render-time mutation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioConfig {
    /// Samples per second (e.g. 48_000.0).
    pub sample_rate: f32,
    /// Samples per Channel per Render block.
    pub block_size: usize,
    /// Logical master channel count. This is **derived from the instrument** at
    /// [`Plan::instantiate`](crate::plan::Plan::instantiate) (max referenced tap `channel` +
    /// 1, floor 2), which overwrites whatever value `new` seeded here — the device's real
    /// channel count never enters core (`audio.rs` owns the logical→device map). The seed
    /// only matters for a renderer built without a Plan.
    pub channels: usize,
    /// Logical **input** channel count — the input master's width, the dual of
    /// [`channels`](Self::channels). Also derived from the instrument at
    /// [`Plan::instantiate`](crate::plan::Plan::instantiate): max bound input `channel` + 1
    /// across the played top graph's input pipes, **`0` when the patch binds none** — the
    /// common case pays nothing (no floor: unlike the output master's stereo floor, a patch
    /// without input pipes has no input master at all). The device's real input geometry
    /// never enters core; the device layer (P4/P5) maps onto these logical indices.
    pub input_channels: usize,
}

impl AudioConfig {
    /// Floor for the logical master width: even a fully-mono patch presents at
    /// least stereo, so a mono/5.1 device needs no special-casing in core.
    pub const MIN_CHANNELS: usize = 2;

    pub fn new(sample_rate: f32, block_size: usize) -> Self {
        assert!(sample_rate > 0.0, "sample_rate must be positive");
        assert!(block_size > 0, "block_size must be positive");
        Self {
            sample_rate,
            block_size,
            // Placeholders; `Plan::instantiate` derives the real widths from the instrument.
            channels: Self::MIN_CHANNELS,
            input_channels: 0,
        }
    }

    /// Duration of one sample, in seconds.
    pub fn sample_period(&self) -> f32 {
        1.0 / self.sample_rate
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self::new(48_000.0, 128)
    }
}
