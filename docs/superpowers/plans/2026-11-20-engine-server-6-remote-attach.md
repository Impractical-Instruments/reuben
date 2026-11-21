# Engine as server, part 6: listen beyond loopback with a token

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `reuben play --structure-addr <ip>:9124 --token …` serves the structure channel beyond
loopback, and `reuben-editor --attach`, `reuben-mcp` and the CLI reach it with that token.

**Architecture:** Each connection may start with one credential line, `{"token":"…"}`. It
draws no reply. A connection from a non-loopback peer to a server that has a token must start
with the right one. If it doesn't, the server answers one `Error` and closes. One pure function
in `reuben-api` (`admit`) makes that decision. The reuben-native accept loop calls it on each
connection's first line, and `TcpTransport::with_token` writes the line ahead of every request
and every subscription. The bind refuses a non-loopback address that has no token, both at the
flags and in `StructureServer`.

**Tech Stack:** Rust, std TCP, serde_json, clap. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-20-engine-as-server-design.md`. The parts this plan
implements are step 6, the "Binding and the token" section, Goal 6, Goal 8 and the anti-goals.
Issue #895 sets the "Done when".

## Global Constraints

- The default stays `127.0.0.1:9124` (`DEFAULT_STRUCTURE_ADDR`), and
  `default_structure_addr_is_loopback_9124` stays.
- Listening beyond loopback takes an explicit `--structure-addr`, and a non-loopback address
  requires a token. Without one, the server refuses to start.
- A server's bind address is a literal IP. It is parsed, never resolved, as today. A client may
  dial a host name, such as a Tailscale MagicDNS name.
- Loopback connections need no token.
- The token comes from `--token <t>` or the `REUBEN_STRUCTURE_TOKEN` env var, on `reuben play`,
  the CLI client subcommands, `reuben-editor` (for `--attach` and for an embedded server on a
  non-loopback `--structure-addr`) and `reuben-mcp` (env only).
- No TLS. The token travels in clear. The docs say so, and say that Tailscale encrypts while a
  plain LAN does not.
- A token is never printed: `Debug` redacts it, and no error message carries it.
- No comment cites an issue number. Use the project's terms.

## Review Focus

1. **IPv4-mapped loopback peers.** On a `[::]` bind, a local client arrives as
   `::ffff:127.0.0.1`. It counts as loopback (`IpAddr::to_canonical`). Tested in Task 1's
   `admit` table.
2. **A credential line after the first line.** It is not a request, so it draws `unreadable
   request` and does not authorize later requests. Tested in Task 2.
3. **A subscription from a non-loopback peer.** The editor follows events over the same remote
   link, so `subscribe` must send the credential too. Tested in Task 6.
4. **CLI path names against a remote server.** `Names::sent` absolutizes a local path, and a
   remote server cannot read that path. When the target is not a literal loopback address, the
   name goes as typed. Tested in Task 3.
5. **A wrong token from a loopback peer.** It is refused rather than ignored, which surfaces a
   misconfigured client at once. A server without a token ignores a credential line, so a
   client configured with a token still reaches a plain local `reuben play`. Tested in Task 1's
   table and in Task 2.

---

### Task 1: The token and the admission rule in `reuben-api`

**Files:**
- Modify: `crates/reuben-api/src/engine/wire.rs` (add `Token`, `Credential`, `Admit`, `admit`)
- Modify: `crates/reuben-api/src/engine/tcp.rs` (`TcpTransport::with_token`)
- Modify: `crates/reuben-api/src/engine/mod.rs` (`STRUCTURE_TOKEN_ENV`, re-exports)

**Interfaces (produces):**
- `pub const STRUCTURE_TOKEN_ENV: &str = "REUBEN_STRUCTURE_TOKEN";`
- `pub struct Token(String)`.
  - `Token::new(impl Into<String>) -> Result<Token, EmptyToken>`. It refuses an empty or
    all-whitespace token.
  - `Debug` prints `Token(<redacted>)`.
  - `Clone + PartialEq` only through `matches`; no derive that compares in variable time.
  - `fn matches(&self, other: &str) -> bool`, constant time over the bytes.
- `pub struct EmptyToken;` with `Display`: "a structure channel token must not be empty".
- `pub fn credential_line(token: &Token) -> String`. It returns `{"token":"…"}\n`, serialized by
  a private `#[serde(deny_unknown_fields)] struct Credential { token: String }`.
