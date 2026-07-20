# Why: A graph's boundary is named interface pipes — an input pipe mints an address internal nodes wire from and an output pipe is fed from an internal port, each pipe declares its own `Arg` type, and N-channel I/O is N mono pipes bound to logical channels that a device profile, not the patch, maps to hardware.

[Rule](../../composition-operators.md#interface-pipes)

A graph's edge had grown two spellings and one gap: nesting gave graphs an `interface` block whose
entries *pointed at* inner ports, the top level kept its own anonymous `outputs` array, and audio
**input did not exist at all**. Interface pipes make one boundary concept for every level. The wiring
direction **flips** so there is one rule: an interface **input** is a named pipe that *mints an address
in the flat namespace* (entry `in` → `/in`), which internal nodes consume with ordinary wire-refs
(`{"from": "/in"}`, fan-out free — the pipe behaves like a source node); an interface **output** is a
pipe *fed from* an internal port (`{"from": "/pan.left"}`). Message/control entries flip too — no entry
points inward anymore ([pipe.rs](../../../../crates/reuben-core/src/operators/pipe.rs)). Because an
entry no longer points at an inner port, there is nothing to inherit a type from, so **the pipe
declares its own `Arg` type** — enforced by the existing pass-2 wire check against every consumer, no
new checker; subpatch face synthesis reads that declared type
([nesting-inline-or-host](nesting-inline-or-host.md)).

The load-bearing foundation is that **the Signal stays mono, permanently**. An N-channel Signal was
rejected on the merits, not re-deferred: it would grow a channel dimension on every operator, every
buffer, every arena, and multiply the contract surface (does a filter share state across channels?),
to buy authoring sugar the pipe model already provides legibly. So **N-channel I/O is N mono pipes** —
width lives at the graph boundary, never inside the Signal, which keeps every existing operator's
`process` untouched forever. This is the modular-synth precedent (one channel per edge).

Hardware binding stays **out of the patch** for portability — the same patch should play on any rig. A
signal pipe may carry an optional **`channel: k`** logical index, honored *only* on the graph played at
top level (nested or Voicer-hosted, the binding is inert — the parent's edge feeds the pipe like any
boundary wire; an unfed nested pipe renders silence + a warning: a graph's inputs are pipes, never a
magic hardware tap from inside a nest). A small **device profile** loaded with `--io-map`, not the
patch, maps logical↔device channels; with no profile, the identity map plus today's implicit
broadcast/downmix/zero-fill policy holds, bit-identical. The device layer that honors those logical
channels resolves the rest of the I/O story — request→grant→adopt rate negotiation, a lock-free SPSC
ring resampling live input into the engine rate, and warn-plus-zeros **dark-degrade** on any reality
mismatch (never fatal; structural errors in the document still fail loudly). Live input is a sanctioned
nondeterministic boundary — the offline path injects known buffers, so render stays reproducible. The
pipe type set is `f32_buffer`/`f32`/`i32`/`note`/`harmony`/enum; an `i32` pipe is an integer control
that widens into a consumer's `f32` port ([per-wire-form-check](per-wire-form-check.md)). The flip was
a breaking change, taken as format v2 with in-loader v1→v2 auto-migration that renders bit-identically.

Distilled from: ADR-0038, ADR-0032, ADR-0034
