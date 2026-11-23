//! `reuben` — the command-line entry point.
//!
//! Seven subcommands — `play`, `describe`, `validate`, `scaffold-operator`, `new-instrument`,
//! `send`, `live` — each documented on its own `--help`. `play` is the server. `describe`,
//! `validate` and `new-instrument` are its clients: they ask the running `reuben play` when one
//! answers, and otherwise a temporary headless server they start in this process. `send` and
//! `live` send text to the running `reuben play` and fail when none answers. A playing instrument
//! takes control at its interface inputs, `/<name>/in`; the default instrument plays notes at its
//! `notes` input:
//!
//! ```text
//! /notes/in  [69.0, 1.0]   # note-on  (MIDI 69 = A4, gate 1)
//! /notes/in  [69.0, 0.0]   # note-off (gate 0)
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;

use clap::{Parser, Subcommand};

use reuben_api::authoring::{self, Diag, PortInfo, COMPACT_DESCRIBE_LEGEND, SCAFFOLD_DEFAULT_NAME};
use reuben_api::engine::call_tool;
use reuben_api::tools::names;
use reuben_native::cli::{
    EngineFlags, InstrumentRoot, RunningServerFlags, ServerFlags, INSTRUMENT_ROOT_ENV, LOG_OSC_ENV,
};
use reuben_native::client::{self, Names, Reached, Temporary};
use reuben_native::engine::{self, Instrument};
use reuben_native::engine_channel::lexical_absolute;
use reuben_native::live::{self, SEND_READ_TIMEOUT};
use reuben_native::scaffold;
use serde::de::DeserializeOwned;

