//! PROTOTYPE — throwaway TUI for issue #576. Not production code; nothing here ships.
//!
//! Run: `cargo run -p reuben-native --example prototype_set_input -- [instrument.json]`
//!
//! **The question.** #576 proposes `set-input` as a pure transform
//! `(document, node, input, value) -> { document, report }` so a desktop-class model changes one
//! parameter without re-emitting the whole document, and claims that makes corruption of the rest
//! "structurally impossible". This app lets you drive that transform by hand against a real
//! instrument and watch what it does to the other 99% of the document — under each of the three
//! serialization strategies in `set_input.rs`.
//!
//! State lives in memory. Nothing is written to disk, ever.

mod set_input;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use reuben_core::contract::{Diag, Report};
use reuben_core::Registry;
use reuben_native::resources::FsResolver;

use set_input::{set_input, Churn, Edit, Policy, Strategy};

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const OFF: &str = "\x1b[0m";

struct App {
    path: PathBuf,
    /// The document exactly as it sits on disk. Never mutated.
    pristine: String,
    /// The in-memory document. Every applied edit lands here.
    current: String,
    strategy: Strategy,
    /// Refuse a set whose target slot currently holds a wire-ref (see `set_input::wire_source`).
    guard_wires: bool,
    target: (String, String),
    last: Option<Applied>,
    /// Output of the last `x` (all-three-strategies) comparison.
    compare: Vec<String>,
}

struct Applied {
    call: String,
    report: Report,
    changed: bool,
    churn: Churn,
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_instrument);
    let pristine =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));

    let target = first_numeric_input(&pristine).unwrap_or_else(|| ("?".into(), "?".into()));
    let mut app = App {
        path,
        current: pristine.clone(),
        pristine,
        strategy: Strategy::Typed,
        guard_wires: false,
        target,
        last: None,
        compare: Vec::new(),
    };

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        app.render();
        print!("{BOLD}> {OFF}");
        let _ = std::io::stdout().flush();
        let Some(Ok(line)) = lines.next() else { break };
        if !app.dispatch(line.trim()) {
            break;
        }
    }
}

fn default_instrument() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments/groovebox.json")
}

impl App {
    /// Returns false to quit.
    fn dispatch(&mut self, line: &str) -> bool {
        let mut parts = line.split_whitespace();
        match parts.next() {
            None => {}
            Some("q") => return false,
            Some("s") => {
                let i = Strategy::ALL
                    .iter()
                    .position(|&s| s == self.strategy)
                    .unwrap();
                self.strategy = Strategy::ALL[(i + 1) % Strategy::ALL.len()];
            }
            Some("r") => {
                self.current = self.pristine.clone();
                self.last = None;
                self.compare.clear();
            }
            Some("t") => {
                if let (Some(node), Some(input)) = (parts.next(), parts.next()) {
                    self.target = (node.to_string(), input.to_string());
                }
            }
            Some("v") => {
                if let Some(value) = parts.next().and_then(|v| v.parse::<f64>().ok()) {
                    self.apply(value);
                }
            }
            Some("x") => {
                if let Some(value) = parts.next().and_then(|v| v.parse::<f64>().ok()) {
                    self.compare_strategies(value);
                }
            }
            Some("g") => self.guard_wires = !self.guard_wires,
            Some("l") => self.compare = self.list_targets(),
            Some(other) => {
                self.compare = vec![format!("{RED}unknown command `{other}`{OFF}")];
            }
        }
        true
    }

    /// One `set-input` call against the in-memory document, under the current strategy.
    fn apply(&mut self, value: f64) {
        let (node, input) = self.target.clone();
        let before = self.current.clone();
        let edit = self.call(&before, &node, &input, value);
        let churn = Churn::between(&before, &edit.document);
        self.current = edit.document;
        self.last = Some(Applied {
            call: format!("set-input {} {node} {input} {value}", self.path.display()),
            report: edit.report,
            changed: edit.changed,
            churn,
        });
        self.compare.clear();
    }

