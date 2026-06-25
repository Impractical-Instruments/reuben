//! `reuben` — the command-line entry point.
//!
//! Three subcommands:
//! - `reuben play [path]` — render an instrument live, driven by OSC over UDP. With no path
//!   it plays the built-in default rig. Send notes with:
//!
//!   ```text
//!   /voicer/note  [69.0, 1.0]   # note-on  (MIDI 69 = A4, gate 1)
//!   /voicer/note  [69.0, 0.0]   # note-off (gate 0)
//!   ```
//!
//! - `reuben describe [op] [--json]` — print the operator set (or one operator's ports,
//!   params, and resource slots). The introspection half of the Patcher skill (ADR-0020).
//! - `reuben validate <path> [--json]` — load + plan an instrument with no audio device and
//!   report structural/wiring errors. Exit 1 if invalid; warnings alone stay exit 0.
//! - `reuben scaffold-operator --spec <path> [--json]` — generate a new Operator's Rust skeleton
//!   from a contract spec and wire its registration. The codegen half of the create-operator
//!   skill (ADR-0021).

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

use clap::{Parser, Subcommand};

use reuben_core::boundary;
use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::{load_instrument, Registry};
use reuben_native::cli::{describe, validate};
use reuben_native::engine::Engine;
use reuben_native::resources::FsResolver;
use reuben_native::rigs::DEFAULT_JSON;
use reuben_native::{audio, osc, scaffold};

const BLOCK_SIZE: usize = 256;
const OSC_BIND: &str = "0.0.0.0:9000";

#[derive(Parser)]
#[command(name = "reuben", about = "Play and author reuben instruments.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render an instrument live, driven by OSC over UDP (default: the built-in rig).
    Play {
        /// Instrument JSON to play; omit for the built-in default rig.
        path: Option<PathBuf>,
        /// Send OSC out to this `host:port` (e.g. `127.0.0.1:9001`) — the static target an
        /// `osc_out` node's Messages are encoded and UDP-sent to (ADR-0026). Omit to disable.
        #[arg(long, value_name = "HOST:PORT")]
        osc_out: Option<String>,
    },
    /// Print the operator set, or one operator's ports/params/resources.
    Describe {
        /// Operator type to describe; omit to list them all.
        op: Option<String>,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
    },
    /// Load + plan an instrument (no audio) and report errors/warnings.
    Validate {
        /// Instrument JSON to validate.
        path: PathBuf,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
    },
    /// Generate a new Operator's Rust skeleton from a contract spec and wire its registration.
    ScaffoldOperator {
        /// Contract JSON (type_name, inputs, outputs, params, resources, lanes).
        #[arg(long)]
        spec: PathBuf,
        /// reuben-core source root holding `operators/` and `registry.rs`.
        #[arg(long, default_value = "crates/reuben-core/src")]
        core_root: PathBuf,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Play { path, osc_out } => {
            play(path, osc_out);
            ExitCode::SUCCESS
        }
        Command::Describe { op, json } => cmd_describe(op.as_deref(), json),
        Command::Validate { path, json } => cmd_validate(&path, json),
        Command::ScaffoldOperator {
            spec,
            core_root,
            json,
        } => cmd_scaffold(&spec, &core_root, json),
    }
}

/// `scaffold-operator`: generate a new operator skeleton + registration from a contract spec.
fn cmd_scaffold(spec: &Path, core_root: &Path, json: bool) -> ExitCode {
    let report = match scaffold::run_scaffold(spec, core_root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize scaffold report")
        );
    } else {
        println!("created {} ({})", report.created, report.struct_name);
        for f in &report.edited {
            println!("  edited {f}");
        }
        if !report.formatted {
            eprintln!("warning: `cargo fmt` did not run; format the touched files manually");
        }
        println!(
            "next: implement `{}` test-first — the scaffolded test starts red (ADR-0021).",
            report.type_name
        );
    }
    ExitCode::SUCCESS
}

/// `describe`: dump the operator set (or one operator) as human text or JSON.
fn cmd_describe(op: Option<&str>, json: bool) -> ExitCode {
    let ops = match describe(&Registry::builtin(), op) {
        Ok(ops) => ops,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ops).expect("serialize describe")
        );
        return ExitCode::SUCCESS;
    }

    for o in &ops {
        println!("{}", o.type_name);
        let ports = |dir: &str, ps: &[reuben_native::cli::PortInfo]| {
            for p in ps {
                println!("  {dir} {} : {}", p.name, p.kind);
            }
        };
        ports("in ", &o.inputs);
        ports("out", &o.outputs);
        for p in &o.params {
            let unit = if p.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", p.unit)
            };
            println!(
                "  param {} = {}{} [{}..{}] ({})",
                p.name, p.default, unit, p.min, p.max, p.curve
            );
        }
        for r in &o.resources {
            println!("  resource {r}");
        }
    }
    ExitCode::SUCCESS
}

