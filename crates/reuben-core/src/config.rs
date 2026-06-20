//! Audio configuration, fixed for the life of a [`crate::plan::Plan`].

/// Fixed audio parameters a Plan is instantiated against.
///
/// Sample rate and block size do not change while a Plan runs; changing either is a
/// re-Instantiate (a Swap), not a Render-time mutation. See ADR-0009.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioConfig {
    /// Samples per second (e.g. 48_000.0).
    pub sample_rate: f32,
    /// Samples per Channel per Render block.
    pub block_size: usize,
}

impl AudioConfig {
    pub fn new(sample_rate: f32, block_size: usize) -> Self {
        assert!(sample_rate > 0.0, "sample_rate must be positive");
        assert!(block_size > 0, "block_size must be positive");
        Self {
            sample_rate,
            block_size,
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
