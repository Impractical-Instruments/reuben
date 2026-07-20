# Why: Any statically-linked or wasm embedder of reuben-core builds it at codegen-units = 1 so every operator's self-registration constructor survives linking.

[Rule](../../web-product-process.md#static-link-operator-registration)

Operator self-registration (`inventory`) plants a constructor in the object file of whatever codegen
unit holds the operator. In a default release build rustc splits `reuben-core` into many CGUs, and
**the linker only pulls an rlib's object files whose symbols are referenced** — a CGU containing
nothing but operator impls and their ctors is silently dropped, and those operators simply do not
exist at runtime. This was observed concretely: 36 of 53 operators registered, with `oscillator`,
`voicer`, and `clock` among the missing; an earlier spike had seen all 53 by CGU-partitioning *luck*,
not by construction. Pinning `codegen-units = 1` puts one object per crate, always pulled, every ctor
linked.

The reason this earns a standing rule rather than a code comment is that the failure is **silent and
misdirected**: it surfaces as a broken registry (a patch referencing a "missing" operator), never as
a broken link, so a wasm or other statically-linked embedder of `reuben-core` will burn hours in the
wrong place. The web shell that first hit this has since left for the product repo, but the trap did
not leave with it — it belongs to anyone building core into a single statically-linked artifact,
which the [C-ABI browser boundary](wasm-c-abi-boundary.md) invites third parties to do. Note this is
a *release/embed* concern; the benchmark harness pins the same flag for an unrelated reason (see
[perf-benchmark-gate](perf-benchmark-gate.md)).

Distilled from: ADR-0040