- `pub enum Admit { Credential, Request, Refused(Response) }`.
- `pub fn admit(expected: Option<&Token>, peer: IpAddr, first: &str) -> Admit`:
  - A credential line:
    - `expected` is `None`: `Credential`.
    - the token matches: `Credential`.
    - otherwise: `Refused(Error{message: REFUSED_TOKEN})`.
  - Any other line:
    - `expected` is `None`, or `peer.to_canonical().is_loopback()`: `Request`.
    - otherwise: `Refused(Error{message: TOKEN_REQUIRED})`.
- `TOKEN_REQUIRED` = "this engine takes connections from other machines only with its token:
  pass --token or set REUBEN_STRUCTURE_TOKEN".
- `REFUSED_TOKEN` = "the engine refused this token".
- `TcpTransport::with_token(self, token: Option<Token>) -> Self`.
  - `round_trip` sends `credential_line + line` in one `write_all`.
  - `subscribe` does the same with the subscribe line.
  - The `MAX_LINE_BYTES` check stays on the request line alone.

- [ ] **Step 1: Write the failing tests** in `wire.rs` tests:
  - `a_token_is_never_empty`: `Token::new("")` and `Token::new("  ")` are `Err`;
    `Token::new("s3cret")` is `Ok`.
  - `a_token_does_not_print`: `format!("{:?}", Token::new("s3cret").unwrap())` does not contain
    `s3cret`.
  - `admission_table`: a table over `expected ∈ {None, Some("good")}`, peers `127.0.0.1`, `::1`,
    `::ffff:127.0.0.1`, `192.168.1.4` and `100.72.0.1`, and first lines `{"token":"good"}`,
    `{"token":"bad"}` and `{"verb":"ping"}`. Expected results:
    - no expected token: always `Credential` or `Request`;
    - good token: `Credential` from any peer;
    - bad token: `Refused` from any peer, loopback included;
    - ping: `Request` from a loopback peer, `Refused(TOKEN_REQUIRED)` from any other.
  - `a_credential_line_is_not_a_request`: `Request::from_ndjson(&credential_line(&t))` is `Err`.

  In `tcp.rs` tests:
  - `a_transport_with_a_token_sends_it_first`: a listener reads two lines. The first parses
    back to the token and the second is `{"verb":"ping"}`. The listener echoes `Pong`.

- [ ] **Step 2:** `cargo test -p reuben-api --features fs-resolver engine::` → the new tests fail
  to compile.
- [ ] **Step 3:** Implement the code in the Interfaces block. Re-export `Token`, `EmptyToken`,
  `Admit`, `admit`, `credential_line` and `STRUCTURE_TOKEN_ENV` from `reuben_api::engine`.
- [ ] **Step 4:** Run the same command → PASS. Then run `cargo test` → green.
- [ ] **Step 5:** Commit: "The structure channel takes a token: one credential line, admitted by
  peer".

### Task 2: The server admits by peer and token; a non-loopback bind needs a token

**Files:**
- Modify: `crates/reuben-native/src/structure.rs` (`StructureServer`, `accept_loop`,
  `handle_connection`)
- Modify: `crates/reuben-native/src/engine.rs` (`EngineConfig::token`, the bind in `start`)
- Test: `crates/reuben-native/tests/structure_server.rs`

