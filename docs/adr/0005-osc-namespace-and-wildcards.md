# OSC namespace: hybrid addressing with first-class wildcard dispatch

## Context

Messages are OSC-shaped (address path + typed args + timetag) and OSC is the lingua franca internally and externally. Something must define what an address *names* and how external OSC maps to internal targets. Composition is recursive (ADR-0003), so structure is hierarchical.

## Decision

- **Hybrid addressing.** Every Operator, port, and param is auto-addressable by its **structural path** through the graph nesting (e.g. `/lead-synth/filter/cutoff`) — zero-config and predictable for agents. In addition, an Instrument may **expose a curated set of stable named addresses** — its public control surface — which do not move when its internals are refactored. Exposing a control is the same act as exposing a boundary port (ADR-0003): publishing the Instrument's public API. External controller mappings bind to the exposed address and survive internal rewiring.
- **First-class wildcard / pattern dispatch, internally as well as externally.** OSC pattern matching (`/drums/*/decay`, `/lead/*/cutoff`) is honored on internal Message dispatch, not just at the external boundary. One gesture can target many destinations.

## Consequences

- **Meta-effects and effect racks fall out for free** — a "rack" is a control that fans a Message across many targets via a wildcard address; no bespoke rack mechanism needed.
- Names must be unique within a parent scope so structural paths are unambiguous.
- Renaming/moving an operator changes its structural path; the curated exposed surface is the refactor-safe binding target, which mitigates this.
- Internal Message dispatch must implement OSC matching semantics (pattern → set of targets).
