# Why: Every sentence a model reads — a tool's description, a field's, a `$defs` entry's — lives once in the window beside the type it describes, and a door that advertises the roster verb-for-verb carries that sentence rather than writing one.

[Rule](../../agent-mcp.md#window-owns-advertised-prose)

A sentence is what a model picks a verb by. So a per-door copy of it is a per-door opinion about what
the verb does, and prose is the least mechanically-defended surface in the stack — the one that had
already rotted twice while every schema test stayed green. It is not the engine's, because the engine
no longer owns the wire; it is not the door's, because a per-door copy is the duplication the window
exists to delete.

Field and `$defs` descriptions ride the window's types and moved with them, which is the bulk of the
advertised bytes. The tool-level sentence was the last piece a door still owned, and it stayed there
one phase longer than it should have on a fact that turned out to be only half true: the MCP door's
tool macro takes a string literal and genuinely cannot name a const. What it can do is leave the
attribute off and **stamp** the window's sentence onto the router it built, which is a mechanism
rather than a decision.

**The trap in that mechanism is worth the paragraph.** With the attribute absent, the macro falls back
to the method's *rustdoc* — prose written for a Rust reader. The fallback is non-empty, so a roster
test, a schema test and a markup scan all pass while a model reads the wrong thing. The guard
therefore asserts the *stamped value* rather than that a description exists, and the door refuses to
start if a roster contract has no sentence at all. A roster contract carries its sentence as a field,
so a contract without one does not compile.

Everything here ships to a model under the same constraint the argument and result docs carry: no
rustdoc markup, no issue numbers, no crate paths, because a model can resolve none of them
([wire-descriptions-are-model-facing](../code-as-grounding/wire-descriptions-are-model-facing.md)).
Notes for a human reader go in ordinary comments beside the string, not in it.

The rule reaches a door that advertises the roster verb-for-verb. A door shaped differently writes its
own help, and that limit is [the door's side of the line](door-owned-shape-and-store.md).

Distilled from: ADR-0068, ADR-0070
