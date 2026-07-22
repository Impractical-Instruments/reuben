//! Filesystem + WAV resource resolution — the native side of the resource seam.
//!
//! The portable core defines [`SampleBuffer`] / [`ResourceResolver`] but stays codec-free.
//! This module fills the seam with a filesystem [`ResourceResolver`]
//! that decodes **WAV** (`hound`; PCM int + float — tiny, deterministic, no codec
//! licensing). Compressed formats and non-file sources drop in behind the same trait later.
//!
//! Paths in a resource table resolve **relative to the referencing document's directory** (a
//! sample or sub-patch lives next to the file that names it), falling back to a configurable
//! [library root](FsResolver::with_root) — so a project keeps local references working while
//! shared patches come from one place. Identity is the resolver's job:
//! [`FsResolver::canonical`] lexically normalizes the winning absolute path, so `a.json`,
//! `./a.json`, and `x/../a.json` are one cycle-guard/dedup key. Symlinks are *not* chased —
//! canonicalization never does IO beyond the sibling-vs-root existence probe.

use std::path::{Component, Path, PathBuf};

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};

/// Resolves resource sources as filesystem paths relative to a base directory, decoding WAV.
pub struct FsResolver {
    base_dir: PathBuf,
    /// Library fallback (the configurable instrument-root): a source that does not exist
    /// next to its referencing document is looked up here instead.
    root: Option<PathBuf>,
    /// Check sample availability (a stat) instead of decoding — for introspection paths like
    /// `describe`, which only report port metadata and never touch audio, so eagerly decoding
    /// every referenced WAV would be pure waste. Patch text still reads for real: nested
    /// boundaries can't be described without building the nested graph.
    stat_only: bool,
}

impl FsResolver {
    /// A resolver rooted at `base_dir` (typically the instrument file's parent directory).
    /// The base is made absolute up front so [`canonical`](ResourceResolver::canonical) ids
    /// are absolute — one identity regardless of how the caller spelled the base.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: absolute(base_dir.into()),
            root: None,
            stat_only: false,
        }
    }

    /// Fall back to a library root: a source that does not exist relative to its referencing
    /// document resolves against `root` instead (sibling-first search). Local project
    /// references keep working; shared patches come from the root.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.root = Some(absolute(root.into()));
        self
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

/// Make `p` absolute against the current directory and lexically normalize it (collapse
/// `.`/`..` without touching the filesystem — symlinks are deliberately not chased).
fn absolute(p: PathBuf) -> PathBuf {
    normalize(&std::path::absolute(&p).unwrap_or(p))
}

