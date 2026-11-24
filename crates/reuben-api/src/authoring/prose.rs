//! The one sentence each authoring verb is advertised by — the tool-level `description` a door
//! puts in front of a model.
//!
//! What a door still decides is how to *carry* it — MCP stamps these onto its tool roster, a
//! generated wrapper writes them into its manifest — and a door whose surface is not the roster
//! (the native CLI's five merged subcommands) advertises its own help instead.
//!
//! Every string here ships to a model, under the same rule the argument and result docs follow: no
//! rustdoc link syntax, no issue numbers, no crate paths — a model can resolve none of them. Notes
//! for humans go in `//` comments. Guarded over the real advertised surface by
//! `advertised_prose_is_model_facing`.

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
     start move; refuses to overwrite an existing document. Then open it and edit it with the \
     other document tools; each accepted edit is live.";

pub const SET_INSTRUMENT_NAME: &str = "Set the instrument's top-level name.";

pub const SET_INSTRUMENT_DESCRIPTION: &str =
    "Set (or, omitting `description`, clear) the instrument's note.";

pub const ADD_INSTRUMENT_NODE: &str =
    "Add a node in one call: required `address` and `type`, plus optional inputs (literal or \
     wire-ref), config constants, description, a sample/voice/patch resource id, and for a \
     subpatch a `bind` map (child resource id -> this document's resource id) that reuses one \
     child with a different sample per use. Atomic — a wire to a missing source or a duplicate \
     address rejects the whole call.";

pub const REMOVE_INSTRUMENT_NODE: &str =
    "Remove a node. Cascades: auto-unwires every consumer wired from it and drops every \
     interface output fed from it, reporting exactly what it broke — no unwire-first dance. \
     Changing a node's type or resource is remove-and-re-add, so those wires must be re-made.";

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

pub const SET_INSTRUMENT_INPUTS_BY_INTENT: &str =
    "Apply one word from the intent vocabulary (warmer, looser, sadder…) as one batch of value \
     edits — the engine does the word -> ports -> arithmetic join, so you spend a word instead of \
     up to nine set_instrument_input calls and a range lookup each. Every node the word's moves \
     match is moved; narrow with `target` (node addresses), and pass `section` only for a word \
     with more than one reading. A wired input is followed to the interface pipe feeding it and \
     that pipe's value moves, so the wire survives. Moves this document has no seat for are \
     skipped, not errors. Echoes each address.input from -> to, plus what it skipped and why.";

pub const WIRE_INSTRUMENT_INPUT: &str =
    "Wire a node input from a source port: `from` is `/node.port`, or `/node` for a sole-output \
     source. This document's own interface input pipes are fed from outside it, so they cannot be \
     wired — you wire *from* one.";

pub const UNWIRE_INSTRUMENT_INPUT: &str =
    "Clear a node input, reverting it to the operator's descriptor default.";

pub const SET_INSTRUMENT_CONSTANT: &str =
    "Set an instantiate-time constant on a node (a plan-time `config` value like a Voicer's \
     `voices`). Echoes the change (from -> to), not the node.";

pub const REMOVE_INSTRUMENT_CONSTANT: &str =
    "Remove a constant from a node, reverting it to the operator's declared default. A declared \
     constant that was not set is noted, not an error; a name the operator does not declare is \
     refused.";

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

pub const CLEAR_INSTRUMENT_INTERFACE_META: &str =
    "Remove metadata fields from an input or output pipe — what the set-meta verbs cannot do, \
     since an omitted field is left unchanged. A field the pipe does not hold is noted, not an \
     error; `curve` exists on input pipes only.";

pub const ADD_INSTRUMENT_RESOURCE: &str =
    "Add a resource entry: a logical `id` (what a node's sample/voice/patch references) mapped \
     to a `resource_source` (a file path for this door).";

pub const REMOVE_INSTRUMENT_RESOURCE: &str = "Remove a resource entry by id.";

// --- surface docs ---------------------------------------------------------------------------------

pub const DESCRIBE_INSTRUMENT_SURFACE: &str =
    "Read an instrument's MIDI map (`surface: \"midi\"`) or on-screen surface doc \
     (`surface: \"screen\"`): its controls in order, each with why it would be skipped and what \
     was adjusted to use it, warnings about the doc as a whole, and the `hash` to pass as \
     `expect`. `exists` is false when the instrument has none yet. A MIDI map control reads \
     `bind` (an interface input), `midi` (`{kind, channel, cc}`: `kind` one of `note`, `cc`, \
     `bend`, `pressure`, `program`; `channel` 1-16 and `cc`, for kind `cc` only, 0-127, each an \
     integer or a pattern string of `*`, ranges and lists such as `\"1-4,10\"`), and an optional \
     `min`/`max` narrowing; a screen control reads `bind`, `label`, `widget`, `group`, `min`, \
     `max`, `note`, `velocity` and `degree`.";

pub const SET_INSTRUMENT_SURFACE_CONTROL: &str =
    "Replace the control at `index` in an instrument's MIDI map or on-screen surface doc, or \
     append it when `index` is omitted; the first set creates the doc. `control` takes the keys \
     `describe_instrument_surface` lists for that doc. Refused, with nothing written, when the \
     control could not be used: its `bind` names no interface input, or its `midi` route or widget \
     cannot drive that input; and while the instrument does not load, since there is nothing to \
     check it against. Echoes every control, one per line, by index.";

pub const REMOVE_INSTRUMENT_SURFACE_CONTROL: &str =
    "Remove the control at `index` from an instrument's MIDI map or on-screen surface doc. \
     Removing the last control leaves an empty doc.";

pub const MOVE_INSTRUMENT_SURFACE_CONTROL: &str =
    "Move one control of an instrument's MIDI map or on-screen surface doc from index `from` to \
     index `to`. Order is draw order on screen and does not change MIDI routing.";

pub const READ_INSTRUMENT: &str = "Read an instrument document back as a projection.";

pub const READ_INSTRUMENT_TREE: &str =
    "Read an instrument document and every document it reaches through a voice or patch, each \
     once, as projections.";

pub const DESCRIBE_BOUNDARY: &str =
    "Describe an instrument's boundary: its interface input and output pipes.";

pub const COPY_INSTRUMENT: &str = "Copy an instrument document from one source to another.";

pub const RESTORE_INSTRUMENT: &str = "Write a previously read document back to its source.";

pub const SET_INSTRUMENT_LAYOUT: &str = "Set the editor layout stored in an instrument document.";

pub const SET_INSTRUMENT_RESOURCE: &str = "Set a resource entry's source by id.";

pub const SET_INSTRUMENT_NODE_RESOURCE: &str =
    "Point a node's resource reference at a resource id.";

pub const WIRE_INSTRUMENT_INTERFACE_OUTPUT: &str =
    "Wire an internal port to an existing master-tap output pipe.";

pub const WIRE_COMPATIBILITY: &str =
    "Report whether one port can be wired to another, with the reason when it cannot.";
