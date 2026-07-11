# Ritual: the #220 M2 demo bar — "make the bass rounder and add a dub delay", gaplessly

The epic's **terminal** acceptance ritual (ADR-0053 §5, [issue #220](https://github.com/Impractical-Instruments/reuben/issues/220)) —
the same top-level bar as the M1 demo (`docs/rituals/m1-demo-bar.md`), raised to its final height.
A human plays a fixed starting instrument, hands the conversational agent one fixed prompt, and
judges — by ear — whether the edit landed **and whether the transport ever audibly stopped**. The
setup is scripted so the run reproduces bar to bar; the **judgment stays human**, because "rounder,"
"the delay is there," and "the music didn't stop" are perceptual calls no automated gate should
stand in for.

**This is the highest bar in the epic.** Every automated per-ticket gate — including the
Coordinator-direct survivor/reset harness and the install-path allocation counter
(`cargo test -p reuben-core --test m2_swap_harness`, ADR-0053 §§2–3) and the ramp-math /
RT-safety harnesses (`install_slot`, `install_slot_rt_safe`) — sits **below** this ritual. They
prove the swap is behaviorally correct and RT-safe off-device; this ritual is the one thing they
cannot judge: whether, on real hardware, the whole conversational loop *feels* seamless.

## What is different from the M1 bar

