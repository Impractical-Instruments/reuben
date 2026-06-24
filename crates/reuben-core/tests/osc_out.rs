//! Integration: the `osc_out` sink collects its input Messages onto the outbound route, the
//! render loop stamps the node's address (the outbound OSC address), and they surface on
//! `render_block_multi`'s outbound out-parameter (ADR-0026).

use reuben_core::graph::Graph;
use reuben_core::message::{Arg, Message};
use reuben_core::operators::{OscOut, Oscillator, Output};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
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

    // An external value addressed at the sink's whole-node address routes in as an event; the
    // sink forwards it out, and the loop stamps the node address `/fb`.
    let msgs = [Message::new("/fb", [Arg::Float(0.7)], 0)];
    r.render_block_multi(&mut plan, &msgs, &mut master, &mut outbound);

    assert_eq!(outbound.len(), 1, "the sink forwarded one Message");
    assert_eq!(outbound[0].addr, "/fb", "stamped with the node address");
    assert_eq!(outbound[0].args.as_slice(), &[Arg::Float(0.7)]);
}

#[test]
fn outbound_is_silent_with_no_input() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();

    r.render_block_multi(&mut plan, &[], &mut master, &mut outbound);
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

    let a = [Message::new("/fb", [Arg::Float(1.0)], 0)];
    let b = [Message::new("/fb", [Arg::Float(2.0)], 0)];
    r.render_block_multi(&mut plan, &a, &mut master, &mut outbound);
    r.render_block_multi(&mut plan, &b, &mut master, &mut outbound);

    assert_eq!(outbound.len(), 2, "appended, not cleared");
    assert_eq!(outbound[0].args.as_slice(), &[Arg::Float(1.0)]);
    assert_eq!(outbound[1].args.as_slice(), &[Arg::Float(2.0)]);
}
