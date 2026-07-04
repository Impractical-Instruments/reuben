//! Device profile (ADR-0038 §6/§7): a small JSON file, loaded with `--io-map <file>` on `play`,
//! that binds an instrument's *logical* master channels to a real device's channels, selects a
//! non-default device by name substring, and states sample-rate/buffer-size **preferences** the
//! engine requests against the device's supported configs. reuben never fights the device: the
//! outcome is granted, then adopted, then logged (`audio.rs`'s job) — never forced.
//!
//! Patches never learn device geography ([ADR-0026](../../../docs/adr/0026-v1-finish-line-osc-out-and-stereo.md));
//! this file is the one place logical↔device binding is spelled out, kept outside the patch so
//! the same instrument plays on any rig.
//!
//! **Structural** problems (malformed JSON, an unknown field, a map key/value that isn't a
//! channel index) are load errors — [`ProfileError`], surfaced by [`DeviceProfile::load`].
//! Once a profile parses, it is never a load error again: a map entry naming a channel that
//! turns out not to exist on the real device is a **reality mismatch**, handled by warn +
//! degrade at the point the mismatch is discovered (`audio.rs`'s output path; `crate::input`'s
//! input path) — see ADR-0038 §7. `input.*` is applied by the input stream
//! (P5, [#182](https://github.com/Impractical-Instruments/reuben/issues/182)), which opens
//! only when the played instrument binds input channels.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

/// One side's (`output` or `input`) device selection + channel map (ADR-0038 §6).
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SideProfile {
    /// Select a device by case-insensitive name substring. Omitted = the host's default
    /// device (today's only behavior).
    #[serde(default)]
    pub device: Option<String>,
    /// Output: logical channel → device channel. Input: device channel → logical channel.
    /// A JSON object, so keys are channel indices spelled as strings (`{"0": 2, "1": 3}`); a
    /// key or value that doesn't parse as a channel index is a structural [`ProfileError`],
    /// not a reality mismatch — that only exists once we know the real channel counts.
    #[serde(default)]
    pub map: BTreeMap<usize, usize>,
}

/// A device profile: the `--io-map <file>` document (ADR-0038 §6). Every field is optional —
/// a profile with every field omitted, or no `--io-map` at all ([`DeviceProfile::default`]),
/// means identity map + today's implicit broadcast/mono-downmix/zero-fill defaults, bit-identical
/// for existing instruments.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeviceProfile {
    #[serde(default)]
    pub output: SideProfile,
    /// Applied by the input stream (`crate::input`, P5/#182), which opens only when the
    /// played instrument binds input channels — see the module doc.
    #[serde(default)]
    pub input: SideProfile,
    /// Preferred output sample rate in Hz, requested against the device's supported configs
    /// (ADR-0038 §6/§8 — the engine renders at whatever the output device grants).
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Preferred output buffer size in frames, clamped into the device's supported range.
    #[serde(default)]
    pub buffer_size: Option<u32>,
}

/// Something wrong with the profile document itself — structural, not a device/reality
/// mismatch (those degrade with a warning at the point they're discovered; ADR-0038 §7).
#[derive(Debug)]
pub enum ProfileError {
    Io(std::io::Error, std::path::PathBuf),
    Json(serde_json::Error),
    /// A field violates a bound the schema declares (`"minimum": 1` on `sample_rate` and
    /// `buffer_size`) but serde can't enforce on a plain `Option<u32>`. Checked once at load,
    /// here — ADR-0038 §7 treats this as a structural document problem (like malformed JSON),
    /// not a reality mismatch, since a `0` would otherwise reach `negotiate_output_config` and
    /// fail opaquely inside cpal instead of at the clean load-time boundary the schema promises.
    OutOfRange(&'static str),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileError::Io(e, path) => {
                write!(f, "read io-map profile {}: {e}", path.display())
            }
            ProfileError::Json(e) => write!(f, "parse io-map profile: {e}"),
            ProfileError::OutOfRange(msg) => write!(f, "invalid io-map profile: {msg}"),
        }
    }
}

impl std::error::Error for ProfileError {}

