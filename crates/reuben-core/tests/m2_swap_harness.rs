//! The M2 swap-correctness + RT-safety harness (ticket #324) — ADR-0053 §§2–3, the epic's
//! **terminal** off-device verification of the gapless mailbox swap.
//!
//! This module drives the **real** RT path: a live [`Coordinator`] builds and installs swaps
//! off-thread (bypassing the TCP structure channel — decision 1's `reuben-native` live-server test
//! owns that seam), and the production [`RenderSlot`] runs the **same callback-side install step the
//! audio callback calls** — [`RenderSlot::fill`]/`fill_duplex`, which peeks the install mailbox, runs
//! ADR-0050's master-gain ramp, box-transplants the survivors via [`Engine::transplant_survivors`],
//! and posts the retiree. It is NOT the synchronous `RenderRig` stand-in in `swap.rs` (a pre-#321
//! test shim); driving the production slot is exactly what ADR-0053 §2 asks for.
//!
//! Two shapes land here, per the ADR:
//!
//! - **§2 — Coordinator-direct behavioral survivor/reset** (part a). Operator state is opaque
//!   (ADR-0046 §4: the operator instance *is* the state — no extraction trait), so survivor/reset is
//!   asserted **behaviorally in rendered audio**:
//!   * a swap that *rewires an already-decaying envelope's neighbors* leaves the envelope a survivor
//!     — its box transplants with its in-progress decay, so the output keeps decaying smoothly with
//!     **no re-attack transient** (ADR-0045 §2 / ADR-0046 §5: rewired neighbors leave a survivor a
//!     survivor);
//!   * a swap that *bumps `voices`* on the same address is a different instantiation (ADR-0046 §5:
//!     `voices` is an instantiate-time Constant), so the voicer **resets** — its old held note falls
//!     silent and a fresh pool takes its place.
//!
//! - **§3 — install-path allocation-counting** (part b). The callback-side install step (mailbox
//!   drain + migration-table pointer-swap loop) is wrapped in the process's thread-local
//!   allocation-counting harness ([`rt_alloc::measure`], ticket #344) and asserted to make **zero**
//!   heap allocation and **zero** frees — the binary RT-safety invariant (ADR-0012), not a trend.
//!
//! The behavioral assertions **red on a broken migration table** (a survivor that fails to transplant
//! re-attacks from cold — the decay assertion trips); the alloc assertion **reds on any heap touch**
//! in the install step. Both bites were verified against the real machinery while authoring this
//! harness.

mod rt_alloc;
mod swap_rt_safe;

use rt_alloc::Counting;
use swap_rt_safe::{assert_counter_is_live, assert_install_step_heap_neutral};

use reuben_core::coordinator::{Coordinator, RenderSlot};
use reuben_core::message::Arg;
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};

/// Each `tests/*.rs` file is its own binary, so it must declare its own global allocator for the
/// thread-local counting harness to observe anything. Unarmed (the behavioral tests below), it is a
/// pure pass-through to the System allocator; `rt_alloc::measure` (via the shared
/// [`swap_rt_safe`] helper) arms it per-thread only for part (b)'s measured window.
#[global_allocator]
static GLOBAL: Counting = Counting;

const BLOCK: usize = 128;

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, BLOCK)
}

/// Build a Coordinator + production [`RenderSlot`] for `doc`, with `resolver` for any resources.
fn setup_with(doc: &str, resolver: MemoryResolver) -> (Coordinator, RenderSlot) {
    let (coord, side, _w) =
        Coordinator::install_initial(doc, Registry::builtin(), Box::new(resolver), cfg())
            .expect("initial install");
    (coord, RenderSlot::new(side))
}

/// Build a Coordinator + production [`RenderSlot`] for a resource-free `doc`.
fn setup(doc: &str) -> (Coordinator, RenderSlot) {
    setup_with(doc, MemoryResolver::new())
}

