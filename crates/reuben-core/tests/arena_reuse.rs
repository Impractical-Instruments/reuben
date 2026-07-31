//! Arena buffer reuse must be invisible in the audio. Instantiate hands a signal edge's arena
//! slot to a later producer once the edge's last reader has run, so a plan holds as many buffers
//! as it has *simultaneously live* edges rather than one per edge for the life of the plan. Every
//! test here renders a real document through the real [`Renderer`] and checks the samples, against
//! expected values the arithmetic in the document fixes independently of how the planner assigns
//! slots — the shapes chosen are the ones a wrong liveness answer breaks.

use std::sync::Arc;

use reuben_core::descriptor::Port;
use reuben_core::operator::{form::SignalF32, Out};
use reuben_core::plan::Plan;
use reuben_core::render::{render_plan, RenderScratch, Renderer, SerialExecutor};
use reuben_core::{load, AudioConfig, Descriptor, Graph, Io, NodeKey, Operator, Registry};

const BLOCK: usize = 64;

/// Instantiate a document and render `blocks` consecutive blocks, returning the last one. Rendering
/// more than one block is what exposes a slot whose contents are supposed to carry (materialize
/// scratch) or supposed not to (a recycled edge).
fn render(json: &str, blocks: usize) -> Vec<f32> {
    let graph = load(json, &Registry::builtin()).expect("the fixture document loads");
    let mut plan =
        Plan::instantiate(graph, AudioConfig::new(48_000.0, BLOCK)).expect("it instantiates");
    let mut renderer = Renderer::new(&plan);
    let mut out = vec![0.0f32; BLOCK];
    for _ in 0..blocks {
        renderer.render_block(&mut plan, &[], &mut out);
    }
    out
}

fn buffers(json: &str) -> usize {
    let graph = load(json, &Registry::builtin()).expect("the fixture document loads");
    Plan::instantiate(graph, AudioConfig::new(48_000.0, BLOCK))
        .expect("it instantiates")
        .num_buffers
}

fn assert_flat(samples: &[f32], want: f32, what: &str) {
    for (i, s) in samples.iter().enumerate() {
        assert!(
            (s - want).abs() < 1e-4,
            "{what}: sample {i} is {s}, want {want}"
        );
    }
}

/// A serial chain, the shape reuse is for. Each stage adds a distinct power of two to the one
/// before it, so the tap reads a number only the intended slot assignment can produce: any stage
/// reading a buffer another stage wrote lands on a different sum. Four producers hold at most two
/// live edges, so the chain's own edges cost two slots however long it gets.
#[test]
fn a_serial_chain_renders_its_sum_from_two_recycled_edges() {
    const CHAIN: &str = r#"{"instrument":"chain","nodes":[
        {"type":"add_f32_signal","address":"/s1","inputs":{"a":1.0,"b":2.0}},
        {"type":"add_f32_signal","address":"/s2","inputs":{"a":{"from":"/s1.out"},"b":4.0}},
        {"type":"add_f32_signal","address":"/s3","inputs":{"a":{"from":"/s2.out"},"b":8.0}},
        {"type":"add_f32_signal","address":"/s4","inputs":{"a":{"from":"/s3.out"},"b":16.0}}],
        "outputs":[{"node":"/s4","port":"out"}]}"#;

    assert_flat(&render(CHAIN, 3), 31.0, "1+2+4+8+16");

    // Five materialize scratch (`/s1.a`, `/s1.b`, and one `b` per later stage) plus two recycled
    // edge slots — not the four the chain's four signal outputs would each have owned.
    assert_eq!(buffers(CHAIN), 7);
}

/// Fan-out with a *late* reader: `/late` reads `/s1` after two more stages have produced. `/s1`'s
/// slot has to stay live across both of them — freeing it at its first reader would let `/s3`
/// overwrite it, and `/late` would sum the wrong stage.
#[test]
fn a_buffer_read_after_later_stages_is_not_recycled_under_them() {
    const FANOUT: &str = r#"{"instrument":"fanout","nodes":[
        {"type":"add_f32_signal","address":"/s1","inputs":{"a":1.0,"b":2.0}},
        {"type":"add_f32_signal","address":"/s2","inputs":{"a":{"from":"/s1.out"},"b":4.0}},
        {"type":"add_f32_signal","address":"/s3","inputs":{"a":{"from":"/s2.out"},"b":8.0}},
        {"type":"add_f32_signal","address":"/late",
         "inputs":{"a":{"from":"/s1.out"},"b":{"from":"/s3.out"}}}],
        "outputs":[{"node":"/late","port":"out"}]}"#;

    // /s1 = 3, /s3 = 15, so the late sum is 18 — and 30 (reading /s3 twice) or 6 (reading /s1
    // twice) is what a slot freed too early would produce.
    assert_flat(&render(FANOUT, 3), 18.0, "/s1 + /s3");
}

