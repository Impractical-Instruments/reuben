//! The **contract roster** — the single source of truth for *which* contracts the tool surface
//! exposes, and in what order.
//!
//! Roster *identity* only: names, channel kind, and the one sentence each is advertised by — one
//! [`Contract`] per verb, so adding a verb is one entry here rather than a roster edit in every
//! door. Output schemas derive from the window's own result types; a door still owns its transport
//! and how it carries the sentence.
//!
//! The same entry also exposes the verb as a **symbol** in [`mod@names`], which is how a door
//! names a contract it cannot reach by type: the document verbs are already compile-coupled
//! through the argument type each one takes, but a read or engine arm projected as a JSON literal
//! is coupled to nothing, and a contract removed here would reach that door as dead plumbing
//! behind a name it no longer advertises.
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

/// Declare the roster once. Each entry is one contract, in four parts: the **symbol** a door names
/// it by, the exact spelling that symbol carries onto the wire, the channel kind, and the sentence
/// it is advertised by. The one invocation below expands to all of [`CONTRACTS`], [`mod@names`]
/// and the test table that checks the two halves of an entry against each other.
macro_rules! roster {
    ($($symbol:ident = $wire:literal, $kind:ident, $description:expr;)*) => {
        /// One `&'static str` per [`CONTRACTS`](crate::tools::CONTRACTS) entry, in roster order:
        /// the symbol a door names a contract by where it would otherwise write a string literal.
        ///
        /// A door writes `names::SWAP_INSTRUMENT` in the projection or the dispatch arm that
        /// spells `"swap_instrument"` today, and a contract removed upstream is `E0425: cannot
        /// find value` at that site — the compile error the roster owes its consumers, in place of
        /// dead plumbing behind a name nobody advertises any more.
        ///
        /// Pairs with `names()`: this module names one contract, that function hands back the
        /// ordered set.
        pub mod names {
            $(pub const $symbol: &str = $wire;)*
        }

        /// The contract roster, in canonical wire order: the pure contracts first, then the
        /// engine contracts, then the document vocabulary. This is the authority every door
        /// derives its advertised name-set and count from; the order here is the order on the
        /// wire.
        ///
        /// Every name follows the `verb_instrument_object` convention, and no contract carries an
        /// instrument document by value: a document is named by an opaque `source` the door's
        /// resolver interprets, and read back as a projection — see rules: agent-mcp.
        pub const CONTRACTS: &[Contract] = &[$(
            Contract {
                name: names::$symbol,
                kind: ContractKind::$kind,
                description: $description,
            },
        )*];

        /// Each entry's symbol, as text, beside the spelling it carries. The roster itself keeps
        /// only the spelling, so this is the one place both halves of an entry are values.
        #[cfg(test)]
        const SYMBOLS_AND_SPELLINGS: &[(&str, &str)] =
            &[$((stringify!($symbol), names::$symbol),)*];
    };
}

