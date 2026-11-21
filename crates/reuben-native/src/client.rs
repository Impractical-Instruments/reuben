//! Which server a one-shot CLI command asks: the running `reuben play`, or a temporary headless one
//! started in this process when none runs.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use std::net::{SocketAddr, TcpStream};

use reuben_api::engine::{
    Channel, ChannelError, TcpTransport, Token, DEFAULT_CONNECT_TIMEOUT, DEFAULT_STRUCTURE_ADDR,
    ENGINE_UNREACHABLE_GUIDANCE,
};

use crate::engine::{start_headless, EngineConfig, Instrument, RunningEngine, StartError};
use crate::structure::lexical_absolute;

/// A validate decodes every sample, and on a large library instrument that takes seconds.
pub const CLI_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Where a CLI command looks for its server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The default structure address; a temporary server when nothing listens there.
    Default,
    /// An address the user named, and the token to open each connection with. Nothing answering
    /// there is an error, never a fallback.
    Named { addr: String, token: Option<Token> },
    /// A temporary server, without dialing anything.
    Temporary,
}

/// What a temporary server offers its client.
#[derive(Debug, Clone, Default)]
pub struct Temporary {
    pub instrument_root: Option<PathBuf>,
    /// Directories offered beside the working directory.
    pub offer: Vec<PathBuf>,
}

/// A server a CLI command can ask.
pub struct Reached {
    channel: Channel,
    running_at: Option<String>,
    // Held to keep the temporary server running; declared after `channel` so it drops last.
    #[cfg_attr(not(test), allow(dead_code))]
    temporary: Option<RunningEngine>,
}

impl Reached {
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// The address of the running server that answered; `None` for a temporary one.
    pub fn running_at(&self) -> Option<&str> {
        self.running_at.as_deref()
    }

    /// Whether the server may be on another machine, where this one's paths mean nothing.
    pub fn remote(&self) -> bool {
        self.running_at.as_deref().is_some_and(is_remote)
    }
}

/// Whether `addr` may name another machine: anything but a literal loopback IP and port. A name is
/// never assumed local, since it may resolve anywhere.
pub fn is_remote(addr: &str) -> bool {
    !addr
        .parse::<SocketAddr>()
        .is_ok_and(|parsed| parsed.ip().to_canonical().is_loopback())
}

/// Why no server could be asked.
#[derive(Debug)]
pub enum ReachError {
    Unreachable {
        addr: String,
    },
    NotAServer {
        addr: String,
        why: String,
    },
    /// A server answered and refused this client, as one does a missing or wrong token.
    Refused {
        addr: String,
        message: String,
    },
    Start(StartError),
}

impl fmt::Display for ReachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReachError::Unreachable { addr } => {
                write!(
                    f,
                    "{ENGINE_UNREACHABLE_GUIDANCE} (nothing answered at {addr})"
                )
            }
            ReachError::NotAServer { addr, why } => write!(
                f,
                "something at {addr} did not answer as a reuben server ({why}); pass \
                 --temporary-server to start a fresh one"
            ),
            ReachError::Refused { addr, message } => {
                write!(f, "the reuben server at {addr} refused: {message}")
            }
            ReachError::Start(e) => write!(f, "could not start a temporary server: {e}"),
        }
    }
}

impl std::error::Error for ReachError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ReachError::Start(e) => Some(e),
            _ => None,
        }
    }
}

/// Reach the server `target` names.
pub fn reach(target: Target, temporary: Temporary) -> Result<Reached, ReachError> {
    reach_with(target, temporary, DEFAULT_STRUCTURE_ADDR, CLI_READ_TIMEOUT)
}