The M1 bar (`docs/rituals/m1-demo-bar.md`) tolerated a brief (~100 ms) **stop-the-world gap** at the
swap: M1's restart-swap tore down the cpal streams, rebuilt the Engine, and reopened (ADR-0046 §10).
**M2 replaces the machinery behind the same `swap` verb** ([#347](https://github.com/Impractical-Instruments/reuben/issues/347)):
the cpal callback now drives `reuben_core::coordinator::RenderSlot::fill_duplex`, which installs the
new Engine at a block boundary behind a fixed **~20 ms raised-cosine master-gain duck** (ADR-0050)
and box-transplants survivor nodes across the swap (ADR-0046 §4). **There is no stream teardown.**

So the M2 bar adds one make-or-break criterion the M1 bar deliberately did not have: **the transport
must not audibly stop.** The edit should arrive under a soft ~20 ms duck — a declick, not a silence —
and anything still sounding on a **survivor** node (same address + type + config + resolved
resources, ADR-0046 §5) rings straight through it.

## The fixed fixture (checked in — do not improvise)

The **same** fixture as the M1 bar, on purpose: only the swap machinery underneath has changed, so
holding the starting instrument and the prompt fixed makes the M1→M2 difference the *only* variable
you are judging.

- **Starting instrument:** `crates/reuben-native/tests/fixtures/m1/bass.json` — an always-on 55 Hz
  sawtooth bass through a gentle low-pass. It sounds the instant the device opens (no note to send),
  so you have a steady reference to judge the edit against, and — because M2 does not stop the
  transport — a steady drone whose **continuity across the swap** you can hear directly. It is a raw
  saw with an open-ish filter and no delay, so there is obvious room for "rounder" and "add a dub
  delay" to be audible.
- **Prompt (verbatim):** `crates/reuben-native/tests/fixtures/m1/prompt.txt` —

  > make the bass rounder and add a dub delay

Both are guarded by `cargo test -p reuben-native --test m1_fixtures` (the bass loads + plans; the
prompt text is pinned), so this ritual — which reuses them — can't silently rot either.

## Setup

You need speakers/headphones and an MCP client (the conversational door) configured to launch the
reuben sidecar. All addresses are the shipped defaults: the structure channel on loopback TCP
`127.0.0.1:9124`, OSC control on UDP `127.0.0.1:9000` (ADR-0044, ADR-0046 §8).

```sh
cargo build -p reuben-native --bin reuben
cargo build -p reuben-mcp --bin reuben-mcp
```

Point your MCP client at the built `reuben-mcp` binary. Per ADR-0044 it is a stdio shim the client
spawns per conversation; on startup it dials the running `reuben play` over the structure channel
and OSC, so start `play` **first**.

## Run

### 1. Play the fixed bass

```sh
cargo run -p reuben-native --bin reuben -- play crates/reuben-native/tests/fixtures/m1/bass.json
```

Wait for the three-surface confirmation and listen: a steady, buzzy low bass drone. Keep it playing —
its continuity is half of what you are about to judge.

```
audio out @ <rate> Hz, block 256
OSC-in listening on 0.0.0.0:9000  (send /voicer/notes [midi, gate])
structure channel on 127.0.0.1:9124
playing — Ctrl-C to quit.
```

### 2. Open the conversation and hand over the prompt

Start (or reconnect) your MCP client so it spawns `reuben-mcp` against the running engine. Then paste
the prompt **verbatim** from `prompt.txt`:

> make the bass rounder and add a dub delay

Hand the agent nothing else — no hints about operators, nodes, or the `swap` tool. Reproducibility
lives in giving every run the same fixed start and the same fixed words.

### 3. Let the agent land the edit

The agent reads the current document (`get_document`), authors a whole edited document, and installs
it with the `swap` tool. Under M2 that is a **mailbox swap**: the Coordinator builds the new Engine
off-thread and hands it to the render callback, which installs it behind the ~20 ms duck. **The audio
stream is never torn down** — same device, same callback, the drone continues under the dip.

### 4. Listen at the swap — this is the whole point

- **No gap.** Instead of the M1 ~100 ms silence, a soft ~20 ms **duck** and recovery — a declick, not
  a stop. If you weren't listening for it you might barely notice the level dip.
- Then the **edited** bass: rounder than the raw saw (the filter closed down / softened) and now
  carrying an audible **dub delay** — repeats trailing the bass with feedback (the engine has a
  `delay` operator with `time` / `feedback` / `mix`).

## Pass criteria (human judgment)

- [ ] **Rounder.** The post-swap bass is perceptibly rounder/softer than the starting raw saw.
- [ ] **Dub delay present.** You can hear delay repeats / feedback that were not there before.
- [ ] **The transport did NOT audibly stop.** The edit arrived under a soft ~20 ms duck, **not** the
      ~100 ms stop-the-world gap of the M1 bar. This is the criterion that distinguishes the M2 bar
      from M1 — if you hear an M1-length silence, the swap did not take the mailbox path.
- [ ] **Seamless, not a click.** The duck is a smooth dip to silence and back — no click, pop, or
      zipper noise on either edge (ADR-0050 §2/§3).
- [ ] **`reuben play` never restarted.** Same process, same terminal; the structure channel and OSC
      socket survived (as they did at M1 — but now the audio stream survived too).
- [ ] **Not a crash or silence.** After the duck there is sound — the new instrument, not dead air.

If the agent's `swap` came back `ok:false`, that is the agent's authored document failing validation,
not a harness failure — read the `errors`, and (this being a demo of the conversational loop) let the
agent try again. A note held over the swap that **hangs** afterwards is the documented, recoverable
hanging-note window (ADR-0050 §5), not a bug — re-send the note-off; the fixed bass has no gated note,
so you are unlikely to hit it here.

### 5. Shut down

Terminal A: `Ctrl-C`. `play` shuts down cleanly (`shutting down…`, then exit).

## Why this is scripted, not automated (ADR-0053 §§5–6)

The setup — the exact starting document and the exact prompt — is pinned so the scenario is the same
every time. What is **not** automated is the ear: whether it sounds rounder, whether the delay is
musically present, and whether the transport stayed continuous are perceptual judgments, deliberately
the one human check sitting above every automated per-ticket gate. An LLM-judged audio diff was
considered and rejected (ADR-0053 §5) as over-engineering the epic's one intentionally-human
acceptance ritual.

For the **pure ramp/duck mechanics** in isolation (a synthetic one-param edit driven over the wire,
without the conversational agent), see `docs/rituals/m2-swap-ramp-duck.md`. This ritual is the
end-to-end acceptance bar; that one is the focused perceptual check on the ramp itself.