/// Lexical normalization: fold `.` away and resolve `..` against the path built so far.
/// Pure string work — no IO, so it holds for missing files and `stat_only` introspection.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
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

    /// Read a patch path (an instrument-kind resource) to its JSON text, relative to
    /// the base dir like a sample. Core then builds it into a sub-`Graph`.
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = self.base_dir.join(source);
        std::fs::read_to_string(&path)
            .map_err(|e| ResolveError::NotFound(format!("{}: {e}", path.display())))
    }

    /// Write JSON text back to a document path — the write half of the seam, resolving
    /// `source` to the same location [`resolve_text`](Self::resolve_text) reads from
    /// (`base_dir.join`, through which the loader's canonical absolute path passes
    /// unchanged). Missing parent directories are created so a new document lands where the
    /// author addressed it. This is the mechanism that makes the MCP sidecar a process which
    /// writes to disk; the stance change that documents it belongs in the ADR ticket.
    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        let path = self.base_dir.join(source);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ResolveError::Write(format!("{}: {e}", parent.display())))?;
        }
        std::fs::write(&path, text)
            .map_err(|e| ResolveError::Write(format!("{}: {e}", path.display())))
    }

    /// Canonical identity = the winning absolute path, lexically normalized. Sibling-first:
    /// resolve relative to the referencing document's directory (`referrer` is that document's
    /// canonical id; the top level uses `base_dir`); if nothing exists there and a
    /// [library root](FsResolver::with_root) is configured, a hit under the root wins instead.
    /// A miss in both canonicalizes to the sibling candidate, so the eventual `NotFound`
    /// warning names the path the author most likely meant.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        let base = referrer
            .and_then(|r| Path::new(r).parent())
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(&self.base_dir);
        let sibling = normalize(&base.join(source));
        if let Some(root) = &self.root {
            if !sibling.is_file() {
                let rooted = normalize(&root.join(source));
                if rooted.is_file() {
                    return rooted.display().to_string();
                }
            }
        }
        sibling.display().to_string()
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

    /// The write half round-trips through `resolve_text`, creating a missing parent directory
    /// so a brand-new document lands where the author addressed it.
    #[test]
    fn write_text_creates_parents_and_round_trips() {
        let base = std::env::temp_dir().join("reuben_write_text_test");
        let _ = std::fs::remove_dir_all(&base);
        let resolver = FsResolver::new(&base);

        // A nested source whose parent does not exist yet.
        resolver
            .write_text("nested/patch.json", "{\"version\":3}")
            .expect("write");
        assert_eq!(
            resolver
                .resolve_text("nested/patch.json")
                .expect("read back"),
            "{\"version\":3}"
        );
        // An overwrite replaces in place — one source, one identity.
        resolver
            .write_text("nested/patch.json", "{\"version\":4}")
            .expect("overwrite");
        assert_eq!(
            resolver
                .resolve_text("nested/patch.json")
                .expect("read back"),
            "{\"version\":4}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The loader hands both halves a **canonical absolute** source (it canonicalizes before
    /// calling `resolve_text`), and `base_dir.join` passes an absolute path through unchanged —
    /// so a write addressed by the canonical id lands where a read by that same id looks.
    #[test]
    fn write_text_round_trips_a_canonical_absolute_source() {
        let base = std::env::temp_dir().join("reuben_write_text_abs_test");
        let _ = std::fs::remove_dir_all(&base);
        let resolver = FsResolver::new(&base);

        // What the loader would pass: the canonical (absolute) form of the source.
        let canon = resolver.canonical("song.json", None);
        assert!(
            Path::new(&canon).is_absolute(),
            "canonical ids are absolute"
        );
        resolver.write_text(&canon, "{\"v\":3}").expect("write abs");
        assert_eq!(
            resolver.resolve_text(&canon).expect("read abs"),
            "{\"v\":3}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// A write that cannot land surfaces `ResolveError::Write` — not a panic, and not the
    /// read-side `NotFound`. Here an ancestor of the target is a regular file, so the parent
    /// `create_dir_all` fails.
    #[test]
    fn write_text_reports_write_error_when_parent_cannot_be_created() {
        let base = std::env::temp_dir().join("reuben_write_text_err_test");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // "blocker" is a file; writing "blocker/child.json" needs it to be a directory.
        std::fs::write(base.join("blocker"), b"x").unwrap();

        let resolver = FsResolver::new(&base);
        let err = resolver
            .write_text("blocker/child.json", "{}")
            .expect_err("must fail");
        assert!(matches!(err, ResolveError::Write(_)), "got {err:?}");

        let _ = std::fs::remove_dir_all(&base);
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

    /// Canonicalization is lexical: spelling variants of one path are one
    /// identity, with no filesystem probe needed when no library root is configured.
    #[test]
    fn canonical_normalizes_path_spellings() {
        let resolver = FsResolver::new("/tmp/reuben_canon_base");
        let plain = resolver.canonical("a.json", None);
        assert_eq!(plain, resolver.canonical("./a.json", None));
        assert_eq!(plain, resolver.canonical("x/../a.json", None));
        assert!(
            Path::new(&plain).is_absolute(),
            "canonical ids are absolute"
        );
    }

    /// A referrer (the canonical id of the referencing document) rebases resolution: a nested
    /// patch's own references resolve next to *it*, not next to the top-level instrument.
    #[test]
    fn canonical_resolves_relative_to_the_referrer() {
        let resolver = FsResolver::new("/tmp/reuben_canon_base");
        let child = resolver.canonical("sub/pad.json", None);
        let leaf = resolver.canonical("kick.wav", Some(&child));
        // Platform-neutral (the base absolutizes to a drive root on Windows): resolving next
        // to the referrer must land on the same identity as spelling the path from the top.
        assert_eq!(leaf, resolver.canonical("sub/kick.wav", None));
        assert_eq!(
            resolver.canonical("../up.wav", Some(&child)),
            resolver.canonical("up.wav", None),
            "`..` climbs out of the referrer's directory"
        );
    }

    /// Sibling-first search: the library root only wins when nothing exists next to the
    /// referencing document; a local file shadows the library copy.
    #[test]
    fn canonical_falls_back_to_the_library_root() {
        let base = std::env::temp_dir().join("reuben_root_test/proj");
        let root = std::env::temp_dir().join("reuben_root_test/lib");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("shared.json"), "{}").unwrap();
        std::fs::write(base.join("local.json"), "{}").unwrap();
        std::fs::write(root.join("local.json"), "{}").unwrap();

        let resolver = FsResolver::new(&base).with_root(&root);
        // Only in the root: the root wins.
        assert_eq!(
            Path::new(&resolver.canonical("shared.json", None)),
            root.join("shared.json")
        );
        // In both: the sibling wins.
        assert_eq!(
            Path::new(&resolver.canonical("local.json", None)),
            base.join("local.json")
        );
        // In neither: canonicalizes to the sibling candidate (what NotFound will name).
        assert_eq!(
            Path::new(&resolver.canonical("missing.json", None)),
            base.join("missing.json")
        );

        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("reuben_root_test"));
    }

    #[test]
    fn resolve_text_reads_a_patch_file_and_builds_a_subgraph() {
        // The instrument-kind resource seam: write a voice patch, resolve its path to
        // text via FsResolver, and build it into a sub-Graph through core's `resolve_instrument`.
        let dir = std::env::temp_dir();
        let path = dir.join("reuben_test_voice.json");
        std::fs::write(
            &path,
            r#"{"instrument":"voice",
                "interface":{"inputs":{"freq":"/osc.freq"},"outputs":{"audio":"/osc.audio"}},
                "nodes":[{"type":"oscillator","address":"/osc"}],
                "outputs":[{"node":"/osc","port":"audio"}]}"#,
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
        // The oscillator plus the `freq` input pipe its migrated interface minted.
        assert_eq!(loaded.graph.nodes.len(), 2);
        assert!(loaded.graph.interface.inputs.contains_key("freq"));

        let _ = std::fs::remove_file(&path);
    }
}
