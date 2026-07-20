# Why: An operator declares its ports, constants, and metadata once in `operator_contract!`, which emits both the typed port handles and the runtime `Descriptor` from the same tokens.

[Rule](../../composition-operators.md#single-source-contract)

Every operator used to declare its ports **twice**: by name in `descriptor()` (a runtime `Vec<Port>`),
and by integer slot in a hand-written `IN_/OUT_/P_` const block, with nothing checking the two against
each other. The slot space is per-kind, so `voicer.rs` legitimately had `IN_NOTES = 0` *and*
`IN_CTX = 0` — a wrong ordinal, or a const that drifts from the descriptor after an edit, compiled
fine and failed silently at runtime. This is the same disease self-registration cured for
*registration* ([operator-self-registration](operator-self-registration.md)), one layer down: a fact
stated in two places that must agree. The cure is the same shape — **declare once, generate both
halves**, so disagreement is a compile error, not a runtime surprise. The consts and the descriptor
`Vec`s are computed from the **same tokens** by one macro pass, so name↔slot drift is impossible by
construction ([macros/lib.rs](../../../../crates/reuben-macros/src/lib.rs)).

Two structural constraints shaped the form. **Shape A (delegate):** `descriptor()` and `process()` are
both required methods of one `impl Operator` block, and a declarative macro cannot inject a method
into a hand-written impl (two `impl` blocks is a duplicate-impl error). So the macro emits an
*inherent* `impl T { fn contract() -> Descriptor }` at module scope, and the trait impl delegates with
a one-liner — the macro is contract-only and never tries to own the DSP body. **Three crates, no
cycle:** the shared spec types and the one `validate()` live in a pure leaf, `reuben-contract`,
because a proc-macro crate can only export macros (the scaffold could not call validators living in
`reuben-macros`) and the macro cannot depend on `reuben-core` without a cycle. One validator, imported
by both the macro and the scaffold, so they cannot themselves drift — the disease, recursively.

The contract has since **grown** rather than been replaced: it emits typed port *handles*, not bare
consts ([typed-port-handles](typed-port-handles.md)), and `params:` was swapped for a `constants:`
block ([constants-are-immutable-ports](constants-are-immutable-ports.md)) — but the single-source
principle, and its greppability trade (`grep IN_FREQ` lands on the `operator_contract!` call, the
census of an operator's ports), are what endure. The stateless-pointwise math family single-sources
even further, generating a whole operator family from one scalar fn
([pointwise-number-operators](pointwise-number-operators.md)).

Distilled from: ADR-0025