/// A tapped port is read by the master sum *after* every node has run, so its slot can never be
/// recycled however early the node sits. Two taps a stage apart: the early tap's buffer has to
/// survive the later stage that would otherwise inherit it.
#[test]
fn a_tapped_buffer_survives_the_stages_that_run_after_it() {
    const TAPPED: &str = r#"{"instrument":"tapped","nodes":[
        {"type":"add_f32_signal","address":"/early","inputs":{"a":1.0,"b":2.0}},
        {"type":"add_f32_signal","address":"/mid","inputs":{"a":{"from":"/early.out"},"b":4.0}},
        {"type":"add_f32_signal","address":"/late","inputs":{"a":{"from":"/mid.out"},"b":8.0}}],
        "outputs":[{"node":"/early","port":"out"},{"node":"/late","port":"out"}]}"#;

    // 3 (the early tap) + 15 (the late tap), summed into the master.
    assert_flat(&render(TAPPED, 3), 18.0, "/early + /late");
}

/// A materialize scratch holds its input's ZOH value across the block boundary and is excluded
/// from the per-block edge clear, so it is never recycled. Its buffer is a constant `1.0` here,
/// read by a stage that runs after two other producers have taken slots: if a scratch slot were
/// handed to one of them, the constant would be replaced by that stage's audio.
#[test]
fn a_held_constant_survives_producers_that_run_between_its_blocks() {
    const HELD: &str = r#"{"instrument":"held","nodes":[
        {"type":"add_f32_signal","address":"/s1","inputs":{"a":100.0,"b":200.0}},
        {"type":"add_f32_signal","address":"/s2","inputs":{"a":{"from":"/s1.out"},"b":400.0}},
        {"type":"mul_f32_signal","address":"/gain","inputs":{"a":{"from":"/s2.out"},"b":0.5}}],
        "outputs":[{"node":"/gain","port":"out"}]}"#;

    // (100+200+400) * 0.5, stable across blocks — the `b` scratch of every stage still holds its
    // constant on the third block, having never been refilled after the first.
    assert_flat(&render(HELD, 3), 350.0, "(100+200+400)*0.5");
}

/// A branch whose output nothing reads still needs a slot to be written into, and that slot is
/// free the moment its producer has run. The dead branch here costs nothing beyond the live
/// chain's own two edges, and the tap is unaffected by it.
#[test]
fn a_dead_branch_returns_its_slot_immediately() {
    const DEAD: &str = r#"{"instrument":"dead","nodes":[
        {"type":"add_f32_signal","address":"/s1","inputs":{"a":1.0,"b":2.0}},
        {"type":"add_f32_signal","address":"/dead","inputs":{"a":{"from":"/s1.out"},"b":99.0}},
        {"type":"add_f32_signal","address":"/s2","inputs":{"a":{"from":"/s1.out"},"b":4.0}}],
        "outputs":[{"node":"/s2","port":"out"}]}"#;

    assert_flat(&render(DEAD, 3), 7.0, "1+2+4");
}

/// An operator that writes `value` into the first `written` frames of its output buffer and
/// leaves the rest to whatever the engine put there. Nothing forbids the gap: an operator owns
/// the samples it writes, and the engine's per-block edge clear is what defines the rest as
/// silence.
struct PartialWriter {
    value: f32,
    written: usize,
}

impl Operator for PartialWriter {
    fn descriptor() -> Descriptor {
        writer_desc(0)
    }
    fn process(&mut self, io: &mut Io) {
        const OUT: Out<SignalF32> = Out::new(0);
        let n = io.frames().min(self.written);
        let out = io.write(OUT);
        out[..n].iter_mut().for_each(|s| *s = self.value);
    }
    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(PartialWriter {
            value: self.value,
            written: self.written,
        })
    }
}

fn writer_desc(n_inputs: usize) -> Descriptor {
    Descriptor {
        type_name: "partial_writer",
        inputs: (0..n_inputs).map(|_| Port::f32_buffer("a")).collect(),
        outputs: vec![Port::f32_buffer("out")],
        constants: vec![],
        resources: vec![],
    }
}