roster! {
    DESCRIBE_OPERATORS = "describe_operators", Pure, authoring_prose::DESCRIBE_OPERATORS;
    DESCRIBE_INSTRUMENT = "describe_instrument", Pure, authoring_prose::DESCRIBE_INSTRUMENT;
    VALIDATE_INSTRUMENT = "validate_instrument", Pure, authoring_prose::VALIDATE_INSTRUMENT;

    SEND_LIVE_CONTROLS = "send_live_controls", Engine, engine_prose::SEND_LIVE_CONTROLS;
    GET_ENGINE_STATUS = "get_engine_status", Engine, engine_prose::GET_ENGINE_STATUS;
    SWAP_INSTRUMENT = "swap_instrument", Engine, engine_prose::SWAP_INSTRUMENT;
    GET_CURRENT_INSTRUMENT = "get_current_instrument", Engine, engine_prose::GET_CURRENT_INSTRUMENT;
    GET_ENGINE_DIAGNOSTICS = "get_engine_diagnostics", Engine, engine_prose::GET_ENGINE_DIAGNOSTICS;

    // The document-manipulation vocabulary: the closed set of engine-free mutators an agent
    // authors a document through, grouped document · nodes · inputs · config · interface ·
    // resources.
    NEW_INSTRUMENT = "new_instrument", Document, authoring_prose::NEW_INSTRUMENT;
    SET_INSTRUMENT_NAME = "set_instrument_name", Document, authoring_prose::SET_INSTRUMENT_NAME;
    SET_INSTRUMENT_DESCRIPTION = "set_instrument_description", Document,
        authoring_prose::SET_INSTRUMENT_DESCRIPTION;

    ADD_INSTRUMENT_NODE = "add_instrument_node", Document, authoring_prose::ADD_INSTRUMENT_NODE;
    REMOVE_INSTRUMENT_NODE = "remove_instrument_node", Document,
        authoring_prose::REMOVE_INSTRUMENT_NODE;
    RENAME_INSTRUMENT_NODE = "rename_instrument_node", Document,
        authoring_prose::RENAME_INSTRUMENT_NODE;
    SET_INSTRUMENT_NODE_DESCRIPTION = "set_instrument_node_description", Document,
        authoring_prose::SET_INSTRUMENT_NODE_DESCRIPTION;

    SET_INSTRUMENT_INPUT = "set_instrument_input", Document, authoring_prose::SET_INSTRUMENT_INPUT;
    SET_INSTRUMENT_INPUTS_BY_INTENT = "set_instrument_inputs_by_intent", Document,
        authoring_prose::SET_INSTRUMENT_INPUTS_BY_INTENT;
    WIRE_INSTRUMENT_INPUT = "wire_instrument_input", Document,
        authoring_prose::WIRE_INSTRUMENT_INPUT;
    UNWIRE_INSTRUMENT_INPUT = "unwire_instrument_input", Document,
        authoring_prose::UNWIRE_INSTRUMENT_INPUT;

    SET_INSTRUMENT_CONSTANT = "set_instrument_constant", Document,
        authoring_prose::SET_INSTRUMENT_CONSTANT;

    ADD_INSTRUMENT_INTERFACE_INPUT = "add_instrument_interface_input", Document,
        authoring_prose::ADD_INSTRUMENT_INTERFACE_INPUT;
    ADD_INSTRUMENT_INTERFACE_OUTPUT = "add_instrument_interface_output", Document,
        authoring_prose::ADD_INSTRUMENT_INTERFACE_OUTPUT;
    REMOVE_INSTRUMENT_INTERFACE_INPUT = "remove_instrument_interface_input", Document,
        authoring_prose::REMOVE_INSTRUMENT_INTERFACE_INPUT;
    REMOVE_INSTRUMENT_INTERFACE_OUTPUT = "remove_instrument_interface_output", Document,
        authoring_prose::REMOVE_INSTRUMENT_INTERFACE_OUTPUT;
    SET_INSTRUMENT_INTERFACE_INPUT_META = "set_instrument_interface_input_meta", Document,
        authoring_prose::SET_INSTRUMENT_INTERFACE_INPUT_META;
    SET_INSTRUMENT_INTERFACE_OUTPUT_META = "set_instrument_interface_output_meta", Document,
        authoring_prose::SET_INSTRUMENT_INTERFACE_OUTPUT_META;

    ADD_INSTRUMENT_RESOURCE = "add_instrument_resource", Document,
        authoring_prose::ADD_INSTRUMENT_RESOURCE;
    REMOVE_INSTRUMENT_RESOURCE = "remove_instrument_resource", Document,
        authoring_prose::REMOVE_INSTRUMENT_RESOURCE;
}

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

/// The `verb_` openings a roster name is built from — the first half of the
/// `verb_instrument_object` convention, widened by the verbs that name something other than an
/// instrument. What makes a token in prose *read* as a tool a model can call.
///
/// Parity: this cannot be derived from [`CONTRACTS`], and deriving it would be worse than useless.
/// Seven of these twelve are carried by exactly one contract, so a derived list would lose the
/// opening at the moment that contract left the roster — un-shaping the very token left dangling in
/// a sibling's sentence, and passing the check whose whole job is to fail there. The list has to
/// outlive the entry to catch its removal.
const VERB_PREFIXES: &[&str] = &[
    "add_",
    "describe_",
    "get_",
    "new_",
    "remove_",
    "rename_",
    "send_",
    "set_",
    "swap_",
    "unwire_",
    "validate_",
    "wire_",
];

