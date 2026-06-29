# Typed edit-time `Constant`: a first-class config field operators read at build

## Status

Proposed (2026-06-29). **Design draft for the grilling session [#131](https://github.com/Impractical-Instruments/reuben/issues/131) (`design-question`) calls for — needs human grilling before build.** No engine/loader code lands with this ADR; it settles the semantics only, so the build can follow without re-litigating the shape.

Finishes the `Constant` carve-out [ADR-0028](0028-one-input-shape.md) specced ("a value is a `Constant` iff changing it would rebuild the graph", with shapes `Enum`/`Int`) and the narrow generic slice [#107](https://github.com/Impractical-Instruments/reuben/issues/107) landed (the loader's `voice_count` reads `descriptor.constant_param()` instead of a hardcoded `"voices"`). Sibling to [#106](https://github.com/Impractical-Instruments/reuben/issues/106) (the hosted sub-patch interface *roles* are still hardcoded) — same de-voicer-ify theme, separate thread.

## Context

[ADR-0028](0028-one-input-shape.md) split a `Constant` off from an `Input`: instantiate-time configuration that, *if changed, rebuilds the graph* — pool sizes, topology- or allocation-fixing flags. The canonical case is the Voicer's `voices`. The intent was always broader than polyphony: an **edit-time config field** any operator declares to *size pools* and *optimize away heavy things or things that allocate* (an `hq`/oversample flag, a delay line's maximum length, a reverb's buffer sizing). [#131](https://github.com/Impractical-Instruments/reuben/issues/131) is that intent.

Today the concept is half-built and still voicer-shaped:

- **One constant per operator, and it must be a numeric `param`.** `Descriptor::constant_param: Option<usize>` ([`descriptor.rs:297`](../../crates/reuben-core/src/descriptor.rs)) is a single slot indexing into `params` (`ParamMeta`, f32-valued). A Constant therefore *is* an f32, rounded — it cannot be a named enum or a bool without abuse.
- **The typed-field scaffolding is dead code.** `ConstantShape { Int, Enum }` ([`descriptor.rs:50-54`](../../crates/reuben-core/src/descriptor.rs)) exists but is never constructed or read — the typed field was sketched in 0028 and never wired in.
- **The only consumer is the loader, and only for voice fan-out.** `voice_count` ([`format.rs:412`](../../crates/reuben-core/src/format.rs)) reads the constant from the node's `config` to decide how many voice sub-graphs to build. **No operator reads its own constant** to size a buffer or skip an allocation.
- **`on_instantiate` can't see the operator's own constants.** Its signature is `on_instantiate(&mut self, config: &AudioConfig)` ([`operator.rs:414`](../../crates/reuben-core/src/operator.rs)) — `config` is sample rate / block size, not the node's resolved config values. The one place allocation is allowed (off the hot path, [ADR-0012](0012-boundary-and-threading.md)) has no access to the values that should drive it.
- **Non-numeric constants are unexercised.** `ConfigValue::Symbol` exists in the format but no operator declares an enum/bool constant.

It kept collapsing into voicer-specialization because the Voicer is the only site. This ADR settles what a Constant is as a typed field; how many an operator may have; how it is declared; **who reads it and when**; and how resolved values reach the operator.

## Decision

### 1. A Constant is its own typed field, not a borrowed numeric param

Introduce a `ConstantMeta { name, shape, default }` that carries the value's own type via the now-live `ConstantShape`, and make Constants a **separate descriptor list**, not entries in `params`:

- `Descriptor::constants: Vec<ConstantMeta>` replaces `constant_param: Option<usize>`.
- `ConstantShape` grows to the set the use cases need: `Int { min, max }`, `Enum(EnumMeta)`, `Bool`.

`Float` is deliberately **excluded** as a Constant shape: a Constant is discrete by nature (it fixes topology/allocation), and a continuously-modulatable number is a runtime `Float` Input ([ADR-0028](0028-one-input-shape.md)), not config. `Int` survives *only* here, exactly as 0028 said ("`Int` survives only as a `Constant` shape").

**Considered and rejected — keep reusing `params`.** It forces every Constant to be an f32, so a bool flag or a named enum can only be smuggled in as a magic number; and it conflates two lifetimes — a runtime-modulatable `param` read every block versus a build-fixed value read once — that 0028 split on purpose. A separate typed list is the smaller honest change.

### 2. Many constants per operator

`Vec`, not `Option<usize>`: an operator may declare a pool size **and** an `hq` flag on the same descriptor. The single-slot model cannot express the common "size it, and pick the cheap path" pair that motivates the whole feature.

### 3. Declared in a typed `constants:` block in the contract macro

Replace the `constant: <param-name>` keyword (which names an existing param, [`reuben-macros/src/lib.rs:371`](../../crates/reuben-macros/src/lib.rs)) with a typed block, single-sourced as [ADR-0025](0025-single-source-operator-contract.md):

```rust
operator_contract!(Voicer {
    // …
    constants: { voices: int { 1..=32, default 8 } },
});

operator_contract!(Echo {
    inputs:  { audio: float, time: float { 0..=2, default 0.3 }, feedback: float { 0..=1, default 0.4 } },
    outputs: { audio: float },
    constants: {
        max_delay: int  { 1..=192_000, default 96_000 },  // sizes the delay line at instantiate
        hq:        bool { default false },                // gates allocating the oversampler
    },
});
```

The macro emits one `ConstantMeta` per entry plus a per-constant index const (`C_VOICES`, `C_MAX_DELAY`, …), mirroring how it already emits `P_*` for params.

### 4. Two read points, one declaration: load-time fan-out and instantiate-time allocation

A Constant is consumed at one of two **build** moments, and the same declaration serves both:

- **Load-time (the loader reads it)** — a Constant that decides *how many resources / sub-graphs to build*. The Voicer's `voices` is read by the loader's fan-out because the registry + resolver only live at load ([ADR-0032](0032-voicer-hosts-voice-subpatches.md) §2): the loader must know the count *before* it can build that many voice graphs. Generic over `descriptor.constants`.
- **Instantiate-time (the operator reads it)** — a Constant that sizes *the operator's own state*: a delay's `max_delay`, an `hq` flag gating an oversampler allocation. Read in `on_instantiate`, off the hot path ([ADR-0012](0012-boundary-and-threading.md)).

Both are Constants — changing either rebuilds the graph — and they differ **only in who reads them and when**. This is a clarification, not two mechanisms: the Voicer's pool is the load-time case of the one general thing, not a separate concept. Naming the split is what stops the next pool-sized operator from being bolted onto the Voicer path.

### 5. Delivery to the operator: a `bind_constants` hook before `on_instantiate`

Operators are type-erased (`Box<dyn Operator>`, [ADR-0024](0024-compile-time-operator-registration.md)) and `on_instantiate` only receives `AudioConfig`. Add a pre-instantiate hook mirroring `bind_resources` / `bind_voices`:

```rust
/// Resolved, type-checked instantiate-time Constants, keyed by name. Default no-op.
fn bind_constants(&mut self, _constants: &ResolvedConstants) {}
```

The loader calls it on any node whose descriptor declares constants, handing resolved values coerced to their shape (`Int → i64`, `Enum → variant`, `Bool → bool`). The operator stashes what it needs; `on_instantiate` then allocates from it. Two-phase init becomes **resources → voices → constants → `on_instantiate`**. The default no-op keeps every operator with no constants untouched.

**Considered and rejected — extend `on_instantiate`'s signature to carry the constants.** Workable, but it reshapes the one hook every operator overrides, and bundles "what values configure me" with "the audio config is now fixed." A dedicated bind hook composes with the existing two-phase init and leaves `on_instantiate` stable.

**Considered and rejected — keep routing constants through `set_param`** (today's path: [`format.rs:511`](../../crates/reuben-core/src/format.rs) does `graph.set_param(key, name, value)` for a `ConfigValue::Number`). `set_param` is f32-only and lands the value in the **runtime** param store read during `process` — the wrong type (no enum/bool) and the wrong lifetime (per-block, not build-once) for edit-time config.

### 6. Loader: validate and coerce by shape; the fan-out reader generalizes

The loader's config pass already rejects a config key that isn't a declared Constant and rejects a Constant appearing in `inputs`. Extend it to **coerce each `ConfigValue` by the Constant's `ConstantShape`** — `Number → Int` (range-checked), `Symbol → Enum` (via `EnumMeta::resolve`, the existing symbol-primary / index-fallback binding) or `Bool` — erroring on mismatch, and to assemble the `ResolvedConstants` handed to `bind_constants`. `voice_count` becomes the load-time reader of an `Int` Constant used as a resource-fan-out count, renamed away from `voice_*` since it is no longer voicer-specific. `ConfigValue::Symbol` stops being dead.

### Out of scope

- **#106** — the hosted sub-patch interface *roles* (host-driven inputs vs summed audio out vs the `active` liveness Value) are still four string literals in the Voicer. Same theme, its own thread.
- **Runtime-switchable enums** (`filter.mode`, `osc.waveform`) — those are runtime `Enum` **Inputs** ([ADR-0028](0028-one-input-shape.md): "shape does not decide Constant-vs-Input"), live-switchable over OSC; they are not Constants.
- **A `Float` Constant** — excluded by §1.

## Consequences

- **Breaking descriptor change.** `constant_param: Option<usize>` → `constants: Vec<ConstantMeta>`; `ConstantShape` goes live as `Int`/`Enum`/`Bool`. The golden descriptor snapshot and the generated instrument schema are re-blessed; the schema's `config` block now emits one typed property per Constant (integer / enum-of-symbols / boolean) instead of today's single voicer-shaped integer.
- **New `Operator::bind_constants` hook**; two-phase init becomes resources → voices → constants → `on_instantiate`. Default no-op, so the common operator is untouched.
- **Contract macro:** the `constant: <param>` keyword is replaced by a typed `constants: { … }` block; it emits `ConstantMeta` + per-constant index consts.
- **Loader:** config values are coerced and range-/symbol-checked by shape; the fan-out reader (`voice_count`) is generalized and renamed; the `ConfigValue::Symbol` path becomes exercised.
- **The Voicer is migrated** to declare `constants: { voices: int { 1..=32, default 8 } }`; behavior is identical (its pool still fans out at load). A **second, non-voicer site lands with the build** — an `Echo`/delay `max_delay` pool and/or an `hq` allocation flag — to prove the generalization can't collapse back into voicer-special-casing. This forcing function is the acceptance bar [#131](https://github.com/Impractical-Instruments/reuben/issues/131) sets.
- **Amends** [ADR-0028](0028-one-input-shape.md) (the `Constant` gains a concrete typed representation, a multiplicity, and a delivery hook) and the [#107](https://github.com/Impractical-Instruments/reuben/issues/107) generalization (one slot → many; param-borrowed → typed). **Reinforces** [ADR-0012](0012-boundary-and-threading.md) (allocation happens at instantiate, off the hot path) and [ADR-0024](0024-compile-time-operator-registration.md)/[ADR-0025](0025-single-source-operator-contract.md) (Constants are compile-time-declared and `&'static` in the descriptor, single-sourced with their index consts).
- **Terminology:** *Constant* = a typed edit-time config field (changing it rebuilds the graph); *load-time Constant* = one the loader reads to fan out resources/sub-graphs (the Voicer's `voices`); *instantiate-time Constant* = one the operator reads in `on_instantiate` to size state or pick a cheaper path.
- **Deferred:** implementation sequencing (descriptor + macro → loader coercion → operator hook → Voicer migration → second site → docs/schema) is its own thread, not settled here.
