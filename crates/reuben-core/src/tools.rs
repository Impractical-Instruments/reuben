//! The engine's agent-tool **contract roster** — the single source of truth for *which* contracts
//! the ADR-0048 tool surface exposes, and in what order (#157, Wave 1 of #156).
//!
//! This declares the roster *identity* — names and channel kind — and nothing else. Descriptions,
//! input/output schemas, and the tool bodies stay per-door (they are host-flavoured and, for the
//! MCP door, carry rmcp/schemars machinery reuben-core must never depend on). "Contracts live in
//! core" (ADR-0052 §5): the roster is OS-free and depends on no engine or protocol type, so every
//! door can derive its name-set and count from [`CONTRACTS`] instead of hand-typing a parallel
//! literal. Adding a verb becomes one entry here rather than a roster edit in every door.

/// Which channel a contract is served over (ADR-0048 §1). Roster metadata only — it does not carry
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
/// Names only — the description and schema are the door's business (ADR-0052 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Contract {
    /// The exact spelling advertised over the tool surface (e.g. `tools/list`).
    pub name: &'static str,
    /// Whether the contract is pure or reaches the engine.
    pub kind: ContractKind,
}

/// The ADR-0048 §1 contract roster, in the ADR's exact order: the three pure contracts first, then
/// the five engine contracts. This is the authority every door derives its advertised name-set and
/// count from; the order here is the order on the wire.
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
                "send",
                "engine_status",
                "swap",
                "get_current_instrument",
                "get_diagnostics",
            ]
        );
        // The three-pure / five-engine split (ADR-0048 §1), and it is a partition (no other kind).
        assert_eq!(
            CONTRACTS
                .iter()
                .filter(|c| c.kind == ContractKind::Pure)
                .count(),
            3
        );
        assert_eq!(
            CONTRACTS
                .iter()
                .filter(|c| c.kind == ContractKind::Engine)
                .count(),
            5
        );
        // Concrete, not tautological: the ADR-0048 roster is exactly eight contracts.
        assert_eq!(CONTRACTS.len(), 8);
    }
}