    /// The same edit, from the pristine document, under all three strategies side by side.
    fn compare_strategies(&mut self, value: f64) {
        let (node, input) = self.target.clone();
        let mut out = vec![
            format!("{BOLD}same edit, three strategies, from the on-disk document{OFF}"),
            format!("{DIM}set-input {node} {input} {value}{OFF}"),
            String::new(),
        ];
        for strategy in Strategy::ALL {
            let edit = {
                let resolver = FsResolver::for_instrument(&self.path);
                set_input(
                    &self.pristine,
                    &node,
                    &input,
                    value,
                    Policy {
                        strategy,
                        guard_wires: self.guard_wires,
                    },
                    &Registry::builtin(),
                    &resolver,
                )
            };
            let churn = Churn::between(&self.pristine, &edit.document);
            let verdict = if !edit.changed {
                format!("{RED}rejected{OFF}")
            } else if churn.collateral() == 0 {
                format!("{GREEN}clean{OFF}")
            } else {
                format!(
                    "{YELLOW}{} lines of collateral churn{OFF}",
                    churn.collateral()
                )
            };
            out.push(format!("  {BOLD}{:<7}{OFF} {verdict}", label_of(strategy)));
            out.push(format!(
                "  {DIM}-{} / +{} lines · doc {} -> {} bytes{OFF}",
                churn.removed.len(),
                churn.added.len(),
                self.pristine.len(),
                edit.document.len()
            ));
            for d in edit.report.errors.iter().take(2) {
                out.push(format!("  {RED}{}{OFF}", fmt_diag(d)));
            }
            for line in churn.removed.iter().take(3) {
                out.push(format!("  {RED}- {}{OFF}", line.trim()));
            }
            for line in churn.added.iter().take(3) {
                out.push(format!("  {GREEN}+ {}{OFF}", line.trim()));
            }
            if churn.removed.len() > 3 {
                out.push(format!(
                    "  {DIM}… {} more removed, {} more added{OFF}",
                    churn.removed.len() - 3,
                    churn.added.len().saturating_sub(3)
                ));
            }
            out.push(String::new());
        }
        self.compare = out;
    }

    fn call(&self, json: &str, node: &str, input: &str, value: f64) -> Edit {
        let resolver = FsResolver::for_instrument(&self.path);
        set_input(
            json,
            node,
            input,
            value,
            Policy {
                strategy: self.strategy,
                guard_wires: self.guard_wires,
            },
            &Registry::builtin(),
            &resolver,
        )
    }

