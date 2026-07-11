# Restart-swap — human testing (the ~100ms audible gap)

A scripted human test for the M1 **restart-swap** (issue #317, [ADR-0046](adr/0046-coordinator-swap-engine-unit.md)
§10). The swap verb's request/response contract — validation, `SwapReport`, doc/hash update,
`expect` arbitration — is covered headlessly by
`crates/reuben-native/tests/structure_server.rs` and the `structure.rs` unit tests (ADR-0046 §8,
[ADR-0053](adr/0053-mcp-epic-verification-strategy.md) §4). What **cannot** be asserted in CI is
the device-level behavior: the actual cpal stream teardown/reopen and the deliberate ~100ms
silence gap ("documented interim rudeness", ADR-0046 §10). CI has no audio device, so — exactly
as this repo's `windows` CI job already does for anything that must open a stream — that half is
verified here, by ear, on real hardware.

This ritual is **scripted** (ADR-0053 §6): fixed commands, one perceptual judgment. It is not an
unscripted "go check it works."

> **Fixed, library-independent fixtures.** The steps below play the built-in default rig and swap
> to library instruments (`instruments/euclidean-drums.json`) — both fine, both checked in. For a
> run that is fully self-contained and pinned against drift, a fixed minimal pair lives with the M1
> harness: play `crates/reuben-native/tests/fixtures/m1/bass.json` (an always-on saw bass) and swap
> to `crates/reuben-native/tests/fixtures/m1/device-gap-swap.json` (a pure sine, obviously
> different) — substitute those for the `play` target in step 1 and the swap source in step 3. Both
> are guarded by `cargo test -p reuben-native --test m1_fixtures`. See
> `crates/reuben-native/tests/fixtures/m1/README.md` for the whole M1 verification harness (this
> device-gap ritual, the golden live-server test, and the #220 demo bar).

## What you're verifying

1. A `swap` over the structure channel replaces the running instrument **without restarting
   `reuben play`** — the process, the OSC-in socket (`0.0.0.0:9000`), and the structure channel
   (`127.0.0.1:9124`) all survive.
2. There is a short (~100ms) **audible gap** at the swap — the stop-the-world restart — and then
   the **new** instrument plays. This gap is the known M1 rudeness, not a bug.
3. Notes keep working **after** the swap (the OSC-in path re-points to the new engine).
4. The swap's `SwapReport` comes back `"ok": true`.

## Setup

You need two terminals at the repo root, and speakers/headphones. `nc` (netcat) drives the
structure channel; any NDJSON-over-TCP tool works (`ncat`, a Python one-liner, …).

```sh
cargo build -p reuben-native --bin reuben
```

## Steps

### 1. Start the default rig

Terminal A:

```sh
cargo run -p reuben-native --bin reuben -- play
```

Wait for these lines (they confirm all three surfaces are up):

```
audio out @ <rate> Hz, block 256
OSC-in listening on 0.0.0.0:9000  (send /voicer/notes [midi, gate])
structure channel on 127.0.0.1:9124
playing — Ctrl-C to quit.
```

### 2. Hold a note so the gap is obvious

Terminal B — send a **note-on** and leave it held (the default voice sustains on gate 1). You
need an OSC sender; the repo's `control-surface` skill or any OSC tool works. With `oscsend`
(liblo):

```sh
oscsend 127.0.0.1 9000 /voicer/notes ff 69.0 1.0     # A4, gate on — a sustained tone
```

You should now hear a steady tone.

### 3. Swap the whole rig — by path

Terminal B — send the swap over the structure channel and read the reply:

```sh
printf '{"verb":"swap","source":{"path":"instruments/euclidean-drums.json"}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** the sustained A4 cuts, there is a brief (~100ms) silence — the restart — and then the
euclidean-drums rig begins (its clock drives the pattern on its own). The held note is **gone**:
M1 restart is all-cold (`survived: 0`), so nothing carries across.

**Read:** `nc` prints one line — a `SwapReport`. Confirm `"ok":true` and a `diff` with
`"survived":0`. For example:

```
{"reply":"swap_report","ok":true,"errors":[],"warnings":[...],"content_hash":"...","diff":{"survived":0,"state_reset":[...],"added":[...],"removed":[...]}}
```

### 4. Confirm notes still work post-swap (optional)

The euclidean-drums rig self-drives, but you can confirm the OSC-in path survived by driving its
pipes (see `instruments/euclidean-drums.json`'s interface). Any accepted OSC that audibly changes
the pattern proves the receiver re-pointed to the new engine.

### 5. Swap again — by value (inline document)

Prove the by-value branch and that the channel takes back-to-back swaps. This inline document is
a bare 330 Hz oscillator through a master output:

```sh
printf '{"verb":"swap","source":{"document":{"format_version":3,"instrument":"swapped-inline","interface":{"outputs":{"main":{"from":"/out.audio"}}},"nodes":[{"type":"oscillator","address":"/osc","inputs":{"freq":330.0}},{"type":"output","address":"/out","inputs":{"audio":{"from":"/osc.audio"}}}]}}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** another brief gap, then a steady 330 Hz tone (the drums stop). `SwapReport.ok` is
`true` again.

### 6. Confirm a bad document is refused with no gap (optional)

A document that fails to load must **not** restart audio — the current instrument keeps playing,
uninterrupted, and the reply reports the error:

```sh
printf '{"verb":"swap","source":{"document":{"format_version":3,"instrument":"bad","nodes":[{"type":"no_such_operator","address":"/x"}]}}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** the 330 Hz tone continues **without any gap** (retain-prior). **Read:** the reply is a
`SwapReport` with `"ok":false` and a non-empty `errors` array; its `content_hash` is unchanged
from step 5.

### 7. Shut down

Terminal A: `Ctrl-C`. `play` shuts down cleanly (`shutting down…`, then exit) — the structure
channel joins its threads and the streams stop.

## Pass criteria

- Steps 3 & 5: a brief (~100ms) audible gap, then the new instrument plays; `SwapReport.ok` is
  `true`; `reuben play` never exited.
- Step 6: **no** gap; the prior instrument keeps playing; the reply is `ok:false` with errors.
- Step 7: clean shutdown.

If the gap is much longer than ~100ms, or clicks/pops beyond the single gap are audible on every
swap, note it — the interim rudeness is *one* short gap, and a fixed master-gain ramp to smooth it
is a later rung ([ADR-0050](adr/0050-swap-sonic-rudeness-ramp.md)), not M1.
