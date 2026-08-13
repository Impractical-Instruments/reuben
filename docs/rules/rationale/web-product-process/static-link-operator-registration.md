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
drop. Confirmed at `codegen-units = 256`, at `lto = "fat"` + `opt-level = "s"`, and at both together
— 79 operators each time, including from an out-of-tree crate depending on `reuben-core` by path,
which is the embedder scenario above.

Two caveats for anyone re-reading this. The **36-of-53 measurement is old** and does not reproduce:
the same out-of-tree probe at `codegen-units = 256` against the pre-change `inventory` tree yields the
full set on the currently pinned toolchain. And `[profile.bench] codegen-units = 1` **stays** — it
pins codegen determinism for the instruction-count perf gate, an unrelated reason
([perf-benchmark-gate](perf-benchmark-gate.md)), as the paragraph above already noted.

Distilled from: ADR-0040
