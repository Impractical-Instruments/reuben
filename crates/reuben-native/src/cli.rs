//! The command line `reuben play` starts the engine from, shared with every binary that starts it
//! the same way: the library root, the engine flags, and the OSC log switch. A binary
//! embedding the engine takes these rather than its own copies, so `reuben play` and it read one
//! set of flags and environment variables to one [`EngineConfig`].

use std::fmt;
use std::net::SocketAddr;
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
    /// search). Falls back to the `REUBEN_INSTRUMENT_ROOT` env var; `reuben play` and the
    /// editor's start screen then look for `instruments/` in the working directory, then beside
    /// the binary.
    #[arg(long, global = true, value_name = "DIR")]
    pub instrument_root: Option<PathBuf>,
}

impl InstrumentRoot {
    /// The effective library root: the flag, else [`INSTRUMENT_ROOT_ENV`].
    pub fn resolve(self) -> Option<PathBuf> {
        self.instrument_root
            .or_else(|| std::env::var_os(INSTRUMENT_ROOT_ENV).map(PathBuf::from))
    }

    /// [`resolve`](Self::resolve), else the [`default_library`].
    pub fn resolve_or_default(self) -> Option<PathBuf> {
        self.resolve().or_else(default_library)
    }
}

/// Where a library is looked for when no flag or environment variable names one, in order:
/// `instruments/` in the working directory, then beside the executable. A release archive carries
/// the library next to the binary, and a launch from a file manager (a double-click in Finder
/// starts it in the home directory) has no say over the working directory.
pub fn library_candidates() -> Vec<PathBuf> {
    let beside_exe = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("instruments")));
    std::iter::once(PathBuf::from("instruments"))
        .chain(beside_exe)
        .collect()
}

/// The first of the [`library_candidates`] that is a directory, made absolute.
pub fn default_library() -> Option<PathBuf> {
    library_candidates()
        .into_iter()
        .find(|dir| dir.is_dir())
        .map(|dir| std::path::absolute(&dir).unwrap_or(dir))
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
    /// name (whole, or any part of it), and sample-rate/buffer-size preferences — outside the patch, so the
    /// same instrument plays on any rig. Omit for the default device and identity map,
    /// bit-identical to today's behavior. See docs/device-profile.md.
    #[arg(long, value_name = "FILE")]
    pub io_map: Option<PathBuf>,
    /// Open no audio device: a thread renders blocks at 48 kHz and discards the output, so the
    /// engine serves its structure channel and OSC on a machine with no sound card.
    #[arg(long, conflicts_with = "io_map")]
    pub headless: bool,
    /// Serve the structure channel on this loopback `ip:port` instead of the default. A literal
    /// address, not a host name: anything that is not a loopback IP is refused.
    #[arg(long, value_name = "HOST:PORT")]
    pub structure_addr: Option<String>,
    /// Listen for OSC on this `host:port` instead of every interface at the default port.
    #[arg(long, value_name = "HOST:PORT")]
    pub osc_in: Option<String>,
}

impl EngineFlags {
    /// [`EngineConfig::new`] with these flags, `instrument_root` and [`log_osc`] applied. A
    /// malformed profile is an error: a structural problem in a document the user named, never
    /// silently replaced by the default devices. So is a `--structure-addr` that is not a literal
    /// loopback IP. `--headless` is not part of the config; the caller picks the entry point.
    pub fn config(
        &self,
        instrument: Instrument,
        instrument_root: Option<PathBuf>,
    ) -> Result<EngineConfig, FlagsError> {
        if let Some(addr) = &self.structure_addr {
            // Parsed, never resolved: a host name could resolve to a routable interface.
            let loopback = addr
                .parse::<SocketAddr>()
                .is_ok_and(|parsed| parsed.ip().is_loopback());
            if !loopback {
                return Err(FlagsError::StructureNotLoopback(addr.clone()));
            }
        }
        let profile = match &self.io_map {
            Some(path) => DeviceProfile::load(path).map_err(|error| IoMapError {
                path: path.clone(),
                error,
            })?,
            None => DeviceProfile::default(),
        };
        let defaults = EngineConfig::new(instrument);
        Ok(EngineConfig {
            instrument_root,
            profile,
            osc_out: self.osc_out.clone(),
            osc_in: self.osc_in.clone().or_else(|| defaults.osc_in.clone()),
            structure: self
                .structure_addr
                .clone()
                .or_else(|| defaults.structure.clone()),
            log_osc: log_osc(),
            ..defaults
        })
    }
}

/// The engine flags would not make an [`EngineConfig`].
#[derive(Debug)]
pub enum FlagsError {
    /// The `--io-map` profile would not load.
    IoMap(IoMapError),
    /// `--structure-addr` is not a literal loopback IP and port.
    StructureNotLoopback(String),
}

impl From<IoMapError> for FlagsError {
    fn from(error: IoMapError) -> Self {
        FlagsError::IoMap(error)
    }
}

impl fmt::Display for FlagsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlagsError::IoMap(e) => write!(f, "{e}"),
            FlagsError::StructureNotLoopback(addr) => write!(
                f,
                "--structure-addr must be a loopback address until token authentication exists: \
                 {addr}"
            ),
        }
    }
}

impl std::error::Error for FlagsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FlagsError::IoMap(e) => Some(e),
            FlagsError::StructureNotLoopback(_) => None,
        }
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Probe {
        #[command(flatten)]
        flags: EngineFlags,
    }

    fn parse(args: &[&str]) -> EngineFlags {
        Probe::try_parse_from(std::iter::once("probe").chain(args.iter().copied()))
            .expect("the flags parse")
            .flags
    }

    #[test]
    fn structure_addr_flag_reaches_the_config() {
        let flags = parse(&["--structure-addr", "127.0.0.1:9124"]);
        let config = flags.config(Instrument::Default, None).expect("a config");
        assert_eq!(config.structure.as_deref(), Some("127.0.0.1:9124"));
    }

    #[test]
    fn a_non_loopback_structure_addr_is_refused() {
        for addr in ["0.0.0.0:9124", "192.168.1.2:9124", "localhost:9124"] {
            let flags = parse(&["--structure-addr", addr]);
            let error = flags
                .config(Instrument::Default, None)
                .expect_err("a non-loopback structure address is refused");
            assert_eq!(
                error.to_string(),
                format!(
                    "--structure-addr must be a loopback address until token authentication \
                     exists: {addr}"
                ),
            );
        }
    }

    #[test]
    fn headless_and_osc_in_flags_parse() {
        let flags = parse(&["--headless", "--osc-in", "127.0.0.1:9000"]);
        assert!(flags.headless);
        let config = flags.config(Instrument::Default, None).expect("a config");
        assert_eq!(config.osc_in.as_deref(), Some("127.0.0.1:9000"));

        let conflict = Probe::try_parse_from(["probe", "--headless", "--io-map", "rig.json"])
            .err()
            .expect("--headless with --io-map is refused");
        assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);

        let defaults = parse(&[]);
        assert!(!defaults.headless);
        let config = defaults
            .config(Instrument::Default, None)
            .expect("a config");
        assert_eq!(config.osc_in, EngineConfig::new(Instrument::Default).osc_in);
        assert_eq!(
            config.structure,
            EngineConfig::new(Instrument::Default).structure
        );
    }
}
