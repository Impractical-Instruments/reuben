//! Coordinator-direct behavioral harness for the RT-side install slot's master-gain ramp
//! (fade-down → install-at-zero → fade-up over ~2× the ramp): a survivor keeps ringing through
//! the ramp, a non-survivor's cut lands at master-zero (inaudible) and stays cold after, and
//! steady state (no swap pending) passes the Engine's audio through unchanged. Driven through
//! the production [`RenderSlot`], not a synchronous test stand-in.
//!
//! see rules: agent-mcp

use reuben_core::coordinator::{Coordinator, RenderSlot};
use reuben_core::resources::MemoryResolver;
use reuben_core::{AudioConfig, Registry};

fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, 128)
}

/// An envelope (gate held, slow attack) whose CV is the master output — so the rendered level *is*
/// the envelope level, a clean signal to read the ramp envelope off. `env_addr` lets a test rename
/// the node to force a reset. Mirrors `swap.rs`'s test fixture.
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

/// [`envelope_doc`]'s rig widened to four logical master channels: the same `/env` envelope and
/// `/out` sink, but the interface also taps channel 3, so master width derives to 4. Swapping
/// between the two documents changes the installed Engine's logical geometry.
fn wide_envelope_doc() -> String {
    r#"{ "format_version": 3, "instrument": "eg",
         "interface": { "outputs": {
             "front": { "from": "/out.audio", "channel": 0 },
             "rear":  { "from": "/out.audio", "channel": 3 } } },
         "nodes": [
           { "type": "envelope", "address": "/env",
              "inputs": { "gate": 1.0, "attack": 0.5, "decay": 0.01,
                          "sustain": 0.8, "release": 0.5 } },
           { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/env.cv" } } } ] }"#
        .to_string()
}

/// A one-pipe mic passthrough bound to logical input channel 0: the rendered output
/// *is* the logical input (one core block later), so a duplex `fill_duplex` drives real input into
/// the render path — the fixture for the short-input dark-degrade regression below.
fn mic_doc() -> String {
    r#"{ "format_version": 3, "instrument": "mic_through",
         "interface": {
           "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
           "outputs": { "main": { "from": "/out.audio" } } },
         "nodes": [
           { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } } ] }"#
        .to_string()
}