#[derive(Parser)]
#[command(name = "reuben", about = "Play and author reuben instruments.")]
struct Cli {
    #[command(flatten)]
    instrument_root: InstrumentRoot,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render an instrument live, driven by OSC over UDP (default: the built-in rig).
    Play {
        /// Instrument JSON to play; omit for the built-in default rig.
        path: Option<PathBuf>,
        #[command(flatten)]
        engine: EngineFlags,
    },
    /// Print the operator set, one operator's ports/params/resources, or — given an instrument
    /// JSON path — a structural view of that instrument. Asks the running `reuben play`, or a
    /// temporary server when none runs.
    Describe {
        /// Operator type to describe, or an instrument JSON path (its structural views). A path
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
        /// Which view of an instrument to cut (instrument paths only). The same views the
        /// `describe_instrument` tool serves, off the same verb, so both doors read a document the
        /// same way. `--json` wraps the rendered view — except `boundary`, whose structured shape
        /// is what control-surface tooling consumes. Defaults to `index`.
        #[arg(long, value_enum)]
        view: Option<InstrumentView>,
        /// Narrow `--view nodes`/`pipes` to these node addresses or pipe names. A name matching
        /// nothing is reported, never silently dropped; any other view refuses it.
        #[arg(long, value_name = "NAME", num_args = 1..)]
        select: Vec<String>,
        /// Narrow `--view nodes`/`pipes` by declared type instead of by name — mutually exclusive
        /// with `--select`.
        #[arg(long, value_name = "TYPE", conflicts_with = "select")]
        select_type: Option<String>,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Load + plan an instrument (no audio) and report errors/warnings. Asks the running
    /// `reuben play`, or a temporary server when none runs; never changes what plays.
    Validate {
        /// Instrument JSON to validate.
        path: PathBuf,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        server: ServerFlags,
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
    /// Create a guaranteed-valid minimal instrument document at a path — the start move for
    /// creating an instrument from scratch. Refuses to overwrite an existing document. The
    /// running `reuben play` writes it (it must be a place that server offers) and does not open
    /// it; with none running, a temporary server writes it.
    NewInstrument {
        /// Where to create the document.
        path: PathBuf,
        /// The `instrument` name for the new document (default `untitled`).
        #[arg(long)]
        name: Option<String>,
        /// Emit machine-readable JSON instead of a human summary.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        server: ServerFlags,
    },
    /// Send one string to an input of the running `reuben play` that compiles its text, such as a
    /// pattern. A string that does not compile is printed as `<file>:<line>:<col>: error: …` with
    /// its span marked, nothing is applied, and the exit code is non-zero. Never starts a
    /// temporary server. With `--file` and `--line`, sends that one line of a set file instead.
    Send {
        /// The input's address, e.g. `/pattern/in`.
        #[arg(required_unless_present = "file")]
        address: Option<String>,
        /// The text, as one argument: quote it, as a shell expands `~` and splits on spaces. Read
        /// from stdin when left out, so an editor can pipe a selection in. Trailing whitespace is
        /// dropped.
        #[arg(conflicts_with = "file")]
        pattern: Option<String>,
        /// Send one line of this set file, read from disk, in place of `<ADDRESS> [PATTERN]`: the
        /// line's address and text, parsed as `reuben live` parses them. Needs `--line`.
        #[arg(long, requires = "line", conflicts_with = "address")]
        file: Option<PathBuf>,
        /// The 1-based line of `--file` to send. A blank or `#` comment line is an error.
        #[arg(long, requires = "file", value_name = "N")]
        line: Option<usize>,
        #[command(flatten)]
        server: RunningServerFlags,
    },
    /// Watch a set file and send its lines to the running `reuben play` on every save. Each line
    /// is an address, whitespace, then the text to the end of the line; blank lines and `#`
    /// comments are skipped. A line is sent when its text changed since it was last queued, or
    /// when its last send was refused. A refused line is reported with file, line and column and
    /// its address keeps playing what it played. Each save's reports sit between
    /// `reuben live: sending <file>` and `reuben live: done`.
    Live {
        /// The set file.
        file: PathBuf,
        #[command(flatten)]
        server: RunningServerFlags,
    },
}

/// Which structural view of an instrument `describe <path>` cuts.
///
/// The window's view set, spelled as a `clap` flag: the enum has to live here because the flag
/// grammar is the door's — what each view *means* is decided once behind the window, and
/// [`window_view`] is the only place the two spellings meet.
#[derive(Copy, Clone, PartialEq, Eq, clap::ValueEnum)]
enum InstrumentView {
    /// Every node, one `address type` line each — the cheapest whole-document read.
    Index,
    /// Node zoom: inputs (literal and wired), config, description, resource ref, and the
    /// consumers reading each node.
    Nodes,
    /// The `interface` pipes a host wires against, with their ranges and metadata.
    Pipes,
    /// The resources table and who references each entry.
    Resources,
    /// The boundary as a *host instrument* sees it — the nesting face, not the document's own
    /// structure. The one view that is not a projection.
    Boundary,
}

/// The flag spelling to the window's view.
fn window_view(view: InstrumentView) -> authoring::InstrumentView {
    match view {
        InstrumentView::Index => authoring::InstrumentView::Index,
        InstrumentView::Nodes => authoring::InstrumentView::Nodes,
        InstrumentView::Pipes => authoring::InstrumentView::Pipes,
        InstrumentView::Resources => authoring::InstrumentView::Resources,
        InstrumentView::Boundary => authoring::InstrumentView::Boundary,
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = cli.instrument_root;
    match cli.command {
        // The server offers its library's instruments to clients, so it looks for one when none
        // is named; a one-shot command keeps references resolving beside the file alone.
        Command::Play { path, engine } => play(path, engine, root.resolve_or_default()),
        Command::Describe {
            op,
            json,
            compact,
            view,
            select,
            select_type,
            server,
        } => cmd_describe(
            op.as_deref(),
            json,
            compact,
            view,
            select,
            select_type,
            &|paths: &[&str]| connect(&server, root.clone().resolve(), paths),
        ),
        Command::Validate { path, json, server } => {
            cmd_validate(&path, json, &server, root.resolve())
        }
        Command::ScaffoldOperator {
            spec,
            core_root,
            json,
        } => cmd_scaffold(&spec, &core_root, json),
        Command::NewInstrument {
            path,
            name,
            json,
            server,
        } => cmd_new_instrument(&path, name.as_deref(), json, &server, root.resolve()),
        Command::Send {
            address,
            pattern,
            file,
            line,
            server,
        } => match (address, file, line) {
            (_, Some(file), Some(line)) => cmd_send_line(&file, line, &server),
            (Some(address), ..) => cmd_send(&address, pattern, &server),
            _ => unreachable!("clap requires an address or --file"),
        },
        Command::Live { file, server } => cmd_live(&file, &server),
    }
}

/// One `--json` payload on stdout. The shape is whatever the window answered with; the framing —
/// pretty-printed, one value, no envelope — is the door's.
fn print_json<T: serde::Serialize>(value: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("serialize a window answer")
    );
}

/// The server a client subcommand asks, per `flags`: on failure the reason is on stderr and the
/// exit code is returned. A temporary server is also offered the directory of each of `paths`, so a
/// file named anywhere is read as before. When a running server answers, stderr says which: its
/// operator set and its library are the ones that answer.
fn connect(
    flags: &ServerFlags,
    root: Option<PathBuf>,
    paths: &[&str],
) -> Result<Reached, ExitCode> {
    let target = flags.target().map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    })?;
    let offer = paths
        .iter()
        .filter_map(|path| lexical_absolute(Path::new(path)))
        .filter_map(|path| path.parent().map(Path::to_path_buf))
        .collect();
    let reached = client::reach(
        target,
        Temporary {
            instrument_root: root,
            offer,
        },
    )
    .map_err(|e| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    })?;
    if let Some(addr) = reached.running_at() {
        eprintln!(
            "note: answered by the reuben server at {addr} (--temporary-server to use a fresh one)"
        );
    }
    Ok(reached)
}

