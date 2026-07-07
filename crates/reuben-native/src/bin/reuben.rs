//! `reuben` — the command-line entry point.
//!
//! Four subcommands:
//! - `reuben play [path] [--io-map <file>]` — render an instrument live, driven by OSC over UDP.
//!   With no path it plays the built-in default rig. `--io-map` loads a device profile (ADR-0038
//!   §6): logical↔device channel maps, device selection by name substring, and sample-rate/
//!   buffer-size preferences — see docs/device-profile.md. Omit it for the default device and
//!   identity map, bit-identical to before. Send notes with:
//!
//!   ```text
//!   /voicer/notes  [69.0, 1.0]   # note-on  (MIDI 69 = A4, gate 1)
//!   /voicer/notes  [69.0, 0.0]   # note-off (gate 0)
//!   ```
//!
//! - `reuben describe [op|patch.json] [--json]` — print the operator set, one operator's ports/
//!   params/resource slots, or — given an instrument JSON path — that instrument's `interface`
//!   boundary as a host sees it (ADR-0034 §4: types inherited from the inner ports,
//!   presentational overrides applied). The introspection half of the Patcher skill (ADR-0020).
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
use reuben_core::{Engine, Registry};
use reuben_native::cli::{describe, describe_patch, validate};
use reuben_native::profile::DeviceProfile;
use reuben_native::resources::FsResolver;
use reuben_native::rigs::DEFAULT_JSON;
use reuben_native::{audio, osc, scaffold};

const BLOCK_SIZE: usize = 256;
const OSC_BIND: &str = "0.0.0.0:9000";

