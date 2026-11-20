# The engine as the server: clients query it and tell it what to do

Issue: #895 (editor attach across machines). This design widens it: #895's remote attach is the
last step of moving every instrument operation into the engine.

## Intent

There is one **server**: the engine. It opens an instrument document, plays it, and is the only
thing that reads or writes instrument files. Everything else is a **client** that asks the server
about the instrument it plays and tells it how to change it. The clients in this repo are the
`reuben` CLI, `reuben-editor` and `reuben-mcp`. A client does not know or care what a file is.

Why: attaching the editor (or an agent) to an engine on another machine, over the LAN or
Tailscale, is the point of attach. It works today only on one machine, because every client runs
the authoring verbs in its own process against its own disk.

## Where it is today

`reuben-api` keeps document bytes out of client code, but it is a library linked into each
client, so authoring runs in the client's process, on the client's disk:

- **`reuben-mcp`** runs the document verbs and introspection in-process through
  `reuben_api::authoring` over an `FsResolver` (`crates/reuben-mcp/src/lib.rs:851`). Only swap,
  controls, status and diagnostics reach the engine.
- **`reuben-editor`** loads the tree (`session/tree.rs`) through `FsResolver`, edits through
  `DocumentHistory<FsResolver>`, watches the files (`session/watch.rs`) to see other writers, and
  Swaps by `DocSource::Path`. The start screen, recent files, library listing (`session/users.rs`)
  and the sample picker (`inspector/resources.rs`) browse the local disk. By default the editor
  embeds a server (`engine.rs` starts what `reuben play` starts); `--attach` reaches one instead.
- **CLI** `describe`, `validate`, `new-instrument` work on local files with no server.
- **The structure channel** (`reuben-api/src/engine/{wire,server,channel}.rs`) has seven
  requests: `ping`, `swap`, `get_document`, `get_diagnostics`, `get_meters`, `set_probes`, `send`.
  It is one request, one response line, per exchange; it cannot push. It binds
  `127.0.0.1:9124` only (`DEFAULT_STRUCTURE_ADDR`, enforced by a test).
- `get_document` returns the whole document as JSON and `swap` accepts one by value
  (`DocSource::Document`): both hand document bytes to a client.

## Goals

1. The server owns the open instrument and its files. Every read and edit is a request to it.
2. **One instrument per server**, with its nested subpatch files. A client can list what the
   server offers, open one, or create a new one, by a name the server gives it. Opening unloads
   the current instrument and loads and plays the new one. A client may work on several
   instruments in a session (the editor switching between them); it does so by opening each in
   turn.
3. **Edits go live immediately.** An accepted edit is validated, written, and swapped in one
   step. There is no separate "swap" a client performs.
4. **An edit that does not fully validate is refused**, and nothing is written. "Fully" means the
   whole open tree still loads and plans with the edit applied, not only the one file it touches
   (removing a voice's interface pipe that its parent wires is refused). The refusal says what
   failed and where, and that nothing changed.
5. Clients are told of every change: an edit by any client, an outside edit to the server's files,
   an open of a different instrument.
6. The server can listen beyond loopback when told to, and then requires a token.
7. **Clients reach servers, never files.** A client names an instrument or a sample only by a
   name the server listed or returned. What the server offers, and from where on its disk, is the
   server's configuration (its library roots and the open instrument's directory today); a client
   can neither name nor learn anything outside it. Getting a client-side file onto a server is
   out of scope.
8. `reuben-editor --attach <host>:<port>` on another machine, no shared filesystem, does
   everything a local attach does: navigate (subpatches, Voicers), edit and hear it, master and
   node meters, live control sends, follow other clients' changes.

## Non-goals

- Several open instruments per server, or an open-but-not-playing document.
- Staging several edits silently and applying them as one swap.
- Changing the instrument file format.
- `reuben-web` (separate repo). It must keep building; it moves to the new shape on its own
  schedule.
- TLS. The token rides in clear; Tailscale encrypts, a plain LAN does not. Documented, not solved.

## Anti-goals

- A client that reads, writes, parses or watches an instrument file, or holds document bytes.
- A second list of verbs to keep in step with the `reuben_api::tools` roster.
- A server that accepts non-loopback connections without a token, or listens beyond loopback
  without an explicit flag.

## Approaches considered

**How the edit verbs travel on the channel.**
- **A — one generic request, `call { tool, args }` (chosen).** The server dispatches it through
  the `reuben_api::tools` roster and answers with the tool's result. Adding a verb changes no
  wire type; `reuben-mcp` becomes close to a pass-through. Cost: arguments are checked against a
  tool's schema at dispatch, not at the wire edge.
- B — one typed request per verb. Schema checked at the edge, but a second roster.
- C — a separate authoring channel on its own port. Rejected: with one server and one instrument,
  engine and document are the same object, and it doubles the bind/auth work.

**How clients learn of a change.**
- **Push (chosen).** A client sends `subscribe`; that connection then receives one line per
  event.
- Poll a content hash. Simpler, but adds latency and steady traffic over a remote link.

**Authentication beyond loopback.**
- **A shared token (chosen).** Given at server start; every connection sends it first.
- None, trusting the bind address / Tailscale ACLs. Rejected: the server writes files.

## The design

### The server

`reuben play` and the editor's embedded engine are both the server. It holds:

- the open instrument's server-side name (today: a path on its disk) and its tree of nested files;
- the authoring state: a `DocumentHistory` over its own resolver;
- the installed engine, as today.

An edit is applied to the open tree in memory, the whole tree is loaded and planned, and only
then is the file written and the new Plan swapped in. Any failure refuses the edit with the
errors and writes nothing, so the files and what plays never diverge through a client.

