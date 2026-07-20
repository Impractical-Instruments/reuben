# Why: The MCP tool surface is a fixed roster split into always-available pure tools and fail-fast engine tools, returning structured Report/Diag results where a failed validation is a successful call, and shipping resources but no prompts.

[Rule](../../agent-mcp.md#tool-surface)

The surface is a **fixed roster** against a fixed process model: the pure tools
(`describe_operators`, `describe_instrument`, `validate`, `scaffold_instrument`) answer in-process
and are always available; the engine tools (`send`, `engine_status`, `swap`,
`get_current_instrument`, `get_diagnostics`) reach the user-owned engine and fail fast when it is
absent ([mcp-stdio-sidecar](mcp-stdio-sidecar.md), [user-owned-engine](user-owned-engine.md)). The
roster is stable across milestones by design — M1 vs M2 change what stands *behind* `swap`, never any
tool's name, schema, or result shape — so an agent's contract does not move under it.

The load-bearing discipline is the **error layering**, because models act on it. Three layers:
protocol errors for malformed calls; `isError: true` only when the tool **could not do its job**
(unreadable path, unknown operator, unreachable engine — carrying the "start `reuben play`"
guidance); and ordinary results for the deliverable — *including* `{ok: false}` reports. **A failed
validation is a successful call:** a report naming the offending node is the tool *working*, and a
rejected swap is the guard guarding, not the tool failing. Conflating the two is exactly wrong —
models read `isError` as "back off / retry differently," precisely the opposite of acting on a
diagnostic they should fix. `engine_status` is therefore never `isError` for a dead engine:
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

Distilled from: ADR-0048