/// Peak magnitude of frame `f` across all channels of an interleaved buffer.
fn frame_mag(buf: &[f32], ch: usize, f: usize) -> f32 {
    (0..ch).fold(0.0f32, |m, c| m.max(buf[f * ch + c].abs()))
}

/// Peak magnitude across an entire interleaved buffer.
fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

/// Render `frames` interleaved frames through the production slot into a fresh buffer and return it —
/// the exact call an audio callback makes (peek → ramp → install-at-zero → render).
fn render(slot: &mut RenderSlot, frames: usize) -> Vec<f32> {
    let ch = slot.channels();
    let mut buf = vec![0.0f32; frames * ch];
    slot.fill(&mut buf);
    buf
}

// ---------------------------------------------------------------------------------------------
// (a) ADR-0053 §2 — Coordinator-direct behavioral survivor / reset, through the real RT install.
// ---------------------------------------------------------------------------------------------

/// An envelope whose CV *is* the master output, so the rendered per-frame level reads back the
/// envelope's contour directly. A short attack then a **long, gentle decay toward `sustain = 0`**
/// makes it a continuously **decaying** signal under a *held* gate — no gate manipulation needed to
/// catch it mid-decay. `env_addr` renames the node (to force a reset in a sibling test if wanted).
fn decaying_envelope_doc(env_addr: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "eg",
             "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "envelope", "address": "{env_addr}",
                  "inputs": {{ "gate": 1.0, "attack": 0.005, "decay": 2.0,
                               "sustain": 0.0, "release": 0.5 }} }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
    )
}

/// The **same** `/env` envelope, but with its downstream **neighbors rewired**: its CV now flows
/// through a unity `add_f32_signal` pass node (`b` unwired ⇒ the additive identity `0`, so
/// `out = a + 0 = /env.cv`, sample-identical, same block) before reaching `/out`. `/env`'s own node
/// identity — address + type + (config-less) fingerprint — is untouched, so it stays a **survivor**
/// (ADR-0045 §2 / ADR-0046 §5: rewired neighbours leave a survivor a survivor); only the graph around
/// it, and the Plan indices, change — which is exactly what the migration table must remap.
fn rewired_neighbors_doc() -> String {
    r#"{ "format_version": 3, "instrument": "eg",
         "interface": { "outputs": { "out": { "from": "/out.audio" } } },
         "nodes": [
           { "type": "envelope", "address": "/env",
             "inputs": { "gate": 1.0, "attack": 0.005, "decay": 2.0,
                         "sustain": 0.0, "release": 0.5 } },
           { "type": "add_f32_signal", "address": "/pass",
             "inputs": { "a": { "from": "/env.cv" } } },
           { "type": "output", "address": "/out",
             "inputs": { "audio": { "from": "/pass.out" } } } ] }"#
        .to_string()
}

