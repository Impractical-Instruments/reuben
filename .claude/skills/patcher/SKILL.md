---
name: patcher
description: Build or modify a reuben Instrument or Rig — the playable graph of Operators behind an instrument document. Introspects the live operator set, then makes each change with one document verb rather than hand-editing the file. Use when the user says "build an instrument", "make a synth/pad/bass", "patch up a rig", "add a node", "wire X to Y", "change this instrument", or describes a sound to construct. Not for applying an already-authored document to the running engine (`swap_instrument`) or auditioning a value live (`send_live_controls`).
---

# patcher

Authors instrument documents — Operator, Instrument, and Rig are *scales* of one recursive graph,
not different file types, so this skill authors all three.

**The procedure is not here.** The authoring loop, the type system and wiring rules, the document
format, addressing, and which door serves which step all live in
[docs/agents/authoring.md](../../../docs/agents/authoring.md) (served to MCP clients as
`reuben://guide/authoring`). Read it and follow it. This file holds only what is true of the
Claude Code checkout and nowhere else.

## Precondition: the document verbs must be reachable

Check that the reuben MCP tools are present before starting. The document verbs are the sidecar's
alone — the CLI is read-only over documents — so without them there is no way to do this work.

**If they are absent, stop and say so.** Do not open the instrument file and edit it by hand.
The repo ships `.mcp.json`, so the usual cause is a sidecar that failed to start; its stderr is in
the client's server logs. Hand-editing would be the one place left telling an agent to write
document bytes itself, and the guide's loop is built on that never happening.

## Finishing in a checkout

Two things the guide's loop does not cover, because they are git-side rather than document-side:

- **Stage the regenerated library index in the same commit** as the instrument change that
  required it. The guide says when regeneration is owed and how; what a checkout adds is that
  leaving it unstaged passes locally and reddens CI.
- **Report as below** — a conversational lane reports by talking; here the run has to end in
  something reviewable.

## Scope

| Thing | Action |
|---|---|
| Instrument/Rig documents — nodes, inputs, wiring, config, `interface` pipes, resources | **author / edit**, through the document verbs |
| Surface docs (`surfaces/*.json` — presentation binding pipes to widgets) | **never** — that is the `control-surface` skill; it delegates graph edits back here |
| New Operator types (Rust) | **never** — that is the `create-operator` skill |
| Core crates (Rust) | **never edit** — grounding comes from the operator set and the guide |

## Report

End with: which instrument document, what was built or changed (nodes added/rewired, values set),
the final validation result and any warnings, whether the library index was regenerated, and how
to play it (`reuben play <file>`, the OSC address to send notes to). If you built a Good Button or
promoted a player-facing control to an interface pipe, suggest the `control-surface` skill to
author its surface doc (TouchOSC and any host-side renderer read from it).