    /// Every `(node, input)` in the document that currently holds a numeric literal — the
    /// targets this slice of `set-input` can address.
    fn list_targets(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{BOLD}numeric-literal inputs in this document{OFF}"
        )];
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&self.current) else {
            return vec![format!("{RED}document is not valid JSON{OFF}")];
        };
        for node in doc["nodes"].as_array().into_iter().flatten() {
            let address = node["address"].as_str().unwrap_or("?");
            let ty = node["type"].as_str().unwrap_or("?");
            let literals: Vec<String> = node["inputs"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(_, v)| v.is_number())
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            if !literals.is_empty() {
                out.push(format!(
                    "  {BOLD}{address}{OFF} {DIM}({ty}){OFF}  {}",
                    literals.join("  ")
                ));
            }
        }
        out
    }

    fn render(&self) {
        print!("\x1b[2J\x1b[H");
        let (node, input) = &self.target;
        let live = literal_at(&self.current, node, input)
            .map(|v| v.to_string())
            .or_else(|| {
                set_input::wire_source(&self.current, node, input)
                    .map(|w| format!("{YELLOW}wired from {w}{OFF}"))
            })
            .unwrap_or_else(|| format!("{DIM}(unset — rides the descriptor default){OFF}"));

        println!(
            "{BOLD}reuben · set-input prototype{OFF}  {DIM}throwaway, issue #576 · in memory only{OFF}"
        );
        println!("{DIM}{}{OFF}", "─".repeat(78));
        println!(
            "{BOLD}document{OFF}  {}  {DIM}{} lines · {} bytes{OFF}",
            self.path.display(),
            self.current.lines().count(),
            self.current.len()
        );
        let drift = Churn::between(&self.pristine, &self.current);
        if !drift.removed.is_empty() || !drift.added.is_empty() {
            println!(
                "{BOLD}drift{OFF}     {DIM}vs on-disk:{OFF} -{} / +{} lines",
                drift.removed.len(),
                drift.added.len()
            );
        }
        println!(
            "{BOLD}strategy{OFF}  {}   {DIM}wire-guard{OFF} {}",
            self.strategy.label(),
            if self.guard_wires { "on" } else { "off" }
        );
        println!("{BOLD}target{OFF}    {node} . {input}   {DIM}currently{OFF} {live}");
        println!();

        match &self.last {
            None => println!("{DIM}no edit applied yet{OFF}"),
            Some(applied) => {
                println!("{BOLD}last call{OFF} {DIM}{}{OFF}", applied.call);
                let verdict = if applied.report.ok {
                    format!("{GREEN}ok{OFF}")
                } else {
                    format!("{RED}rejected — document unchanged{OFF}")
                };
                println!(
                    "{BOLD}report{OFF}    {verdict}  {DIM}{} error(s), {} warning(s){OFF}",
                    applied.report.errors.len(),
                    applied.report.warnings.len()
                );
                for d in applied.report.errors.iter().take(3) {
                    println!("          {RED}{}{OFF}", fmt_diag(d));
                }
                if applied.changed {
                    let collateral = applied.churn.collateral();
                    let verdict = if collateral == 0 {
                        format!("{GREEN}no collateral{OFF}")
                    } else {
                        format!("{YELLOW}{collateral} lines of collateral churn{OFF}")
                    };
                    println!(
                        "{BOLD}churn{OFF}     -{} / +{} lines   {verdict}",
                        applied.churn.removed.len(),
                        applied.churn.added.len()
                    );
                    for line in applied.churn.removed.iter().take(4) {
                        println!("          {RED}- {}{OFF}", line.trim());
                    }
                    for line in applied.churn.added.iter().take(4) {
                        println!("          {GREEN}+ {}{OFF}", line.trim());
                    }
                    if applied.churn.removed.len() > 4 {
                        println!(
                            "          {DIM}… {} more removed{OFF}",
                            applied.churn.removed.len() - 4
                        );
                    }
                }
                println!(
                    "{BOLD}yardstick{OFF} {DIM}model emits{OFF} {} chars {DIM}· whole-document \
                     re-emission would be{OFF} {} chars",
                    applied.call.len(),
                    self.current.len()
                );
            }
        }

        if !self.compare.is_empty() {
            println!();
            for line in &self.compare {
                println!("{line}");
            }
        }

        println!();
        println!("{DIM}{}{OFF}", "─".repeat(78));
        println!(
            "{BOLD}[t <node> <input>]{OFF} target   {BOLD}[v <value>]{OFF} set + apply   \
             {BOLD}[x <value>]{OFF} all 3 strategies"
        );
        println!(
            "{BOLD}[s]{OFF} cycle strategy   {BOLD}[g]{OFF} toggle wire-guard   {BOLD}[l]{OFF} list \
             targets   {BOLD}[r]{OFF} reset doc   {BOLD}[q]{OFF} quit"
        );
    }
}

fn label_of(strategy: Strategy) -> &'static str {
    strategy.label().split_whitespace().next().unwrap()
}

fn fmt_diag(d: &Diag) -> String {
    match (&d.node, &d.port) {
        (Some(n), Some(p)) => format!("{n}.{p}: {}", d.message),
        (Some(n), None) => format!("{n}: {}", d.message),
        _ => d.message.clone(),
    }
}

/// The literal currently sitting at `node.input`, read straight off the document text.
fn literal_at(json: &str, node: &str, input: &str) -> Option<serde_json::Value> {
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    doc["nodes"]
        .as_array()?
        .iter()
        .find(|n| n["address"].as_str() == Some(node))?
        .get("inputs")?
        .get(input)
        .filter(|v| v.is_number())
        .cloned()
}

/// A sensible opening target: the first node input holding a numeric literal.
fn first_numeric_input(json: &str) -> Option<(String, String)> {
    let doc: serde_json::Value = serde_json::from_str(json).ok()?;
    for node in doc["nodes"].as_array()? {
        let address = node["address"].as_str()?;
        for (name, value) in node
            .get("inputs")
            .and_then(|i| i.as_object())
            .into_iter()
            .flatten()
        {
            if value.is_number() {
                return Some((address.to_string(), name.clone()));
            }
        }
    }
    None
}