/// `validate`: report whether an instrument loads + plans cleanly. Exit 1 only on hard errors;
/// warnings (e.g. an unresolved sample) are advisory and keep exit 0.
fn cmd_validate(path: &Path, json: bool) -> ExitCode {
    let instrument_json = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: read {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let base_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let report = validate(
        &instrument_json,
        &Registry::builtin(),
        &FsResolver::new(base_dir),
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("serialize report")
        );
    } else {
        for e in &report.errors {
            match (&e.node, &e.port) {
                (Some(n), Some(p)) => eprintln!("error [{n} {p}]: {}", e.message),
                (Some(n), None) => eprintln!("error [{n}]: {}", e.message),
                _ => eprintln!("error: {}", e.message),
            }
        }
        for w in &report.warnings {
            eprintln!("warning: {w}");
        }
        if report.ok {
            println!("ok ({} warning(s))", report.warnings.len());
        }
    }

    if report.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `play`: the live audio path — load an instrument and render it, driven by incoming OSC.
fn play(path: Option<PathBuf>, osc_out_target: Option<String>) {
    let (tx, rx) = mpsc::channel();

    // Log incoming/outgoing OSC only when asked: this runs on the I/O paths, and the stdout
    // lock would add latency/jitter if it fired on every message while playing. Off by
    // default; flip on to confirm wiring during bring-up.
    let log_osc = std::env::var_os("REUBEN_LOG_OSC").is_some();

    // OSC-out sender thread (ADR-0026): bind a UDP socket to the static `--osc-out host:port`
    // target and encode + send each outbound Message off the audio thread. `None` when no target
    // is configured — the engine still drains its outbound route, but audio.rs drops it (warning
    // once if a rig actually sends). Mirrors the OSC-in receiver thread below.
    let osc_out_tx = osc_out_target.map(|target| {
        let socket = UdpSocket::bind("0.0.0.0:0").expect("bind OSC-out socket");
        socket
            .connect(&target)
            .unwrap_or_else(|e| panic!("connect OSC-out {target}: {e}"));
        println!("OSC-out sending to {target}");
        let (out_tx, out_rx) = mpsc::channel::<Message>();
        thread::spawn(move || {
            // Each outbound Message carries one typed Arg; expand it to the flat OSC primitive form
            // (ADR-0030 boundary) before encoding the datagram.
            let mut flat = Vec::new();
            for m in out_rx {
                flat.clear();
                boundary::osc_out_args(&m.arg, &mut flat);
                match osc::encode(&m.address, &flat) {
                    Ok(bytes) => {
                        if log_osc {
                            println!("send {} {:?}", m.address, flat);
                        }
                        let _ = socket.send(&bytes);
                    }
                    Err(e) => eprintln!("OSC encode error: {e}"),
                }
            }
        });
        out_tx
    });

    // OSC/UDP receiver thread: decode datagrams and forward Messages to the audio thread.
    let socket = UdpSocket::bind(OSC_BIND).expect("bind OSC socket");
    println!("OSC-in listening on {OSC_BIND}  (send /voicer/note [midi, gate])");
    if !log_osc {
        println!("  (set REUBEN_LOG_OSC=1 to log received OSC)");
    }
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => match osc::decode(&buf[..n]) {
                    Ok(msgs) => {
                        for m in msgs {
                            if log_osc {
                                println!("recv {} {:?}", m.address, m.args.as_slice());
                            }
                            let _ = tx.send(m);
                        }
                    }
                    Err(e) => eprintln!("OSC decode error: {e}"),
                },
                Err(e) => {
                    eprintln!("OSC recv error: {e}");
                    break;
                }
            }
        }
    });

    // Instrument source: a path argument, else the embedded default. Resource paths (sample
    // files) resolve relative to the instrument file's directory; the embedded default has
    // none, so it roots at the current directory.
    let (instrument_json, base_dir) = match path {
        Some(path) => {
            println!("instrument: {}", path.display());
            let json = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let base = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            (json, base)
        }
        None => {
            println!("instrument: <default> (pass a path to load your own)");
            (DEFAULT_JSON.to_string(), PathBuf::from("."))
        }
    };

    let _stream = audio::start(rx, BLOCK_SIZE, osc_out_tx, |cfg| {
        println!(
            "audio out @ {} Hz, block {}",
            cfg.sample_rate, cfg.block_size
        );
        let resolver = FsResolver::new(&base_dir);
        let loaded = load_instrument(&instrument_json, &Registry::builtin(), &resolver)
            .expect("load instrument");
        // Resource problems are non-fatal (ADR-0016): the rig still plays, but the user must
        // see them — they are authoring errors.
        for w in &loaded.warnings {
            eprintln!("warning: {w}");
        }
        let plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate rig");
        Engine::new(plan)
    })
    .expect("start audio");

    println!("playing — Ctrl-C to quit.");
    // Keep the process (and thus the audio stream) alive.
    loop {
        thread::park();
    }
}