/// [`mic_doc`]'s passthrough widened to two bound input channels: the *input* width derives to 2
/// while the master output width stays at the stereo floor, so a swap between the two changes the
/// geometry a duplex caller's `input` buffer was sized against and nothing else.
fn stereo_mic_doc() -> String {
    r#"{ "format_version": 3, "instrument": "mic_through",
         "interface": {
           "inputs":  { "mic_l": { "type": "f32_buffer", "channel": 0 },
                        "mic_r": { "type": "f32_buffer", "channel": 1 } },
           "outputs": { "main": { "from": "/out.audio" } } },
         "nodes": [
           { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic_l" } } } ] }"#
        .to_string()
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
    // The heart of the install cut. Warm to sustain, swap to the identical document (both nodes survive
    // — address + type + fingerprint all match), then read the ramp envelope off the rendered
    // buffer: it starts at full gain, hits exactly zero at the install frame (`ramp_edge_frames`),
    // and recovers to the survivor's held sustain over the up-ramp — a ~2× ramp duck, not a click.
    let (mut coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    let sustain = warm_to_sustain(&mut slot);

    let report = coord.swap_document(&envelope_doc("/env"));
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
    // Exactly zero at the install frame (install-at-zero): this is where a non-survivor's
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
    // A non-survivor's fresh box starts cold and its cut lands at master-zero
    // (inaudible). Warm to sustain, then swap to a document that *renames* the envelope: `/env` is
    // removed (reset) and `/eg` is added cold, while `/out` survives. The ramp still dips to zero at
    // the install frame (masking the cut), but after the up-ramp the fresh envelope has barely left
    // zero — a stark contrast to the survivor case, proving the reset node was cut inaudibly.
    let (mut coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    let sustain = warm_to_sustain(&mut slot);

    let report = coord.swap_document(&envelope_doc("/eg"));
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

#[test]
fn an_install_that_changes_the_master_width_does_not_render_a_malformed_quantum() {
    // REGRESSION: the ramp path read the Engine's logical width once, at the top of the callback,
    // and kept using it across `install_at_zero` — so on the block where an install lands, the
    // post-install segments were sliced at the *retiring* Engine's stride and handed to an Engine
    // of a different width. In debug that trips Engine's "fill buffer must be a multiple of
    // channels"; in release it renders one malformed quantum. The buffer the caller sized for the
    // old width cannot describe the new one, so the honest answer for the rest of that block is
    // silence — which is also nearly free, because the up-ramp is still at (or just off) zero.
    let (mut coord, mut slot) = setup(&envelope_doc("/env"));
    let ch = slot.channels();
    assert_eq!(ch, 2, "the base rig is a stereo master");
    warm_to_sustain(&mut slot);

    let report = coord.swap_document(&wide_envelope_doc());
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    assert_eq!(
        coord.installed_channels(),
        4,
        "the swapped-in rig taps channel 3, so master width derives to 4"
    );

    // One frame past the install point: the down edge fills exactly `edge` frames, then the install
    // lands and a single frame of the up edge remains — a remainder whose old-width sample count
    // (2) is not a whole number of new-width frames (4).
    let edge = slot.ramp_edge_frames();
    let span = edge + 1;
    let mut buf = vec![0.123f32; span * ch];
    slot.fill(&mut buf);

    // The post-install remainder is exact silence, not a smeared or unwritten quantum.
    for f in edge..span {
        assert_eq!(
            frame_mag(&buf, ch, f),
            0.0,
            "the width-changing install silences the rest of the block at frame {f}"
        );
    }
    assert!(
        buf.iter().all(|s| s.is_finite()),
        "a width-changing install must never render NaN/garbage"
    );

    // The ramp is not abandoned: the up edge resumes on the next callback, which the caller now
    // sizes to the width the slot reports.
    assert!(slot.is_ramping(), "the up edge is still owed");
    assert_eq!(slot.channels(), 4, "the slot reports the installed width");
    let wide_ch = slot.channels();
    let mut wide = vec![0.0f32; 2 * edge * wide_ch];
    slot.fill_duplex(&[], &mut wide);
    assert!(!slot.is_ramping(), "the up edge completed at the new width");
    assert!(wide.iter().all(|s| s.is_finite()));
}

#[test]
fn an_install_that_changes_the_input_width_does_not_smear_the_callers_input() {
    // The input half of the same stale-geometry read: the master output width is identical either
    // side of this swap, so only the *input* stride moves. A duplex caller's `input` was
    // de-interleaved at the retiring Engine's width, and the post-install segments were still
    // slicing it at that width — one channel-smeared quantum, and Engine's duplex-stride
    // debug_assert in a debug build.
    let (mut coord, mut slot) = setup(&mic_doc());
    let ch = slot.channels();
    let in_ch = slot.input_channels();
    assert_eq!(in_ch, 1, "the base rig binds one logical input channel");

    let report = coord.swap_document(&stereo_mic_doc());
    assert!(report.report.ok, "swap should install: {:?}", report.report);
    assert_eq!(
        coord.installed_input_channels(),
        2,
        "the swapped-in rig binds two logical input channels"
    );
    assert_eq!(
        coord.installed_channels(),
        ch,
        "the output width is unchanged — only the input stride moves"
    );

    let edge = slot.ramp_edge_frames();
    let span = edge + 1;
    let mut out = vec![0.456f32; span * ch];
    let input: Vec<f32> = (0..span * in_ch)
        .map(|i| ((i % 89) as f32 / 89.0) - 0.5)
        .collect();
    slot.fill_duplex(&input, &mut out);

    for f in edge..span {
        assert_eq!(
            frame_mag(&out, ch, f),
            0.0,
            "the width-changing install silences the rest of the block at frame {f}"
        );
    }
    assert!(slot.is_ramping(), "the up edge is still owed");
    assert_eq!(slot.input_channels(), 2, "the slot reports the new stride");
}

#[test]
fn a_short_duplex_input_dark_degrades_instead_of_panicking_during_the_ramp() {
    // REGRESSION (adversarial hot-path review): the ramp path renders the buffer in phase-bounded
    // segments, slicing `input` per segment. A SHORT-but-nonempty duplex `input` — a capture
    // underrun — must NOT panic on that slice on the render thread; it must dark-degrade (stage the
    // missing tail as zeros), exactly as the steady-state fast path already does via
    // Engine's per-frame `input.get().unwrap_or(0.0)`. Before the clamp fix this panicked with
    // slice-end-out-of-range inside `render_segment` while a swap's ramp was in flight.
    let (mut coord, mut slot) = setup(&mic_doc());
    let ch = slot.channels();
    let in_ch = slot.input_channels();
    assert_eq!(
        in_ch, 1,
        "mic passthrough declares one logical input channel"
    );

    // Arm a swap so the very next fill runs the master-gain ramp (down → install-at-zero → up).
    let report = coord.swap_document(&mic_doc());
    assert!(report.report.ok, "swap should install: {:?}", report.report);

    let edge = slot.ramp_edge_frames();
    let span = 3 * edge; // down (edge) + up (edge) + steady tail (edge): the whole ramp in one call
    let mut out = vec![0.0f32; span * ch];

    // Feed input for only the down edge; the up edge + steady tail have NO input (the underrun).
    // `edge` frames is short (< `span`) yet lands exactly on a segment boundary, so every rendered
    // segment sees either a full or an empty input slice — never a wrong-width one that the Engine's
    // debug_assert would (rightly) flag. Without the clamp, the up segment's `&input[edge..2*edge]`
    // slice is out of range and panics.
    let fed_frames = edge;
    assert!(fed_frames < span, "input is genuinely short of the buffer");
    let input: Vec<f32> = (0..fed_frames * in_ch)
        .map(|i| ((i % 89) as f32 / 89.0) - 0.5) // an audible, nonzero test signal
        .collect();

    // The load-bearing assertion: this call must return, not unwind across the (would-be) FFI seam.
    slot.fill_duplex(&input, &mut out);

    assert!(!slot.is_ramping(), "the ramp completed within the buffer");
    assert!(
        out.iter().all(|s| s.is_finite()),
        "a short duplex input must render finite samples, never NaN/garbage"
    );
    // Dark-degrade proof: after the ramp (gain == 1) and well past where the fed input drained
    // (one core block of duplex latency), the missing-input tail renders as exact silence — zeros
    // for the frames Engine staged from `input.get().unwrap_or(0.0)`, not stale or garbage samples.
    let tail_start = 2 * edge + slot.block_size();
    for f in tail_start..span {
        assert_eq!(
            frame_mag(&out, ch, f),
            0.0,
            "missing-input tail must dark-degrade to silence at frame {f}"
        );
    }
}
