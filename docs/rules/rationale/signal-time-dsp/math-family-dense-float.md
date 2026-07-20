# Why: Every math op is a dense Float-to-Float operator authored as one operator per module, and the shared Number-trait core is retired.

[Rule](../../signal-time-dsp.md#math-family-dense-float)

The math family once lived in one `math.rs` file behind a `Number` trait and a `signal_pointwise!`
macro: each op's arithmetic written once against the trait, per-domain shells generated so the Message
and Signal domains could not drift. Two forces dissolved that core. First, the **boundary was
undefined and the name misled** — a `Number` trait in a `math` file reads as "all arithmetic lives
here," but it never did (`power` is unambiguously math yet couldn't be emitted by the symmetric-binary
macro), so nothing documented why some ops were in the file and some weren't, and the next curve op was
a coin-flip. Second, the trait's **premises were gone**: after the one-input-shape unification there is
**one runtime number type** (`Float`), and the param-vs-input split is gone — so there is nothing left
for the trait to abstract over and no second domain to keep in sync.

The resolution: **`math.rs` is deleted**, the `Number` trait and `signal_pointwise!` are removed, and
every math op (`add`, `mul`, `map`, `differentiate`, `integrate`, `power`) becomes a dense
`Float`→`Float` operator authored the standard way — **one operator per module**, its `shape` naming
its family. The "is it a math op?" boundary disappears by deletion: every op is a file. The op's scalar
math stays a tiny pure fn and `process` is the dense-buffer shell over it — the "write the math once"
instinct kept for the *right* reason (carrier reuse — a future sparse-`Float` or `Note`-field shell
reuses the same fn), not type abstraction, which is gone. **What has since moved on:** the per-domain
carriers are no longer hand-written per file — they are generated from that one scalar fn by the
`number_operator_contract!` macro, so `add`/`mul`/`power`/`map` each emit a signal carrier and a value
carrier from a single declaration. The durable decisions this rule fixes — **one operator per module**
and **the retired `Number` core** — are what survive that; the per-op generation mechanism is settled
elsewhere. (Carrier overload — one op over dense `Float`, sparse `Float`, or a `Note`'s numeric fields
— is the parked follow-up the scalar-fn + shell seam is chosen to keep additive.)

Distilled from: ADR-0029
