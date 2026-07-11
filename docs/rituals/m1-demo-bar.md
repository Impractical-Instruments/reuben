# Ritual: the #220 M1 demo bar — "make the bass rounder and add a dub delay"

The epic's top-level acceptance ritual (ADR-0053 §5, [issue #220](https://github.com/Impractical-Instruments/reuben/issues/220)),
scripted for the **M1** bar. A human plays a fixed starting instrument, hands the conversational
agent one fixed prompt, and judges — by ear — whether the edit landed. The setup is scripted so
the run reproduces bar to bar; the **judgment stays human**, because "rounder" and "the delay is
there" are perceptual calls no automated gate should stand in for.

**The M1 bar is deliberately lower than the epic's final bar:** the edit lands via an M1
**restart-swap**, so a brief (~100 ms) stop-the-world gap at the swap is **tolerated** (ADR-0046
§10). The seamless, transport-never-stops version is the M2 mailbox-swap bar
(`docs/rituals/m2-swap-ramp-duck.md`, ADR-0050) — not this one. Here, all that must be true is:
you hear the bass change into the thing you asked for, and `reuben play` never restarts.

## The fixed fixture (checked in — do not improvise)

- **Starting instrument:** `crates/reuben-native/tests/fixtures/m1/bass.json` — an always-on 55 Hz
  sawtooth bass through a gentle low-pass. It sounds the instant the device opens (no note to
  send), so you have a steady reference to judge the edit against. It is a raw saw with an open-ish
  filter and no delay, so there is obvious room for "rounder" and "add a dub delay" to be audible.
- **Prompt (verbatim):** `crates/reuben-native/tests/fixtures/m1/prompt.txt` —

  > make the bass rounder and add a dub delay

Both are guarded by `cargo test -p reuben-native --test m1_fixtures` (the bass loads + plans; the
prompt text is pinned), so this ritual can't silently rot.

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

Wait for the three-surface confirmation and listen: a steady, buzzy low bass drone.

```
audio out @ <rate> Hz, block 256
OSC-in listening on 0.0.0.0:9000  (send /voicer/notes [midi, gate])
structure channel on 127.0.0.1:9124
playing — Ctrl-C to quit.
```

### 2. Open the conversation and hand over the prompt

Start (or reconnect) your MCP client so it spawns `reuben-mcp` against the running engine. Then
paste the prompt **verbatim** from `prompt.txt`:

> make the bass rounder and add a dub delay

Hand the agent nothing else — no hints about operators, nodes, or the `swap` tool. Reproducibility
lives in giving every run the same fixed start and the same fixed words.

### 3. Let the agent land the edit

The agent reads the current document (`get_document`), authors a whole edited document, and
installs it with the `swap` tool. Under M1 that is a restart-swap: the audio streams stop and
reopen on the new document.

### 4. Listen at the swap

- A brief (~100 ms) **gap** as the streams restart — **expected** at M1, not a failure.
- Then the **edited** bass: rounder than the raw saw (the filter closed down / softened) and now
  carrying an audible **dub delay** — repeats trailing the bass with feedback (the engine has a
  `delay` operator with `time` / `feedback` / `mix`).

## Pass criteria (human judgment)

- [ ] **Rounder.** The post-swap bass is perceptibly rounder/softer than the starting raw saw.
- [ ] **Dub delay present.** You can hear delay repeats / feedback that were not there before.
- [ ] **The edit landed via restart-swap.** A single brief (~100 ms) gap is fine; `reuben play`
      **never exited** (same process, same terminal, structure channel and OSC socket survived).
- [ ] **Not a crash or silence.** After the gap there is sound — the new instrument, not dead air.

If the agent's `swap` came back `ok:false`, that is the agent's authored document failing
validation, not a harness failure — read the `errors`, and (this being a demo of the conversational
loop) let the agent try again. A longer-than-~100 ms gap, or clicks beyond the single restart gap,
is worth noting: smoothing that gap is the M2 rung ([ADR-0050](../adr/0050-swap-sonic-rudeness-ramp.md)),
not M1.

### 5. Shut down

Terminal A: `Ctrl-C`. `play` shuts down cleanly (`shutting down…`, then exit).

## Why this is scripted, not automated (ADR-0053 §§5–6)

The setup — the exact starting document and the exact prompt — is pinned so the scenario is the
same every time. What is **not** automated is the ear: whether it sounds rounder and whether the
delay is musically present are perceptual judgments, deliberately the one human check sitting above
every automated per-ticket gate. An LLM-judged audio diff was considered and rejected (ADR-0053 §5)
as over-engineering the epic's one intentionally-human acceptance ritual.
