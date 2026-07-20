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
                "describe_operators",
                "describe_instrument",
                "validate",
                "scaffold_instrument",
                "send",
                "engine_status",
                "swap",
                "get_current_instrument",
                "get_diagnostics",
            ]
        );
        // The four-pure / five-engine split (`scaffold_instrument` added by #158), and
        // it is a partition (no other kind).
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
        // Concrete, not tautological: the roster is exactly nine contracts.
        assert_eq!(CONTRACTS.len(), 9);
    }
}
