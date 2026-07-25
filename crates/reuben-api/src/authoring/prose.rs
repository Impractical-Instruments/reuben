//! The one sentence each authoring verb is advertised by — the tool-level `description` a door
//! puts in front of a model.
//!
//! It lives here for the reason every other piece of advertised prose does: a door that copies it
//! owns a second copy free to drift, and there is more than one door. What a door still decides is
//! how to *carry* it — MCP stamps these onto its tool roster, a generated wrapper writes them into
//! its manifest.
//!
//! Not every door has a place to put them. The native CLI's five subcommands are not the roster:
//! `describe` alone serves two verbs, `play` and `scaffold-operator` serve none, so its `--help`
//! prose is its own and single-sourcing it would mean writing a merged sentence twice instead of
//! once. A door advertising the roster verb-for-verb takes these.
//!
//! Every string here ships to a model, under the same rule the argument and result docs follow: no
//! rustdoc link syntax, no issue numbers, no crate paths — a model can resolve none of them. Notes
//! for humans go in `//` comments. Guarded over the real advertised surface by
//! `advertised_prose_is_model_facing`. see rules: code-as-grounding
//!
//! see rules: agent-mcp

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
    "Set a node input to a literal value: a number, or an enum symbol string. The one-value \
     point-edit — no re-emitting the whole document. Use wire_instrument_input to connect a port.";

pub const WIRE_INSTRUMENT_INPUT: &str =
    "Wire a node input from a source port: `from` is `/node.port`, or `/node` for a sole-output source.";

pub const UNWIRE_INSTRUMENT_INPUT: &str =
    "Clear a node input, reverting it to the operator's descriptor default.";

pub const SET_INSTRUMENT_CONSTANT: &str =
    "Set an instantiate-time constant on a node (a plan-time `config` value like a Voicer's `voices`).";

pub const ADD_INSTRUMENT_INTERFACE_INPUT: &str =
    "Add a boundary input pipe: a declared-type input that mints an address `/name` internal \
     nodes consume from, with optional channel, default, min/max, curve (lin/exp), and unit.";

pub const ADD_INSTRUMENT_INTERFACE_OUTPUT: &str =
    "Add a master-tap output pipe fed from an internal port (`from` = `/node.port` or `/node`), \
     with optional channel, min/max, and unit.";

pub const REMOVE_INSTRUMENT_INTERFACE_INPUT: &str = "Remove a boundary input pipe by name.";

pub const REMOVE_INSTRUMENT_INTERFACE_OUTPUT: &str = "Remove a master-tap output pipe by name.";

pub const SET_INSTRUMENT_INTERFACE_INPUT_META: &str =
    "Update an input pipe's metadata (channel, default, min/max, curve lin/exp, unit); each \
     provided field is written, omitted fields are unchanged.";

pub const SET_INSTRUMENT_INTERFACE_OUTPUT_META: &str =
    "Update an output pipe's metadata (channel, min/max, unit); each provided field is \
     written, omitted fields are unchanged.";

pub const ADD_INSTRUMENT_RESOURCE: &str =
    "Add a resource entry: a logical `id` (what a node's sample/voice/patch references) mapped \
     to a `resource_source` (a file path for this door).";

pub const REMOVE_INSTRUMENT_RESOURCE: &str = "Remove a resource entry by id.";

/// Every authoring verb's roster name paired with its sentence — what a door iterates to advertise
/// the whole set without naming each verb.
///
/// Roster **order** is not decided here: the contract roster is, and a door advertises in its
/// order. This is a lookup, and a door that finds a roster name missing from it is looking at a
/// verb the window does not serve yet.
pub const DESCRIPTIONS: &[(&str, &str)] = &[
    ("describe_operators", DESCRIBE_OPERATORS),
    ("describe_instrument", DESCRIBE_INSTRUMENT),
    ("validate_instrument", VALIDATE_INSTRUMENT),
    ("new_instrument", NEW_INSTRUMENT),
    ("set_instrument_name", SET_INSTRUMENT_NAME),
    ("set_instrument_description", SET_INSTRUMENT_DESCRIPTION),
    ("add_instrument_node", ADD_INSTRUMENT_NODE),
    ("remove_instrument_node", REMOVE_INSTRUMENT_NODE),
    ("rename_instrument_node", RENAME_INSTRUMENT_NODE),
    (
        "set_instrument_node_description",
        SET_INSTRUMENT_NODE_DESCRIPTION,
    ),
    ("set_instrument_input", SET_INSTRUMENT_INPUT),
    ("wire_instrument_input", WIRE_INSTRUMENT_INPUT),
    ("unwire_instrument_input", UNWIRE_INSTRUMENT_INPUT),
    ("set_instrument_constant", SET_INSTRUMENT_CONSTANT),
    (
        "add_instrument_interface_input",
        ADD_INSTRUMENT_INTERFACE_INPUT,
    ),
    (
        "add_instrument_interface_output",
        ADD_INSTRUMENT_INTERFACE_OUTPUT,
    ),
    (
        "remove_instrument_interface_input",
        REMOVE_INSTRUMENT_INTERFACE_INPUT,
    ),
    (
        "remove_instrument_interface_output",
        REMOVE_INSTRUMENT_INTERFACE_OUTPUT,
    ),
    (
        "set_instrument_interface_input_meta",
        SET_INSTRUMENT_INTERFACE_INPUT_META,
    ),
    (
        "set_instrument_interface_output_meta",
        SET_INSTRUMENT_INTERFACE_OUTPUT_META,
    ),
    ("add_instrument_resource", ADD_INSTRUMENT_RESOURCE),
    ("remove_instrument_resource", REMOVE_INSTRUMENT_RESOURCE),
];