#[derive(Parser)]
#[command(name = "reuben", about = "Play and author reuben instruments.")]
struct Cli {
    /// Instrument library root: a sample or nested-patch reference that does not exist next
    /// to the file referencing it is looked up under this directory instead (sibling-first
    /// search). Falls back to the `REUBEN_INSTRUMENT_ROOT` env var.
    #[arg(long, global = true, value_name = "DIR")]
    instrument_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

/// The effective library root: the `--instrument-root` flag, else `REUBEN_INSTRUMENT_ROOT`.
fn instrument_root(flag: Option<PathBuf>) -> Option<PathBuf> {
    flag.or_else(|| std::env::var_os("REUBEN_INSTRUMENT_ROOT").map(PathBuf::from))
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
        /// Device profile JSON (ADR-0038 §6): logical↔device channel maps, device selection by
        /// name substring, and sample-rate/buffer-size preferences — outside the patch, so the
        /// same instrument plays on any rig. Omit for the default device and identity map,
        /// bit-identical to today's behavior. See docs/device-profile.md.
        #[arg(long, value_name = "FILE")]
        io_map: Option<PathBuf>,
    },
    /// Print the operator set, one operator's ports/params/resources, or — given an instrument
    /// JSON path — that instrument's `interface` boundary as a host sees it (ADR-0034).
    Describe {
        /// Operator type to describe, or an instrument JSON path (its nested boundary). A path
        /// is recognized by shape — ends in `.json` or contains a separator; a bare name is
        /// always an operator. Omit to list every operator.
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
        /// Contract JSON (type_name, inputs, outputs, constants, resources).
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
    let cli = Cli::parse();
    let root = instrument_root(cli.instrument_root);
    match cli.command {
        Command::Play {
            path,
            osc_out,
            io_map,
        } => {
            play(path, osc_out, io_map, root);
            ExitCode::SUCCESS
        }
        Command::Describe { op, json } => cmd_describe(op.as_deref(), json, root),
        Command::Validate { path, json } => cmd_validate(&path, json, root),
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

/// Render one port line of `describe` output, shared by the operator and patch-boundary views.
fn print_ports(dir: &str, ps: &[reuben_native::cli::PortInfo]) {
    for p in ps {
        let mut s = format!("  {dir} {} : {}", p.name, p.kind);
        if p.constant {
            s.push_str(" (constant)");
        }
        if let Some(d) = &p.default {
            s.push_str(&format!(" = {d}"));
        }
        if !p.unit.is_empty() {
            s.push_str(&format!(" {}", p.unit));
        }
        if let (Some(min), Some(max)) = (p.min, p.max) {
            s.push_str(&format!(" [{min}..{max}]"));
        }
        if let Some(c) = &p.curve {
            s.push_str(&format!(" ({c})"));
        }
        if !p.variants.is_empty() {
            s.push_str(&format!(" {{{}}}", p.variants.join(", ")));
        }
        if let Some(l) = &p.label {
            s.push_str(&format!(" \"{l}\""));
        }
        if let Some(w) = &p.widget {
            s.push_str(&format!(" <{w}>"));
        }
        if let Some(ch) = p.channel {
            s.push_str(&format!(" @channel {ch}"));
        }
        println!("{s}");
    }
}

/// Read an instrument file to its JSON text, paired with a resolver rooted at its directory —
/// resource paths (samples, nested instruments) resolve relative to the referencing file,
/// falling back to the library `root` when configured. The one loading preamble behind
/// `describe`, `validate`, and `play`.
fn read_instrument(path: &Path, root: Option<PathBuf>) -> Result<(String, FsResolver), String> {
    let json =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut resolver = FsResolver::for_instrument(path);
    if let Some(root) = root {
        resolver = resolver.with_root(root);
    }
    Ok((json, resolver))
}

/// `describe`'s argument is an instrument **path** by shape alone: it ends in `.json` or contains a
/// path separator. A bare name is always an operator, so a stray file named `filter` in the
/// cwd cannot shadow `describe filter` — routing must not depend on directory contents.
fn is_patch_path(arg: &str) -> bool {
    Path::new(arg).extension().is_some_and(|e| e == "json")
        || arg.chars().any(std::path::is_separator)
}

/// `describe`: dump the operator set, one operator, or — for an instrument JSON path — that
/// instrument's boundary as a host sees it (ADR-0034 §4), as human text or JSON.
fn cmd_describe(op: Option<&str>, json: bool, root: Option<PathBuf>) -> ExitCode {
    // A path-shaped argument is an instrument: describe its boundary.
    if let Some(arg) = op {
        if is_patch_path(arg) {
            return cmd_describe_patch(Path::new(arg), json, root);
        }
    }

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
        print_ports("in ", &o.inputs);
        print_ports("out", &o.outputs);
        for r in &o.resources {
            println!("  resource {r}");
        }
    }
    ExitCode::SUCCESS
}

/// `describe <patch.json>`: the nested-instrument boundary view — the `interface` ports a host
/// wires against, with metadata inherited from the inner ports and the entry overrides applied.
fn cmd_describe_patch(path: &Path, json: bool, root: Option<PathBuf>) -> ExitCode {
    let (instrument_json, resolver) = match read_instrument(path, root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Introspection never renders audio: stat samples for availability instead of decoding them.
    let boundary = match describe_patch(
        &instrument_json,
        &Registry::builtin(),
        &resolver.stat_only(),
    ) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&boundary).expect("serialize boundary")
        );
        return ExitCode::SUCCESS;
    }

    for w in &boundary.warnings {
        eprintln!("warning: {w}");
    }
    println!("{} (instrument boundary)", boundary.instrument);
    if boundary.is_empty() {
        println!("  (no `interface` boundary — nests, but exposes nothing to wire)");
    }
    print_ports("in ", &boundary.inputs);
    print_ports("out", &boundary.outputs);
    for d in &boundary.dark_inputs {
        println!("  in  {d} : (dark — unresolved this load)");
    }
    for d in &boundary.dark_outputs {
        println!("  out {d} : (dark — unresolved this load)");
    }
    ExitCode::SUCCESS
}

