//! The engine's agent-tool **contract roster** — the single source of truth for *which* contracts
//! the tool surface exposes, and in what order.
//!
//! This declares the roster *identity* — names and channel kind — and nothing else. Descriptions,
//! input/output schemas, and the tool bodies stay per-door (they are host-flavoured and, for the
//! MCP door, carry rmcp/schemars machinery reuben-core must never depend on). "Contracts live in
//! core": the roster is OS-free and depends on no engine or protocol type, so every
//! door can derive its name-set and count from [`CONTRACTS`] instead of hand-typing a parallel
//! literal. Adding a verb becomes one entry here rather than a roster edit in every door.

/// Which channel a contract is served over. Roster metadata only — it does not carry
/// the tool's behaviour, just how the door reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractKind {
    /// A pure introspection contract, answerable in-process with no live engine
    /// (`describe_operators`/`describe_instrument`/`validate`).
    Pure,
    /// An engine contract that reaches a running engine over the door's channel
    /// (`send`/`engine_status`/`swap`/`get_current_instrument`/`get_diagnostics`).
    Engine,
    /// A **document-manipulation** contract (#603): a pure, engine-free *mutator* over an
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
/// engine contracts. `scaffold_instrument` (#158, closes #146) joins the pure group as the
/// first-creation start move — a read-only producer of a guaranteed-valid minimal document. This
/// is the authority every door derives its advertised name-set and count from; the order here is
/// the order on the wire.
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
        name: "validate",
        kind: ContractKind::Pure,
    },
    Contract {
        name: "scaffold_instrument",
        kind: ContractKind::Pure,
    },
    Contract {
        name: "send",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "engine_status",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "swap",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "get_current_instrument",
        kind: ContractKind::Engine,
    },
    Contract {
        name: "get_diagnostics",
        kind: ContractKind::Engine,
    },
    // The document-manipulation vocabulary (#603): the closed set of engine-free mutators an agent
    // authors a document through, in the #611 group order (document · nodes · inputs · config ·
    // interface · resources). The existing-tool renames to the `verb_instrument_object` convention
    // are #604's job, so the read/engine names above keep their current spelling for now.
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
pub fn names() -> Vec<&'static str> {
    CONTRACTS.iter().map(|c| c.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_is_the_adr_0048_set_in_order() {
        // The roster identity: exactly these names, in this exact order, with this kind split.
        // A door derives its wire surface from CONTRACTS, so this pins what every door advertises.
        assert_eq!(
            names(),
            [
                // read/introspect + live engine (the pre-#603 roster, names unchanged; the
                // `verb_instrument_object` renames land in #604).
                "describe_operators",
                "describe_instrument",
                "validate",
                "scaffold_instrument",
                "send",
                "engine_status",
                "swap",
                "get_current_instrument",
                "get_diagnostics",
                // the #603 document-manipulation vocabulary, in #611 group order.
                "new_instrument",
                "set_instrument_name",
                "set_instrument_description",
                "add_instrument_node",
                "remove_instrument_node",
                "rename_instrument_node",
                "set_instrument_node_description",
                "set_instrument_input",
                "wire_instrument_input",
                "unwire_instrument_input",
                "set_instrument_constant",
                "add_instrument_interface_input",
                "add_instrument_interface_output",
                "remove_instrument_interface_input",
                "remove_instrument_interface_output",
                "set_instrument_interface_input_meta",
                "set_instrument_interface_output_meta",
                "add_instrument_resource",
                "remove_instrument_resource",
            ]
        );
        // The kind split is a partition: four read-only Pure (`scaffold_instrument` added by #158),
        // five Engine, and the nineteen #603 Document mutators.
        assert_eq!(
            CONTRACTS
                .iter()
                .filter(|c| c.kind == ContractKind::Pure)
                .count(),
            4
        );
        assert_eq!(
            CONTRACTS
                .iter()
                .filter(|c| c.kind == ContractKind::Engine)
                .count(),
            5
        );
        assert_eq!(
            CONTRACTS
                .iter()
                .filter(|c| c.kind == ContractKind::Document)
                .count(),
            19
        );
        // Concrete, not tautological: the roster is exactly 28 contracts.
        assert_eq!(CONTRACTS.len(), 28);
    }
}
