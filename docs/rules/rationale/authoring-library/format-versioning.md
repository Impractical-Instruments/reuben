# Why: The document carries an integer format_version where absent means 1; a breaking shape change bumps it and ships a parse-time migration, additive changes never bump, and only a newer-than-known document is refused.

[Rule](../../authoring-library.md#format-versioning)

The format had no version marker, so a breaking shape change had no way to announce itself to an
older engine. An optional integer `format_version` fixes that with a deliberately asymmetric stance:
**absent means 1** — every document written before versioning is a valid v1 — **save always writes
the current version**, and **only the future is unreadable**. A too-new document is refused at the
parse boundary (`UnsupportedVersion`, naming both versions and the remedy) before its shape is
trusted; a too-new *child* is fatal to the host load like any structural error.

The bump rules keep the marker meaningful: **additive** changes (a new optional field) never bump —
so a document stays loadable by the same-or-newer engine with no version dance — while **breaking**
shape changes bump *and* ship a parse-time migration, so older documents keep loading and only the
future is unreadable. The parse boundary also normalizes the accepted document to the current
version, so "save writes the current version" is a mechanism, not a coincidence: a migrated document
never saves back under its old number.

Two consequences are accepted on purpose. The format is **fail-closed on unknown fields**
(`deny_unknown_fields` throughout), so a typo in a hand-authored document fails loudly at parse
rather than being silently dropped — which means an *older* engine rejects a newer-but-still-v1
document carrying an additive field it doesn't know. That is deliberate: the engine and its
instruments version together (upgrade the engine), forward-reading old→new is a non-goal, and strict
typo detection is worth more than it to a hand-authored format. A bare policy doc with no field was
rejected — tools could not then distinguish "old file" from "file that predates versioning," and the
field costs one optional integer.

(§4's defensive "load path re-checks the version" mechanism this ADR described is retired: the
invariant is now held by a type — see [normalized-doc-gate](normalized-doc-gate.md).)

Distilled from: ADR-0036
