# Why: The MCP tool surface is a fixed roster of three kinds — always-available pure tools, engine-free document verbs, and fail-fast engine tools — returning structured Report/Diag results where a failed validation is a successful call, and shipping resources but no prompts.

[Rule](../../agent-mcp.md#tool-surface)

The surface is a **fixed roster** against a fixed process model: the pure tools
(`describe_operators`, `describe_instrument`, `validate_instrument`) answer in-process and are always
available, as do the document verbs; the engine tools (`send_live_controls`, `get_engine_status`,
`swap_instrument`, `get_current_instrument`, `get_engine_diagnostics`) reach the user-owned engine and
fail fast when it is absent ([mcp-stdio-sidecar](mcp-stdio-sidecar.md),
[user-owned-engine](user-owned-engine.md)). The roster's third kind is the document vocabulary —
nineteen engine-free mutators ([document-verbs](document-verbs.md)) — and no arm takes or returns
instrument JSON: a document is named by an opaque `source` and read back as a projection. Names
follow the `verb_instrument_object` convention.

The load-bearing discipline is the **error layering**, because models act on it. Three layers:
protocol errors for malformed calls; `isError: true` only when the tool **could not do its job**
(unreadable path, unknown operator, unreachable engine — carrying the "start `reuben play`"
guidance); and ordinary results for the deliverable — *including* `{ok: false}` reports. **A failed
validation is a successful call:** a report naming the offending node is the tool *working*, and a
rejected swap is the guard guarding, not the tool failing. Conflating the two is exactly wrong —
models read `isError` as "back off / retry differently," precisely the opposite of acting on a
diagnostic they should fix. `get_engine_status` is therefore never `isError` for a dead engine:
answering "reachable?" *is* its job. Every tool declares an `outputSchema` and returns
`structuredContent` (the model's payload) plus a human text gloss; reports are `Report = {ok,
errors: Diag[], warnings: Diag[]}` with `Diag = {node?, port?, message}`, so warnings localize to a
node exactly as errors do.

Finally, **resources ship, prompts do not.** The server declares a small static resource set (the
authoring guide and, by later amendment, the intent vocabulary and library index) so clients can
`@`-mention stable browsable documents — the rule of thumb is resources for documents, tools for
anything computed. Prompts are withheld because MCP prompts surface as user-invoked slash commands
that would instantly duplicate the repo skills — the drift the grounding single-source exists to
prevent ([grounding-single-source](grounding-single-source.md)). The server `instructions` field
carries only the one-paragraph workflow gist and points at the guide.

Distilled from: ADR-0048, ADR-0066
