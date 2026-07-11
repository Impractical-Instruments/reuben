# Ritual: the M2 swap master-gain duck (ADR-0050)

A **scripted human test** (ADR-0053 §§4–6): the perceptual half of ticket #321's verification.
Automation proves the ramp math (the `master_dips_to_zero_and_recovers_*` behavioral harness) and
the RT-safety (the `install_slot_rt_safe` allocation counter); what a machine can't judge is whether
the duck *sounds* right on a real device. This ritual scripts the setup so the run reproduces; the
**judgment stays human**.

**What you are listening for (ADR-0050 §2/§3):** a swap should duck the master to silence and back
over a **~20ms raised-cosine fade** — a smooth declick, *not* a click/pop, and *not* the ~100ms
stop-the-world silence of M1 restart-swap (ADR-0046 §10). A held note on a **survivor** node keeps
ringing straight through the up-ramp (edit-while-playing); the transport never audibly stops.

---

## Precondition — gated on #323

As of #321 the master-gain ramp lives in `reuben_core::coordinator::RenderSlot`, but the **native
shell does not drive it yet**: `reuben play`'s structure channel still answers `swap` with
"unimplemented" (`crates/reuben-native/src/structure.rs`). **This ritual becomes runnable once #323
flips the native `swap` verb onto the mailbox/slot path** (`RenderSlot::fill` in the audio callback,
`Coordinator::swap_document` on the channel thread).

Until then you can only hear the automated proof indirectly:

```sh
# The ramp envelope + survivor ring-through, asserted on the rendered buffer (no device):
cargo test -p reuben-core --test install_slot
# Zero heap alloc/free across a full swap callback (own binary; run isolated):
cargo test -p reuben-core --test install_slot_rt_safe -- --test-threads=1
```

When #323 has landed, run the ritual below.

---

## Setup

You need an OSC sender (`oscsend` from `liblo-utils`/`liblo-tools`, or any OSC app) and `nc`
(netcat). All addresses are the shipped defaults: OSC control on UDP `0.0.0.0:9000`, the structure
channel on loopback TCP `127.0.0.1:9124` (ADR-0046 §8).

1. **Play an instrument with a sustaining voice** (opens your default audio device):

   ```sh
   cargo run -p reuben-native --bin reuben -- play instruments/chord-player.json
   ```

2. **Hold a note** so a survivor voice is ringing when the swap lands:

   ```sh
   oscsend 127.0.0.1 9000 /voicer/notes ff 60.0 1.0
   ```

   You should now hear a sustained tone. Leave it ringing.

## Run

3. **Swap the document** over the structure channel while the note sustains. Point the swap at a
   *lightly edited* copy of the same instrument (change a param on a node whose address + type +
   config are unchanged, so that node stays a **survivor** and its held note keeps sounding):

   ```sh
   printf '{"verb":"swap","source":{"path":"instruments/chord-player-edited.json"}}\n' \
     | nc 127.0.0.1 9124
   ```

   (Prepare `chord-player-edited.json` first: copy `chord-player.json` and nudge, e.g., a filter
   cutoff or delay time — anything that does **not** change a node's address, type, `config`
   constants, or resolved resources, per the survivor key ADR-0046 §5.)

4. **Listen at the moment the swap lands.**

## Pass criteria (human judgment)

- [ ] **Clean duck, not a click.** The output briefly ducks to silence and comes back — a soft
      ~20ms dip, no click, pop, or zipper noise on either edge.
- [ ] **Survivor rings through.** The held note is still sounding after the duck — it was not
      re-triggered or cut. (This is the audible face of the box transplant, ADR-0046 §4.)
- [ ] **Transport does not stop.** The dip is momentary (~20ms), clearly *not* the ~100ms
      stop-the-world gap of M1 restart-swap.
- [ ] **The edit took.** The swapped-in change is audible after recovery (the point of the loop).

## Variations worth a listen

- **Non-survivor cut (ADR-0050 §4).** Swap to a document that *renames* or *retypes* the ringing
  node. Its old voice is cut and a fresh cold box replaces it — but the cut lands at master-zero, so
  you should hear the duck, then silence-then-new-sound, with **no click** at the cut.
- **Hanging-note window (ADR-0050 §5, accepted).** Send a note-**off** in the same instant as the
  swap; the off can be lost in the discard window and leave the note hanging. This is documented,
  recoverable behavior — re-send the off (`oscsend 127.0.0.1 9000 /voicer/notes ff 60.0 0.0`). Do
  **not** file this as a bug.