#[test]
fn rewiring_a_decaying_envelopes_neighbors_keeps_it_decaying_no_reattack() {
    // ADR-0053 §2, case one. Warm the envelope past its 5ms attack and into its long linear decay,
    // capture the level mid-decay, then swap to a document that REWIRES its neighbors (inserts a
    // unity pass node between `/env` and `/out`) while keeping `/env` a survivor. Driven through the
    // production RenderSlot — the same install step the audio callback runs — the survivor's box
    // transplants with its in-progress decay: the output must keep decaying smoothly, LOWER than
    // before, with no re-attack jump back toward the peak.
    //
    // RED on a broken migration table: if `/env`'s warm box fails to transplant, the freshly built
    // Engine's cold `/env` re-attacks from zero (gate is held high in the new doc) back toward 1.0 —
    // so `after` lands near the peak, far ABOVE `before`, tripping `after < before`.
    let (mut coord, mut slot) = setup(&decaying_envelope_doc("/env"));
    let ch = slot.channels();

    // Warm ~1s: past the 5ms attack, ~halfway down a 2s decay (level ≈ 0.5), still falling.
    let warm = render(&mut slot, 48_000);
    let before = frame_mag(&warm, ch, 48_000 - 1);
    assert!(
        (0.30..0.85).contains(&before),
        "the envelope must be caught genuinely mid-decay: level {before}"
    );

    // Swap: same `/env`, neighbours rewired through `/pass`. `/env` and `/out` survive; `/pass` is new.
    let report = coord.swap_document(&rewired_neighbors_doc(), None);
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    let diff = report.diff.as_ref().unwrap();
    assert_eq!(diff.survived, 2, "/env and /out survive the rewire");
    assert!(
        diff.state_reset.is_empty(),
        "no node resets on a pure neighbour rewire: {:?}",
        diff.state_reset
    );
    assert_eq!(
        diff.added,
        vec!["/pass".to_string()],
        "the pass node is added"
    );

    // Render well past the ~20ms ramp so the master gain is back at 1.0 and the rendered level is the
    // true (undipped) envelope level again.
    let span = 6 * slot.ramp_edge_frames();
    let after_buf = render(&mut slot, span);
    assert!(
        !slot.is_ramping(),
        "the swap ramp completed within the render"
    );
    let after = frame_mag(&after_buf, ch, span - 1);

    // The survivor kept decaying: strictly below where it was (a re-attacked cold box would be near
    // the peak, far above), and the drop is small — a smooth continuation of the same slope, not a
    // collapse to silence (which a reset-with-slow-attack would show).
    assert!(
        after < before,
        "a survivor keeps decaying — no re-attack transient: before {before} after {after}"
    );
    assert!(
        before - after < 0.08,
        "the decay continued smoothly across the swap (not a reset): before {before} after {after}"
    );
}

/// The default subtractive voice patch (ADR-0032) a `voicer` hosts, loaded as a resource.
const DEFAULT_VOICE_JSON: &str = include_str!("../../../instruments/voices/default-voice.json");

/// A resolver carrying the default voice patch under the path the voicer document references.
fn voice_resolver() -> MemoryResolver {
    let mut r = MemoryResolver::new();
    r.insert_text("voices/default-voice.json", DEFAULT_VOICE_JSON);
    r
}

/// A voicer hosting `voices` copies of the default voice, summed to the master. `voices` is an
/// instantiate-time Constant (ADR-0046 §5), so changing it changes the node's fingerprint.
fn voicer_doc(voices: u32) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "top",
             "resources": {{ "dv": "voices/default-voice.json" }},
             "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "voicer", "address": "/voicer", "config": {{ "voices": {voices} }},
                  "voice": "dv" }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "/voicer.audio" }} }} }} ] }}"#
    )
}

