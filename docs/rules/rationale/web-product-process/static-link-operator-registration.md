# Why: Any statically-linked or wasm embedder of reuben-core builds it at codegen-units = 1 so every operator's self-registration constructor survives linking.

[Rule](../../web-product-process.md#static-link-operator-registration)

Operator self-registration (`inventory`) plants a constructor in the object file of whatever codegen
unit holds the operator. In a default release build rustc splits `reuben-core` into many CGUs, and
**the linker only pulls an rlib's object files whose symbols are referenced** — a CGU containing
nothing but operator impls and their ctors is silently dropped, and those operators simply do not
exist at runtime. This was observed concretely: 36 of 53 operators registered, with `oscillator`,
`voicer`, and `clock` among the missing. Pinning `codegen-units = 1` puts one object per crate,
always pulled, every ctor linked.

The reason this earns a standing rule rather than a code comment is that the failure is **silent and
misdirected**: it surfaces as a broken registry (a patch referencing a "missing" operator), never as
a broken link, so a wasm or other statically-linked embedder of `reuben-core` will burn hours in the
wrong place. It belongs to anyone building core into a single statically-linked artifact,
which the [C-ABI browser boundary](wasm-c-abi-boundary.md) invites third parties to do. Note this is
a *release/embed* concern; the benchmark harness pins the same flag for an unrelated reason (see
[perf-benchmark-gate](perf-benchmark-gate.md)).

**ADR-0084 retires this rule, on the mechanism rather than on a re-measurement.** Registration no
longer plants a constructor anywhere: the built-in set is a plain `const` array that
`Registry::builtin()` reads, so every operator's descriptor and constructor `fn` pointer is reachable
from a symbol the caller names and there is no "codegen unit nothing references" for the linker to
drop.

**This rule was live and load-bearing until the commit that retired it** — the measurement above is
not a historical curiosity. Reproducing it needs the link shape this rule names, and only that shape:
a Rust crate with a path dependency cannot show the hazard, because rustc drives that link and hands
the linker the whole rlib, so no object file is ever left unextracted. Built as the rule describes
instead — `--crate-type staticlib`, an `extern "C"` entry calling only `Registry::builtin()`,
consumed by a C `main` through the system linker, on the pinned toolchain — the `inventory` tree
registers 79 operators at `codegen-units = 1` and at `16`, and **0** at `256`. The same tree as a
`wasm32-unknown-unknown` `cdylib` at `256` keeps **1** of 79 operator names in a 39 KB image. The
census tree registers all 79 in every one of those configurations, which is what makes the rule
removable rather than merely absorbed.

One thing that **stays**: `[profile.bench] codegen-units = 1` pins codegen determinism for the
instruction-count perf gate, an unrelated reason ([perf-benchmark-gate](perf-benchmark-gate.md)), as
the paragraph above already noted.

Distilled from: ADR-0040
