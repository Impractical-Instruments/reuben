# Surface edit verbs and the editor's MIDI map table — design

## Intent

An instrument's MIDI map (`surfaces/<stem>.midi.json`) and its on-screen surface doc
(`surfaces/<stem>.json`) are edited today only by hand. The control-surface skill writes the
files directly, and no client can write either one through the engine. The engine's `Surface`
request hands the on-screen doc to the editor as raw text, read-only. That breaks two rules the
project keeps: clients talk to the engine, which owns the files, and no consumer holds document
bytes.

This change adds one set of `reuben-api` roster verbs that read and edit either doc, served by the
engine through `Call`. `reuben-editor` gets a MIDI map table built on those verbs, and
`reuben-mcp` picks them up from the roster. Any other client uses the same verbs.

## Goals

- Read and edit an instrument's MIDI map through the engine, from any client, one change per call.
- The same verbs cover the on-screen surface doc. The two docs share one format and one parser,
  so covering both costs one argument.
- Refuse an edit that would leave the edited control unusable, the way instrument edits are
  refused: say why and write nothing.
- Report, per control, the warnings `reuben play` prints today only to stderr (skipped, clamped,
  written for another instrument).
- An editor table for the MIDI map: one row per control, with its input, pattern, min/max and
  warning, plus add, remove, reorder and edit.
- An open editor sees map edits made by other clients.
- Map edits are undoable in the editor, on the same undo stack as instrument edits, because they
  go through the same store and history.

## Non-goals

- MIDI learn (bind by moving a knob). It needs a stream of incoming MIDI to the client; that comes
  in a later pass.
- An editor UI for the on-screen surface's layout (label, widget, group, cols). The verbs support
  it; this pass builds no screen for it.
- Watching for outside writes to a surface file and pushing an event for them. Only edits made
  through the verbs publish one.
- Changing the surface-doc format. `surfaces/surface.schema.json` stays at `surface_version` 1.

## Anti-goals

- A client that reads or writes a surface file itself, or receives its text. After this change the
  editor reads the structured doc; the `Surface` request and `Response::Surface` are removed.
- Two copies of the "would this control route / resolve" checks. The checks the engine refuses
  with are the same code `reuben play`'s MIDI router and the editor's panel resolver use.
- Relaxing a check to make an edit go through. A control that would be skipped is refused.

## Constraints

- Invariants from `docs/agents/authoring.md`: determinism, RT-safe render, OSC-only core,
  single-writer Coordinator. Nothing here touches render. Surface edits run on the channel thread
  under the same Coordinator lock `Call` already takes, so edits stay serialized with instrument
  edits.
- `reuben-api`'s `render` feature stays `no_std` and serde-free. Everything new is behind
  `authoring`.
- `reuben-core` gains nothing. Surface docs are presentation, not engine.
- The roster convention is `verb_instrument_object`, and every verb is one `CONTRACTS` entry with
  its prose beside its argument type.
- `reuben-mcp` reaches everything through `reuben-api`.

## Approaches considered

