//! The engine's agent-tool **contract roster** — the single source of truth for *which* contracts
//! the tool surface exposes, and in what order.
//!
//! Roster *identity* only: names and channel kind. Descriptions and the tool bodies stay per-door;
//! output schemas derive from the contract types behind core's optional `schemars` feature. Adding
//! a verb is one entry here rather than a roster edit in every door.
//!
//! see rules: agent-mcp

/// Which channel a contract is served over. Roster metadata only — it does not carry
/// the tool's behaviour, just how the door reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    /// A pure introspection contract, answerable in-process with no live engine
    /// (`describe_operators`/`describe_instrument`/`validate_instrument`).
    Pure,
    /// An engine contract that reaches a running engine over the door's channel
    /// (`send_live_controls`/`get_engine_status`/`swap_instrument`/`get_current_instrument`/
    /// `get_engine_diagnostics`).
    Engine,
    /// A **document-manipulation** contract: a pure, engine-free *mutator* over an
    /// instrument document through the resolver seam — read, apply one surgical edit, re-validate
    /// the whole document, write iff valid ([`crate::edit`]). Distinct from [`Pure`](Self::Pure),
    /// which is read-only introspection: a door hosts both in-process, but only these write.
    Document,
}

/// One entry in the contract roster: the exact name advertised on the wire, plus its channel kind.
/// Names only — the description and schema are the door's business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contract {
    /// The exact spelling advertised over the tool surface (e.g. `tools/list`).
    pub name: &'static str,
    /// Whether the contract is pure or reaches the engine.
    pub kind: ContractKind,
}

/// The contract roster, in canonical wire order: the pure contracts first, then the
/// engine contracts, then the document vocabulary. This is the authority every door derives its
/// advertised name-set and count from; the order here is the order on the wire.
///
/// Every name follows the `verb_instrument_object` convention, and no contract carries an
/// instrument document by value: a document is named by an opaque `source` the door's resolver
/// interprets, and read back as a [`projection`](crate::projection) — see rules: agent-mcp.
/// `scaffold_instrument` retired here: it returned a seed *by value*, and
/// [`crate::edit::new_instrument`] lands the same seed at a source instead.
pub const CONTRACTS: &[Contract] = &[
    Contract {
        name: "describe_operators",
        kind: ContractKind::Pure,
    },
    Contract {
        name: "describe_instrument",
        kind: ContractKind::Pure,
    },
    Contract {
        name: "validate_instrument",
        kind: ContractKind::Pure,
    },
    Contract {
        name: "send_live_controls",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "get_engine_status",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "swap_instrument",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "get_current_instrument",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "get_engine_diagnostics",
        kind: ContractKind::Engine,
    },
    // The document-manipulation vocabulary: the closed set of engine-free mutators an agent
    // authors a document through, grouped document · nodes · inputs · config · interface ·
    // resources.
    Contract {
        name: "new_instrument",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_name",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_description",
        kind: ContractKind::Document,
    },
    Contract {
        name: "add_instrument_node",
        kind: ContractKind::Document,
    },
    Contract {
        name: "remove_instrument_node",
        kind: ContractKind::Document,
    },
    Contract {
        name: "rename_instrument_node",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_node_description",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_input",
        kind: ContractKind::Document,
    },
    Contract {
        name: "wire_instrument_input",
        kind: ContractKind::Document,
    },
    Contract {
        name: "unwire_instrument_input",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_constant",
        kind: ContractKind::Document,
    },
    Contract {
        name: "add_instrument_interface_input",
        kind: ContractKind::Document,
    },
    Contract {
        name: "add_instrument_interface_output",
        kind: ContractKind::Document,
    },
    Contract {
        name: "remove_instrument_interface_input",
        kind: ContractKind::Document,
    },
    Contract {
        name: "remove_instrument_interface_output",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_interface_input_meta",
        kind: ContractKind::Document,
    },
    Contract {
        name: "set_instrument_interface_output_meta",
        kind: ContractKind::Document,
    },
    Contract {
        name: "add_instrument_resource",
        kind: ContractKind::Document,
    },
    Contract {
        name: "remove_instrument_resource",
        kind: ContractKind::Document,
    },
];

/// The roster's contract names, in [`CONTRACTS`] order — the ordered name-set a door advertises.
/// A door builds its wire surface from this rather than a hand-typed list.
///
/// The roster identity is verified end-to-end where it matters — `reuben-mcp`'s
/// `advertises_the_declared_roster_over_stdio` asserts the real `tools/list` wire surface equals
/// this derivation — so there is no hand-maintained literal duplicate of the names here to drift.
pub fn names() -> Vec<&'static str> {
    CONTRACTS.iter().map(|c| c.name).collect()
}