It watches its own instrument tree's files. An outside change (a text editor) cannot be refused,
so it is the one exception: it is read and validated; an invalid tree is reported as `invalid`
and not swapped, and the engine keeps playing the last good one until the files load again.

### Requests added to the structure channel

- `call { tool, args }` → the tool's result. Covers the document verbs, describe/introspection
  reads, `validate_instrument`, `new_instrument`, `restore_instrument`, and the verbs the editor
  uses that are not advertised today (`read_instrument`, `describe_boundary`, `hash_instrument`,
  `copy_instrument`, `set_instrument_layout`, `set_instrument_resource`,
  `set_instrument_node_resource`, `wire_instrument_interface_output`). For an edit, success means
  written **and** swapped; the result carries the new content hash. The existing `expect` guard
  carries over: an edit with a stale `expect` is a conflict.
- A verb's `source` names a document **in the open tree** (the top or a nested file, by the name
  the server gave it). Creating a nested file (the editor's new subpatch/voice, its local copy) is
  an edit of the open tree. Reads that span the library (who else uses this file) are allowed;
  writes outside the open tree are not: to edit another instrument, open it.
- Opening: `open { name }` unloads the current instrument and loads and plays the named one.
  Creating a new top-level instrument creates it and opens it.
- Listing: what the server offers to open (instruments under its library roots, recent ones) and
  the resources a sample picker can choose from. Names are opaque to clients.
- `subscribe` → this connection receives events until closed:
  - `changed { hash, by }` — the open document changed (`by`: a client edit, an outside file edit);
  - `opened { name, hash }` — a different instrument was opened;
  - `invalid { errors }` — the files on disk stopped loading; the engine keeps playing.

The dispatcher (`call` → tool) lives in `reuben-api`, next to the roster, so every server and every
in-process host shares one.

### Requests removed

- `swap` from clients. Editing and opening replace it. (`reuben-web` uses its own in-page swap,
  not this request; verify before removing.)
- The whole-document body of `get_document`, and `DocSource::Document`. Clients read structure
  through `call`.

### Binding and the token

- `reuben play --structure-addr <addr>`. Default stays `127.0.0.1:9124`; the loopback test stays,
  now asserting the default.
- A non-loopback address requires a token (`--token <t>` or an env var); the server refuses to
  start without one. A connection's first line must carry it; anything else is closed. Loopback
  connections need no token.
- Clients take the token by flag or env var: `reuben-editor --attach host:port`, `reuben-mcp`,
  the CLI.

### Clients

- **`reuben-mcp`**: every tool becomes a `call` to the server. No `FsResolver`. The engine tools
  stay. `reuben-mcp` stops offering `swap_instrument` (the roster entry stays, see Calls); the agent guidance (`docs/agents/`, the
  server `instructions`) changes from "edit, then swap" to "every edit is live; audition with
  `send_live_controls` first". The eval gate runs after that prose change.
- **CLI**: `describe`, `validate`, `new-instrument` reach a running server, or start a temporary
  one with no audio device when none is reachable. `scaffold-operator` is unchanged (Rust source).
- **`reuben-editor`**: `Session` reads structure through `call`, edits through `call`, and follows
  `subscribe` events instead of its file watcher. Start screen, recent instruments, library list
  and sample picker list from the server; picking an instrument is `open`. Standalone, the editor
  launches its own server (embedded, as today) to load and play the instrument, and is its client
  through the in-process transport; `--attach` is the same client over TCP.

### Order of work

Each step lands green on its own.

1. `reuben-api`: the `call` dispatcher over the roster, `open`, listing, and `subscribe` with its
   events; the server's own file watching and edit-is-live behavior.
2. `reuben-mcp` onto `call`; roster and guidance updated for live edits.
3. CLI onto a server (temporary headless one when none runs).
4. `reuben-editor` onto the server.
5. Remove client `swap`, `DocSource::Document`, and `get_document`'s body.
6. `--structure-addr`, the token, and the remote-attach check from #895's "Done when".

## Calls I made (correct me)

- **Held swaps** (the embedded editor asking its user before installing another client's swap):
  kept for **opening a different instrument**; edits to the open instrument apply without asking.
  A held `open` answers "held" at once (today's `HeldSwap`), never blocks. Only an embedded editor
  holds; `reuben play` never does.
- **Unadvertised verbs**: the editor-only verbs join the roster as a new kind that the server
  dispatches but no door advertises to a model, so the eval gate and the model-facing prose are
  untouched by them.
- **`reuben-web` must keep working**, not just building: `swap_instrument` stays on the roster
  (its browser agent walks `CONTRACTS`); only `reuben-mcp` stops serving it.
- **CLI names are the server's.** A path argument names an instrument the server offers. With no
  server reachable, the CLI starts an in-process headless server rooted at the current directory,
  so `reuben validate ./foo.json` behaves as today. `describe` and `validate` are reads and never
  change what plays.
- **Creating a top-level instrument opens it**; creating a nested file is an edit of the open tree.
- **Undo** stays a client feature over server state: the server keeps the history of content
  hashes; the editor's undo calls `restore_instrument` with a hash, as today.
- **`by` in events** names the kind of writer, not a client identity.
- **CLI** starts a temporary headless server rather than refusing when none is running.

## Left to the builder

- Wire spellings of new requests and events, framing of the subscribe stream, the token's first
  line.
- How the in-process transport delivers events to the embedded editor.
- Debounce timing for the server's file watching (the editor's `QUIET` / `LONGEST_WAIT` are a
  starting point).
- Splitting each step into PRs.