#[test]
fn bumping_voices_resets_the_pool_old_note_silent_fresh_pool_lives() {
    // ADR-0053 §2, case two. A voicer's `voices` pool size is an instantiate-time Constant
    // (ADR-0046 §5): the box carries a fixed-size pool built at instantiate. Hold a note into a
    // 4-voice pool so a voice rings at sustain, then swap to an 8-voice document on the SAME address.
    // Bumping `voices` is a different instantiation, so `/voicer` does NOT survive — the report says
    // so, the old held note falls silent (its 4-voice pool retired off-thread), and a fresh pool
    // takes over. Driven through the production RenderSlot, exactly as the audio callback would.
    //
    // RED on a broken survivor key: if the fingerprint logic wrongly treated the config change as a
    // survivor, the old 4-voice pool (with its ringing note) would transplant and keep sounding —
    // tripping the `after` silence assertion below.
    let (mut coord, mut slot) = setup_with(&voicer_doc(4), voice_resolver());

    // Note-on (MIDI 69, velocity 1), held; warm ~0.5s so the voice's envelope reaches sustain.
    slot.queue_osc("/voicer/notes", &[Arg::F32(69.0), Arg::F32(1.0)]);
    render(&mut slot, 24_000);
    let ringing_before = peak(&render(&mut slot, 2_048));
    assert!(
        ringing_before > 0.02,
        "the held note must be audibly ringing before the swap: peak {ringing_before}"
    );

    // Swap 4 -> 8 voices: a config change, so `/voicer` resets (only `/out` survives).
    let report = coord.swap_document(&voicer_doc(8), None);
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    let diff = report.diff.as_ref().unwrap();
    assert!(
        diff.state_reset.contains(&"/voicer".to_string()),
        "bumping voices resets the voicer: {:?}",
        diff.state_reset
    );
    assert_eq!(diff.survived, 1, "only /out survives a voices bump");

    // Drive the install through the ramp; the fresh (untriggered) 8-voice pool renders silence.
    let span = 6 * slot.ramp_edge_frames();
    render(&mut slot, span);
    assert!(
        !slot.is_ramping(),
        "the swap ramp completed within the render"
    );
    let after = peak(&render(&mut slot, 2_048));
    assert!(
        after < ringing_before * 0.25,
        "the old held note is gone — the reset pool is fresh and silent: \
         before {ringing_before} after {after}"
    );

    // Fresh voice count: the reset pool is a live, playable 8-voice pool — a NEW note-on sounds.
    slot.queue_osc("/voicer/notes", &[Arg::F32(72.0), Arg::F32(1.0)]);
    render(&mut slot, 24_000);
    let fresh_note = peak(&render(&mut slot, 2_048));
    assert!(
        fresh_note > 0.02,
        "the fresh pool plays a new note: peak {fresh_note}"
    );
}

// ---------------------------------------------------------------------------------------------
// (b) ADR-0053 §3 — the callback-side install step allocates and frees nothing (RT-safety).
// ---------------------------------------------------------------------------------------------

#[test]
fn the_install_step_makes_zero_heap_allocation() {
    // ADR-0053 §3 / ADR-0012: the callback-side install step — drain the install bundle, run the
    // master-gain ramp, box-transplant the survivors, post the retiree — must be **heap-neutral** on
    // the render thread: no allocation and no free. This wraps the exact fills the audio callback
    // makes in the process's THREAD-LOCAL counting harness (`rt_alloc::measure`, ticket #344), NOT a
    // process-global counter: counting is armed only on this thread for the duration of the closure,
    // so a sibling test allocating on another thread during the same window can never perturb the
    // result. A simple envelope→output graph is used so a freshly built Engine renders alloc-free
    // from its very first block (no first-render pool growth to muddy the window).
    let doc = decaying_envelope_doc("/env");
    let (mut coord, mut slot) = setup(&doc);
    let ch = slot.channels();

    // Grow every internal scratch buffer to steady-state capacity OFF the measured path (the harness
    // requires warm-up outside the closure), exactly as `rt_safe.rs` does before measuring.
    let mut out = vec![0.0f32; 64 * BLOCK * ch];
    for _ in 0..8 {
        slot.fill(&mut out);
    }

    // Live probe: an ordinary Box allocation inside a measured window must register, so the zeros
    // below cannot be vacuous (a dead counter would also read zero).
    assert_counter_is_live();

    // Off-thread build (ADR-0009): the Coordinator allocates the new Engine + migration table here,
    // OUTSIDE the measured window. Swapping to the identical document keeps both nodes survivors, so
    // the transplant loop genuinely runs (two pointer swaps) inside the window.
    let report = coord.swap_document(&doc, None);
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    assert_eq!(
        report.diff.as_ref().unwrap().survived,
        2,
        "both nodes survive"
    );

    // Measured window (shared skeleton): block fills spanning the whole ramp — including the
    // install-at-zero transplant + retiree post — asserting drain/ramp/transplant/post is
    // heap-neutral and the window was non-vacuous. Counting is armed only on this thread.
    assert_install_step_heap_neutral(&mut coord, &mut slot, BLOCK);
}