fn writer(g: &mut Graph, address: &str, value: f32, written: usize, n_inputs: usize) -> NodeKey {
    g.add_boxed(
        address,
        Box::new(PartialWriter { value, written }),
        Arc::new(writer_desc(n_inputs)),
    )
}

/// The tail an operator does not write must read **silence**, not the audio the previous owner of
/// its slot left there. The per-block edge clear zeroes each slot once, before any node runs,
/// which covers a slot's first producer and no one else — so a recycled slot is zeroed again the
/// moment before its next producer runs. `/c` inherits `/a`'s slot here (`/a` dies at `/b`), and
/// writes only the first half of it; without the re-zero the second half would still be `/a`'s 5.0.
#[test]
fn a_recycled_slot_reads_as_silence_where_its_new_producer_did_not_write() {
    let mut g = Graph::new();
    // `/a` fills its whole buffer, so the slot `/c` inherits is non-zero end to end.
    let a = writer(&mut g, "/a", 5.0, BLOCK, 0);
    let b = writer(&mut g, "/b", 7.0, BLOCK, 1);
    let c = writer(&mut g, "/c", 9.0, BLOCK / 2, 1);
    g.connect(a, 0, b, 0);
    g.connect(b, 0, c, 0);
    g.tap_output(c, 0usize);

    let mut plan =
        Plan::instantiate(g, AudioConfig::new(48_000.0, BLOCK)).expect("it instantiates");
    // The precondition the test is about: /c did inherit a slot an earlier producer wrote.
    assert_eq!(
        plan.num_buffers, 2,
        "three producers, at most two live edges — /c must be reusing a slot"
    );

    let mut renderer = Renderer::new(&plan);
    let mut out = vec![0.0f32; BLOCK];
    for _ in 0..3 {
        renderer.render_block(&mut plan, &[], &mut out);
    }
    assert_flat(&out[..BLOCK / 2], 9.0, "/c writes the first half");
    assert_flat(
        &out[BLOCK / 2..],
        0.0,
        "the half /c leaves alone is silence",
    );
}

/// A Signal `interface` output is read by the **host**, off the end of the schedule — the Voicer
/// reads a voice's `audio` out of that arena slot after `render_plan` returns, and a host that
/// binds graphs directly ([`Operator::bind_voices`]) supplies the boundary without a master tap
/// to pin it a second time. So the boundary pins the slot on its own: here `/a` feeds `/b` feeds
/// `/c` and `/a.out` is the boundary output, and freeing it at `/b` would let `/c` inherit the
/// slot and hand the host `/c`'s audio under the name `audio`.
#[test]
fn an_interface_signal_output_is_pinned_for_the_host_that_reads_it() {
    let mut g = Graph::new();
    let a = writer(&mut g, "/a", 3.0, BLOCK, 0);
    let b = writer(&mut g, "/b", 7.0, BLOCK, 1);
    let c = writer(&mut g, "/c", 15.0, BLOCK, 1);
    g.connect(a, 0, b, 0);
    g.connect(b, 0, c, 0);
    // A boundary output and no master tap — the shape a host-bound voice graph presents.
    g.interface.outputs.insert("audio".to_string(), (a, 0));

    let mut plan =
        Plan::instantiate(g, AudioConfig::new(48_000.0, BLOCK)).expect("it instantiates");
    let buf = plan
        .interface_signal_buf("audio")
        .expect("`audio` is a Signal interface output");

    // The host's own arena + scratch, exactly as `Voicer` drives a voice sub-plan.
    let mut arena: Vec<Vec<f32>> = (0..plan.num_buffers).map(|_| vec![0.0; BLOCK]).collect();
    let mut scratch = RenderScratch::new(&plan);
    let mut master: Vec<Vec<f32>> = (0..plan.config.channels)
        .map(|_| vec![0.0; BLOCK])
        .collect();
    let mut outbound = Vec::new();
    for _ in 0..3 {
        render_plan(
            &mut plan,
            &mut arena,
            &mut scratch,
            &SerialExecutor,
            &[],
            BLOCK,
            &[],
            &mut master,
            &mut outbound,
        );
    }

    // /a's own 3.0 — /c's 15.0 is what a boundary slot handed on to a later stage would read.
    assert_flat(&arena[buf], 3.0, "the `audio` boundary output");
}
