//! Behavioral harness for the RT-side install slot (ticket #321, ADR-0046 §7, ADR-0050 §2).
//!
//! Coordinator-direct: a real [`Coordinator`] builds and installs swaps off-thread while a real
//! [`RenderSlot`] drives the render side — the production RT path, not the synchronous `RenderRig`
//! stand-in in `swap.rs`. These tests observe the **rendered buffer** to prove the master-gain
//! ramp's sonic contract:
//!
//! - the master output **dips to zero and recovers over ~2× the ramp** (ADR-0050 §2 fade-down →
//!   install-at-zero → fade-up), hitting exactly zero at the install frame;
//! - a **survivor keeps ringing** through the up-ramp (its held level rides the box transplant,
//!   ADR-0046 §4 / ADR-0050 §4);
//! - a **non-survivor's cut lands at master-zero** — inaudible — and its fresh cold box is silent
//!   after the ramp (ADR-0050 §4);
//! - **steady state is transparent**: with no swap pending the slot passes the Engine's audio
//!   through unchanged (no dip, no ramp) — the fast path.

use reuben_core::coordinator::{Coordinator, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, 128)
}

/// An envelope (gate held, slow attack) whose CV is the master output — so the rendered level *is*
/// the envelope level, a clean signal to read the ramp envelope off. `env_addr` lets a test rename
/// the node to force a reset (ADR-0045 §2). Mirrors `swap.rs`'s test fixture.
fn envelope_doc(env_addr: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "eg",
             "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
             "nodes": [
               {{ "type": "envelope", "address": "{env_addr}",
                  "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                               "sustain": 0.8, "release": 0.5 }} }},
               {{ "type": "output", "address": "/out",
                  "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
    )
}

/// Build a Coordinator + production RenderSlot for `doc`.
fn setup(doc: &str) -> (Coordinator, RenderSlot) {
    let (coord, side, _w) = Coordinator::install_initial(
        doc,
        Registry::builtin(),
        Box::new(MemoryResolver::new()),
        cfg(),
    )
    .expect("initial install");
    (coord, RenderSlot::new(side))
}

/// Peak magnitude of frame `f` across all channels of an interleaved buffer.
fn frame_mag(buf: &[f32], ch: usize, f: usize) -> f32 {
    (0..ch).fold(0.0f32, |m, c| m.max(buf[f * ch + c].abs()))
}

/// Render `frames` interleaved frames into a fresh buffer and return it.
fn render(slot: &mut RenderSlot, frames: usize) -> Vec<f32> {
    let ch = slot.channels();
    let mut buf = vec![0.0f32; frames * ch];
    slot.fill(&mut buf);
    buf
}

/// Warm the envelope to its sustain level and return that steady per-frame level.
fn warm_to_sustain(slot: &mut RenderSlot) -> f32 {
    let ch = slot.channels();
    let buf = render(slot, 48_000); // ~1s: past attack + decay, sitting at sustain
    frame_mag(&buf, ch, buf.len() / ch - 1)
}

#[test]
fn steady_state_is_transparent_no_ramp_no_dip() {
    // With no swap pending, the slot must be a pass-through: the fast path renders the Engine and
    // applies no gain. The sustained envelope stays flat — no ramp begins, no frame dips to zero.
    let (_coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    let sustain = warm_to_sustain(&mut slot);
    assert!(
        sustain > 0.5,
        "envelope should warm to a strong sustain: {sustain}"
    );

    let buf = render(&mut slot, 4_000);
    assert!(!slot.is_ramping(), "no swap pending ⇒ no ramp");
    let min = (0..4_000)
        .map(|f| frame_mag(&buf, ch, f))
        .fold(f32::INFINITY, f32::min);
    assert!(
        min > 0.5 * sustain,
        "steady state must not duck: min frame {min} vs sustain {sustain}"
    );
}

#[test]
fn master_dips_to_zero_and_recovers_while_survivor_rings_through() {
    // The heart of ADR-0050 §2. Warm to sustain, swap to the identical document (both nodes survive
    // — address + type + fingerprint all match), then read the ramp envelope off the rendered
    // buffer: it starts at full gain, hits exactly zero at the install frame (`ramp_edge_frames`),
    // and recovers to the survivor's held sustain over the up-ramp — a ~2× ramp duck, not a click.
    let (mut coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    let sustain = warm_to_sustain(&mut slot);

    let report = coord.swap_document(&envelope_doc("/env"), None);
    assert!(report.report.ok, "swap should succeed: {:?}", report.report);
    assert_eq!(
        report.diff.as_ref().unwrap().survived,
        2,
        "both nodes survive"
    );

    let edge = slot.ramp_edge_frames();
    let span = 3 * edge; // down (edge) + up (edge) + steady tail (edge)
    let mut buf = vec![0.0f32; span * ch];
    slot.fill(&mut buf); // the whole ramp happens inside this one callback

    // Full gain at the ramp's start.
    assert!(
        frame_mag(&buf, ch, 0) > 0.5 * sustain,
        "the ramp opens at full gain: {}",
        frame_mag(&buf, ch, 0)
    );
    // Exactly zero at the install frame (ADR-0050 §2 install-at-zero): this is where a non-survivor's
    // hard cut would land — masked to silence.
    assert!(
        frame_mag(&buf, ch, edge) < 0.02,
        "the master hits zero at the install frame {edge}: {}",
        frame_mag(&buf, ch, edge)
    );
    // The dip's minimum sits at (or adjacent to) the install frame, not elsewhere.
    let (min_frame, _) = (0..span)
        .map(|f| (f, frame_mag(&buf, ch, f)))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    assert!(
        (min_frame as isize - edge as isize).abs() <= 1,
        "the dip bottoms out at the install frame: min at {min_frame}, edge {edge}"
    );
    // Recovered to the survivor's held level after 2× the ramp — the held note rang straight through.
    let recovered = frame_mag(&buf, ch, span - 1);
    assert!(
        recovered > 0.5 * sustain,
        "survivor keeps ringing through the up-ramp: recovered {recovered} vs sustain {sustain}"
    );
    assert!(!slot.is_ramping(), "the ramp completed within the buffer");
}

#[test]
fn a_non_survivor_is_cut_at_master_zero_and_stays_silent() {
    // ADR-0050 §4: a non-survivor's fresh box starts cold and its cut lands at master-zero
    // (inaudible). Warm to sustain, then swap to a document that *renames* the envelope: `/env` is
    // removed (reset) and `/eg` is added cold, while `/out` survives. The ramp still dips to zero at
    // the install frame (masking the cut), but after the up-ramp the fresh envelope has barely left
    // zero — a stark contrast to the survivor case, proving the reset node was cut inaudibly.
    let (mut coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    let sustain = warm_to_sustain(&mut slot);

    let report = coord.swap_document(&envelope_doc("/eg"), None);
    assert!(report.report.ok, "swap should succeed: {:?}", report.report);
    assert_eq!(
        report.diff.as_ref().unwrap().survived,
        1,
        "only /out survives a rename"
    );

    let edge = slot.ramp_edge_frames();
    let span = 3 * edge;
    let mut buf = vec![0.0f32; span * ch];
    slot.fill(&mut buf);

    // The cut is masked: the master is at zero at the install frame.
    assert!(
        frame_mag(&buf, ch, edge) < 0.02,
        "the reset node's cut lands at master-zero: {}",
        frame_mag(&buf, ch, edge)
    );
    // The fresh cold envelope is near-silent after the ramp (attack 0.5s, only ~20ms elapsed) — a
    // reset, unlike the survivor that recovered to full sustain.
    let recovered = frame_mag(&buf, ch, span - 1);
    assert!(
        recovered < 0.15 && recovered < 0.3 * sustain,
        "a reset node stays cold after the cut: recovered {recovered} vs sustain {sustain}"
    );
}
