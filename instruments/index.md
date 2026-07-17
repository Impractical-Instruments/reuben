# Library index

One signature line per instrument in the available-set (ADR-0057 §4): name — role line
(the document's `doc` first sentence), (interface input pipes) → output pipes. Trusted for
selection only — wiring facts come from `describe_patch` or the document itself. Generated:
regenerate with `cargo run -p reuben-native --example gen_library_index`; never hand-edit
(the `library_index_is_in_sync` test compares this file against a fresh generation).

chord-player — V1.3 Toy 2 — the Chord player (ADR-0022/0032; ADR-0043 pipes). (brightness:f32 %=0.5, chord:note, key:f32=60) → out
chord-player-voice — Voice patch for chord-player.json (ADR-0032). (freq:f32_buffer Hz=440, gate:f32=0) → active, audio
default — Default polyphonic playable rig (8 voices, ADR-0032). () → out
default-voice — Default subtractive voice patch (ADR-0032). (freq:f32_buffer Hz=440, gate:f32=0) → active, audio
euclidean-drums — A self-playing 4-channel Euclidean rhythm machine (kick, snare, tom, hat). (hat_decay:f32 s=0.001, hat_filter:f32=0, hat_level:f32=0.5, hat_pulses:f32 pulses=8, hat_rotation:f32 steps=0, hat_steps:f32 steps=16, kick_decay:f32 s=0.001, kick_filter:f32=0, kick_level:f32=0.85, kick_pulses:f32 pulses=4, kick_rotation:f32 steps=0, kick_steps:f32 steps=16, snare_decay:f32 s=0.001, snare_filter:f32=0, snare_level:f32=0.6, snare_pulses:f32 pulses=2, snare_rotation:f32 steps=4, snare_steps:f32 steps=16, tempo:f32 BPM=120, tom_decay:f32 s=0.001, tom_filter:f32=0, tom_level:f32=0.6, tom_pulses:f32 pulses=3, tom_rotation:f32 steps=2, tom_steps:f32 steps=16) → out
groovebox — A free-running synthesized step-sequenced beatmaker (V1.3 Toy 1, ADR-0022/0032). (drive:f32 x=3, hat_step1:f32=1, hat_step10:f32=0, hat_step11:f32=1, hat_step12:f32=0, hat_step13:f32=1, hat_step14:f32=0, hat_step15:f32=1, hat_step16:f32=0, hat_step2:f32=0, hat_step3:f32=1, hat_step4:f32=0, hat_step5:f32=1, hat_step6:f32=0, hat_step7:f32=1, hat_step8:f32=0, hat_step9:f32=1, hat_vol:f32=0.35, kick_step1:f32=1, kick_step10:f32=0, kick_step11:f32=0, kick_step12:f32=0, kick_step13:f32=1, kick_step14:f32=0, kick_step15:f32=0, kick_step16:f32=0, kick_step2:f32=0, kick_step3:f32=0, kick_step4:f32=0, kick_step5:f32=1, kick_step6:f32=0, kick_step7:f32=0, kick_step8:f32=0, kick_step9:f32=1, kick_vol:f32=0.7, snare_step1:f32=0, snare_step10:f32=0, snare_step11:f32=0, snare_step12:f32=0, snare_step13:f32=1, snare_step14:f32=0, snare_step15:f32=0, snare_step16:f32=0, snare_step2:f32=0, snare_step3:f32=0, snare_step4:f32=0, snare_step5:f32=1, snare_step6:f32=0, snare_step7:f32=0, snare_step8:f32=0, snare_step9:f32=0, snare_vol:f32=0.5, tempo:f32 BPM=120, tone:f32=0.5, volume:f32=0.5) → out
hat-voice — Hi-hat voice (ADR-0032). (gate:f32=0) → active, audio
kick-voice — Kick drum voice (ADR-0032). (gate:f32=0) → active, audio
mic-space — Live-input demo (ADR-0038): a top-level input pipe bound to logical input channel 0 feeds the nested `space` patch (instruments/patches/space.json) — one file showing both halves of the pipe model. (mic:f32_buffer) → out
snare-voice — Snare drum voice (ADR-0032). (gate:f32=0) → active, audio
space — Nestable tone + space send effect (ADR-0034/0038): a lowpass into a Freeverb, exposed through interface pipes so a host patch uses it as one node. (in:f32_buffer, space:f32=0.35, tone:f32_buffer Hz=4000) → out
strum-harp — Toy 3 of 3 (V1.3 'The Toys', ADR-0022 §3 / ADR-0032): a drag-to-strum harp. (brightness:f32=0.6, key:f32=60, octaves:f32=1, strum:f32=0) → out
strum-harp-voice — Voice patch for strum-harp.json (ADR-0032). (freq:f32 Hz=440, gate:f32=0) → audio