pub(crate) fn reach_with(
    target: Target,
    temporary: Temporary,
    default_addr: &str,
    read_timeout: Duration,
) -> Result<Reached, ReachError> {
    match target {
        Target::Temporary => start_temporary(temporary),
        Target::Default => {
            // `Channel::ping` reports a refused connect and a listener that never answers alike,
            // and only the first may fall back: something holding the port is not ours to bypass.
            let refused = default_addr
                .parse::<SocketAddr>()
                .map_err(|e| ReachError::NotAServer {
                    addr: default_addr.to_string(),
                    why: e.to_string(),
                })
                .map(|parsed| TcpStream::connect_timeout(&parsed, DEFAULT_CONNECT_TIMEOUT))?;
            match refused {
                Err(e) if nobody_listening(e.kind()) => start_temporary(temporary),
                Err(e) => Err(ReachError::NotAServer {
                    addr: default_addr.to_string(),
                    why: e.to_string(),
                }),
                Ok(_) => running(default_addr, None, read_timeout, |why| {
                    ReachError::NotAServer {
                        addr: default_addr.to_string(),
                        why,
                    }
                }),
            }
        }
        Target::Named { addr, token } => {
            running(&addr, token, read_timeout, |why| ReachError::Unreachable {
                addr: format!("{addr}: {why}"),
            })
        }
    }
}

/// Whether a failed connect to the loopback default address means nothing listens there. Windows
/// retries a SYN the closed port resets and reports the refusal only after the connect budget, so a
/// timeout counts too; a hung server still completes the handshake and is never mistaken for none.
fn nobody_listening(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
    )
}

/// The server at `addr`, if it answers a ping; `unanswered` says why not otherwise.
fn running(
    addr: &str,
    token: Option<Token>,
    read_timeout: Duration,
    unanswered: impl FnOnce(String) -> ReachError,
) -> Result<Reached, ReachError> {
    let channel = Channel::with_read_timeout(
        TcpTransport::new(addr, DEFAULT_CONNECT_TIMEOUT).with_token(token),
        read_timeout,
    );
    match channel.ping() {
        Ok(()) => Ok(Reached {
            channel,
            running_at: Some(addr.to_string()),
            temporary: None,
        }),
        Err(e) if e.is_unreachable() => Err(unanswered("no answer to a ping".to_string())),
        Err(ChannelError::Channel(message)) => Err(ReachError::Refused {
            addr: addr.to_string(),
            message,
        }),
        Err(e) => Err(ReachError::NotAServer {
            addr: addr.to_string(),
            why: e.to_string(),
        }),
    }
}

/// A headless server in this process with nothing open and no socket bound, offering the working
/// directory and `temporary.offer`.
fn start_temporary(temporary: Temporary) -> Result<Reached, ReachError> {
    let engine = start_headless(EngineConfig {
        instrument_root: temporary.instrument_root,
        osc_in: None,
        osc_out: None,
        structure: None,
        follow_files: false,
        ..EngineConfig::new(Instrument::Silent)
    })
    .map_err(ReachError::Start)?;
    if let Ok(cwd) = std::env::current_dir() {
        engine.offer_dir(&cwd);
    }
    for dir in &temporary.offer {
        engine.offer_dir(dir);
    }
    Ok(Reached {
        channel: engine.channel(),
        running_at: None,
        temporary: Some(engine),
    })
}

/// The path arguments a command sent, each with how the user typed it.
#[derive(Debug, Default)]
pub struct Names {
    typed: Vec<(String, String)>,
    /// The server may be on another machine: a name goes as typed, for it to resolve.
    remote: bool,
}

impl Names {
    /// The names a command sends to a server that is [`remote`](Reached::remote) or not.
    pub fn for_target(remote: bool) -> Self {
        Self {
            typed: Vec::new(),
            remote,
        }
    }

