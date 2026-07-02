//! Filesystem + WAV resource resolution — the native side of the resource seam (ADR-0016).
//!
//! The portable core defines [`SampleBuffer`] / [`ResourceResolver`] but stays codec-free
//! (ADR-0007, ADR-0012). This module fills the seam with a filesystem [`ResourceResolver`]
//! that decodes **WAV** (`hound`; PCM int + float — tiny, deterministic, no codec
//! licensing). Compressed formats and non-file sources drop in behind the same trait later.
//!
//! Paths in a resource table resolve **relative to the instrument file's directory** (a
//! sample lives next to its rig); a configurable sample-root can come later.

use std::path::{Path, PathBuf};

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};

/// Resolves resource sources as filesystem paths relative to a base directory, decoding WAV.
pub struct FsResolver {
    base_dir: PathBuf,
    /// Check sample availability (a stat) instead of decoding — for introspection paths like
    /// `describe`, which only report port metadata and never touch audio, so eagerly decoding
    /// every referenced WAV would be pure waste. Patch text still reads for real: nested
    /// boundaries can't be described without building the nested graph.
    stat_only: bool,
}

impl FsResolver {
    /// A resolver rooted at `base_dir` (typically the instrument file's parent directory).
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            stat_only: false,
        }
    }

    /// A resolver rooted at the directory containing `instrument_path` (or `.` if it has
    /// no parent).
    pub fn for_instrument(instrument_path: &Path) -> Self {
        let base = instrument_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self::new(base)
    }

    /// Only stat samples instead of decoding them; missing files still report `NotFound`.
    pub fn stat_only(mut self) -> Self {
        self.stat_only = true;
        self
    }
}

impl ResourceResolver for FsResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        let path = self.base_dir.join(source);
        if self.stat_only {
            return match std::fs::metadata(&path) {
                Ok(m) if m.is_file() => Ok(SampleBuffer::empty()),
                Ok(_) => Err(ResolveError::NotFound(format!(
                    "{}: not a file",
                    path.display()
                ))),
                Err(e) => Err(ResolveError::NotFound(format!("{}: {e}", path.display()))),
            };
        }
        decode_wav(&path)
    }

    /// Read a patch path (an instrument-kind resource, ADR-0032 §2) to its JSON text, relative to
    /// the base dir like a sample. Core then builds it into a sub-`Graph`.
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = self.base_dir.join(source);
        std::fs::read_to_string(&path)
            .map_err(|e| ResolveError::NotFound(format!("{}: {e}", path.display())))
    }
}

/// Decode a WAV file into a planar [`SampleBuffer`] at its native sample rate. Integer PCM
/// is normalized to `[-1, 1)`; float PCM passes through.
pub fn decode_wav(path: &Path) -> Result<SampleBuffer, ResolveError> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| ResolveError::NotFound(format!("{}: {e}", path.display())))?;
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
    use reuben_core::resources::ResourceResolver;

    /// Write a tiny 2-channel int WAV to a temp path and read it back through the resolver.
    #[test]
    fn decodes_a_stereo_int_wav_and_normalizes() {
        let dir = std::env::temp_dir();
        let path = dir.join("reuben_test_stereo.wav");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 44_100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        {
            let mut w = hound::WavWriter::create(&path, spec).expect("create wav");
            // Frame 0: L=+full, R=0. Frame 1: L=0, R=-full.
            let full = i16::MAX;
            w.write_sample(full).unwrap();
            w.write_sample(0i16).unwrap();
            w.write_sample(0i16).unwrap();
            w.write_sample(i16::MIN).unwrap();
            w.finalize().unwrap();
        }

        let resolver = FsResolver::new(&dir);
        let buf = resolver.resolve("reuben_test_stereo.wav").expect("resolve");
        assert_eq!(buf.channel_count(), 2);
        assert_eq!(buf.frame_count(), 2);
        assert_eq!(buf.sample_rate(), 44_100.0);
        assert!((buf.sample(0, 0) - 1.0).abs() < 1e-3, "L0 ~ +1");
        assert_eq!(buf.sample(1, 0), 0.0);
        assert!((buf.sample(1, 1) + 1.0).abs() < 1e-3, "R1 ~ -1");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_is_not_found() {
        let resolver = FsResolver::new(".");
        assert!(matches!(
            resolver.resolve("does_not_exist_xyz.wav"),
            Err(ResolveError::NotFound(_))
        ));
    }

    /// Stat-only mode reports availability without decoding: a file whose bytes are not WAV at
    /// all still resolves (to an empty buffer), while a missing file is still `NotFound`.
    #[test]
    fn stat_only_checks_availability_without_decoding() {
        let dir = std::env::temp_dir();
        let path = dir.join("reuben_test_stat_only.wav");
        std::fs::write(&path, b"not a wav").unwrap();

        let resolver = FsResolver::new(&dir).stat_only();
        let buf = resolver.resolve("reuben_test_stat_only.wav").expect("stat");
        assert_eq!(buf.frame_count(), 0);
        assert!(matches!(
            resolver.resolve("does_not_exist_xyz.wav"),
            Err(ResolveError::NotFound(_))
        ));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn resolve_text_reads_a_patch_file_and_builds_a_subgraph() {
        // The instrument-kind resource seam (ADR-0032 §2): write a voice patch, resolve its path to
        // text via FsResolver, and build it into a sub-Graph through core's `resolve_instrument`.
        let dir = std::env::temp_dir();
        let path = dir.join("reuben_test_voice.json");
        std::fs::write(
            &path,
            r#"{"instrument":"voice",
                "interface":{"inputs":{"freq":"/osc.freq"},"outputs":{"audio":"/osc.audio"}},
                "nodes":[{"type":"oscillator","address":"/osc"}]}"#,
        )
        .unwrap();

        let resolver = FsResolver::new(&dir);
        let loaded = reuben_core::resolve_instrument(
            "reuben_test_voice.json",
            &reuben_core::Registry::builtin(),
            &resolver,
        )
        .expect("resolve patch");
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.graph.nodes.len(), 1);
        assert!(loaded.graph.interface.inputs.contains_key("freq"));

        let _ = std::fs::remove_file(&path);
    }
}
