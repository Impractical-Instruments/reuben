//! The **contract roster** — the single source of truth for *which* contracts the tool surface
//! exposes, and in what order.
//!
//! Roster *identity* only: names, channel kind, and the one sentence each is advertised by. Adding
//! a verb is one entry here rather than a roster edit in every door. Output schemas derive from the
//! window's own result types; a door still owns its transport and how it carries the sentence.
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
/// interprets, and read back as a projection — see rules: agent-mcp.
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

/// Every roster contract's name paired with the sentence it is advertised by — what a door
/// iterates to advertise the whole set without naming each verb.
///
/// A lookup over [`CONTRACTS`], which is the authority on both which verbs exist and what order
/// they advertise in; neither is decided here. The sentences themselves live beside the types they
/// describe, in the authoring and engine halves. Not every window verb appears: `describe_boundary`
/// answers a door that reads a document structurally rather than a roster entry, so it has no
/// advertised sentence to own.
pub const DESCRIPTIONS: &[(&str, &str)] = &[
    ("describe_operators", authoring_prose::DESCRIBE_OPERATORS),
    ("describe_instrument", authoring_prose::DESCRIBE_INSTRUMENT),
    ("validate_instrument", authoring_prose::VALIDATE_INSTRUMENT),
    ("send_live_controls", engine_prose::SEND_LIVE_CONTROLS),
    ("get_engine_status", engine_prose::GET_ENGINE_STATUS),
    ("swap_instrument", engine_prose::SWAP_INSTRUMENT),
    (
        "get_current_instrument",
        engine_prose::GET_CURRENT_INSTRUMENT,
    ),
    (
        "get_engine_diagnostics",
        engine_prose::GET_ENGINE_DIAGNOSTICS,
    ),
    ("new_instrument", authoring_prose::NEW_INSTRUMENT),
    ("set_instrument_name", authoring_prose::SET_INSTRUMENT_NAME),
    (
        "set_instrument_description",
        authoring_prose::SET_INSTRUMENT_DESCRIPTION,
    ),
    ("add_instrument_node", authoring_prose::ADD_INSTRUMENT_NODE),
    (
        "remove_instrument_node",
        authoring_prose::REMOVE_INSTRUMENT_NODE,
    ),
    (
        "rename_instrument_node",
        authoring_prose::RENAME_INSTRUMENT_NODE,
    ),
    (
        "set_instrument_node_description",
        authoring_prose::SET_INSTRUMENT_NODE_DESCRIPTION,
    ),
    (
        "set_instrument_input",
        authoring_prose::SET_INSTRUMENT_INPUT,
    ),
    (
        "wire_instrument_input",
        authoring_prose::WIRE_INSTRUMENT_INPUT,
    ),
    (
        "unwire_instrument_input",
        authoring_prose::UNWIRE_INSTRUMENT_INPUT,
    ),
    (
        "set_instrument_constant",
        authoring_prose::SET_INSTRUMENT_CONSTANT,
    ),
    (
        "add_instrument_interface_input",
        authoring_prose::ADD_INSTRUMENT_INTERFACE_INPUT,
    ),
    (
        "add_instrument_interface_output",
        authoring_prose::ADD_INSTRUMENT_INTERFACE_OUTPUT,
    ),
    (
        "remove_instrument_interface_input",
        authoring_prose::REMOVE_INSTRUMENT_INTERFACE_INPUT,
    ),
    (
        "remove_instrument_interface_output",
        authoring_prose::REMOVE_INSTRUMENT_INTERFACE_OUTPUT,
    ),
    (
        "set_instrument_interface_input_meta",
        authoring_prose::SET_INSTRUMENT_INTERFACE_INPUT_META,
    ),
    (
        "set_instrument_interface_output_meta",
        authoring_prose::SET_INSTRUMENT_INTERFACE_OUTPUT_META,
    ),
    (
        "add_instrument_resource",
        authoring_prose::ADD_INSTRUMENT_RESOURCE,
    ),
    (
        "remove_instrument_resource",
        authoring_prose::REMOVE_INSTRUMENT_RESOURCE,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every contract has a sentence, and every sentence names a contract. The door refuses to
    /// start otherwise, but it can only say so at runtime — this says it at build time, and names
    /// the verb rather than the door that failed to come up.
    #[test]
    fn the_sentence_table_covers_the_roster_exactly() {
        let roster: std::collections::BTreeSet<&str> = CONTRACTS.iter().map(|c| c.name).collect();
        let described: std::collections::BTreeSet<&str> =
            DESCRIPTIONS.iter().map(|(name, _)| *name).collect();
        assert_eq!(roster, described);
    }
}
