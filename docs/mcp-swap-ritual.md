# Swap — human testing (the M2 gapless mailbox swap)

A scripted human test for the **M2 mailbox swap** (issue #323,
[ADR-0046](adr/0046-coordinator-swap-engine-unit.md) §§1–10). The swap verb's request/response
contract — validation, `SwapReport`, doc/hash update, `expect` arbitration, **real survivor
stats** — is covered headlessly by `crates/reuben-native/tests/structure_server.rs` and the
`structure.rs` unit tests ([ADR-0053](adr/0053-mcp-epic-verification-strategy.md) §4). What
**cannot** be asserted in CI is the device-level behavior: that the swap installs **gaplessly via
the mailbox** with **no stream teardown** — the streams are fixed at `play` start (ADR-0046 §6), and
a swap box-transplants survivors under a ~20ms master-gain ramp
([ADR-0050](adr/0050-swap-sonic-rudeness-ramp.md)). CI has no audio device, so — exactly as this
repo's `windows` CI job already does for anything that opens a stream — that half is verified here,
by ear, on real hardware.

> **Supersedes M1's restart-swap ritual.** M1's stop-the-world restart (drop streams, reopen, ~100ms
> silence, every node cold) is **deleted**. There is no ~100ms gap anymore. This ritual verifies the
> swap works end-to-end over the channel and is gapless; for the fine perceptual judgment of the duck
> shape and survivor ring-through, see [`docs/rituals/m2-swap-ramp-duck.md`](rituals/m2-swap-ramp-duck.md).

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
   (`127.0.0.1:9124`) all survive — **and the cpal streams are never torn down** (a swap is a mailbox
   install, not a restart).
2. There is **no ~100ms gap**: the swap is gapless (at most a ~20ms duck, ADR-0050), then the new
   instrument plays.
3. Notes keep working **across and after** the swap: the *same* OSC-in receiver and callback stay
   live (M2 never re-points them), and a **survivor** node keeps its held note ringing through the
   swap.
4. The swap's `SwapReport` comes back `"ok": true` with **real survivor/reset stats** — `survived`
   can be **> 0** for a node that survives (M1's blanket `"survived": 0` is gone).

## Setup

Two terminals at the repo root, and speakers/headphones. `nc` (netcat) drives the structure channel;
any NDJSON-over-TCP tool works.

```sh
cargo build -p reuben-native --bin reuben
```

## Steps

### 1. Start the default rig

Terminal A:

```sh
cargo run -p reuben-native --bin reuben -- play
```

Wait for these lines (all three surfaces up):

```
audio out @ <rate> Hz, block 256
OSC-in listening on 0.0.0.0:9000  (send /voicer/notes [midi, gate])
structure channel on 127.0.0.1:9124
playing — Ctrl-C to quit.
```

### 2. Hold a note so a survivor is audible

Terminal B — a **note-on**, left held (the default voice sustains on gate 1). Any OSC sender works;
with `oscsend` (liblo):

```sh
oscsend 127.0.0.1 9000 /voicer/notes ff 69.0 1.0     # A4, gate on — a sustained tone
```

You should hear a steady tone. Leave it ringing.

### 3. Survivor swap — edit the running rig in place

Prepare a **lightly edited copy** of the default rig that keeps a node's address, type, `config`
constants, and resolved resources unchanged (so it stays a **survivor**, ADR-0046 §5) — e.g. copy the
played instrument and nudge a filter cutoff or a level:

```sh
# In another shell: dump what's playing, edit, save as edited.json (or hand-write a tweaked copy).
printf '{"verb":"get_document"}\n' | nc -q1 127.0.0.1 9124
# ...save an edited copy to /tmp/edited.json with only a param changed...
printf '{"verb":"swap","source":{"path":"/tmp/edited.json"}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** the held A4 keeps ringing straight through — **no ~100ms silence**, at most a soft ~20ms
duck. The edit is audible after the duck (the point of the loop). This is the box transplant + ramp.

**Read:** `nc` prints one `SwapReport` line. Confirm `"ok":true` and a `diff` whose `"survived"` is
**greater than 0** (the survivor voicer and any unchanged nodes). For example:

```
{"reply":"swap_report","ok":true,"errors":[],"warnings":[...],"content_hash":"...","diff":{"survived":N,"state_reset":[...],"added":[...],"removed":[...]}}
```

### 4. Reset swap — swap to a different rig

Terminal B — swap the whole rig to something unrelated:

```sh
printf '{"verb":"swap","source":{"path":"instruments/euclidean-drums.json"}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** the tone gives way to the euclidean-drums pattern **gaplessly** (a ~20ms duck, not a
~100ms silence). The held note is gone — none of its nodes survive into the new rig, so they reset;
the cut lands under the duck, so there's **no click**. `SwapReport.ok` is `true`.

### 5. Swap again — by value (inline document)

Prove the by-value branch and back-to-back swaps (the mailbox turns over each time):

```sh
printf '{"verb":"swap","source":{"document":{"format_version":3,"instrument":"swapped-inline","interface":{"outputs":{"main":{"from":"/out.audio"}}},"nodes":[{"type":"oscillator","address":"/osc","inputs":{"freq":330.0}},{"type":"output","address":"/out","inputs":{"audio":{"from":"/osc.audio"}}}]}}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** another gapless transition, then a steady 330 Hz tone. `SwapReport.ok` is `true`.

### 6. A bad document is refused with no gap (optional)

A document that fails to load must **not** touch audio — the current instrument keeps playing,
uninterrupted:

```sh
printf '{"verb":"swap","source":{"document":{"format_version":3,"instrument":"bad","nodes":[{"type":"no_such_operator","address":"/x"}]}}}\n' | nc -q1 127.0.0.1 9124
```

**Listen:** the 330 Hz tone continues **without any duck** (retain-prior). **Read:** a `SwapReport`
with `"ok":false`, a non-empty `errors` array, and the `content_hash` unchanged from step 5.

### 7. Shut down

Terminal A: `Ctrl-C`. `play` shuts down cleanly (`shutting down…`, then exit): the structure channel
joins its threads (reclaiming the last retired engine off-thread) and the streams stop.

## Pass criteria

- Steps 3–5: **no ~100ms gap** — a gapless transition (at most a ~20ms duck), then the new
  instrument plays; `SwapReport.ok` is `true`; step 3's `diff.survived` is `> 0` and the held note
  rings through; `reuben play` never exited and never reopened its streams.
- Step 6: **no** duck at all; the prior instrument keeps playing; the reply is `ok:false` with errors.
- Step 7: clean shutdown.

If you hear a ~100ms silence on any swap, or clicks/pops on the duck edges, note it — the M2 swap is
gapless by construction (mailbox install, no stream teardown) and the duck is a fixed raised-cosine
ramp (ADR-0050). A stop-the-world gap would mean the mailbox path did not run.