impl DeviceProfile {
    /// Load + parse a device profile from `path`. Any structural problem (bad JSON, an unknown
    /// field, a non-numeric map key/value, an out-of-range `sample_rate`/`buffer_size`) is a
    /// load error — never silently ignored, per ADR-0038 §7's distinction between a broken
    /// document and a reality mismatch.
    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| ProfileError::Io(e, path.to_path_buf()))?;
        let profile: Self = serde_json::from_str(&text).map_err(ProfileError::Json)?;
        profile.validate()?;
        Ok(profile)
    }

    /// The schema's `"minimum": 1` bound on `sample_rate`/`buffer_size` (review finding #3) —
    /// `0` isn't a meaningful preference for either (cpal has no "silent"/"unbuffered" mode),
    /// so it's rejected here rather than flowing through to `negotiate_output_config`.
    fn validate(&self) -> Result<(), ProfileError> {
        if self.sample_rate == Some(0) {
            return Err(ProfileError::OutOfRange(
                "sample_rate must be at least 1 (0 is not a valid rate)",
            ));
        }
        if self.buffer_size == Some(0) {
            return Err(ProfileError::OutOfRange(
                "buffer_size must be at least 1 (0 is not a valid buffer size)",
            ));
        }
        Ok(())
    }

    /// True when the profile carries any `input.*` field (ADR-0038 §6/P5). `play` uses this
    /// to note that input settings take effect only when the played instrument binds input
    /// channels — an instrument without input pipes never opens an input device.
    pub fn has_input(&self) -> bool {
        self.input.device.is_some() || !self.input.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_identity() {
        let p = DeviceProfile::default();
        assert_eq!(p.output.device, None);
        assert!(p.output.map.is_empty());
        assert_eq!(p.input.device, None);
        assert!(p.input.map.is_empty());
        assert_eq!(p.sample_rate, None);
        assert_eq!(p.buffer_size, None);
        assert!(!p.has_input());
    }

    #[test]
    fn parses_a_full_profile() {
        let json = r#"{
            "output": { "device": "Scarlett", "map": {"0": 2, "1": 3} },
            "input": { "device": "Mic", "map": {"0": 0, "1": 1} },
            "sample_rate": 48000,
            "buffer_size": 256
        }"#;
        let p: DeviceProfile = serde_json::from_str(json).expect("parse full profile");
        assert_eq!(p.output.device.as_deref(), Some("Scarlett"));
        assert_eq!(p.output.map.get(&0), Some(&2));
        assert_eq!(p.output.map.get(&1), Some(&3));
        assert_eq!(p.input.device.as_deref(), Some("Mic"));
        assert_eq!(p.input.map.get(&0), Some(&0));
        assert_eq!(p.input.map.get(&1), Some(&1));
        assert_eq!(p.sample_rate, Some(48_000));
        assert_eq!(p.buffer_size, Some(256));
        assert!(p.has_input());
    }

    #[test]
    fn omitted_fields_default_to_identity() {
        let p: DeviceProfile = serde_json::from_str(r#"{"output": {"map": {"0": 1}}}"#)
            .expect("parse partial profile");
        assert_eq!(p.output.map.get(&0), Some(&1));
        assert_eq!(p.output.device, None);
        assert!(p.input.map.is_empty());
        assert_eq!(p.sample_rate, None);
        assert!(!p.has_input());
    }

    #[test]
    fn unknown_top_level_field_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>(r#"{"outputs": {}}"#);
        assert!(err.is_err(), "unknown field `outputs` should be rejected");
    }

    #[test]
    fn unknown_side_field_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>(r#"{"output": {"gain": 1}}"#);
        assert!(err.is_err(), "unknown field `gain` should be rejected");
    }

    #[test]
    fn non_numeric_map_key_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>(r#"{"output": {"map": {"left": 2}}}"#);
        assert!(err.is_err(), "non-numeric channel key should be rejected");
    }

    #[test]
    fn malformed_json_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>("{ not json");
        assert!(err.is_err());
    }

    #[test]
    fn load_missing_file_is_an_io_error() {
        let path = Path::new("/nonexistent/io-map.json");
        let err = DeviceProfile::load(path).expect_err("missing file should error");
        assert!(matches!(err, ProfileError::Io(_, _)));
        // Finding #7: the path should reach the user via Display, matching read_instrument's
        // "read <path>: <e>" convention elsewhere in this crate.
        assert!(err.to_string().contains(&path.display().to_string()));
    }

    #[test]
    fn load_malformed_file_is_a_json_error() {
        let dir = std::env::temp_dir().join(format!("reuben-profile-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.json");
        std::fs::write(&path, "{ not json").unwrap();
        let err = DeviceProfile::load(&path).expect_err("malformed json should error");
        assert!(matches!(err, ProfileError::Json(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_sample_rate_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>(r#"{"sample_rate": 0}"#)
            .map(|p| p.validate())
            .expect("valid JSON shape")
            .expect_err("sample_rate: 0 should be rejected");
        assert!(matches!(err, ProfileError::OutOfRange(_)));
    }

    #[test]
    fn zero_buffer_size_is_a_load_error() {
        let err = serde_json::from_str::<DeviceProfile>(r#"{"buffer_size": 0}"#)
            .map(|p| p.validate())
            .expect("valid JSON shape")
            .expect_err("buffer_size: 0 should be rejected");
        assert!(matches!(err, ProfileError::OutOfRange(_)));
    }

    #[test]
    fn load_rejects_zero_sample_rate_end_to_end() {
        let dir =
            std::env::temp_dir().join(format!("reuben-profile-test-zero-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zero.json");
        std::fs::write(&path, r#"{"sample_rate": 0}"#).unwrap();
        let err = DeviceProfile::load(&path).expect_err("sample_rate: 0 should be rejected");
        assert!(matches!(err, ProfileError::OutOfRange(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nonzero_sample_rate_and_buffer_size_are_accepted() {
        let p: DeviceProfile =
            serde_json::from_str(r#"{"sample_rate": 48000, "buffer_size": 256}"#)
                .expect("parse profile");
        assert!(p.validate().is_ok());
    }
}
