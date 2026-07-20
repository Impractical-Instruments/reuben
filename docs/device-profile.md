# The device profile (`--io-map`)

[The device layer](rules/composition-operators.md) is the design record; this
is the quick-reference for the JSON shape. Schema:
[`crates/reuben-native/schema/device-profile.schema.json`](../crates/reuben-native/schema/device-profile.schema.json).

## Why it exists

Patches speak only **logical** channels:
an instrument's master taps are indexed 0, 1, 2… with no idea what hardware they'll land on.
The device profile is the one place that logical→device binding is spelled out — a small JSON
file, kept outside the patch (checked in, named, versioned like any other rig asset) and loaded
with:

```sh
cargo run -p reuben-native --bin reuben -- play instruments/my-rig.json --io-map my-rig.io-map.json
```

No `--io-map` (or a profile with every field omitted) means **identity map + today's implicit
defaults** (broadcast to all logical channels, mono-downmix, zero-fill) — bit-identical to
before. The profile only changes behavior where you say so.

## Shape

```jsonc
{
  // Output device selection + channel map.
  "output": {
    "device": "Scarlett",       // case-insensitive name substring; omit for the default device
    "map": { "0": 2, "1": 3 }   // logical channel -> device channel
  },

  // Input device selection + channel map, applied when the played instrument binds input
  // channels (P5, #182) — an instrument without input pipes never opens an input device.
  "input": {
    "device": "Scarlett",
    "map": { "0": 0, "1": 1 }   // device channel -> logical channel (the reverse direction)
  },

  // Preferences, not commands: requested against the device's supported configs, and the
  // engine adopts whatever is granted (never fights the device). Omit either for the device's
  // own default.
  "sample_rate": 48000,
  "buffer_size": 256
}
```

Every field, and both top-level blocks, are optional. `output.map`/`input.map` keys are JSON
object keys, so they're spelled as integer strings — `{"0": 2}`, not `{0: 2}`.

## Mismatch policy: warn + degrade, never fatal

Once a profile parses there are two different kinds of "wrong":

- **Structural** — malformed JSON, an unknown field, a `map` key/value that isn't a
  non-negative integer. This is a **load error**: `reuben play` refuses to start. The document
  itself is broken.
- **Reality mismatch** — the document is well-formed, but reality (the actual device) doesn't
  match it: `output.map` names a device channel the real device doesn't have, or a logical
  channel the instrument doesn't produce. This **degrades**: a warning is printed and the
  offending pairing is dropped (the device channel involved falls back to silence — the
  broader zero-fill default `output.map` already applies to every unmapped device channel). A
  patch never fails to play because the rig is smaller than the author's studio.

## Output mapping, applied here

An explicit `output.map` **overrides** `audio.rs`'s implicit `map_frame` policy entirely: every
device channel not named as a `map` target is zero-filled, and every named `(logical, device)`
pair is checked once, at stream startup, against the real logical/device channel counts.
`output.device` selects the device (by case-insensitive name substring) that map is checked
against.

## Input mapping, applied by the input stream

`input.device`/`input.map` are applied by the input stream (P5,
[#182](https://github.com/Impractical-Instruments/reuben/issues/182)), which opens **only when
the played instrument binds input channels** — an instrument without input pipes never touches
an input device. `input.map` runs device→logical (the reverse direction of `output.map`), and
the same warn+degrade policy applies at stream startup: out-of-range pairs are dropped with a
warning, and a logical input channel nothing feeds reads silence. The input device runs at its
own rate; audio is resampled (with drift compensation) into the engine rate, so
mismatched-rate and dual-device setups work.

One deliberate exception to "never fatal": when the played
instrument explicitly binds input channels but **no input device exists at all** (or none
matches `input.device`), `play` fails fast instead of playing silently — the same precedent as
a missing output device.

## Sample-rate / buffer-size negotiation

`sample_rate`/`buffer_size` are requested against the output device's supported configs
(`cpal::Device::supported_output_configs`); reuben never fights the device:

- A requested sample rate the device doesn't support falls back to the device's own default,
  with a warning naming both numbers.
- A requested buffer size is clamped into the device's supported range (if it reports one),
  with a warning when clamping actually changed the value.
- Either preference the device **can** grant is logged as granted, so `play`'s startup output
  always states what rate/buffer size is actually in effect.