- **A — one edit per call (chosen).** A structured read plus set / remove / move of one control,
  addressed by index, all taking `surface: "screen" | "midi"`. This matches how the instrument
  verbs already work, and it is what the `reuben-mcp` instructions teach models ("one change at a
  time"). An edit can be checked against what it changed, so a control that was already broken
  doesn't block an unrelated edit. The editor's table rows map onto it one to one.
- **B — read the whole doc, write the whole doc.** Two verbs and a smaller roster. But a model
  resends the whole list to change one CC, and validation can't tell a control the edit just broke
  from one that was already broken.
- **C — editor-only engine requests plus separate MCP tools.** This splits the API between
  clients, which is what the request rules out.

Chosen: A.

## Design

### Verbs (roster entries, kind `Document`)

All four take `source` (the instrument, as the instrument verbs name it) and
`surface: "screen" | "midi"`. *(Builder's call.)* `source` must name the open top instrument;
any other source is refused, because `reuben play` reads only the top instrument's docs. The
argument is kept so the verbs read like the instrument verbs, and so a later pass can widen it.

Every edit also takes an optional `expect`: the content hash of the
surface doc as the client last read it. If the doc has changed since, the edit is a conflict and
writes nothing. Without `expect`, the last write wins. Indexes are positions in `controls`.

| Verb | Arguments beyond `source`, `surface`, `expect` | Does |
| --- | --- | --- |
| `describe_instrument_surface` | — | Read. Answers the parsed doc (`instrument`, `cols`, `controls`), its hash, and per-control warnings, or "none" when the instrument has no such doc. |
| `set_instrument_surface_control` | `index?`, `control` | Replaces the control at `index`, or appends when `index` is absent. Creates the doc if there is none. |
| `remove_instrument_surface_control` | `index` | Removes the control at `index`. Removing the last control leaves an empty doc and does not delete the file. |
| `move_instrument_surface_control` | `from`, `to` | Moves one control. Order is render order for the screen doc and has no effect on MIDI routing. |

`control` is the typed `SurfaceControl` (`bind`, `label`, `widget`, `group`, `min`, `max`,
`note`, `velocity`, `degree`, `midi`). It is never text. An edit answers the same shape as the
read, after the edit, so a client never needs to re-read.

A `midi` field on a screen-doc control, or `label`/`widget`/`group` on a MIDI-map control, is
accepted and kept as the schema already allows. The docs say which keys each doc reads.

Setting `cols` is left out. Nothing in this pass needs it, and a fifth verb can add it.

### Checking

For each control, the check yields either a usable control (with any adjustment warnings) or the
reason it is skipped:

- **MIDI map:** the route checks `reuben play` runs today. The pattern is well-formed, starts with
  `/midi/`, and matches one message kind. The bind names an interface input whose kind takes that
  message. There is a range to scale into. A narrowing that has to be clamped is a warning.
- **Screen:** what the format and the instrument allow, not what any one renderer draws. The bind
  names an input, the widget is a schema kind that fits the input's kind, and `note-toggle` /
  `chord-button` carry their payload. A clamped narrowing is a warning. *(Builder's call.)*
  Renderer limits stay with each renderer as warnings at draw time: the editor's "does not draw"
  for `xy-pad`/`grid`/`visualizer`, and the TouchOSC generator skipping `keyboard`. The engine
  never refuses a widget kind the schema reserves.

An edit is refused if the control it sets is skipped. Remove and move never refuse on these
grounds. A doc written for another instrument is a warning, not a refusal; the first `set` on a
doc that doesn't exist writes `instrument` as the open instrument's name.

All the checks run against the instrument's interface inputs (`Boundary`). If the open instrument
doesn't load, there is no boundary to check against. The edit is then refused with that reason,
as `reuben play` already keeps routes rather than rebuilding them while the file doesn't load.

### Where the code goes

- `reuben-api` (authoring half) gains a `surface` module:
  - `SurfaceDoc`, `SurfaceControl` and the parser, moved from `reuben-native/src/surface.rs`, with
    `Serialize` and `JsonSchema` added.
  - The OSC 1.0 address-pattern matcher and `MidiKind`'s address segment, moved from
    `reuben-native/src/midi/pattern.rs` and the address half of `translate.rs`.
  - The per-control checks for both docs. They return what a consumer needs to act on: for MIDI,
    the pattern, kind, target and `Scale`; for screen, what the editor's `Panel` builds from.
  - The verbs' argument and result types and their prose.
- `reuben-native` keeps byte translation, the router, devices and the watch. It imports the moved
  types and builds its `RouteTable` from the shared checks. `scale_value` stays native (it runs per
  MIDI message, not per edit).
- `reuben-editor`'s panel resolver uses the shared screen checks instead of its own.
- **Surface docs are store documents, edited through the same plumbing as instrument documents.**
  The four verbs are ordinary `Document` roster entries. They are dispatched by
  `authoring::call_scoped` and read and write through the `DocumentHistory` store, so each edit is
  staged, committed, kept in the engine's history, and undoable from the editor like any other
  file. `named_sources` names the instrument (`source`), which is in the open tree, so the
  offered-name check passes unchanged.
  - The `Resources` trait gains `surface_source(instrument, surface) -> Option<String>`: where the
    instrument's screen doc or MIDI map lives, or would be created. It defaults to `None`, and the
    verbs refuse with "this store keeps no surface docs." `FsResolver` implements it, taking over
    `locate_surface` from `reuben-native`. `DocumentHistory` forwards it.
  - The verb reads the doc with `read_text(surface_source)` and writes with `write_text`, staged as
    any edit is. It answers `CallKind::Edit` with the surface doc's source.
  - `handle_call` tells a surface doc from an instrument document by comparing an `Edit` source
    with the open instrument's two `surface_source`s. A surface doc skips `edit_or_refuse`'s
    in-tree check and its reload/plan/swap. The verb has already checked the doc against the
    instrument's inputs, so the engine just commits the staged write, publishes
    `SurfaceChanged`, and tells the host the doc changed (below). No Swap happens.
  - `restore_instrument` on a surface doc's source (the editor's undo/redo) takes the same branch.
    Its offered-name check accepts the open instrument's two surface sources. A restore is never
    refused for controls that would be skipped, since it puts back content the doc already had.
  - `EngineHost` gains one notification, `surface_written(surface)`, and no file access.
    `reuben-native` uses it to have the route watch rebuild when the `midi` doc changes.
  - The `.json` create guard needs no change: both surface paths end in `.json`, and the first
    `set` on a missing doc is an `Edit` whose staged write creates the file.
  *(Builder's call.)* A doc that doesn't exist yet is created in the `surfaces/` directory that
  already holds its sibling doc (the screen doc for a new MIDI map, and the other way round). If
  neither exists, it goes in the nearest `surfaces/` directory `locate_surface` searches that
  already exists, and only failing that in `<instrument dir>/surfaces/`. That way
  `instruments/acid-techno.json` gets `surfaces/acid-techno.midi.json` at the repo root, next to
  its screen doc.
- *(Builder's call.)* Routing follows a verb edit whether or not `follow_files` is on:
  `reuben-native`'s `surface_written` for the `midi` doc tells the route watch to rebuild, instead
  of relying on the file-stamp poll.
- `EngineHost::surface`, `Request::Surface`, `Response::Surface` and `Channel::surface` are
  removed. The editor's controls panel calls `describe_instrument_surface` instead.

### Change notification

A successful surface edit publishes a new `Event::SurfaceChanged { surface, hash }` to
subscribers. The editor's controls panel rebuilds on a `screen` change, and its MIDI map table
re-reads on a `midi` change. `reuben play`'s route watch already polls the map file's modification
stamp, so routing follows an edit without an event.

### Editor: the MIDI map table

A "MIDI map" view of the open instrument. It sits beside the controls panel, the same way the
existing panels are shown. One row per control, in order:

```
 #  Input      Pattern              Min    Max     Status
 1  notes      /midi/1/note                         ok
 2  cutoff     /midi/{1,2}/cc/74    200    8000     ok
 3  drive      /midi/1/cc/1                         skipped: no range to scale into …
    [+ add]
```

- The input is a dropdown of the open instrument's interface inputs.
- The pattern is a text field. Min and max are optional number fields.
- Each row has remove and move up/down.
- Editing a field and committing (Enter or focus loss) sends one `set_instrument_surface_control`
  with `expect`. A refusal shows its reason on the row and leaves the doc unchanged.
- Every table edit goes through the editor's one write path (`edit::write`) and records an undo
  step for the map file, so the editor's undo and redo cover it.
- The table shows the doc's own warning, if it was written for another instrument, above the rows.

### MCP and docs

`reuben-mcp` advertises the four verbs. Each needs its own `#[tool]` method in
`crates/reuben-mcp/src/lib.rs`, because the router refuses to start unless its tools match the
roster exactly. The
control-surface skill and `docs/agents/authoring.md`'s MIDI section switch from hand-editing the
file to the verbs. `cd eval && python3 -m reuben_eval.gate` is rerun, since the advertised surface
changes.

## Left to the builder

- Whether the screen and MIDI checks share one result type or two.
- The exact refusal wording, as long as it names the control and the reason.
- Where the MIDI map view sits in the editor's layout, consistent with the existing panels.
- Test layout: checks unit-tested in `reuben-api`; the verbs tested through the server's
  dispatcher with an in-memory store; `FsResolver::surface_source`'s find/create placement against
  a scratch dir; undo/redo of a map edit through `restore_instrument`;
  `reuben-mcp`'s `tools/list` test updated.
