# Why: Every verb's argument surface is closed — an argument the window does not declare is a refusal, never a silently dropped key — because a wrong document reported as written has nothing downstream to catch it.

[Rule](../../agent-mcp.md#closed-argument-surface)

Serde's default is to drop what it does not recognize. On an authoring verb that default is the
worst available behaviour, because of an asymmetry the live evals measure directly: a **loader
rejection is recoverable** — the model reads the report, sees what it broke, and repairs — while a
**silent no-op is not**. The verb answers `written: true` with a clean report, the model believes the
edit landed, and nothing downstream disagrees; the document is simply wrong, and stays wrong until a
human listens to it.

Two ways in, both real:

- **A parameter that moved.** When a slot changes hands from one verb to another, every caller
  written against the old surface — a stored skill snippet, an example in a transcript, a generated
  client that has not regenerated — keeps passing the old key. Open, they are told the edit
  succeeded. Closed, they are told exactly which key is no longer this verb's, on the first call.
- **A word that spread unevenly.** Renaming an argument on one verb teaches the model a word it will
  reasonably try on the verb next door. Open, that guess writes a document missing the field it
  meant to set and reports success. Closed, the guess costs one bounce.

The cost is a real one and it is accepted: `additionalProperties: false` rides every tool's input
schema, and a door or client that was sending a stray field starts failing. That is the point — it
was already being ignored, and being told is strictly better than not.

This is why unifying a name and closing the surface are one change and not two: closing it makes the
divergence loud, and unifying it removes the reason to diverge. Neither alone is enough.

Decided in: issue #622 — settled directly, no ADR.
