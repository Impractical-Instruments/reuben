//! The command line `reuben play` starts the engine from, shared with every binary that starts it
//! the same way: the library root, the engine flags, and the OSC log switch. A binary
//! embedding the engine takes these rather than its own copies, so `reuben play` and it read one
//! set of flags and environment variables to one [`EngineConfig`].

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use reuben_api::engine::{EmptyToken, Token, STRUCTURE_ADDR_ENV, STRUCTURE_TOKEN_ENV};

use crate::client::Target;
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
    /// Serve the structure channel on this `ip:port` instead of the default `127.0.0.1:9124`. A
    /// literal IP, not a host name. Anything beyond loopback (`0.0.0.0`, a LAN or Tailscale
    /// address) takes a token, and clients on other machines must send it.
    #[arg(long, value_name = "IP:PORT")]
    pub structure_addr: Option<String>,
    /// The structure channel's token, which a client on another machine must send. Else
    /// `REUBEN_STRUCTURE_TOKEN`, which keeps it out of the process list. It travels in clear:
    /// use a network that encrypts (Tailscale) or one you trust.
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
    /// Listen for OSC on this `host:port` instead of every interface at the default port.
    #[arg(long, value_name = "HOST:PORT")]
    pub osc_in: Option<String>,
}

impl EngineFlags {
    /// [`EngineConfig::new`] with these flags, `instrument_root`, [`log_osc`] and the token from
    /// [`STRUCTURE_TOKEN_ENV`] applied. A malformed profile is an error: a structural problem in a
    /// document the user named, never silently replaced by the default devices. So is a
    /// `--structure-addr` that is not a literal IP, or one beyond loopback with no token.
    /// `--headless` is not part of the config; the caller picks the entry point.
    pub fn config(
        &self,
        instrument: Instrument,
        instrument_root: Option<PathBuf>,
    ) -> Result<EngineConfig, FlagsError> {
        self.config_with_env(
            instrument,
            instrument_root,
            std::env::var(STRUCTURE_TOKEN_ENV).ok(),
        )
    }

    /// The structure channel token: `--token`, else [`STRUCTURE_TOKEN_ENV`]. For a binary that
    /// attaches to a server rather than starting one.
    pub fn token(&self) -> Result<Option<Token>, FlagsError> {
        token(self.token.clone(), std::env::var(STRUCTURE_TOKEN_ENV).ok())
    }

    /// [`config`](Self::config) with the token's environment variable passed in.
    pub fn config_with_env(
        &self,
        instrument: Instrument,
        instrument_root: Option<PathBuf>,
        token_env: Option<String>,
    ) -> Result<EngineConfig, FlagsError> {
        let token = token(self.token.clone(), token_env)?;
        if let Some(addr) = &self.structure_addr {
            let Ok(parsed) = addr.parse::<SocketAddr>() else {
                return Err(FlagsError::StructureNotAnIp(addr.clone()));
            };
            if token.is_none() && !parsed.ip().to_canonical().is_loopback() {
                return Err(FlagsError::TokenRequired(addr.clone()));
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
            token,
            log_osc: log_osc(),
            ..defaults
        })
    }
}

/// The `--token` flag, else the environment variable's value; refused when empty.
fn token(flag: Option<String>, env: Option<String>) -> Result<Option<Token>, FlagsError> {
    flag.or(env)
        .map(Token::new)
        .transpose()
        .map_err(|_| FlagsError::EmptyToken)
}

/// Which server a client subcommand asks.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct ServerFlags {
    /// Ask the server at this `host:port` (else `REUBEN_STRUCTURE_ADDR`, else the running
    /// `reuben play` at the default address, or a temporary one when none runs there). A server on
    /// another machine takes a token.
    #[arg(long, value_name = "HOST:PORT")]
    pub structure_addr: Option<String>,
    /// The token a server on another machine asks for. Else `REUBEN_STRUCTURE_TOKEN`, which keeps
    /// it out of the process list.
    #[arg(long, value_name = "TOKEN")]
    pub token: Option<String>,
    /// Start a temporary server in this process rather than ask a running `reuben play`.
    #[arg(long, conflicts_with = "structure_addr")]
    pub temporary_server: bool,
}

impl ServerFlags {
    /// The flag, then [`STRUCTURE_ADDR_ENV`], then the default; with the token from `--token`, then
    /// [`STRUCTURE_TOKEN_ENV`].
    pub fn target(&self) -> Result<Target, FlagsError> {
        self.target_with_env(
            std::env::var(STRUCTURE_ADDR_ENV).ok(),
            std::env::var(STRUCTURE_TOKEN_ENV).ok(),
        )
    }

    /// [`target`](Self::target) with the environment variables' values passed in.
    pub fn target_with_env(
        &self,
        addr_env: Option<String>,
        token_env: Option<String>,
    ) -> Result<Target, FlagsError> {
        if self.temporary_server {
            return Ok(Target::Temporary);
        }
        let token = token(self.token.clone(), token_env)?;
        Ok(match self.structure_addr.clone().or(addr_env) {
            None => Target::Default,
            Some(addr) => Target::Named { addr, token },
        })
    }
}

/// The engine flags would not make an [`EngineConfig`].
#[derive(Debug)]
pub enum FlagsError {
    /// The `--io-map` profile would not load.
    IoMap(IoMapError),
    /// `--structure-addr` is not a literal IP and port.
    StructureNotAnIp(String),
    /// `--structure-addr` is beyond loopback and no token was given.
    TokenRequired(String),
    /// The token given is empty.
    EmptyToken,
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
            FlagsError::StructureNotAnIp(addr) => {
                write!(f, "--structure-addr must be a literal IP and port: {addr}")
            }
            FlagsError::TokenRequired(addr) => write!(
                f,
                "--structure-addr {addr} is not loopback, so the structure channel needs a token: \
                 pass --token or set {STRUCTURE_TOKEN_ENV}"
            ),
            FlagsError::EmptyToken => write!(f, "--token: {EmptyToken}"),
        }
    }
}

