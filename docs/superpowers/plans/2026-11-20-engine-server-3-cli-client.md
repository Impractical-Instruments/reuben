# Engine as server, part 3: the `reuben` CLI becomes a client — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `reuben describe`, `reuben validate` and `reuben new-instrument` ask a server instead of reading files: the running `reuben play` when one answers, otherwise a temporary headless server the CLI starts in its own process. `bin/reuben.rs` stops naming `FsResolver`.

**Architecture:**
- A new module, `reuben_native::client`, decides which server a CLI command talks to and hands back a `Channel` to it. A running server is reached over `TcpTransport`; a temporary one is `engine::start_headless` with nothing open, no sockets bound, reached through `RunningEngine::channel()`.
- Every CLI read and the one CLI create go through `reuben_api::engine::call_tool`, the verb reuben-mcp already uses (part 2). The CLI deserializes the tool's JSON output into the window's existing result types and keeps its current human and `--json` output.
- No server-side change. Part 1's `call` already serves `describe_operators`, `describe_instrument`, `describe_boundary`, `validate_instrument` and `new_instrument`, confines names to what the server offers, and never swaps on a read or a create.

**Tech Stack:** Rust (clap, serde_json). No async.

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md`, step 3 of "Order of work". Also read the "Calls I made" bullets on the CLI.

**Branch:** `engine-server-cli`, cut from `dev`. PR against `dev`.

## Decisions this plan makes (the spec leaves them open)

1. **Which server.** Each client subcommand takes `--structure-addr <ip:port>` (the same flag `play` binds with, here naming where to dial) and `--temporary-server`. The flag wins, then the `REUBEN_STRUCTURE_ADDR` env var, then the default `127.0.0.1:9124`.
   - **Default address, connection refused** → start the temporary server. This is the spec's "behaves as today".
   - **Flag or env address unreachable** → exit 1 with the window's unreachable refusal (`ENGINE_UNREACHABLE_GUIDANCE`). The user named a server, so silently using another one would be wrong.
   - **Anything at the address that does not answer `ping`** (a hung server, a foreign process) → exit 1, naming the address and `--temporary-server`. No fallback.
   - **`--temporary-server`** never dials. It conflicts with `--structure-addr`.
2. **The temporary server** is `start_headless` on `EngineConfig { instrument: Instrument::Silent, instrument_root: <--instrument-root / env, as resolve() gives today>, osc_in: None, osc_out: None, structure: None, follow_files: false, ..EngineConfig::new(Instrument::Silent) }`. After start, it calls `offer_dir` with the CLI's cwd and with the lexical parent of each path argument, so `reuben validate /elsewhere/x.json` works as today. This follows the editor's standalone "Open file…" precedent (part 4): only an in-process server is offered a directory the user typed.
3. **Path arguments are sent absolute.** The CLI makes each path argument absolute and lexically normalized against its own cwd before sending (`structure::lexical_absolute`, made `pub`). Messages printed for humans are mapped back to the spelling the user typed. Without this, a `reuben play` started in another directory would resolve `foo.json` against its own cwd and could validate a different file. Part 6 (remote attach) must revisit this, since a local cwd means nothing to a remote server. Keep it in one function (`Names::sent`) so part 6 has one place to change.
4. **A running server says so.** When a running server (not the temporary one) answered, the CLI writes one stderr line: `note: answered by the reuben server at <addr> (--temporary-server to use a fresh one)`. That server's operator set is the one listed, which matters right after adding an operator (Review Focus 1).
5. **`new-instrument` creates and does not open**, the same as part 1's `call` and reuben-mcp's `new_instrument`. The spec's "creating a top-level instrument opens it" call was not built that way in parts 1–2. A CLI command that replaced what a running `reuben play` is playing would also surprise the user. Flag this in the PR body.
6. **`describe` with no instrument** (the operator set, one operator, `--compact`) also goes through the server, so the CLI lists the server's registry. With no server running, that means starting the temporary one.

## Global Constraints

- **Clients reach servers, never files.** After this part, the non-test code in `crates/reuben-native/src/bin/reuben.rs` names neither `FsResolver` nor any `authoring::<verb>` that takes a store. `scaffold-operator` is unchanged (it edits Rust source, not instruments). `play` is the server and keeps its file access.
- **One roster.** The CLI calls roster verbs by their `reuben_api::tools::names` constants through `engine::call_tool`. No second list of verbs.
- **Output unchanged.** Every human line, every `--json` shape and every exit code a subcommand produces today stays the same, except for the stderr note in Decision 4. The existing binary tests (`describe_select.rs`) pass unmodified, apart from the server flag their helper adds.
- **Default address unchanged** (`DEFAULT_STRUCTURE_ADDR`, `127.0.0.1:9124`). In this part, the client's `--structure-addr` accepts only a literal loopback IP, the same check `EngineFlags::config` applies. Non-loopback plus the token is step 6.
- **Tests never dial 9124.** Every binary test passes `--temporary-server` or an explicit `--structure-addr` to a server the test started, so a developer's own `reuben play` cannot change a result.
- **Invariants:** determinism, an RT-safe Render, an OSC-only core and the single-writer Coordinator. The temporary server adds no work to the render path.
- **Comments state mechanics only.** Never cite an issue number in a comment.
- **Gates for every task:** `cargo fmt --all --check` · `cargo clippy --all-targets --features reuben-document/bench -- -D warnings` · `cargo test` · `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --features reuben-document/bench,reuben-document/schemars`.
- **Task 4** also runs `cd eval && python3 -m reuben_eval.gate` (it edits `docs/agents/`) and `python3 -m unittest .claude/skills/control-surface/test_gen_surface.py`.

## Review Focus

1. **A `reuben play` built before the operator the user just added is still running.** `reuben describe newop` answers "unknown operator" from the old registry. The user expects to learn why: the stderr note names the server that answered, and the create-operator skill says to restart `reuben play` or pass `--temporary-server`. Tested in Task 2 (`a_running_server_is_named_on_stderr`).
2. **`reuben play` and the CLI started in different directories, with a relative path argument.** The CLI's own cwd decides which file is meant, as it does today. Tested in Task 2 (`a_relative_name_means_the_clis_directory`).
3. **Port 9124 is held by something that accepts and never answers** (a hung server, another program). The CLI exits 1 within the read timeout, names the address, and does not fall back. Tested in Task 1 (`a_silent_listener_at_the_default_is_an_error_not_a_fallback`).
4. **The test suite runs while the developer's `reuben play` holds 9124.** `--temporary-server` never dials. Tested in Task 1 (`temporary_never_dials`).
5. **`validate` on a document whose sample is present but cannot be decoded.** This is still a warning with exit 0, which proves the server's store decodes samples as the old `Samples::Decode` resolver did. Tested in Task 3 (the converted `an_undecodable_sample_is_a_warning`).

---

### Task 1: `reuben_native::client` — reach a server or start a temporary one

**Files:**
- Create: `crates/reuben-native/src/client.rs` (module and unit tests)
- Modify: `crates/reuben-native/src/lib.rs` (`pub mod client;`)
- Modify: `crates/reuben-native/src/cli.rs` (`ServerFlags`, its parse tests, and the loopback check factored into `fn loopback(addr: &str) -> bool`, which `EngineFlags::config` also uses)
- Modify: `crates/reuben-native/src/structure.rs` (`lexical_absolute`: `pub(crate)` → `pub`)
- Modify: `crates/reuben-api/src/engine/mod.rs` (`pub const STRUCTURE_ADDR_ENV: &str = "REUBEN_STRUCTURE_ADDR";`, moved here)
- Modify: `crates/reuben-mcp/src/lib.rs` (replace the const with `pub use reuben_api::engine::STRUCTURE_ADDR_ENV;`. Its users keep compiling.)

**Interfaces:**
- Produces, in `cli.rs`:
  - `#[derive(clap::Args, Debug, Clone, Default)] pub struct ServerFlags { #[arg(long, value_name = "HOST:PORT")] pub structure_addr: Option<String>, #[arg(long, conflicts_with = "structure_addr")] pub temporary_server: bool }`
  - `impl ServerFlags { pub fn target(&self) -> Result<Target, FlagsError> }`. It reads `STRUCTURE_ADDR_ENV` when the flag is absent and refuses a non-loopback address with the existing `FlagsError::StructureNotLoopback`.
