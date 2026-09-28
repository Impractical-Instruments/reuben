//! The command line `reuben play` starts the engine from, shared with every binary that starts it
//! the same way: the library root, the engine flags, and the OSC log switch. A binary
//! embedding the engine takes these rather than its own copies, so `reuben play` and it read one
//! set of flags and environment variables to one [`EngineConfig`].

use std::fmt;
use std::path::PathBuf;

use crate::engine::{EngineConfig, Instrument};
use crate::profile::{DeviceProfile, ProfileError};

/// The environment variable the library root falls back to when no `--instrument-root` names one.
pub const INSTRUMENT_ROOT_ENV: &str = "REUBEN_INSTRUMENT_ROOT";

/// Set (to anything) to log every OSC message received and sent to stdout.
pub const LOG_OSC_ENV: &str = "REUBEN_LOG_OSC";

/// The library root flag. `global`, so a binary with subcommands takes it before or after any.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct InstrumentRoot {
    /// Instrument library root: a sample or nested-patch reference that does not exist next
    /// to the file referencing it is looked up under this directory instead (sibling-first
    /// search). Falls back to the `REUBEN_INSTRUMENT_ROOT` env var.
    #[arg(long, global = true, value_name = "DIR")]
    pub instrument_root: Option<PathBuf>,
}

impl InstrumentRoot {
    /// The effective library root: the flag, else [`INSTRUMENT_ROOT_ENV`].
    pub fn resolve(self) -> Option<PathBuf> {
        self.instrument_root
            .or_else(|| std::env::var_os(INSTRUMENT_ROOT_ENV).map(PathBuf::from))
    }
}

/// Whether [`LOG_OSC_ENV`] is set.
pub fn log_osc() -> bool {
    std::env::var_os(LOG_OSC_ENV).is_some()
}

/// The engine flags beyond the instrument and the library root.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct EngineFlags {
    /// Send OSC out to this `host:port` (e.g. `127.0.0.1:9001`) — the static target an
    /// `osc_out` node's Messages are encoded and UDP-sent to. Omit to disable.
    #[arg(long, value_name = "HOST:PORT")]
    pub osc_out: Option<String>,
    /// Device profile JSON: logical↔device channel maps, device selection by
    /// name substring, and sample-rate/buffer-size preferences — outside the patch, so the
    /// same instrument plays on any rig. Omit for the default device and identity map,
    /// bit-identical to today's behavior. See docs/device-profile.md.
    #[arg(long, value_name = "FILE")]
    pub io_map: Option<PathBuf>,
}

impl EngineFlags {
    /// [`EngineConfig::new`] with these flags, `instrument_root` and [`log_osc`] applied. A
    /// malformed profile is an error: a structural problem in a document the user named, never
    /// silently replaced by the default devices.
    pub fn config(
        &self,
        instrument: Instrument,
        instrument_root: Option<PathBuf>,
    ) -> Result<EngineConfig, IoMapError> {
        let profile = match &self.io_map {
            Some(path) => DeviceProfile::load(path).map_err(|error| IoMapError {
                path: path.clone(),
                error,
            })?,
            None => DeviceProfile::default(),
        };
        Ok(EngineConfig {
            instrument_root,
            profile,
            osc_out: self.osc_out.clone(),
            log_osc: log_osc(),
            ..EngineConfig::new(instrument)
        })
    }
}

/// The `--io-map` profile would not load.
#[derive(Debug)]
pub struct IoMapError {
    pub path: PathBuf,
    pub error: ProfileError,
}

impl fmt::Display for IoMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "io-map {}: {}", self.path.display(), self.error)
    }
}

impl std::error::Error for IoMapError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