    /// `arg` absolute and lexically normalized against this process's working directory, so a
    /// server started elsewhere on this machine reads the file the user meant. Unchanged for a
    /// remote server, which resolves the name against what it offers.
    pub fn sent(&mut self, arg: &str) -> String {
        if self.remote {
            return arg.to_string();
        }
        let sent = lexical_absolute(std::path::Path::new(arg))
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| arg.to_string());
        if sent != arg {
            self.typed.push((sent.clone(), arg.to_string()));
        }
        sent
    }

    /// `text` with each sent name put back as the user typed it.
    pub fn in_user_terms(&self, text: &str) -> String {
        let mut by_length: Vec<&(String, String)> = self.typed.iter().collect();
        // Longest first, so a name that begins another is not replaced inside it.
        by_length.sort_by_key(|(sent, _)| std::cmp::Reverse(sent.len()));
        let mut text = text.to_string();
        for (sent, typed) in by_length {
            text = text.replace(sent.as_str(), typed);
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::path::Path;

    use reuben_api::engine::{call_tool, DocumentSnapshot, ENGINE_UNREACHABLE_GUIDANCE};
    use reuben_api::tools::names;
    use serde_json::json;

    use super::*;
    use crate::engine::{start_headless, EngineConfig, Instrument};

    const DOC: &str = r#"{"format_version":3,"instrument":"client",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// A fresh directory under the temp dir, removed when dropped.
    struct Dir(PathBuf);

    impl Dir {
        fn new(case: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("reuben_client_{case}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create the test dir");
            Dir(dir)
        }

        fn doc(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::File::create(&path)
                .and_then(|mut f| f.write_all(DOC.as_bytes()))
                .expect("write the doc");
            path
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An address nothing listens on.
    fn dead_addr() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").to_string()
    }

    /// A running headless server on a free loopback port, nothing open.
    fn running() -> (RunningEngine, String) {
        let engine = start_headless(EngineConfig {
            structure: Some("127.0.0.1:0".to_string()),
            osc_in: None,
            follow_files: false,
            ..EngineConfig::new(Instrument::Silent)
        })
        .expect("a headless server starts");
        let addr = engine.structure_addr().expect("bound").to_string();
        (engine, addr)
    }

    fn validate(reached: &Reached, source: &Path) -> Result<serde_json::Value, String> {
        call_tool(
            names::VALIDATE_INSTRUMENT,
            &json!({"source": source.display().to_string()}),
            reached.channel(),
        )
        .map(|answer| answer.output)
        .map_err(|refusal| refusal.message)
    }

    #[test]
    fn default_unreachable_starts_the_temporary_server() {
        let reached = reach_with(
            Target::Default,
            Temporary::default(),
            &dead_addr(),
            CLI_READ_TIMEOUT,
        )
        .expect("a temporary server");
        assert_eq!(reached.running_at(), None);
        reached
            .channel()
            .ping()
            .expect("the temporary server answers");
    }

    #[test]
    fn the_temporary_server_binds_nothing_and_offers_cwd_and_the_given_dirs() {
        let offered = Dir::new("offered");
        let unoffered = Dir::new("unoffered");
        let inside = offered.doc("a.json");
        let outside = unoffered.doc("b.json");
        let reached = reach_with(
            Target::Temporary,
            Temporary {
                instrument_root: None,
                offer: vec![offered.0.clone()],
            },
            &dead_addr(),
            CLI_READ_TIMEOUT,
        )
        .expect("a temporary server");
        let engine = reached.temporary.as_ref().expect("temporary");
        assert_eq!(engine.structure_addr(), None);
        assert_eq!(engine.osc_in_addr(), None);

        let report = validate(&reached, &inside).expect("an offered doc validates");
        assert_eq!(report["ok"], true, "{report}");
        let refusal = validate(&reached, &outside).expect_err("an unoffered doc is refused");
        assert!(
            refusal.contains("is not offered by this server"),
            "{refusal}"
        );

        let cwd_doc = std::env::current_dir()
            .expect("cwd")
            .join("Cargo.toml")
            .display()
            .to_string();
        let refusal = call_tool(
            names::VALIDATE_INSTRUMENT,
            &json!({ "source": cwd_doc }),
            reached.channel(),
        )
        .err()
        .map(|r| r.message)
        .unwrap_or_default();
        assert!(
            !refusal.contains("is not offered"),
            "the working directory is offered: {refusal}"
        );
    }

    #[test]
    fn names_for_a_remote_server_go_as_typed() {
        let mut remote = Names::for_target(true);
        assert_eq!(remote.sent("./a.json"), "./a.json");
        assert_eq!(remote.in_user_terms("`./a.json`"), "`./a.json`");
        let mut local = Names::for_target(false);
        assert!(Path::new(&local.sent("./a.json")).is_absolute());
    }

    #[test]
    fn only_a_loopback_server_shares_this_machines_paths() {
        assert!(!is_remote("127.0.0.1:9124"));
        assert!(!is_remote("[::1]:9124"));
        assert!(is_remote("192.168.1.2:9124"));
        assert!(is_remote("studio.tail:9124"));
        assert!(is_remote("localhost:9124"), "a name is never assumed local");
    }

    #[test]
    fn a_named_unreachable_address_is_an_error_not_a_fallback() {
        let addr = dead_addr();
        let err = reach_with(
            Target::Named {
                addr: addr.clone(),
                token: None,
            },
            Temporary::default(),
            &dead_addr(),
            CLI_READ_TIMEOUT,
        )
        .err()
        .expect("no fallback");
        assert!(matches!(err, ReachError::Unreachable { .. }), "{err:?}");
        let shown = err.to_string();
        assert!(shown.contains(ENGINE_UNREACHABLE_GUIDANCE), "{shown}");
        assert!(shown.contains(&addr), "{shown}");
    }

    #[test]
    fn default_reachable_uses_the_running_server() {
        let (_engine, addr) = running();
        let reached = reach_with(
            Target::Default,
            Temporary::default(),
            &addr,
            CLI_READ_TIMEOUT,
        )
        .expect("the running server");
        assert_eq!(reached.running_at(), Some(addr.as_str()));
        assert!(reached.temporary.is_none());
        call_tool(names::DESCRIBE_OPERATORS, &json!({}), reached.channel())
            .expect("the operator set");
        let _: DocumentSnapshot = reached.channel().get_document().expect("a snapshot");
    }

    #[test]
    fn temporary_never_dials() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let reached = reach_with(
            Target::Temporary,
            Temporary::default(),
            &addr,
            CLI_READ_TIMEOUT,
        )
        .expect("a temporary server");
        assert_eq!(reached.running_at(), None);
        listener.set_nonblocking(true).expect("nonblocking");
        let accepted = listener.accept();
        assert!(
            matches!(&accepted, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "something dialed the default address: {accepted:?}"
        );
    }

    #[test]
    fn a_silent_listener_at_the_default_is_an_error_not_a_fallback() {
        // Bound and listening, so the kernel completes the connect, but nothing ever answers.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let err = reach_with(
            Target::Default,
            Temporary::default(),
            &addr,
            Duration::from_millis(200),
        )
        .err()
        .expect("no fallback");
        assert!(matches!(err, ReachError::NotAServer { .. }), "{err:?}");
        let shown = err.to_string();
        assert!(shown.contains(&addr), "{shown}");
        assert!(shown.contains("--temporary-server"), "{shown}");
    }

    /// A loopback port with nothing on it is refused, or on Windows, which retries the SYN, times
    /// out first. A hung server completes the handshake, so neither is a server not answering.
    #[test]
    fn a_refused_or_timed_out_loopback_connect_means_nobody_listens() {
        use std::io::ErrorKind;
        assert!(nobody_listening(ErrorKind::ConnectionRefused));
        assert!(nobody_listening(ErrorKind::TimedOut));
        assert!(!nobody_listening(ErrorKind::PermissionDenied));
        assert!(!nobody_listening(ErrorKind::AddrNotAvailable));
    }

    #[test]
    fn names_map_back_to_what_was_typed() {
        let mut names = Names::default();
        let short = names.sent("x.json");
        let long = names.sent("x.json.bak/y.json");
        assert!(Path::new(&short).is_absolute(), "{short}");
        assert!(Path::new(&long).is_absolute(), "{long}");
        assert_eq!(
            names.in_user_terms(&format!("`{short}` and `{long}` failed")),
            "`x.json` and `x.json.bak/y.json` failed"
        );
    }
}