- Produces, in `client.rs`:
  - `pub enum Target { Default, Named(String), Temporary }`
  - `pub struct Temporary { pub instrument_root: Option<PathBuf>, pub offer: Vec<PathBuf> }`
  - `pub struct Reached { .. }` with `pub fn channel(&self) -> &Channel`, `pub fn running_at(&self) -> Option<&str>` (the address when a running server answered; `None` for the temporary one), and a private `Option<RunningEngine>` it drops last.
  - `pub enum ReachError { Unreachable { addr: String, refusal: Refusal }, NotAServer { addr: String, why: String }, Start(StartError) }`, implementing `Display`. `Unreachable` shows the refusal `engine::refuse` gives (which carries `ENGINE_UNREACHABLE_GUIDANCE`). `NotAServer` reads `something at {addr} did not answer as a reuben server ({why}); pass --temporary-server to start a fresh one`.
  - `pub fn reach(target: Target, temporary: Temporary) -> Result<Reached, ReachError>` dials `DEFAULT_STRUCTURE_ADDR` for `Target::Default`.
  - `pub(crate) fn reach_with(target: Target, temporary: Temporary, default_addr: &str, read_timeout: Duration) -> Result<Reached, ReachError>` is the testable core. `reach` passes `CLI_READ_TIMEOUT`.
  - `pub const CLI_READ_TIMEOUT: Duration = Duration::from_secs(30);` A validate decodes every sample, and on a large library instrument that can take seconds.
  - `pub struct Names { typed: Vec<(String, String)> }` with `pub fn sent(&mut self, arg: &str) -> String` (lexical absolute, recorded with what was typed) and `pub fn in_user_terms(&self, text: &str) -> String` (each sent spelling replaced by its typed one, longest first).