/// `validate`: report whether an instrument loads + plans cleanly. Exit 1 only on hard errors;
/// warnings (e.g. an unresolved sample) are advisory and keep exit 0.
fn cmd_validate(path: &Path, json: bool, root: Option<PathBuf>) -> ExitCode {
    let (instrument_json, resolver) = match read_instrument(path, root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let report = validate(&instrument_json, &Registry::builtin(), &resolver);

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
fn play(
    path: Option<PathBuf>,
    osc_out_target: Option<String>,
    io_map: Option<PathBuf>,
    root: Option<PathBuf>,
) {
    // Device profile (ADR-0038 §6): `--io-map <file>` loads logical↔device channel maps, device
    // selection, and sample-rate/buffer-size preferences. No flag -> the default profile, which
    // is identity map + the default device (today's behavior, unchanged). A malformed profile is
    // a structural load error (§7) — fatal, like any other bad instrument input this binary reads.
    let profile = match &io_map {
        Some(path) => {
            let profile = DeviceProfile::load(path)
                .unwrap_or_else(|e| panic!("io-map {}: {e}", path.display()));
            println!("io-map: {}", path.display());
            if profile.has_input() {
                println!(
                    "  note: input.* takes effect only when the played instrument binds input \
                     channels; an instrument without input pipes never opens an input device"
                );
            }
            profile
        }
        None => DeviceProfile::default(),
    };

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
                // `false` means the Arg has no OSC form and expanded to nothing — skip the
                // datagram (the rule is `osc_out_args`' contract, see its docs).
                if !boundary::osc_out_args(&m.arg, &mut flat) {
                    continue;
                }
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
    println!("OSC-in listening on {OSC_BIND}  (send /voicer/notes [midi, gate])");
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
    let (instrument_json, resolver) = match path {
        Some(path) => {
            println!("instrument: {}", path.display());
            read_instrument(&path, root).unwrap_or_else(|e| panic!("{e}"))
        }
        None => {
            println!("instrument: <default> (pass a path to load your own)");
            let resolver = match root {
                Some(root) => FsResolver::new(".").with_root(root),
                None => FsResolver::new("."),
            };
            (DEFAULT_JSON.to_string(), resolver)
        }
    };

    // `_diagnostics` is the shared xrun/ring counter surface (ADR-0038 §9) — both the output
    // callback and the input stream (P5/#182) feed it, and `audio::start` is already logging
    // it periodically to stderr. `_streams` holds the live cpal stream(s): output always,
    // plus the input stream when the played instrument binds input channels; dropping it
    // stops audio, so it lives until the park loop below.
    let (_streams, _diagnostics) = audio::start(rx, BLOCK_SIZE, osc_out_tx, &profile, |cfg| {
        println!(
            "audio out @ {} Hz, block {}",
            cfg.sample_rate, cfg.block_size
        );
        let (engine, warnings) =
            Engine::from_document(&instrument_json, &Registry::builtin(), &resolver, cfg)
                .expect("load instrument");
        // Resource problems are non-fatal (ADR-0016): the rig still plays, but the user must
        // see them — they are authoring errors.
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        engine
    })
    .unwrap_or_else(|e| panic!("start audio: {e}"));

    println!("playing — Ctrl-C to quit.");
    // Keep the process (and thus the audio stream) alive.
    loop {
        thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::is_patch_path;

    /// Dispatch is by argument shape alone: a bare name is an operator even when a file of the
    /// same name exists in the cwd (the shadowing bug this rule exists to prevent).
    #[test]
    fn bare_name_is_never_a_path() {
        assert!(!is_patch_path("filter"));
        assert!(!is_patch_path("oscillator"));
    }

    #[test]
    fn json_extension_or_separator_is_a_path() {
        assert!(is_patch_path("space.json"));
        assert!(is_patch_path("instruments/patches/space.json"));
        assert!(is_patch_path("./filter"));
        assert!(is_patch_path("instruments/space"));
    }
}