/// Names that read as a tool and are not one, so prose may name them with no contract behind them.
/// `describe_boundary` is the window verb answering a door that reads a document structurally
/// rather than a tool a model calls — see the module header.
///
/// Kept as short as it can be. Prose that names something a model cannot call is usually a defect
/// the scan has just found, not an exception it needs: an entry here silences the one check that
/// would have caught it.
const UNADVERTISED_VERBS: &[&str] = &["describe_boundary"];

/// The verb names in `text` that no contract serves — empty for prose that only sends a model
/// somewhere it can actually go.
///
/// Model-facing prose names sibling verbs constantly ("unwire it first", "call `new_instrument`"),
/// and prose held in a `const &str` cannot interpolate a symbol for one, because `concat!` takes
/// literals only. So a door that writes such a sentence holds it to the roster with this instead:
/// the coupling is a test rather than a type, but it fails on the same event — a contract leaving
/// the roster while live prose goes on pointing a model at it.
///
/// A token is *verb-shaped* if it opens with one of the `verb_` forms a roster name is built from.
/// Anything else in the prose is invisible here, which is the intended blind spot: this answers
/// "does this sentence send a model to a verb nobody serves", not "is every word in it a name".
///
/// The shape is broader than the roster, so a name of the door's own that happens to wear it — an
/// operator type like `add_f`, a host verb this window never hears of — comes back as unserved. A
/// door with such names filters them out of the returned list; only the two that belong to the
/// window itself are excluded here.
pub fn unserved_verbs(text: &str) -> Vec<&str> {
    text.split(|c: char| !(c.is_ascii_lowercase() || c == '_'))
        .filter(|token| {
            VERB_PREFIXES.iter().any(|prefix| {
                token
                    .strip_prefix(prefix)
                    .is_some_and(|rest| !rest.is_empty())
            })
        })
        .filter(|token| {
            !CONTRACTS.iter().any(|c| c.name == *token) && !UNADVERTISED_VERBS.contains(token)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parity: one token per entry is buildable — `stringify!` derives the spelling from the
    /// symbol — but only by naming the consts in the case of the wire, and a roster of
    /// `non_upper_case_globals` reads as a set of variables at every door that imports it. The
    /// second token buys the casing back, and this holds the pair to the convention a door reads a
    /// symbol by.
    #[test]
    fn a_symbol_is_the_upper_case_of_the_name_it_carries() {
        for (symbol, spelling) in SYMBOLS_AND_SPELLINGS {
            assert_eq!(
                *symbol,
                spelling.to_ascii_uppercase(),
                "the roster entry for `{spelling}` names a symbol that does not spell it"
            );
        }
    }

    /// The sentences are the roster's own prose, so they are the first thing held to it.
    #[test]
    fn a_sentence_names_only_verbs_the_roster_still_serves() {
        for contract in CONTRACTS {
            assert_eq!(
                unserved_verbs(contract.description),
                Vec::<&str>::new(),
                "`{}`'s sentence sends a model to a verb no contract serves; if the name is not a \
                 tool a model can call, it belongs in UNADVERTISED_VERBS",
                contract.name
            );
        }
    }

    /// [`unserved_verbs`] can only see a token whose opening it recognises, so a roster name built
    /// from an opening [`VERB_PREFIXES`] does not carry would be invisible to it — and so would
    /// every dangling reference to that name. Adding such a verb has to fail *here*, when the list
    /// can still be widened, rather than silently narrowing the check for good.
    #[test]
    fn every_roster_name_is_shaped_like_a_verb_the_scan_can_see() {
        for contract in CONTRACTS {
            assert!(
                VERB_PREFIXES
                    .iter()
                    .any(|prefix| contract.name.starts_with(prefix)),
                "`{}` opens with a verb form VERB_PREFIXES does not list, so prose naming it \
                 would go unscanned",
                contract.name
            );
        }
    }
}