- Probe with `channel.ping()`. A `ChannelError` where `is_unreachable()` is true counts as connection refused. Any other error is `NotAServer`.

- [ ] **Step 1: Write the failing tests** in `client.rs`'s test module. For a running server, use `engine::start_headless` with `structure: Some("127.0.0.1:0")`, `osc_in: None` and `Instrument::Silent`, and read `structure_addr()`. For an address nothing listens on, bind a `TcpListener` on `127.0.0.1:0`, take its port, then drop the listener.
  - `default_unreachable_starts_the_temporary_server`: `reach_with(Target::Default, .., dead_addr, ..)` is `Ok`, `running_at()` is `None`, and `channel().ping()` is `Ok`.
  - `the_temporary_server_binds_nothing_and_offers_cwd_and_the_given_dirs`: write `a.json` into a temp dir D (not under cwd). Then `call_tool(VALIDATE_INSTRUMENT, {source: D/a.json})` is `Ok`. A file in a sibling dir E that was not offered is refused with "is not offered by this server". The temporary engine's `structure_addr()` and `osc_in_addr()` are `None`. (Expose a `#[cfg(test)]` accessor on `Reached` for the engine.)
  - `a_named_unreachable_address_is_an_error_not_a_fallback`: `Target::Named(dead_addr)` gives `ReachError::Unreachable`, and its display contains `ENGINE_UNREACHABLE_GUIDANCE`.
  - `default_reachable_uses_the_running_server`: `running_at() == Some(addr)`. A `call_tool(DESCRIBE_OPERATORS, {})` answers.
  - `temporary_never_dials`: a `TcpListener` on `127.0.0.1:0` is the default address. `reach_with(Target::Temporary, ..)` is `Ok` with `running_at() == None`, and `listener.set_nonblocking(true); listener.accept()` gives `WouldBlock`.
  - `a_silent_listener_at_the_default_is_an_error_not_a_fallback`: a listener that never accepts reads, `read_timeout = 200ms`. The result is `ReachError::NotAServer`, and its display contains the address and `--temporary-server`.
  - `names_map_back_to_what_was_typed`: `sent("x.json")` is absolute. `in_user_terms` of a message containing it shows `x.json`, and a sent path that is a prefix of another is not replaced inside the longer one.
  - In `cli.rs` tests: `server_flags_refuse_a_non_loopback_address`, and `temporary_server_conflicts_with_structure_addr` (clap error).
- [ ] **Step 2:** Run `cargo test -p reuben-native client::`. Expected: fails to compile.
- [ ] **Step 3:** Implement the interfaces above, then move `STRUCTURE_ADDR_ENV`.
- [ ] **Step 4:** Run `cargo test -p reuben-native -p reuben-mcp`. Expected: PASS. Then run the gates.
- [ ] **Step 5:** Commit: `CLI client: reach the running server, or start a temporary headless one`.

### Task 2: `reuben describe` through the server

**Files:**
- Modify: `crates/reuben-native/src/bin/reuben.rs` (`Describe` gets `#[command(flatten)] server: ServerFlags`; `cmd_describe`, `cmd_describe_operators`, `cmd_describe_boundary`, `cmd_project`)
- Create: `crates/reuben-native/tests/common/mod.rs` (`spawn_play`, moved from `play_headless.rs`)
- Modify: `crates/reuben-native/tests/play_headless.rs` (use `common`)
- Modify: `crates/reuben-native/tests/describe_select.rs` (helpers pass `--temporary-server`)
- Create: `crates/reuben-native/tests/cli_server.rs`

