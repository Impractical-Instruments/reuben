# OSC-only core; protocol adapters at the boundary

## Context

reuben must interoperate with MIDI, Ableton Link, and other control protocols, and OSC is already the lingua franca for internal Messages. Letting multiple protocols into the core would multiply representations and confuse every operator with protocol-specific cases.

## Decision

The core speaks **only** OSC-shaped Messages. All other protocols (MIDI, Ableton Link, OSC tempo sync from foreign clocks, future control protocols) are **converted to/from OSC at the I/O boundary** by adapters. No non-OSC protocol representation exists inside the core. A MIDI note-on becomes an OSC Message on the way in and is reconstructed as MIDI on the way out; Ableton Link becomes Clock sync at the boundary.

## Consequences

- Operators only ever handle one Message shape — no protocol branching in the graph.
- Each supported protocol is an isolated, removable boundary adapter (fits the removable-native-layer goal).
- Some protocol-specific nuance must be mapped into an OSC convention at the edge (e.g. MIDI CC → an address + value); the mapping conventions live in the adapters, not the core.
