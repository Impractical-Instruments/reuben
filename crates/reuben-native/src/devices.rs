//! The output devices a [`DeviceProfile`](crate::profile::DeviceProfile) can name, for a caller
//! that offers the choice instead of reading it from an `--io-map` file.

use std::ops::RangeInclusive;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

use crate::audio::AudioError;

/// One output device of the default host, as it was when [`output_devices`] ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDevice {
    /// The name `output.device` selects it by. Given whole, it selects this device ahead of any
    /// whose name merely contains it. Names are compared without regard to case, so of two
    /// devices whose names differ only in case, or not at all, the first listed wins.
    pub name: String,
    /// Whether this is the device an omitted `output.device` opens.
    pub is_default: bool,
    /// The rate the device runs at when the profile states no `sample_rate`. `None` when the
    /// device would not report a default config (it may be busy, or unusable).
    pub default_sample_rate: Option<u32>,
    /// The rates in Hz a profile's `sample_rate` is granted at, ascending and non-overlapping.
    /// Empty when the device would not report its configs: unknown, not unsupported.
    pub sample_rates: Vec<RangeInclusive<u32>>,
    /// The frames a profile's `buffer_size` is clamped into. `None` when the device reports no
    /// range, in which case a `buffer_size` is requested as given.
    pub buffer_sizes: Option<RangeInclusive<u32>>,
}

impl OutputDevice {
    /// Whether a profile's `sample_rate` of `hz` would be granted. `true` when the rates are
    /// unknown: the engine falls back to the device default with a warning if it is not.
    pub fn supports_sample_rate(&self, hz: u32) -> bool {
        self.sample_rates.is_empty() || self.sample_rates.iter().any(|r| r.contains(&hz))
    }

    /// Whether a profile's `buffer_size` of `frames` would be granted unclamped. `true` when the
    /// range is unknown.
    pub fn supports_buffer_size(&self, frames: u32) -> bool {
        self.buffer_sizes
            .as_ref()
            .is_none_or(|r| r.contains(&frames))
    }
}

/// The one of `devices` a profile's `output.device` of `name` selects, by the rule the engine
/// opens a device with. `devices` must be in the order [`output_devices`] listed them.
pub fn selected<'a>(devices: &'a [OutputDevice], name: &str) -> Option<&'a OutputDevice> {
    let index = crate::audio::named_index(devices.iter().map(|d| Some(d.name.clone())), name)?;
    devices.get(index)
}

/// List the default host's output devices, in the order `output.device` is matched against them.
///
/// Opens no stream, but asks each device for its configs, which some hosts answer by briefly
/// opening the device: call it on demand, not per frame. A device whose name cannot be read is
/// left out, since no profile could select it.
pub fn output_devices() -> Result<Vec<OutputDevice>, AudioError> {
    let host = cpal::default_host();
    let default_name = host.default_output_device().and_then(|d| d.name().ok());
    let devices = host.output_devices().map_err(AudioError::DevicesQuery)?;
    Ok(devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            let default_config = device.default_output_config().ok();
            let configs: Vec<_> = device
                .supported_output_configs()
                .map(Iterator::collect)
                .unwrap_or_default();
            Some(OutputDevice {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
                default_sample_rate: default_config.as_ref().map(|c| c.sample_rate().0),
                sample_rates: sample_rates(&configs),
                buffer_sizes: default_config.and_then(|c| match c.buffer_size() {
                    SupportedBufferSize::Range { min, max } => Some(*min..=*max),
                    SupportedBufferSize::Unknown => None,
                }),
            })
        })
        .collect())
}

/// The union of the f32 configs' rate ranges — the only format the output stream opens in — with
/// overlapping and adjacent ranges merged.
fn sample_rates(configs: &[SupportedStreamConfigRange]) -> Vec<RangeInclusive<u32>> {
    let mut ranges: Vec<(u32, u32)> = configs
        .iter()
        .filter(|c| c.sample_format() == SampleFormat::F32)
        .map(|c| (c.min_sample_rate().0, c.max_sample_rate().0))
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (min, max) in ranges {
        match merged.last_mut() {
            Some(last) if min <= last.1.saturating_add(1) => last.1 = last.1.max(max),
            _ => merged.push((min, max)),
        }
    }
    merged.into_iter().map(|(min, max)| min..=max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(format: SampleFormat, min: u32, max: u32) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(
            2,
            cpal::SampleRate(min),
            cpal::SampleRate(max),
            SupportedBufferSize::Unknown,
            format,
        )
    }

    fn device(
        sample_rates: Vec<RangeInclusive<u32>>,
        buffer_sizes: Option<RangeInclusive<u32>>,
    ) -> OutputDevice {
        OutputDevice {
            name: "Scarlett".to_string(),
            is_default: false,
            default_sample_rate: None,
            sample_rates,
            buffer_sizes,
        }
    }

    #[test]
    fn sample_rates_are_the_merged_f32_ranges() {
        let configs = [
            config(SampleFormat::F32, 88_200, 96_000),
            config(SampleFormat::I16, 8_000, 192_000),
            config(SampleFormat::F32, 44_100, 48_000),
            config(SampleFormat::F32, 48_000, 48_000),
            config(SampleFormat::F32, 48_001, 50_000),
        ];
        assert_eq!(
            sample_rates(&configs),
            vec![44_100..=50_000, 88_200..=96_000]
        );
        assert!(sample_rates(&[]).is_empty());
    }

    #[test]
    fn an_unreported_rate_or_buffer_range_supports_anything() {
        let unknown = device(Vec::new(), None);
        assert!(unknown.supports_sample_rate(44_100));
        assert!(unknown.supports_buffer_size(7));

        let known = device(std::iter::once(44_100..=48_000).collect(), Some(64..=1024));
        assert!(known.supports_sample_rate(48_000));
        assert!(!known.supports_sample_rate(96_000));
        assert!(known.supports_buffer_size(64));
        assert!(!known.supports_buffer_size(2048));
    }
}