**Interfaces:**
- Consumes: Task 1's `Token`, `admit` and `Admit`.
- Produces:
  - `StructureServer::bind_with_token<A: ToSocketAddrs>(addr: A, state: StructureState, token:
    Option<Token>) -> io::Result<Self>`.
    - When `local_addr()` is not loopback and `token` is `None`, it returns
      `io::ErrorKind::InvalidInput` ("serving the structure channel beyond loopback takes a
      token"). It checks after binding, so a host name is caught too, and drops the listener.
    - `bind(addr, state)` = `bind_with_token(addr, state, None)`.
  - `EngineConfig { token: Option<Token>, .. }`, defaulting to `None`. `start` and
    `start_headless` pass it to the bind.
  - `handle_connection` takes `peer: IpAddr` and `token: Option<Arc<Token>>`. Its first
    non-blank line goes through `admit`:
    - `Credential`: no reply; read on.
    - `Refused(r)`: write `r`, then close.
    - `Request`: serve that line as today.
    Only the first line is admitted.

- [ ] **Step 1: Write the failing tests** in `tests/structure_server.rs`:
  - `a_bind_beyond_loopback_without_a_token_is_refused`: `bind_with_token("0.0.0.0:0", state,
    None)` gives `Err` of kind `InvalidInput`.
  - `a_loopback_client_needs_no_token_on_a_server_that_has_one`: bind `0.0.0.0:0` with a token
    and dial `127.0.0.1:<port>` with a `TcpTransport` that has no token → `Pong`.
  - `a_wrong_token_is_refused_even_from_loopback`: dial with `with_token(bad)` → `Error`
    containing "refused this token". A second dial with the good token → `Pong`.
  - `a_credential_after_the_first_line_is_unreadable`: on a raw socket to a token server, send a
    ping and then the credential line. The second reply is an `Error` starting with
    `unreadable request`.
  - `a_server_without_a_token_ignores_a_credential`: a plain `bind("127.0.0.1:0")` dialed
    `with_token(any)` → `Pong`.
- [ ] **Step 2:** `cargo test -p reuben-native --test structure_server` → compile failure (no
  `bind_with_token`).
- [ ] **Step 3:** Implement. `accept_loop` hands each handler `peer.ip()` and a clone of the
  `Arc<Token>`.
- [ ] **Step 4:** Same command → PASS. Then run `cargo test` → green.
- [ ] **Step 5:** Commit: "The structure server admits a connection by its peer and the token".

### Task 3: CLI flags: `--token`, a non-loopback bind, and remote names

**Files:**
- Modify: `crates/reuben-native/src/cli.rs` (`EngineFlags`, `ServerFlags`, `FlagsError`)
- Modify: `crates/reuben-native/src/client.rs` (`Target::Named`, `reach`, `Names`)
- Modify: `crates/reuben-native/src/bin/reuben.rs` (the `play` startup line, and `Names` built
  for the target)

**Interfaces:**
- Consumes: `Token`, `STRUCTURE_TOKEN_ENV` and `StructureServer::bind_with_token` via
  `EngineConfig::token`.
- Produces:
  - `EngineFlags.token: Option<String>`, `#[arg(long, env = STRUCTURE_TOKEN_ENV, hide_env_values
    = true)]`. clap's `env` feature: check `Cargo.toml`. If it is off, read the env var in
    `config`/`target` as `ServerFlags` does today, and record a ruling.
  - `EngineFlags::config`:
    - a host name: `FlagsError::StructureNotAnIp`;
    - a non-loopback IP with no token: `FlagsError::TokenRequired(addr)`;
    - an empty token: `FlagsError::EmptyToken`;
    - otherwise `config.token = Some(Token)`.
  - `ServerFlags.token: Option<String>` (same `env`). `Target::Named(String)` becomes
    `Target::Named { addr: String, token: Option<Token> }`, and `Target::Default` carries the
    token too. Any address is accepted, host names included. `FlagsError::StructureNotLoopback`
    is removed.
  - `Names::for_target(remote: bool)`. When `remote`, `sent(arg)` returns `arg` unchanged.
    `remote` = the named address is not a literal loopback IP.
  - The `reuben play` startup line names the bound address. When that address is not loopback,
    `play` also prints a line saying that the token travels in clear and that the network should
    be trusted or encrypted (Tailscale).
- Error texts:
  - `TokenRequired`: "--structure-addr {addr} is not loopback, so the structure channel needs a
    token: pass --token or set REUBEN_STRUCTURE_TOKEN"
  - `StructureNotAnIp`: "--structure-addr must be a literal IP and port: {addr}"

- [ ] **Step 1: Write the failing tests** in `cli.rs` tests. Rewrite the loopback-only tests:
  - `a_non_loopback_structure_addr_needs_a_token`: with `0.0.0.0:9124` and `192.168.1.2:9124`
    and no token, the error text is the one above. With `--token t`, `config.token` is `Some`
    and `config.structure` is that address.
  - `a_structure_addr_must_be_an_ip`: `localhost:9124` gives the `StructureNotAnIp` text.
  - `an_empty_token_is_refused`.
  - `server_flags_accept_any_address_with_a_token`: `--structure-addr studio.tail:9124 --token
    t` gives `Target::Named` with that address and the token present.

  In `client.rs` tests:
  - `names_for_a_remote_server_go_as_typed`: `Names::for_target(true).sent("./a.json") ==
    "./a.json"`, and `for_target(false)` still absolutizes.

  In `tests/cli_server.rs`:
  - `describe_reaches_a_token_server_through_loopback_without_a_token`: start `reuben play
    --headless --structure-addr 0.0.0.0:<free> --token t`. `describe --structure-addr
    127.0.0.1:<port>` succeeds. Pick the port by binding `0.0.0.0:0` and dropping it, as the
    existing `play_headless` tests do.
  - `play_refuses_a_non_loopback_addr_without_a_token`: exit status non-zero, with
    `TokenRequired`'s text on stderr.
- [ ] **Step 2:** `cargo test -p reuben-native cli:: client::` and `--test cli_server` → FAIL or
  compile errors.
- [ ] **Step 3:** Implement. `reach` builds its `TcpTransport` `.with_token(token)`.
- [ ] **Step 4:** Same commands → PASS. Then run `cargo test` → green.
- [ ] **Step 5:** Commit: "reuben play listens beyond loopback with a token; the CLI dials any
  server".

### Task 4: `reuben-mcp` dials any address, with the token from the env

**Files:**
- Modify: `crates/reuben-mcp/src/engine_link.rs`
- Modify: `crates/reuben-mcp/src/main.rs`
- Modify: `crates/reuben-mcp/src/lib.rs` (re-export `STRUCTURE_TOKEN_ENV`; fix any call to
  `EngineLink::new`)

**Interfaces:**
- `EngineLink::new(addr: impl Into<String>, token: Option<Token>) -> Self`. It is infallible:
  `NotLoopback` is removed. `Default` dials `DEFAULT_STRUCTURE_ADDR` with no token.
- `main`:
  - reads `STRUCTURE_TOKEN_ENV`;
  - an empty or non-UTF-8 value exits 2 with `reuben-mcp: REUBEN_STRUCTURE_TOKEN: …`.

- [ ] **Step 1: Write the failing tests.** In `engine_link.rs`, replace
  `anything_but_a_literal_loopback_address_is_refused` with
  `any_address_is_dialed_as_given`, covering `192.168.1.4:9124` and `studio.tail:9124`. Add
  `a_token_link_sends_the_credential_first`: a listener reads the credential line and then the
  request line, and answers `Pong`; `link.structure().ping()` (or the channel's ping verb)
  succeeds.
- [ ] **Step 2:** `cargo test -p reuben-mcp engine_link` → compile failure.
- [ ] **Step 3:** Implement, and fix the call sites the compiler names.
- [ ] **Step 4:** Same command → PASS. Then run `cargo test` → green.
- [ ] **Step 5:** Commit: "reuben-mcp dials a server anywhere, with REUBEN_STRUCTURE_TOKEN".

### Task 5: `reuben-editor --attach` with a token

**Files:**
- Modify: `crates/reuben-editor/src/main.rs` (pass the token to `Engine::attach`, and to the
  embedded engine's config through `EngineFlags::config`, which is already the path)
- Modify: `crates/reuben-editor/src/engine.rs` (`Engine::attach(addr, token, wake)`)
- Modify: `crates/reuben-editor/src/link.rs` (`Link::dial(addr, token, wake)` builds its
  transport `.with_token(token)`)
- Modify: the `EngineLink::new` site at `main.rs` near line 565 → `EngineLink::new(addr, None)`
- Test: `crates/reuben-editor/src/ui_tests.rs`

**Interfaces:**
- `--token` comes from the flattened `EngineFlags.token`. With `--attach`, it is the token
  sent. Without it, it is the embedded server's token. An empty token is a clap usage error.

- [ ] **Step 1: Write the failing test** in `ui_tests.rs`:
  `an_editor_attached_with_the_token_follows_a_token_server`.
  - Start a headless engine with `structure: Some("0.0.0.0:0")` and a token.
  - Attach over `127.0.0.1:<port>` with the token, so the editor's credential line goes
    through the admit path.
  - Assert the editor follows what plays (the existing attach test's assertion), and that one
    accepted edit reaches the engine's content hash.

  A second case attaches over the same loopback address with a *wrong* token. Its link reports
  the refusal in the editor's top-bar report.
- [ ] **Step 2:** `cargo test -p reuben-editor an_editor_attached_with_the_token` → compile
  failure.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Same command → PASS. Then run `cargo test -p reuben-editor` → green. Run
  `cargo clippy -p reuben-editor --all-targets -- -D warnings`.
- [ ] **Step 5:** Commit: "reuben-editor attaches with a token".

### Task 6: The remote-attach check, and the docs

**Files:**
- Create: `crates/reuben-native/tests/structure_remote.rs`
- Modify: `README.md` ("Attaching to a running engine": replace the SSH-tunnel paragraph)
- Modify: the `EngineConfig::structure`, `StructureServer`, `TcpTransport` and `engine_link`
  module docs that say "loopback only"

**Interfaces:** Consumes everything above.

- [ ] **Step 1: Write the test.** `structure_remote.rs` works over this machine's non-loopback
  address.
  - **Address:** `UdpSocket::bind("0.0.0.0:0")?.connect("192.0.2.1:9")?.local_addr()`, which
    sends no packet. With no route, print `skipped: no non-loopback interface` and return.
  - **Setup:** a headless engine on `0.0.0.0:0` with a token, in a directory it offers.
  - **The client** dials `<lan-ip>:<port>` through a `Channel` and holds no file:
    - with no token: `ping` → `Error` with `TOKEN_REQUIRED`;
    - with a wrong token: `REFUSED_TOKEN`;
    - with the token:
      - `list` names the offered instrument;
      - `open` it;
      - `call` `describe_instrument` (it shows a subpatch node);
      - `call` an edit, after which the content hash changes;
      - `get_meters` answers;
      - a `subscribe` from a second token client sees the edit's `Changed` event.

  The test is named `a_client_on_another_interface_does_everything_with_the_token`.
- [ ] **Step 2:** `cargo test -p reuben-native --test structure_remote`. It passes if Tasks 1–5
  are right. Before committing, prove it can fail: remove `.with_token` from the client once,
  confirm it goes red, then restore.
- [ ] **Step 3:** Docs.
  - README:
    - `--structure-addr <ip>:9124` and `REUBEN_STRUCTURE_TOKEN` on `reuben play`;
    - `--attach host:9124` with the same env var on the editor;
    - the token travels in clear: use Tailscale or a trusted LAN;
    - the SSH tunnel still works and needs no token.
  - Module docs: remove "must stay loopback". The default is loopback, and beyond it needs a
    token.
- [ ] **Step 4:** Gates:
  - `cargo fmt --all --check`
  - both clippy gates
  - `cargo test`
  - `cargo test -p reuben-editor`
  - both doc gates
  - `cd eval && python3 -m reuben_eval.gate`
- [ ] **Step 5:** Manual check over Tailscale, recorded for the PR body:
  - `REUBEN_STRUCTURE_TOKEN=… reuben play --headless --structure-addr <tailscale-ip>:9124 <x>`;
  - from an empty temp dir: `reuben describe --structure-addr <tailscale-ip>:9124` with and
    without the token;
  - `reuben play --structure-addr 0.0.0.0:9124` with no token refuses to start.
- [ ] **Step 6:** Commit: "Remote attach: the check over a non-loopback interface, and the docs".

## Out of scope (known limits for the PR)

- TCP keepalive on a subscription. A remote engine that vanishes without a FIN leaves the
  editor's event read waiting until the OS gives up. Probes still report it unreachable.
- `DEFAULT_CONNECT_TIMEOUT` stays 500 ms. A relayed Tailscale link with a slow first connect
  may read as unreachable until it warms.
- A true two-machine editor check is left to the user. The automated check uses a non-loopback
  interface on one machine, and the manual check uses this machine's Tailscale address.
