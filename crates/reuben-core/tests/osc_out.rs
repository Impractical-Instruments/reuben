//! Integration: the `osc_out` sink collects its input Messages onto the outbound route, the
//! render loop stamps the node's address (the outbound OSC address), and they surface on
//! `render_block_multi`'s outbound out-parameter.

use reuben_core::graph::Graph;
use reuben_core::message::{Arg, Message};
use reuben_core::operators::{MapF32Value, OscOut, Oscillator, Output};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::AudioConfig;

/// A normal audio path (so the rig has a master tap) plus an `osc_out` sink at `/fb`.
fn build_rig() -> Graph {
    let mut g = Graph::new();
    let osc = g.add("/osc", Oscillator::new());
    let out = g.add("/out", Output::new());
    g.connect(osc, 0, out, 0);
    g.tap_output(out, 0);
    g.add("/fb", OscOut::new());
    g
}

#[test]
fn forwards_input_to_outbound_stamped_with_node_address() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    // An external Note addressed to the sink's `in` port routes in as an event; the sink forwards
    // it out, and the loop stamps the node address `/fb` (the input's local address is dropped).
    let note = Note::new(Pitch::Degree(0), 1.0);
    let msgs = [Message::new("/fb/in", note, 0)];
    r.render_block_multi(&mut plan, &msgs, &[], &mut master, &mut outbound);

    assert_eq!(outbound.len(), 1, "the sink forwarded one Message");
    assert_eq!(outbound[0].address, "/fb", "stamped with the node address");
    assert_eq!(outbound[0].arg, Arg::Note(note));
}

/// The sink is type-agnostic (issue #141): a **wired Value source** — the two-way
/// control-surface case, a Good Button's `map` output echoing a control value — reaches the
/// outbound route too, not just a `Note`. The Value's emission delivers to the pass-through
/// input as a raw Event and forwards verbatim, stamped with the sink's address.
#[test]
fn forwards_a_wired_value_source_for_control_feedback() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut g = Graph::new();
    let osc = g.add("/osc", Oscillator::new());
    let out = g.add("/out", Output::new());
    g.connect(osc, 0, out, 0);
    g.tap_output(out, 0);
    // A held-value map (identity [0,1]→[0,1]) wired into the sink: `in` change → `out` Message.
    let map = g.add("/map", MapF32Value::new());
    let fb = g.add("/fb", OscOut::new());
    let map_out = 0; // map's sole output `out`
    g.connect(map, map_out, fb, 0);

    let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    // Drive the map's held `in` with an external control message; the remapped value emits and
    // the sink forwards it out as a scalar — the enum/scalar path that was unreachable when the
    // sink decoded only `Note`.
    let msgs = [Message::float("/map/in", 0.75, 0)];
    r.render_block_multi(&mut plan, &msgs, &[], &mut master, &mut outbound);

    assert_eq!(outbound.len(), 1, "the control echo reached the boundary");
    assert_eq!(outbound[0].address, "/fb", "stamped with the sink address");
    assert_eq!(
        outbound[0].arg,
        Arg::F32(0.75),
        "identity map echoes the value"
    );
}

/// The frame on an outbound Message is **block-absolute** (timing + feedback):
/// the render loop stamps operator emissions by adding each segment's start (`with_emit(..,
/// seg_start)`), and the outbound drain forwards that frame verbatim — it must neither re-stamp
/// nor lose the offset. Two held changes to the map's `in` split its block into segments [0,100)
/// and [100,256); the second emission is produced at segment-relative frame 0, so a regression
/// passing 0 instead of `seg_start` would surface it at frame 0 — this pins that it surfaces at
/// 100. (The values differ so each frame is tied to its own emission.)
#[test]
fn outbound_message_carries_its_block_absolute_frame() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut g = Graph::new();
    let osc = g.add("/osc", Oscillator::new());
    let out = g.add("/out", Output::new());
    g.connect(osc, 0, out, 0);
    g.tap_output(out, 0);
    let map = g.add("/map", MapF32Value::new());
    let fb = g.add("/fb", OscOut::new());
    g.connect(map, 0, fb, 0);

    let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    // Two held-value changes in one block: the frame-100 change is a Held-change segment bound,
    // so the map's second `process` call runs on [100,256) and emits at segment-relative 0.
    let msgs = [
        Message::float("/map/in", 0.25, 0),
        Message::float("/map/in", 0.75, 100),
    ];
    r.render_block_multi(&mut plan, &msgs, &[], &mut master, &mut outbound);

    assert_eq!(outbound.len(), 2, "one emission per segment");
    assert_eq!(
        (outbound[0].frame, outbound[1].frame),
        (0, 100),
        "emission frames surface block-absolute, not segment-relative"
    );
    assert_eq!(
        outbound[0].arg,
        Arg::F32(0.25),
        "frame 0 is the first value"
    );
    assert_eq!(
        outbound[1].arg,
        Arg::F32(0.75),
        "frame 100 is the second value"
    );

    // The external-event forward path preserves a mid-block frame too: routing delivers the
    // Note to the pass-through as a segment-relative event, the sink re-emits it, and the
    // drain surfaces it back at its original block-absolute frame. A fresh mapless rig, so the
    // map's per-block held re-emission can't sit beside the Note in the outbound list.
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    outbound.clear();
    let msgs = [Message::new("/fb/in", Note::new(Pitch::Degree(0), 1.0), 37)];
    r.render_block_multi(&mut plan, &msgs, &[], &mut master, &mut outbound);
    assert_eq!(outbound.len(), 1, "the forwarded Note reached the boundary");
    assert_eq!(
        outbound[0].frame, 37,
        "a mid-block external frame survives routing, re-emit, and drain"
    );
}

#[test]
fn outbound_is_silent_with_no_input() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
    assert!(outbound.is_empty(), "no input -> nothing sent out");
}

#[test]
fn outbound_appends_across_blocks() {
    // The out-parameter is append-only (the caller drains it), so an Engine can accumulate
    // several blocks of one callback. Two blocks, each with one input -> two outbound Messages.
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    let note_a = Note::new(Pitch::Degree(1), 1.0);
    let note_b = Note::new(Pitch::Degree(2), 1.0);
    let a = [Message::new("/fb/in", note_a, 0)];
    let b = [Message::new("/fb/in", note_b, 0)];
    r.render_block_multi(&mut plan, &a, &[], &mut master, &mut outbound);
    r.render_block_multi(&mut plan, &b, &[], &mut master, &mut outbound);

    assert_eq!(outbound.len(), 2, "appended, not cleared");
    assert_eq!(outbound[0].arg, Arg::Note(note_a));
    assert_eq!(outbound[1].arg, Arg::Note(note_b));
}
