//! `reuben` — the command-line entry point.
//!
//! Four subcommands:
//! - `reuben play [path] [--io-map <file>]` — render an instrument live, driven by OSC over UDP.
//!   With no path it plays the built-in default rig. `--io-map` loads a device profile:
//!   logical↔device channel maps, device selection by name substring, and sample-rate/
//!   buffer-size preferences — see docs/device-profile.md. Omit it for the default device and
//!   identity map, bit-identical to before. Send notes with:
//!
//!   ```text
//!   /voicer/notes  [69.0, 1.0]   # note-on  (MIDI 69 = A4, gate 1)
//!   /voicer/notes  [69.0, 0.0]   # note-off (gate 0)
//!   ```
//!
//! - `reuben describe [op|patch.json] [--json] [--compact]` — print the operator set, one
//!   operator's ports/params/resource slots, or — given an instrument JSON path — that
//!   instrument's `interface` boundary as a host sees it (an input pipe from its own declared
//!   type/range, an output pipe inheriting from the port feeding it, both carrying the entry's
//!   presentational fields). The introspection half of the Patcher skill. `--compact` is the
//!   generated signature-line mode of the operator view: one line per operator, legend first —
//!   the bundle-able grounding artifact the web build consumes.
//! - `reuben validate <path> [--json]` — load + plan an instrument with no audio device and
//!   report structural/wiring errors. Exit 1 if invalid; warnings alone stay exit 0.
//! - `reuben scaffold-operator --spec <path> [--json]` — generate a new Operator's Rust skeleton
//!   from a contract spec and wire its registration. The codegen half of the create-operator
//!   skill.
//! - `reuben scaffold-instrument [--name <name>] [--json]` — print a guaranteed-valid minimal
//!   instrument document (`{format_version, instrument, nodes:[]}`) to edit then swap — the
//!   first-creation start move (#146). Default output is pretty-printed; `--json` emits it as a
//!   single compact line. Both pipe cleanly into `reuben validate`.

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;

use clap::{Parser, Subcommand};

use reuben_core::boundary;
use reuben_core::coordinator::Coordinator;
use reuben_core::message::Message;
use reuben_core::Registry;
use reuben_native::cli::{
    describe, describe_compact, describe_patch, validate, COMPACT_DESCRIBE_LEGEND,
};
use reuben_native::profile::DeviceProfile;
use reuben_native::resources::FsResolver;
use reuben_native::rigs::DEFAULT_JSON;
use reuben_native::structure::StructureState;
use reuben_native::{audio, osc, scaffold, structure};

const BLOCK_SIZE: usize = 256;
/// The structure channel's default loopback bind, hoisted to a shared const in
/// `reuben_core::coordinator` so this server and the reuben-mcp client dial the *same* address
/// and can never drift. `127.0.0.1` only — structure edits are more powerful than OSC control,
/// so unlike OSC's `0.0.0.0:9000` this must never be network-exposed; a taken port is non-fatal
/// (see `play`).
use reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR as STRUCTURE_BIND;
use reuben_native::osc::DEFAULT_OSC_PORT;

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
        /// `osc_out` node's Messages are encoded and UDP-sent to. Omit to disable.
        #[arg(long, value_name = "HOST:PORT")]
        osc_out: Option<String>,
        /// Device profile JSON: logical↔device channel maps, device selection by
        /// name substring, and sample-rate/buffer-size preferences — outside the patch, so the
        /// same instrument plays on any rig. Omit for the default device and identity map,
        /// bit-identical to today's behavior. See docs/device-profile.md.
        #[arg(long, value_name = "FILE")]
        io_map: Option<PathBuf>,
    },
    /// Print the operator set, one operator's ports/params/resources, or — given an instrument
    /// JSON path — that instrument's `interface` boundary as a host sees it.
    Describe {
        /// Operator type to describe, or an instrument JSON path (its nested boundary). A path
        /// is recognized by shape — ends in `.json` or contains a separator; a bare name is
        /// always an operator. Omit to list every operator.
        op: Option<String>,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
        /// Compact mode: one generated signature line per operator instead of the
        /// full port listing — the same registry truth, projected for grounding budgets. Operator
        /// view only; full describe stays the zoom for port detail.
        #[arg(long)]
        compact: bool,
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
    /// Print a guaranteed-valid minimal instrument document to edit then swap — the start move for
    /// creating an instrument from scratch (#146).
    ScaffoldInstrument {
        /// The `instrument` name for the scaffolded document (default `untitled`).
        #[arg(long)]
        name: Option<String>,
        /// Emit the document as a single compact JSON line instead of pretty-printed.
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
        Command::Describe { op, json, compact } => cmd_describe(op.as_deref(), json, compact, root),
        Command::Validate { path, json } => cmd_validate(&path, json, root),
        Command::ScaffoldOperator {
            spec,
            core_root,
            json,
        } => cmd_scaffold(&spec, &core_root, json),
        Command::ScaffoldInstrument { name, json } => {
            cmd_scaffold_instrument(name.as_deref(), json)
        }
    }
}