impl std::error::Error for FlagsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FlagsError::IoMap(e) => Some(e),
            FlagsError::StructureNotAnIp(_)
            | FlagsError::TokenRequired(_)
            | FlagsError::EmptyToken => None,
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

    fn token(t: &str) -> Token {
        Token::new(t).expect("a token")
    }

    #[test]
    fn a_non_loopback_structure_addr_needs_a_token() {
        for addr in ["0.0.0.0:9124", "192.168.1.2:9124", "[::]:9124"] {
            let flags = parse(&["--structure-addr", addr]);
            let error = flags
                .config_with_env(Instrument::Default, None, None)
                .expect_err("a non-loopback structure address needs a token");
            assert_eq!(
                error.to_string(),
                format!(
                    "--structure-addr {addr} is not loopback, so the structure channel needs a \
                     token: pass --token or set REUBEN_STRUCTURE_TOKEN"
                ),
            );

            let flagged = parse(&["--structure-addr", addr, "--token", "t"]);
            let config = flagged
                .config_with_env(Instrument::Default, None, None)
                .expect("a token makes it a config");
            assert_eq!(config.structure.as_deref(), Some(addr));
            assert_eq!(config.token, Some(token("t")));

            let from_env = flags
                .config_with_env(Instrument::Default, None, Some("e".to_string()))
                .expect("the env var is a token too");
            assert_eq!(from_env.token, Some(token("e")));
        }
    }

    #[test]
    fn the_token_flag_wins_over_the_env() {
        let flags = parse(&["--token", "flag"]);
        let config = flags
            .config_with_env(Instrument::Default, None, Some("env".to_string()))
            .expect("a config");
        assert_eq!(config.token, Some(token("flag")));
    }

    #[test]
    fn a_structure_addr_must_be_an_ip() {
        let flags = parse(&["--structure-addr", "localhost:9124", "--token", "t"]);
        let error = flags
            .config_with_env(Instrument::Default, None, None)
            .expect_err("a host name is refused");
        assert_eq!(
            error.to_string(),
            "--structure-addr must be a literal IP and port: localhost:9124"
        );
    }

    #[test]
    fn an_empty_token_is_refused() {
        let flags = parse(&["--token", ""]);
        let error = flags
            .config_with_env(Instrument::Default, None, None)
            .expect_err("an empty token is refused");
        assert_eq!(
            error.to_string(),
            "--token: a structure channel token must not be empty"
        );
        let from_env = parse(&[]).config_with_env(Instrument::Default, None, Some(" ".into()));
        assert!(matches!(from_env, Err(FlagsError::EmptyToken)));
    }

    #[derive(Parser)]
    struct ClientProbe {
        #[command(flatten)]
        server: ServerFlags,
    }

    fn server(args: &[&str]) -> Result<ServerFlags, clap::Error> {
        ClientProbe::try_parse_from(std::iter::once("probe").chain(args.iter().copied()))
            .map(|probe| probe.server)
    }

    fn named(addr: &str, token: Option<Token>) -> Target {
        Target::Named {
            addr: addr.to_string(),
            token,
        }
    }

    #[test]
    fn server_flags_pick_the_flag_then_the_env_then_the_default() {
        let flags = server(&["--structure-addr", "127.0.0.1:7000"]).expect("parse");
        assert_eq!(
            flags
                .target_with_env(Some("127.0.0.1:8000".to_string()), None)
                .expect("a target"),
            named("127.0.0.1:7000", None)
        );
        let flags = server(&[]).expect("parse");
        assert_eq!(
            flags
                .target_with_env(Some("127.0.0.1:8000".to_string()), None)
                .expect("a target"),
            named("127.0.0.1:8000", None)
        );
        assert_eq!(
            flags.target_with_env(None, None).expect("a target"),
            Target::Default
        );
        let flags = server(&["--temporary-server"]).expect("parse");
        assert_eq!(
            flags
                .target_with_env(Some("127.0.0.1:8000".to_string()), None)
                .expect("a target"),
            Target::Temporary
        );
    }

    #[test]
    fn server_flags_accept_any_address_with_a_token() {
        for addr in ["192.168.1.2:9124", "studio.tail:9124", "localhost:9124"] {
            let flags = server(&["--structure-addr", addr, "--token", "t"]).expect("parse");
            assert_eq!(
                flags.target_with_env(None, None).expect("a target"),
                named(addr, Some(token("t")))
            );
            let from_env = server(&[])
                .expect("parse")
                .target_with_env(Some(addr.to_string()), Some("e".to_string()))
                .expect("a target");
            assert_eq!(from_env, named(addr, Some(token("e"))));
        }
    }

    #[test]
    fn server_flags_refuse_an_empty_token() {
        let flags =
            server(&["--structure-addr", "192.168.1.2:9124", "--token", " "]).expect("parse");
        assert!(matches!(
            flags.target_with_env(None, None),
            Err(FlagsError::EmptyToken)
        ));
    }

    #[test]
    fn temporary_server_conflicts_with_structure_addr() {
        let conflict = server(&["--temporary-server", "--structure-addr", "127.0.0.1:7000"])
            .expect_err("refused");
        assert_eq!(conflict.kind(), clap::error::ErrorKind::ArgumentConflict);
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