/// Run `tool` on the server and read its output as `T`. A refusal is printed in the user's own
/// spelling of the names it carries.
fn ask<T: DeserializeOwned>(
    reached: &Reached,
    tool: &str,
    args: &impl serde::Serialize,
    names: &Names,
) -> Result<T, ExitCode> {
    let answer = call_tool(tool, args, reached.channel()).map_err(|refusal| {
        eprintln!("error: {}", names.in_user_terms(&refusal.message));
        ExitCode::FAILURE
    })?;
    serde_json::from_value(answer.output).map_err(|e| {
        eprintln!("error: the server answered `{tool}` with something unexpected: {e}");
        ExitCode::FAILURE
    })
}

/// The running server `flags` name; on failure the reason is on stderr and the exit code returned.
fn connect_running(flags: &RunningServerFlags) -> Result<Reached, ExitCode> {
    let fail = |e: &dyn std::fmt::Display| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    };
    let (addr, token) = flags.server().map_err(|e| fail(&e))?;
    client::reach_running(&addr, token, SEND_READ_TIMEOUT).map_err(|e| fail(&e))
}

/// `send`: one string to one address, from the argument or stdin.
fn cmd_send(address: &str, pattern: Option<String>, server: &RunningServerFlags) -> ExitCode {
    let (file, text) = match pattern {
        Some(text) => ("<arg>", text),
        None => {
            let mut text = String::new();
            if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text) {
                eprintln!("error: reading the pattern from stdin: {e}");
                return ExitCode::FAILURE;
            }
            ("<stdin>", text)
        }
    };
    let text = text.trim_end();
    let reached = match connect_running(server) {
        Ok(reached) => reached,
        Err(code) => return code,
    };
    match live::send_text(reached.channel(), address, text) {
        Ok(reuben_api::engine::SendOutcome::Queued) => ExitCode::SUCCESS,
        Ok(reuben_api::engine::SendOutcome::Refused(refusal)) => {
            eprintln!("{}", live::refusal_report(file, 1, 1, text, &refusal));
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `send --file --line`: one line of a set file, as `live` would send it.
fn cmd_send_line(file: &Path, line: usize, server: &RunningServerFlags) -> ExitCode {
    let shown = file.display().to_string();
    let source = match std::fs::read_to_string(file) {
        Ok(source) => source,
        Err(e) => {
            eprintln!("error: reading {shown}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let set_line = match live::set_line(&shown, &source, line) {
        Ok(set_line) => set_line,
        Err(report) => {
            eprintln!("{report}");
            return ExitCode::FAILURE;
        }
    };
    let reached = match connect_running(server) {
        Ok(reached) => reached,
        Err(code) => return code,
    };
    match live::send_text(reached.channel(), &set_line.address, &set_line.text) {
        Ok(reuben_api::engine::SendOutcome::Queued) => {
            println!("reuben send: queued {} (line {line})", set_line.address);
            ExitCode::SUCCESS
        }
        Ok(reuben_api::engine::SendOutcome::Refused(refusal)) => {
            eprintln!(
                "{}",
                live::refusal_report(&shown, line, set_line.column, &set_line.text, &refusal)
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `live`: send a set file's lines on every save, until interrupted.
fn cmd_live(file: &Path, server: &RunningServerFlags) -> ExitCode {
    let reached = match connect_running(server) {
        Ok(reached) => reached,
        Err(code) => return code,
    };
    let shown = file.display().to_string();
    match live::run(file, &shown, reached.channel(), &mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `new-instrument`: land a guaranteed-valid minimal instrument document **at a path** — the
/// first-creation start move.
///
/// The window's `new_instrument`, the very verb the sidecar calls, so both doors create a document
/// by exactly one procedure and the result is validated and written by the same code.
fn cmd_new_instrument(
    path: &Path,
    name: Option<&str>,
    json: bool,
    server: &ServerFlags,
    root: Option<PathBuf>,
) -> ExitCode {
    let source = path.display().to_string();
    let reached = match connect(server, root, &[&source]) {
        Ok(reached) => reached,
        Err(code) => return code,
    };
    let mut spelled = Names::for_target(reached.remote());
    let result: authoring::EditResult = match ask(
        &reached,
        names::NEW_INSTRUMENT,
        &authoring::NewInstrument {
            source: spelled.sent(&source),
            name: name.unwrap_or(SCAFFOLD_DEFAULT_NAME).to_string(),
        },
        &spelled,
    ) {
        Ok(result) => result,
        Err(code) => return code,
    };

    if json {
        print_json(&result);
    } else {
        for e in &result.report.errors {
            print_diag("error", e, &spelled);
        }
        if result.written {
            println!("created {source} (content_hash {})", result.hash);
            println!("{}", result.zoom);
        }
    }

    if result.written {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
            "next: implement `{}` test-first — the scaffolded test starts red.",
            report.type_name
        );
    }
    ExitCode::SUCCESS
}

/// Render one port line of `describe` output, shared by the operator and patch-boundary views.
fn print_ports(dir: &str, ps: &[PortInfo]) {
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

/// `describe`'s argument is an instrument **path** by shape alone: it ends in `.json` or contains a
/// path separator. A bare name is always an operator, so a stray file named `filter` in the
/// cwd cannot shadow `describe filter` — routing must not depend on directory contents.
fn is_patch_path(arg: &str) -> bool {
    Path::new(arg).extension().is_some_and(|e| e == "json")
        || arg.chars().any(std::path::is_separator)
}

/// `describe`: dump the operator set, one operator, or — for an instrument JSON path — the
/// `--view` the caller asked for (the node index by default), as human text or JSON. `--compact`
/// switches the operator view to the generated signature-line mode.
fn cmd_describe(
    op: Option<&str>,
    json: bool,
    compact: bool,
    view: Option<InstrumentView>,
    select: Vec<String>,
    select_type: Option<String>,
    connect: &dyn Fn(&[&str]) -> Result<Reached, ExitCode>,
) -> ExitCode {
    // A path-shaped argument is an instrument: cut the asked-for view of it.
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
            let view = view.unwrap_or(InstrumentView::Index);
            // `describe_boundary` takes no terms. Terms on `boundary` are always refused, so they
            // go to `describe_instrument`, which refuses them in the window's own order — an
            // unreadable source before the selection.
            let has_terms = !select.is_empty() || select_type.is_some();
            let reached = match connect(&[arg]) {
                Ok(reached) => reached,
                Err(code) => return code,
            };
            if view == InstrumentView::Boundary && !has_terms {
                return cmd_describe_boundary(&reached, arg, json);
            }
            return cmd_project(&reached, arg, view, select, select_type, json);
        }
    }

    // The instrument flags have no meaning for an operator; printing the operator anyway would
    // answer a question the caller did not ask.
    let instrument_only = [
        ("--view", view.is_some()),
        ("--select", !select.is_empty()),
        ("--select-type", select_type.is_some()),
    ];
    if let Some((flag, _)) = instrument_only.iter().find(|(_, given)| *given) {
        eprintln!(
            "error: {flag} applies only to describing an instrument path, not the operator \
             set"
        );
        return ExitCode::FAILURE;
    }

    match connect(&[]) {
        Ok(reached) => cmd_describe_operators(&reached, op, json, compact),
        Err(code) => code,
    }
}

/// `describe [op]`: the operator set, or one operator, off the window's `describe_operators`.
///
/// The window answers both modes under one object because a tool result needs an object root; a
/// shell consumer wants the bare list it can pipe, so the door unwraps the mode it asked for. The
/// framing is the door's, the content is not.
fn cmd_describe_operators(
    reached: &Reached,
    name: Option<&str>,
    json: bool,
    compact: bool,
) -> ExitCode {
    let operators: authoring::Operators = match ask(
        reached,
        names::DESCRIBE_OPERATORS,
        &authoring::DescribeOperators {
            name: name.map(str::to_string),
            compact,
        },
        &Names::default(),
    ) {
        Ok(operators) => operators,
        Err(code) => return code,
    };

    // Compact mode: one generated signature line per operator. Human output prints the notation
    // legend first, so the listing is a self-describing artifact a bundle can carry; `--json` emits
    // the bare line array for machine consumers that compose their own framing.
    if compact {
        let signatures = operators.signatures.unwrap_or_default();
        if json {
            print_json(&signatures);
            return ExitCode::SUCCESS;
        }
        println!("{COMPACT_DESCRIBE_LEGEND}");
        for line in &signatures {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }

    let operators = operators.operators.unwrap_or_default();
    if json {
        print_json(&operators);
        return ExitCode::SUCCESS;
    }
    for o in &operators {
        println!("{}", o.type_name);
        print_ports("in ", &o.inputs);
        print_ports("out", &o.outputs);
        for r in &o.resources {
            println!("  resource {r}");
        }
    }
    ExitCode::SUCCESS
}

/// One diagnostic on stderr, the loader's localization in brackets when it named one:
/// `error [/osc freq]: …` / `warning [/voicer]: …` / `error: …`. Errors and warnings share
/// this shape — warnings are localized Diags too.
fn print_diag(level: &str, d: &Diag, names: &Names) {
    let message = names.in_user_terms(&d.message);
    match (&d.node, &d.port) {
        (Some(n), Some(p)) => eprintln!("{level} [{n} {p}]: {message}"),
        (Some(n), None) => eprintln!("{level} [{n}]: {message}"),
        (None, Some(p)) => eprintln!("{level} [{p}]: {message}"),
        (None, None) => eprintln!("{level}: {message}"),
    }
}

/// `describe <patch.json> --view boundary`: the nested-instrument boundary view — the `interface`
/// pipes a host wires against: an input pipe's own declared type/range/default, an output pipe's
/// type and metadata inherited from the internal port feeding it plus optional min/max range
/// overrides (a subset of that port's range), both decorated by the entry's presentational fields
/// (label/unit/widget).
///
/// The one view that is **not** a projection, and the one whose `--json` is deliberately not the
/// rendered line the sidecar shows a model: this emits the window's structured boundary, which the
/// control-surface tooling consumes. Same question, two consumers — a program here, a model there —
/// and one window verb behind both, so they cannot describe different pipes.
fn cmd_describe_boundary(reached: &Reached, source: &str, json: bool) -> ExitCode {
    let mut spelled = Names::for_target(reached.remote());
    let boundary: authoring::Boundary = match ask(
        reached,
        names::DESCRIBE_BOUNDARY,
        &authoring::DescribeBoundary {
            source: spelled.sent(source),
        },
        &spelled,
    ) {
        Ok(boundary) => boundary,
        Err(code) => return code,
    };

    if json {
        print_json(&boundary);
        return ExitCode::SUCCESS;
    }

    for w in &boundary.warnings {
        print_diag("warning", w, &spelled);
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

/// `describe <patch.json> --view index|nodes|pipes|resources`: the structural projection — the
/// agent's whole view of a document, off the same window verb `describe_instrument` serves over
/// MCP, selection grammar included.
///
/// `--json` wraps the rendered view rather than emitting a per-view structured shape: the compact
/// line grammar is the projection's deliverable, and a door does not carry a second serialization
/// of it.
///
/// A document that fails to load still projects (`loadable: false` in the header): `validate` is
/// the single authority on validity, and going blind is the worst way to report invalidity. So this
/// exits 0 on an unloadable document — it answered the question it was asked.
fn cmd_project(
    reached: &Reached,
    source: &str,
    view: InstrumentView,
    select: Vec<String>,
    select_type: Option<String>,
    json: bool,
) -> ExitCode {
    // `clap`'s `conflicts_with` refuses select-and-type-at-once first with a nicer message; the
    // window refuses it too, and that is the one that decides the grammar. Neither door reads the
    // terms for itself.
    let mut spelled = Names::for_target(reached.remote());
    let projected: authoring::DocumentView = match ask(
        reached,
        names::DESCRIBE_INSTRUMENT,
        &authoring::DescribeInstrument {
            source: spelled.sent(source),
            view: window_view(view),
            select,
            type_name: select_type,
        },
        &spelled,
    ) {
        Ok(projected) => projected,
        Err(code) => return code,
    };

    if json {
        print_json(&projected);
    } else {
        println!("{}", projected.text);
    }
    ExitCode::SUCCESS
}

/// `validate`: report whether an instrument loads + plans cleanly, through the window's
/// `validate_instrument` — the single authority every document verb re-validates through. Exit 1
/// only on hard errors; warnings (e.g. an unresolved sample) are advisory and keep exit 0.
fn cmd_validate(path: &Path, json: bool, server: &ServerFlags, root: Option<PathBuf>) -> ExitCode {
    let source = path.display().to_string();
    let reached = match connect(server, root, &[&source]) {
        Ok(reached) => reached,
        Err(code) => return code,
    };
    let mut spelled = Names::for_target(reached.remote());
    let report: authoring::Report = match ask(
        &reached,
        names::VALIDATE_INSTRUMENT,
        &authoring::ValidateInstrument {
            source: spelled.sent(&source),
        },
        &spelled,
    ) {
        Ok(report) => report,
        Err(code) => return code,
    };

    if json {
        print_json(&report);
    } else {
        for e in &report.errors {
            print_diag("error", e, &spelled);
        }
        for w in &report.warnings {
            print_diag("warning", w, &spelled);
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
/// Everything it starts is [`engine::start`]'s; what is left here is the terminal — the flags, the
/// startup lines, and the signal that ends the session.
fn play(path: Option<PathBuf>, flags: EngineFlags, root: Option<PathBuf>) -> ExitCode {
    let instrument = path.map_or(Instrument::Default, Instrument::Path);
    // A malformed profile is a structural load error — fatal, like any other bad input this
    // binary reads.
    let config = match flags.config(instrument, root) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(path) = &flags.io_map {
        println!("io-map: {}", path.display());
        if config.profile.has_input() {
            println!(
                "  note: input.* takes effect only when the played instrument binds input \
                 channels; an instrument without input pipes never opens an input device"
            );
        }
    }
    match &config.instrument {
        Instrument::Path(path) => println!("instrument: {}", path.display()),
        Instrument::Default => println!("instrument: <default> (pass a path to load your own)"),
        Instrument::Silent => println!("instrument: none"),
    }
    match &config.instrument_root {
        Some(root) => println!("library: {}", root.display()),
        None => println!(
            "library: none (no instruments/ here or beside reuben; name one with \
             --instrument-root or {INSTRUMENT_ROOT_ENV})"
        ),
    }

    let log_osc = config.log_osc;
    let engine_addr = config.engine_addr.clone().unwrap_or_default();
    let start = if flags.headless {
        engine::start_headless
    } else {
        engine::start
    };
    let running = match start(config) {
        Ok(running) => running,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(target) = running.osc_out_target() {
        println!("OSC-out sending to {target}");
    }
    if let Some(addr) = running.osc_in_addr() {
        println!(
            "OSC-in listening on {addr}  (send to an interface input, /<name>/in; \
             the default instrument plays /notes/in [midi, gate])"
        );
        if !log_osc {
            println!("  (set {LOG_OSC_ENV}=1 to log received OSC)");
        }
    }
    println!(
        "{} @ {} Hz, block {}",
        if flags.headless {
            "headless, no audio device,"
        } else {
            "audio out"
        },
        running.sample_rate(),
        running.block_size()
    );
    // Resource problems are non-fatal: the rig still plays, but surface them.
    for w in running.warnings() {
        eprintln!("warning: {w}");
    }
    match (running.engine_addr(), running.channel_error()) {
        (Some(addr), _) => {
            println!("engine channel on {addr}");
            if !addr.ip().to_canonical().is_loopback() {
                println!(
                    "  open to other machines that send its token; the token travels in clear, \
                     so use a network that encrypts (Tailscale) or one you trust"
                );
            }
        }
        (None, Some(e)) => eprintln!(
            "warning: engine channel unavailable on {engine_addr} ({e}); no client \
             (reuben-mcp, the editor, the CLI) can call, open, list or subscribe this run"
        ),
        (None, None) => {}
    }

    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
    install_shutdown_handler(shutdown_tx.clone());

    println!("playing — Ctrl-C to quit.");
    // Block until a signal arrives (unix). On non-unix there is no signal bridge, so `shutdown_tx`
    // (held here) keeps the channel open and this parks until the OS default terminates on Ctrl-C.
    let _ = shutdown_rx.recv();
    println!("shutting down…");

    reuben_native::diagnostics::log_snapshot(&running.shutdown());
    ExitCode::SUCCESS
}

/// Install the SIGINT/SIGTERM → shutdown bridge (unix). `std` has no signal API, so this uses
/// `libc` (already in the tree, not async — the tokio fence is untouched). The signal
/// handler itself only stores into an atomic (all that is async-signal-safe); a small watcher
/// thread bridges that atomic to a unit message on `tx`, waking `play`'s blocked shutdown receiver
/// without the handler ever touching a channel or lock. Swaps do not ride this channel — they
/// install via the mailbox on the engine-channel threads — so it carries only the one shutdown signal.
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
    std::thread::spawn(move || loop {
        if SIGNALED.load(Ordering::SeqCst) {
            let _ = tx.send(());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
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