/// `scaffold-instrument`: print a guaranteed-valid minimal instrument document (#146) to stdout —
/// the first-creation start move. The document is the deliverable, so it is always JSON: default
/// pretty-printed for a human to edit, `--json` as one compact line for a machine. Both pipe
/// cleanly into `reuben validate`. Single-sourced in `reuben_core` and returned by value; this
/// door adds the native gesture of writing it out.
fn cmd_scaffold_instrument(name: Option<&str>, json: bool) -> ExitCode {
    let document = reuben_core::scaffold_instrument(name);
    let text = if json {
        serde_json::to_string(&document)
    } else {
        serde_json::to_string_pretty(&document)
    }
    .expect("serialize scaffold document");
    println!("{text}");
    ExitCode::SUCCESS
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
            "next: implement `{}` test-first — the scaffolded test starts red.",
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
/// instrument's boundary as a host sees it, as human text or JSON. `--compact`
/// switches the operator view to the generated signature-line mode.
fn cmd_describe(op: Option<&str>, json: bool, compact: bool, root: Option<PathBuf>) -> ExitCode {
    // A path-shaped argument is an instrument: describe its boundary.
    if let Some(arg) = op {
        if is_patch_path(arg) {
            if compact {
                // The compact projection is a mode of the *operator* listing; an
                // instrument's compact face line is the library index's job.
                eprintln!(
                    "error: --compact describes the operator set, not an instrument boundary"
                );
                return ExitCode::FAILURE;
            }
            return cmd_describe_patch(Path::new(arg), json, root);
        }
    }

    if compact {
        return cmd_describe_compact(op, json);
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

/// `describe --compact`: the generated signature-line mode of the operator view —
/// one line per operator off the same registry truth as the full mode, never a hand-written
/// digest. Human output prints the notation legend first, so the listing is the
/// self-describing bundle-able artifact the web build consumes; `--json` emits the bare line
/// array for machine consumers that compose their own framing.
fn cmd_describe_compact(op: Option<&str>, json: bool) -> ExitCode {
    let lines = match describe_compact(&Registry::builtin(), op) {
        Ok(lines) => lines,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&lines).expect("serialize compact describe")
        );
        return ExitCode::SUCCESS;
    }

    println!("{COMPACT_DESCRIBE_LEGEND}");
    for line in &lines {
        println!("{line}");
    }
    ExitCode::SUCCESS
}

/// One diagnostic on stderr, the loader's localization in brackets when it named one:
/// `error [/osc freq]: …` / `warning [/voicer]: …` / `error: …`. Errors and warnings share
/// this shape — warnings are localized Diags too.
fn print_diag(level: &str, d: &reuben_core::contract::Diag) {
    match (&d.node, &d.port) {
        (Some(n), Some(p)) => eprintln!("{level} [{n} {p}]: {}", d.message),
        (Some(n), None) => eprintln!("{level} [{n}]: {}", d.message),
        (None, Some(p)) => eprintln!("{level} [{p}]: {}", d.message),
        (None, None) => eprintln!("{level}: {}", d.message),
    }
}

/// `describe <patch.json>`: the nested-instrument boundary view — the `interface` pipes a host
/// wires against: an input pipe's own declared type/range/default, an output pipe's
/// type and metadata inherited from the internal port feeding it plus optional min/max range
/// overrides (a subset of that port's range), both decorated by the entry's presentational fields
/// (label/unit/widget).
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
        print_diag("warning", w);
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
            print_diag("error", e);
        }
        for w in &report.warnings {
            print_diag("warning", w);
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
///
/// M2 (#323): the structure channel's `swap` verb is now a **gapless mailbox swap**,
/// not M1's stop-the-world restart. Streams are opened once here and fixed for the session:
/// a swap fills the install mailbox, the RT callback drains it and box-transplants survivors
/// under a master-gain ramp, and this process — the OSC socket, the structure channel, the
/// streams — is never torn down. The [`Coordinator`] the structure channel owns is the single
/// writer of graph structure.
///
/// see rules: execution-runtime
fn play(
    path: Option<PathBuf>,
    osc_out_target: Option<String>,
    io_map: Option<PathBuf>,
    root: Option<PathBuf>,
) {
    // Device profile: `--io-map <file>` loads logical↔device channel maps, device
    // selection, and sample-rate/buffer-size preferences. No flag -> the default profile, which
    // is identity map + the default device (today's behavior, unchanged). A malformed profile is
    // a structural load error — fatal, like any other bad instrument input this binary reads.
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

    // The control channel feeding the audio callback. Streams are fixed for the session
    // — a swap installs via the mailbox and never reopens the callback — so this single
    // receiver lives in the callback for the whole run and each producer forwards straight through
    // its own `osc_tx` clone. No swappable sink, no lock anywhere near the audio path (M1's restart
    // is gone).
    //
    // **Two producers, one ingress**: the UDP decode thread below (the foreign edge, where external
    // controllers arrive) and the structure channel's `send` verb (the loopback authoring door).
    // Both converge here, so a `send` and an external datagram are indistinguishable downstream.
    let (osc_tx, osc_rx) = mpsc::channel::<osc::OscIn>();

    // Log incoming/outgoing OSC only when asked: this runs on the I/O paths, and the stdout
    // lock would add latency/jitter if it fired on every message while playing. Off by
    // default; flip on to confirm wiring during bring-up.
    let log_osc = std::env::var_os("REUBEN_LOG_OSC").is_some();

    // OSC-out sender thread: bind a UDP socket to the static `--osc-out host:port`
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
            // at the boundary before encoding the datagram.
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

    // OSC/UDP receiver thread: decode datagrams and forward Messages straight to the audio callback
    // through `udp_tx`. The callback (and its receiver) live for the whole session now, so a forward
    // never races a swap — the mailbox swap keeps the same callback alive (M2, #323).
    // Host `0.0.0.0` (all interfaces) + the port shared with the reuben-mcp sidecar's dial target,
    // derived from the one `DEFAULT_OSC_PORT` const so the two can never drift on it.
    let osc_bind = format!("0.0.0.0:{DEFAULT_OSC_PORT}");
    let socket = UdpSocket::bind(&osc_bind).expect("bind OSC socket");
    println!("OSC-in listening on {osc_bind}  (send /voicer/notes [midi, gate])");
    if !log_osc {
        println!("  (set REUBEN_LOG_OSC=1 to log received OSC)");
    }
    let udp_tx = osc_tx.clone();
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
                            let _ = udp_tx.send(m);
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

    // Instrument source: a path argument, else the embedded default. Resource paths (sample files)
    // resolve through this `resolver`, anchored at the instrument file's directory (the embedded
    // default roots at the current directory) with the optional library `root` fallback. The
    // Coordinator owns this resolver for the session; a by-path swap resolves its resources through
    // it too — M2 does not re-anchor per swap source, so a by-path document's
    // relative resources resolve against the initial anchor + the library root.
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

    // Start live audio: open the device once, build the Coordinator + its RT
    // RenderSide at the device rate, and drive the RenderSlot in the callback. `install_initial`
    // mints the canonical document the structure channel then owns (`get_document` reports exactly
    // what plays). Streams are fixed for the session — a swap installs via the mailbox, never a
    // restart, so the device is opened here and never reopened.
    let live = audio::start(osc_rx, BLOCK_SIZE, osc_out_tx, &profile, |cfg| {
        println!(
            "audio out @ {} Hz, block {}",
            cfg.sample_rate, cfg.block_size
        );
        Coordinator::install_initial(
            &instrument_json,
            Registry::builtin(),
            Box::new(resolver),
            cfg,
        )
        .expect("load instrument")
    })
    .unwrap_or_else(|e| panic!("start audio: {e}"));

    // Resource problems are non-fatal: the rig still plays, but surface them.
    for w in &live.warnings {
        eprintln!("warning: {w}");
    }

    let audio::LiveAudio {
        streams,
        diagnostics,
        coordinator,
        render_config,
        warnings: _,
    } = live;

    // The structure channel: a loopback-TCP/NDJSON server answering the MCP sidecar's
    // ping/get_document/get_diagnostics/swap/send off dedicated std threads (no async runtime keeps
    // reuben-native tokio-free). It owns the Coordinator — the single writer of graph
    // structure — and publishes each swap's freshly-validated device output map
    // through the native render seam. Non-fatal: audio is the primary function, so a taken port
    // disables the channel with a warning rather than killing playback.
    //
    // Its control sink is a clone of the very sender the UDP thread holds, so `send` converges with
    // external OSC at the callback's `queue_osc` and this door needs no wire format of its own.
    let state = StructureState::from_coordinator(coordinator, diagnostics.clone())
        .with_render_config(render_config)
        .with_control_sink(osc_tx);
    let structure_server = match structure::StructureServer::bind(STRUCTURE_BIND, state) {
        Ok(server) => {
            println!("structure channel on {}", server.local_addr());
            Some(server)
        }
        Err(e) => {
            eprintln!(
                "warning: structure channel unavailable on {STRUCTURE_BIND} ({e}); MCP structure \
                 ops (get_document/get_diagnostics/swap) are disabled this run"
            );
            None
        }
    };

    // Clean shutdown: a SIGINT/SIGTERM handler wakes this thread, which
    // then tears down in order — stop the structure channel (joining its threads, which drops the
    // Coordinator and frees any last retired Engine off-thread), stop audio (dropping `streams`
    // stops the callback), and flush a final diagnostics snapshot (the exit-time log).
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    install_shutdown_handler(shutdown_tx.clone());

    println!("playing — Ctrl-C to quit.");
    // Block until a signal arrives (unix). On non-unix there is no signal bridge, so `shutdown_tx`
    // (held here) keeps the channel open and this parks until the OS default terminates on Ctrl-C.
    let _ = shutdown_rx.recv();
    println!("shutting down…");

    if let Some(server) = structure_server {
        server.shutdown();
    }
    drop(streams);
    reuben_native::diagnostics::log_snapshot(&diagnostics.snapshot());
}

/// Install the SIGINT/SIGTERM → shutdown bridge (unix). `std` has no signal API, so this uses
/// `libc` (already in the tree, not async — the tokio fence is untouched). The signal
/// handler itself only stores into an atomic (all that is async-signal-safe); a small watcher
/// thread bridges that atomic to a unit message on `tx`, waking `play`'s blocked shutdown receiver
/// without the handler ever touching a channel or lock. M2 (#323): swaps no longer ride this
/// channel — they install via the mailbox on the structure threads — so it now carries only the
/// one shutdown signal.
#[cfg(unix)]
fn install_shutdown_handler(tx: mpsc::Sender<()>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    static SIGNALED: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_sig: libc::c_int) {
        SIGNALED.store(true, Ordering::SeqCst);
    }

    // SAFETY: `on_signal` is async-signal-safe (a single atomic store, nothing else), and this
    // one-time install runs at startup before any signal can race a second registration.
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    thread::spawn(move || loop {
        if SIGNALED.load(Ordering::SeqCst) {
            let _ = tx.send(());
            break;
        }
        thread::sleep(Duration::from_millis(100));
    });
}

/// Non-unix fallback: no portable `std` signal facility, so Ctrl-C keeps the OS default
/// (terminate). Clean shutdown off unix is a follow-up — `play`'s persona is a unix checkout and a
/// terminal. `_tx` is dropped, but `play` holds the other sender so its shutdown
/// receiver parks until the OS default terminates the process.
#[cfg(not(unix))]
fn install_shutdown_handler(_tx: mpsc::Sender<()>) {}

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
