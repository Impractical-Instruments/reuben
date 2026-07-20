# M1 verification harness fixtures

The checked-in fixtures for the M1 milestone's verification harness. They sit
next to the golden live-server test they belong with, so the epic's M1 acceptance materials — the
automated wire test and the two scripted human rituals — are one co-located set that reproduces run
to run.

## The three parts

**(a) Automated — the golden-pinned live-server test.** `../../structure_golden.rs` starts the real
structure-channel server in-process (device-free, no cpal) and drives all four verbs over a raw TCP
NDJSON client, pinning each response's exact wire bytes as goldens under `../../golden/`. This is
the self-verifying deliverable; it runs in `cargo test --workspace`. Its companion
`../../structure_server.rs` asserts the same channel's behavior field-by-field.

**(b) Scripted human — the restart-swap device-gap ritual.** `docs/mcp-swap-ritual.md` scripts the
one thing CI cannot see: the actual cpal stream teardown/reopen and the audible ~100 ms restart gap.
For a run that is fully self-contained — independent of the evolving instrument
library — play `bass.json` and swap to `device-gap-swap.json` (both here, both minimal and fixed).

**(c) Scripted human — the #220 demo bar.** `docs/rituals/m1-demo-bar.md` scripts the epic's
top-level acceptance ritual at the M1 bar: play `bass.json`, hand the conversational agent the fixed
`prompt.txt`, and judge by ear whether the edit landed via restart-swap (the ~100 ms gap is
tolerated at M1).

## The files

| File | Part | Role |
| --- | --- | --- |
| `bass.json` | (b), (c) | Fixed starting instrument — an always-on 55 Hz saw bass drone through a gentle low-pass. Audible the instant `reuben play` opens the device; raw enough to have room for "rounder" and "add a dub delay". Resource-free so it never drifts against the library. |
| `prompt.txt` | (c) | The fixed demo prompt, verbatim: `make the bass rounder and add a dub delay`. |
| `device-gap-swap.json` | (b) | Fixed second document for the device-gap swap — a pure 220 Hz sine, obviously different from the bass, so the ~100 ms gap and the switch to the new sound are unmistakable by ear. |

`bass.json` and `device-gap-swap.json` must load + plan; `prompt.txt` must stay the exact fixed
string. `cargo test -p reuben-native --test m1_fixtures` guards all three, so a change that breaks a
fixture reds CI here instead of surfacing only when someone next runs the manual ritual on hardware.
