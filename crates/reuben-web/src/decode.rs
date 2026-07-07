//! WAV bytes → [`SampleBuffer`], entirely in Rust (issue #224: hound-in-WASM v1).
//!
//! The web shell stages sample resources as raw fetched bytes; a worklet can't call
//! `decodeAudioData` (and the discovery instance shouldn't round-trip audio through JS), so
//! decode happens behind the same [`ResourceResolver`](reuben_core::resources::ResourceResolver)
//! seam the native shell fills — portable to the game shell (#222) unchanged. Decoded at the
//! file's native rate; the sample player already resamples at render (ADR-0016).
//! `decodeAudioData` is deferred to when compressed formats land.
//!
//! This mirrors native's `decode_wav` (`reuben-native/src/resources.rs`) over an in-memory
//! cursor instead of a file: integer PCM normalized to `[-1, 1)`, float PCM passes through,
//! every channel kept planar.

use std::io::Cursor;

use reuben_core::resources::{ResolveError, SampleBuffer};

/// Decode a WAV byte buffer into a planar [`SampleBuffer`] at its native sample rate.
pub fn decode_wav_bytes(bytes: &[u8]) -> Result<SampleBuffer, ResolveError> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))
        .map_err(|e| ResolveError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err(ResolveError::Decode("zero channels".to_string()));
    }
    let sample_rate = spec.sample_rate as f32;

    // De-interleave into one Vec per channel.
    let mut planar: Vec<Vec<f32>> = vec![Vec::new(); channels];
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (i, s) in reader.samples::<f32>().enumerate() {
                let v = s.map_err(|e| ResolveError::Decode(e.to_string()))?;
                planar[i % channels].push(v);
            }
        }
        hound::SampleFormat::Int => {
            // Normalize by the full-scale magnitude for the bit depth.
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for (i, s) in reader.samples::<i32>().enumerate() {
                let v = s.map_err(|e| ResolveError::Decode(e.to_string()))?;
                planar[i % channels].push(v as f32 / scale);
            }
        }
    }

    Ok(SampleBuffer::new(planar, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a tiny stereo 16-bit WAV in memory and decode it back.
    #[test]
    fn decodes_stereo_int_wav_from_bytes_and_normalizes() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut bytes: Vec<u8> = Vec::new();
        {
            let mut w = hound::WavWriter::new(Cursor::new(&mut bytes), spec).expect("create wav");
            // Frame 0: L=+full, R=0. Frame 1: L=0, R=-full.
            w.write_sample(i16::MAX).unwrap();
            w.write_sample(0i16).unwrap();
            w.write_sample(0i16).unwrap();
            w.write_sample(i16::MIN).unwrap();
            w.finalize().unwrap();
        }

        let buf = decode_wav_bytes(&bytes).expect("decode");
        assert_eq!(buf.channel_count(), 2);
        assert_eq!(buf.frame_count(), 2);
        assert_eq!(buf.sample_rate(), 44_100.0);
        assert!((buf.sample(0, 0) - 1.0).abs() < 1e-3, "L0 ~ +1");
        assert_eq!(buf.sample(1, 0), 0.0);
        assert!((buf.sample(1, 1) + 1.0).abs() < 1e-3, "R1 ~ -1");
    }

    #[test]
    fn garbage_bytes_are_a_decode_error_not_a_panic() {
        assert!(matches!(
            decode_wav_bytes(b"not a wav at all"),
            Err(ResolveError::Decode(_))
        ));
        assert!(matches!(
            decode_wav_bytes(&[]),
            Err(ResolveError::Decode(_))
        ));
    }

    /// The repo's real sample assets decode — the exact bytes the browser will fetch.
    #[test]
    fn decodes_the_repo_sample_assets() {
        for (name, bytes) in [
            (
                "blip.wav",
                &include_bytes!("../../../instruments/samples/blip.wav")[..],
            ),
            (
                "testvoice.wav",
                &include_bytes!("../../../instruments/samples/testvoice.wav")[..],
            ),
        ] {
            let buf = decode_wav_bytes(bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(buf.frame_count() > 0, "{name}: empty decode");
            assert!(buf.sample_rate() > 0.0, "{name}: zero sample rate");
        }
    }
}
