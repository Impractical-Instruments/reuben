//! The **contract roster** — the single source of truth for *which* contracts the tool surface
//! exposes, and in what order.
//!
//! Roster *identity* only: names, channel kind, and the one sentence each is advertised by — one
//! [`Contract`] per verb, so adding a verb is one entry here rather than a roster edit in every
//! door. Output schemas derive from the window's own result types; a door still owns its transport
//! and how it carries the sentence.
//!
//! Not every window verb is a roster entry: `describe_boundary` answers a door that reads a
//! document structurally rather than a tool a model calls, so it has no advertised sentence to own.
//!
//! see rules: agent-mcp

use crate::authoring::prose as authoring_prose;
use crate::engine::prose as engine_prose;

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
    /// the whole document, write iff valid. Distinct from [`Pure`](Self::Pure), which is read-only
    /// introspection: a door hosts both in-process, but only these write.
    Document,
}

/// One entry in the contract roster: the exact name advertised on the wire, its channel kind, and
/// the one sentence it is advertised by. The input schema is still the door's business.
///
/// The sentence is a **field rather than a second table keyed by name**: a lookup table makes
/// "every contract has exactly one sentence" a runtime property something has to check, and it was
/// being checked three times over. Here a contract without a sentence does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contract {
    /// The exact spelling advertised over the tool surface (e.g. `tools/list`).
    pub name: &'static str,
    /// Whether the contract is pure or reaches the engine.
    pub kind: ContractKind,
    /// The sentence a door advertises this contract by. Lives beside the argument and result types
    /// it describes, in the authoring and engine halves; named here so the roster is one entry per
    /// verb.
    pub description: &'static str,
}

/// The contract roster, in canonical wire order: the pure contracts first, then the
/// engine contracts, then the document vocabulary. This is the authority every door derives its
/// advertised name-set and count from; the order here is the order on the wire.
///
/// Every name follows the `verb_instrument_object` convention, and no contract carries an
/// instrument document by value: a document is named by an opaque `source` the door's resolver
/// interprets, and read back as a projection — see rules: agent-mcp.
pub const CONTRACTS: &[Contract] = &[
    Contract {
        name: "describe_operators",
        kind: ContractKind::Pure,
        description: authoring_prose::DESCRIBE_OPERATORS,
    },
    Contract {
        name: "describe_instrument",
        kind: ContractKind::Pure,
        description: authoring_prose::DESCRIBE_INSTRUMENT,
    },
    Contract {
        name: "validate_instrument",
        kind: ContractKind::Pure,
        description: authoring_prose::VALIDATE_INSTRUMENT,
    },
    Contract {
        name: "send_live_controls",
        kind: ContractKind::Engine,
        description: engine_prose::SEND_LIVE_CONTROLS,
    },
    Contract {
        name: "get_engine_status",
        kind: ContractKind::Engine,
        description: engine_prose::GET_ENGINE_STATUS,
    },
    Contract {
        name: "swap_instrument",
        kind: ContractKind::Engine,
        description: engine_prose::SWAP_INSTRUMENT,
    },
    Contract {
        name: "get_current_instrument",
        kind: ContractKind::Engine,
        description: engine_prose::GET_CURRENT_INSTRUMENT,
    },
    Contract {
        name: "get_engine_diagnostics",
        kind: ContractKind::Engine,
        description: engine_prose::GET_ENGINE_DIAGNOSTICS,
    },
    // The document-manipulation vocabulary: the closed set of engine-free mutators an agent
    // authors a document through, grouped document · nodes · inputs · config · interface ·
    // resources.
    Contract {
        name: "new_instrument",
        kind: ContractKind::Document,
        description: authoring_prose::NEW_INSTRUMENT,
    },
    Contract {
        name: "set_instrument_name",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_NAME,
    },
    Contract {
        name: "set_instrument_description",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_DESCRIPTION,
    },
    Contract {
        name: "add_instrument_node",
        kind: ContractKind::Document,
        description: authoring_prose::ADD_INSTRUMENT_NODE,
    },
    Contract {
        name: "remove_instrument_node",
        kind: ContractKind::Document,
        description: authoring_prose::REMOVE_INSTRUMENT_NODE,
    },
    Contract {
        name: "rename_instrument_node",
        kind: ContractKind::Document,
        description: authoring_prose::RENAME_INSTRUMENT_NODE,
    },
    Contract {
        name: "set_instrument_node_description",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_NODE_DESCRIPTION,
    },
    Contract {
        name: "set_instrument_input",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_INPUT,
    },
    Contract {
        name: "wire_instrument_input",
        kind: ContractKind::Document,
        description: authoring_prose::WIRE_INSTRUMENT_INPUT,
    },
    Contract {
        name: "unwire_instrument_input",
        kind: ContractKind::Document,
        description: authoring_prose::UNWIRE_INSTRUMENT_INPUT,
    },
    Contract {
        name: "set_instrument_constant",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_CONSTANT,
    },
    Contract {
        name: "add_instrument_interface_input",
        kind: ContractKind::Document,
        description: authoring_prose::ADD_INSTRUMENT_INTERFACE_INPUT,
    },
    Contract {
        name: "add_instrument_interface_output",
        kind: ContractKind::Document,
        description: authoring_prose::ADD_INSTRUMENT_INTERFACE_OUTPUT,
    },
    Contract {
        name: "remove_instrument_interface_input",
        kind: ContractKind::Document,
        description: authoring_prose::REMOVE_INSTRUMENT_INTERFACE_INPUT,
    },
    Contract {
        name: "remove_instrument_interface_output",
        kind: ContractKind::Document,
        description: authoring_prose::REMOVE_INSTRUMENT_INTERFACE_OUTPUT,
    },
    Contract {
        name: "set_instrument_interface_input_meta",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_INTERFACE_INPUT_META,
    },
    Contract {
        name: "set_instrument_interface_output_meta",
        kind: ContractKind::Document,
        description: authoring_prose::SET_INSTRUMENT_INTERFACE_OUTPUT_META,
    },
    Contract {
        name: "add_instrument_resource",
        kind: ContractKind::Document,
        description: authoring_prose::ADD_INSTRUMENT_RESOURCE,
    },
    Contract {
        name: "remove_instrument_resource",
        kind: ContractKind::Document,
        description: authoring_prose::REMOVE_INSTRUMENT_RESOURCE,
    },
];

/// The roster's contract names, in [`CONTRACTS`] order — the ordered name-set a door advertises.
/// A door builds its wire surface from this rather than a hand-typed list.
///
/// A door is held to it at **construction**, not by a test observing the wire afterwards: the MCP
/// door stamps each contract's sentence onto its built router and refuses to start unless the two
/// name-sets match exactly, which is what keeps this derivation from needing a duplicate to check
/// it against. A door that cannot make the same refusal owes itself the equivalent check.
pub fn names() -> Vec<&'static str> {
    CONTRACTS.iter().map(|c| c.name).collect()
}