**Interfaces:**
- Consumes: Task 1's `ServerFlags::target`, `client::{reach, Temporary, Reached, Names}`; `reuben_api::engine::call_tool`; `reuben_api::tools::names::{DESCRIBE_OPERATORS, DESCRIBE_INSTRUMENT, DESCRIBE_BOUNDARY}`.
- Produces, in `bin/reuben.rs`:
  - `fn connect(flags: &ServerFlags, root: Option<PathBuf>, paths: &[&str]) -> Result<Reached, ExitCode>`. It prints the error and returns `FAILURE`. On success, if `running_at()` is `Some`, it prints the Decision 4 note.
  - `fn ask<T: DeserializeOwned>(reached: &Reached, tool: &str, args: impl Serialize, names: &Names) -> Result<T, ExitCode>`. It runs `call_tool` and deserializes `answer.output` into `T`. A refusal prints `error: {names.in_user_terms(message)}` and gives `FAILURE`. A body that does not deserialize gives `error: the server answered `{tool}` with something unexpected: {e}`.
  - The output types are `authoring::Operators`, `DocumentView` and `Boundary`. If one lacks `Deserialize`, derive it in `reuben-api/src/authoring/result.rs`.
  - `test_support` (in `src/`) is not used for the binary tests. `tests/common/mod.rs` produces `pub struct Play { pub addr: String, .. }` (kill-on-drop) and `pub fn spawn_play(cwd: &Path, args: &[&str]) -> Play`. It starts `reuben play --headless --structure-addr 127.0.0.1:<free> --osc-in 127.0.0.1:<free>` with `current_dir(cwd)` and waits for the announcement line. It also produces `pub fn reuben(cwd: &Path, args: &[&str]) -> Output`.
- The flag checks that only apply to instruments (`--view`/`--select` on the operator route) stay in the CLI and run before `connect`, so a refusal there starts no server.

- [ ] **Step 1: Write the failing tests** in `tests/cli_server.rs`. Each one seeds its own temp dirs with a small valid document (the `DOC` shape from `play_headless.rs`).
  - `describe_reaches_the_running_server`: run `play` with cwd A and `--instrument-root A`. Run `reuben describe A/doc.json --structure-addr <addr>` with cwd B. Exit 0. The stdout equals the same command's stdout with `--temporary-server`.
  - `a_running_server_is_named_on_stderr`: in the same setup, stderr contains `answered by the reuben server at <addr>`. With `--temporary-server`, stderr does not contain `answered by`.
  - `a_relative_name_means_the_clis_directory`: run `play` with cwd B and `--instrument-root A`. Put a different `doc.json` in B (say, instrument name `wrong`). Run `reuben describe doc.json --structure-addr <addr>` with cwd A. Its stdout names A's instrument, not `wrong`.
  - `a_name_the_server_does_not_offer_is_refused_in_the_users_spelling`: a doc in an unoffered dir C, named relatively from cwd C. Exit 1. Stderr contains `` `doc.json` is not offered by this server `` and does not contain C's absolute path.
  - `describe_never_changes_what_plays`: start `play` with A/doc.json. The `Channel::get_document()` hash is the same before and after `describe` and `describe --view boundary`.
  - `an_explicit_unreachable_address_does_not_fall_back`: `--structure-addr` on a dead port exits 1, stderr contains `reuben play`, and stdout is empty.
  - `operator_listing_comes_from_the_server`: `describe --json --structure-addr <addr>` parses as a JSON array whose `type_name`s include `"oscillator"`.
- [ ] **Step 2:** Run `cargo test -p reuben-native --test cli_server`. Expected: FAIL, because `--structure-addr` is an unknown argument to `describe`.
- [ ] **Step 3:** Implement. `cmd_describe` routes as today, then calls `connect` with the instrument argument, if there is one, as the only path. `Names::sent` spells the `source` argument. `describe_select.rs`'s `describe` and `reuben` helpers add `--temporary-server`. `play_headless.rs` moves its spawn helpers to `common`.
- [ ] **Step 4:** Run `cargo test -p reuben-native`. Expected: PASS, including `describe_select.rs` unchanged apart from the flag. Then run the gates.
- [ ] **Step 5:** Commit: `reuben describe asks a server: the running one, or a temporary one`.

### Task 3: `reuben validate` and `reuben new-instrument` through the server; the CLI drops `FsResolver`

