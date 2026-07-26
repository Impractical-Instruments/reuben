# Why: Every shell-edge failure degrades to defined silence, is accounted for on the one diagnostics surface, and is never configurable.

[Rule](../../host-shell-io.md#degradation-is-fixed-and-counted)

A shell's edges fail for reasons the engine cannot see and the musician cannot diagnose: a device
that stops delivering, a callback that ran long, a rate mismatch wider than the servo's authority,
an instrument binding a channel this device does not have. Each needs an answer chosen in advance,
because the moment to invent one is the worst possible moment.

The answer is always **defined silence, counted**. A render that misses its real-time budget lets
the device play its own underrun silence and increments a counter; nothing about rendering changes
in response, because a shell that reacted to a deadline miss by dropping quality, resizing a buffer,
or skipping a block would make the *next* block's behavior depend on the last one's timing — and
that is exactly the nondeterminism the engine is built to exclude. An empty ring reads zeros and
re-enters warmup so a stalled device re-primes cleanly. A full ring trims oldest back to the floor.
An unsupplied input channel reads its declared default. None of these is fatal, and none is a
setting.

**Not configurable is the load-bearing half.** A knob here would be a knob on how the system lies to
you: two installations with different xrun policies produce different sound from the same document
under the same load, and neither can be reasoned about from the document. Fixed policy plus a
counter means the behavior is one thing everywhere and the *deviation* is what varies and gets
reported.

Which is why the counters are one surface rather than per-subsystem tallies. Output xruns and input
ring underruns/overruns live in the same struct because they are read together — a glitch is
diagnosed by which counter moved, and that comparison is impossible across two parallel counter
surfaces that were sampled at different instants. The counters are also chosen to *distinguish
diagnoses*: consumer-side drop-oldest and producer-side drop are counted separately because they
mean opposite things — a real rate mismatch versus a stalled output callback. Every counter is a
`Relaxed` atomic bumped with a single `fetch_add` and read by snapshot copy, so a logger never
forces the audio callback to synchronize and never holds a reference into the live struct.

The startup exception is the one place silence is *not* counted: before the ring has ever flowed,
silence is expected rather than a failure. Counting it would make every session start with a
spurious underrun and train everyone to ignore the number.

Decided in: issue #639 — settled directly, no ADR.
