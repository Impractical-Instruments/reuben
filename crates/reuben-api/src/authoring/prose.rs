//! The one sentence each authoring verb is advertised by — the tool-level `description` a door
//! puts in front of a model.
//!
//! What a door still decides is how to *carry* it — MCP stamps these onto its tool roster, a
//! generated wrapper writes them into its manifest — and a door whose surface is not the roster
//! (the native CLI's five merged subcommands) advertises its own help instead.
//! see rules: agent-mcp
//!
//! Every string here ships to a model, under the same rule the argument and result docs follow: no
//! rustdoc link syntax, no issue numbers, no crate paths — a model can resolve none of them. Notes
//! for humans go in `//` comments. Guarded over the real advertised surface by
//! `advertised_prose_is_model_facing`. see rules: code-as-grounding

// --- pure reads -----------------------------------------------------------------------------------

pub const DESCRIBE_OPERATORS: &str =
    "List the registered operators and their ports/params, optionally filtered by name. \
     Set compact:true for one generated signature line per operator — \
     name(inputs; config: constants; res: resource-slots) -> outputs, each port as \
     name:kind with enum [variants], unit, exp for an exponential curve, lo..hi, =default \
     — instead of full port objects; the full mode stays the zoom for port detail.";

pub const DESCRIBE_INSTRUMENT: &str =
    "Read an instrument document's structure: `index` (every node, one line each — the default), \
     `nodes` (a node's inputs, wire sources, config and consumers), `pipes` (the interface \
     pipes with ranges and curves), `resources`, or `boundary` (the face a host sees when \
     nesting it). Narrow `nodes`/`pipes` with `select` (addresses or pipe names; `/` is the \
     document itself) or `type`. This is how you read a document — never open the file.";

pub const VALIDATE_INSTRUMENT: &str =
    "Validate an instrument document (load + instantiate); returns a report of errors and warnings. \
     The single authority on whether a document is legal — every document verb re-validates \
     through it before writing.";

// --- document verbs -------------------------------------------------------------------------------

pub const NEW_INSTRUMENT: &str =
    "Create a new valid minimal instrument document at `source` and write it. The from-scratch \
     start move; refuses to overwrite an existing document. Then edit it with the other \
     document tools and swap it.";

pub const SET_INSTRUMENT_NAME: &str = "Set the instrument's top-level name.";

pub const SET_INSTRUMENT_DESCRIPTION: &str =
    "Set (or, omitting `description`, clear) the instrument's note.";

pub const ADD_INSTRUMENT_NODE: &str =
    "Add a node in one call: required `address` and `type`, plus optional inputs (literal or \
     wire-ref), config constants, description, and a sample/voice/patch resource id. Atomic — \
     a wire to a missing source or a duplicate address rejects the whole call.";

pub const REMOVE_INSTRUMENT_NODE: &str =
    "Remove a node. Cascades: auto-unwires every consumer wired from it and drops every \
     interface output fed from it, reporting exactly what it broke — no unwire-first dance.";

pub const RENAME_INSTRUMENT_NODE: &str =
    "Rename a node from one address to another (which must be free), rewiring every consumer \
     to the new address and reporting each rewire.";

pub const SET_INSTRUMENT_NODE_DESCRIPTION: &str =
    "Set (or, omitting `description`, clear) a node's note.";

pub const SET_INSTRUMENT_INPUT: &str =
    "Set an input to a literal value: a number, or an enum symbol string. The one-value \
     point-edit — no re-emitting the whole document. An interface input pipe is a node too: \
     address it `/name`, input `in`. Echoes from -> to. Refuses an input that is already wired: \
     unwire_instrument_input first, or wire_instrument_input to re-point it.";

pub const WIRE_INSTRUMENT_INPUT: &str =
    "Wire a node input from a source port: `from` is `/node.port`, or `/node` for a sole-output \
     source. This document's own interface input pipes are fed from outside it, so they cannot be \
     wired — you wire *from* one.";

pub const UNWIRE_INSTRUMENT_INPUT: &str =
    "Clear a node input, reverting it to the operator's descriptor default.";

pub const SET_INSTRUMENT_CONSTANT: &str =
    "Set an instantiate-time constant on a node (a plan-time `config` value like a Voicer's \
     `voices`). Echoes the change (from -> to), not the node.";

pub const ADD_INSTRUMENT_INTERFACE_INPUT: &str =
    "Add a boundary input pipe: a declared-type input that mints an address `/name` internal \
     nodes consume from, with optional channel, value, min/max, curve (lin/exp), and unit.";

pub const ADD_INSTRUMENT_INTERFACE_OUTPUT: &str =
    "Add a master-tap output pipe fed from an internal port (`from` = `/node.port` or `/node`), \
     with optional channel, min/max, and unit.";

pub const REMOVE_INSTRUMENT_INTERFACE_INPUT: &str = "Remove a boundary input pipe by name.";

pub const REMOVE_INSTRUMENT_INTERFACE_OUTPUT: &str = "Remove a master-tap output pipe by name.";

pub const SET_INSTRUMENT_INTERFACE_INPUT_META: &str =
    "Update an input pipe's quantity contract (channel, min/max, curve lin/exp, unit); each \
     provided field is written, omitted fields are unchanged. Not its value: that is \
     set_instrument_input on `/name`, input `in`.";

pub const SET_INSTRUMENT_INTERFACE_OUTPUT_META: &str =
    "Update an output pipe's metadata (channel, min/max, unit); each provided field is \
     written, omitted fields are unchanged.";

pub const ADD_INSTRUMENT_RESOURCE: &str =
    "Add a resource entry: a logical `id` (what a node's sample/voice/patch references) mapped \
     to a `resource_source` (a file path for this door).";

pub const REMOVE_INSTRUMENT_RESOURCE: &str = "Remove a resource entry by id.";