**Files:**
- Modify: `crates/reuben-native/src/bin/reuben.rs` (`Validate`, `NewInstrument` take `ServerFlags`; `cmd_validate`, `cmd_new_instrument`; delete `Samples`, `store`, the `FsResolver` import)
- Modify: `crates/reuben-native/tests/cli.rs` (drive `client::reach(Target::Temporary, ..)` and `call_tool` in place of `authoring::validate_instrument` over `FsResolver`)
- Modify: `crates/reuben-native/tests/cli_server.rs`

**Interfaces:**
- Consumes: Task 2's `connect`, `ask`, `Names`; `names::{VALIDATE_INSTRUMENT, NEW_INSTRUMENT}`; output types `authoring::Report` and `authoring::EditResult`.
- `new-instrument` prints `created {typed path} (content_hash …)`, which is the user's spelling, as today.

- [ ] **Step 1: Write the failing tests.**
  - In `cli.rs`, change `validate_source` to `client::reach(Target::Temporary, Temporary { instrument_root: None, offer: vec![parent of source] })` followed by `call_tool(VALIDATE_INSTRUMENT, ..)`, deserialized into `authoring::Report`. Every existing assertion stays, including `an_undecodable_sample_is_a_warning`. Update the module doc: it now drives the path the binary takes, a temporary server.
  - In `cli_server.rs`:
    - `validate_on_the_running_server_reports_like_the_temporary_one`: for one valid and one broken doc, the exit code and stderr lines match between `--structure-addr <addr>` and `--temporary-server`, apart from the note line.
    - `new_instrument_on_the_running_server_writes_and_does_not_open`: `reuben new-instrument A/n.json --structure-addr <addr>` exits 0. `A/n.json` exists, and `get_document()`'s `source` and `content_hash` are the same as before.
    - `new_instrument_refuses_to_overwrite`: the second run exits 1, and the file's bytes are unchanged.
    - `new_instrument_outside_what_the_server_offers_writes_nothing`: the target is in an unoffered dir C. Exit 1, and no file in C.
    - `new_instrument_with_no_server_writes_in_cwd`: with `--temporary-server` and cwd A, `reuben new-instrument n.json` creates `A/n.json`.
- [ ] **Step 2:** Run `cargo test -p reuben-native --test cli --test cli_server`. Expected: FAIL (unknown argument, missing imports).
- [ ] **Step 3:** Implement. Then confirm `grep -n "FsResolver\|authoring::\(validate\|describe\|new\)_" crates/reuben-native/src/bin/reuben.rs` prints nothing.
- [ ] **Step 4:** Run `cargo test`. Expected: PASS. Then run the gates.
- [ ] **Step 5:** Commit: `reuben validate and new-instrument ask a server; the CLI reads no instrument file`.

### Task 4: Docs, skills and the help text say where answers come from

**Files:**
- Modify: `crates/reuben-native/src/bin/reuben.rs` (module doc and each client subcommand's `///` help: "asks the running `reuben play`, or a temporary one when none runs")
- Modify: `docs/agents/authoring.md` (around lines 100 and 153: the CLI `validate`/`describe` are the same verbs, served by the same server)
- Modify: `.claude/skills/create-operator/SKILL.md` (steps 5–6: `reuben describe <op> --json --temporary-server`, or restart `reuben play` first; a running one lists the operator set it was built with)
- Modify: `.claude/skills/control-surface/SKILL.md` (around line 223) and `.claude/skills/control-surface/test_gen_surface.py` (around line 750: pass `--temporary-server`)
- Modify: `.claude/hooks/session-start.sh` (comment at line 5, if it describes how `validate`/`describe` read files)
- Modify: `AGENTS.md` / `CLAUDE.md` command list only if a line there becomes wrong

- [ ] **Step 1:** Make the edits. Every reader-facing sentence names the server that answers and how to force a fresh one.
- [ ] **Step 2:** Run `python3 -m unittest .claude/skills/control-surface/test_gen_surface.py`. Expected: PASS. Then run `cargo build -p reuben-mcp -p reuben-native && cd eval && python3 -m reuben_eval.gate`. Expected: PASS. If the authoring.md edit lands in an MCP-lane cut and the gate moves, report the delta.
- [ ] **Step 3:** Run the gates. Then commit: `Docs: the CLI's answers come from a server`.

---

After merge: update the `engine-server-part4-decisions` memory. Part 3 is merged; parts 5 and 6 inherit Decision 3 (absolute names from the CLI's cwd, which part 6 must skip for a remote server) and Decision 5 (create does not open).
