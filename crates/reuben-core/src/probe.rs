//! Per-node level probes: one slot per Plan node with a Signal output, measured as that node runs.
//!
//! A node's output buffer is readable only right after the node runs — the arena hands it to a
//! later producer once its last consumer is done — so the fold happens in Render, between one
//! node's `process` and the next. Each slot is switched on and off by a reader outside Render, but
//! only Render writes a slot's level and pending block: turning a slot on asks Render to reset it
//! rather than resetting it from the reader's thread, where Render could store a stale value back
//! over the reset. With every slot of a set off, Render reads one atomic per `render_plan` call and
//! skips the rest; with any on, it reads one more per probed node it runs.
//!
//! Measuring and committing are separate steps so that one node run several times a block (a
//! node inside a voice, every voice of a Voicer sharing one slot set) folds into one block whose
//! ballistics advance once: `accumulate` after each run, [`ProbeSlots::commit`] once
//! at the block's end.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::config::AudioConfig;
use crate::graph::Graph;
use crate::meter::{Ballistics, Block, Level};

/// Every probe slot of one Plan (or of the voice patch a Voicer's voices share), sorted by node
/// address so [`ProbeSlots::find`] is a binary search.
#[derive(Debug)]
pub struct ProbeSlots {
    pub nodes: Vec<NodeProbe>,
    ballistics: Ballistics,
    /// How many of `nodes` are on, shared with each of them, so a Render with every probe off
    /// reads one atomic rather than one per node.
    on_count: Arc<AtomicU32>,
}

/// One node's probe: a level per Signal output port, and the flag that says whether Render
/// measures it.
#[derive(Debug)]
pub struct NodeProbe {
    pub address: Arc<str>,
    /// One per Signal output, in signal-output ordinal order — the order of
    /// [`PlanNode::outputs`](crate::plan::PlanNode::outputs).
    pub ports: Vec<PortProbe>,
    /// The slot set of the voice patch this node hosts, for a Voicer node.
    pub hosted: Option<Arc<ProbeSlots>>,
    on: AtomicBool,
    /// Set by the reader when the probe turns on; Render clears the levels and the pending block,
    /// then this, when it next measures or commits the node.
    reset: AtomicBool,
    on_count: Arc<AtomicU32>,
}

/// One Signal output port's level, and the block it is accumulating towards the next commit.
///
/// Only the render thread writes the accumulator and the level, so a load-merge-store is not a
/// lost update.
#[derive(Debug)]
pub struct PortProbe {
    pub name: Arc<str>,
    level: Level,
    peak: AtomicU32,
    sum_squares: AtomicU32,
}

impl PortProbe {
    /// The linear held peak and averaged square as of the last commit.
    pub fn read(&self) -> (f32, f32) {
        self.level.read()
    }

    fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            level: Level::default(),
            peak: AtomicU32::new(0),
            sum_squares: AtomicU32::new(0),
        }
    }

    fn pending(&self) -> Block {
        Block {
            peak: f32::from_bits(self.peak.load(Ordering::Relaxed)),
            sum_squares: f32::from_bits(self.sum_squares.load(Ordering::Relaxed)),
        }
    }

    fn set_pending(&self, block: Block) {
        self.peak.store(block.peak.to_bits(), Ordering::Relaxed);
        self.sum_squares
            .store(block.sum_squares.to_bits(), Ordering::Relaxed);
    }
}

impl NodeProbe {
    /// Switch measuring on or off. Turning an off probe on starts every port from silence at the
    /// next block Render measures or commits, so a level left from an earlier session — or a block
    /// accumulated just before it was turned off, which no commit cleared — never shows. Turning
    /// on a probe that is already on changes nothing, so a reader may renew its probes freely.
    pub fn set_on(&self, on: bool) {
        if on {
            if self.on.load(Ordering::Relaxed) {
                return;
            }
            // Before the `Release` swap, so Render, seeing the probe on, also sees the request.
            self.reset.store(true, Ordering::Relaxed);
            if !self.on.swap(true, Ordering::Release) {
                self.on_count.fetch_add(1, Ordering::Relaxed);
            }
        } else if self.on.swap(false, Ordering::Relaxed) {
            self.on_count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Acquire)
    }

    /// Carry out a reset the reader asked for, if any, and say whether there was one.
    /// Render-side.
    fn take_reset(&self) -> bool {
        // Claimed before the clear, so a request the reader makes during it survives for the next
        // pass rather than being wiped by a trailing store. The load keeps the common case, no
        // request, off a read-modify-write.
        if !self.reset.load(Ordering::Relaxed) || !self.reset.swap(false, Ordering::Relaxed) {
            return false;
        }
        for port in &self.ports {
            port.level.reset();
            port.set_pending(Block::default());
        }
        true
    }
}

impl ProbeSlots {
    /// A slot, off, for every node of `graph` with at least one Signal output.
    pub fn for_graph(graph: &Graph, config: &AudioConfig) -> Arc<Self> {
        let on_count = Arc::new(AtomicU32::new(0));
        let mut nodes: Vec<NodeProbe> = graph
            .nodes
            .values()
            .filter_map(|node| {
                let ports: Vec<PortProbe> = node
                    .descriptor
                    .outputs
                    .iter()
                    .filter(|p| p.ty.is_buffer())
                    .map(|p| PortProbe::new(p.name))
                    .collect();
                (!ports.is_empty()).then(|| NodeProbe {
                    address: node.address.clone(),
                    ports,
                    hosted: None,
                    on: AtomicBool::new(false),
                    reset: AtomicBool::new(false),
                    on_count: on_count.clone(),
                })
            })
            .collect();
        nodes.sort_by(|a, b| a.address.cmp(&b.address));
        Arc::new(Self {
            nodes,
            ballistics: Ballistics::new(config.sample_rate),
            on_count,
        })
    }

    pub fn any_on(&self) -> bool {
        self.on_count.load(Ordering::Relaxed) != 0
    }

    pub fn find(&self, address: &str) -> Option<&NodeProbe> {
        self.index_of(address).map(|i| &self.nodes[i])
    }

    pub(crate) fn index_of(&self, address: &str) -> Option<usize> {
        self.nodes
            .binary_search_by(|n| (*n.address).cmp(address))
            .ok()
    }

    /// Fold `block` into the pending block of `node`'s `port`, as more samples of the same block,
    /// after any reset the node is owed.
    ///
    /// RT-safe: no allocation, lock or syscall.
    /// An index this set has no slot for — a Plan measuring into a set built from another patch —
    /// is skipped rather than panicking the render thread.
    pub(crate) fn accumulate(&self, node: usize, port: usize, block: Block) {
        let slot = self.nodes.get(node);
        let found = slot.and_then(|n| n.ports.get(port));
        debug_assert!(found.is_some(), "no probe slot for node {node} port {port}");
        let (Some(node), Some(port)) = (slot, found) else {
            return;
        };
        node.take_reset();
        let mut pending = port.pending();
        pending.merge(block);
        port.set_pending(pending);
    }

    /// Fold every on node's pending block, `frames` frames long, into its levels, and clear it. A
    /// node that still owes its reset — turned on after Render passed it this block, or not run at
    /// all, as a voice node of an idle Voicer — is reset here instead, so it reads silence from
    /// this block on and its stale pending block is never committed.
    ///
    /// RT-safe: no allocation, lock or syscall.
    pub fn commit(&self, frames: usize) {
        for node in self.nodes.iter().filter(|n| n.is_on()) {
            if node.take_reset() {
                continue;
            }
            for port in &node.ports {
                port.level.commit(port.pending(), frames, &self.ballistics);
                port.set_pending(Block::default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;
    use alloc::vec;

    use super::*;
    use crate::descriptor::Descriptor;
    use crate::operator::{Io, Operator};
    use crate::operators::{oscillator, output, pan, Oscillator, Output, Pan};
    use crate::plan::Plan;
    use crate::render::Renderer;

    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 128;

    mod blowup {
        use super::*;
        crate::operator_contract!(BlowUp {
            inputs:  {},
            outputs: { audio: f32_buffer },
        });
        /// Writes NaN for its first block, silence after.
        pub struct BlowUp {
            pub blocks: usize,
        }
        impl Operator for BlowUp {
            fn descriptor() -> Descriptor {
                Self::contract()
            }
            fn process(&mut self, io: &mut Io) {
                let n = io.frames();
                let fill = if self.blocks == 0 { f32::NAN } else { 0.0 };
                io.write(OUT_AUDIO)[..n].fill(fill);
                self.blocks += 1;
            }
            fn spawn(&self) -> Box<dyn Operator> {
                Box::new(BlowUp { blocks: 0 })
            }
        }
    }

    mod hold {
        use super::*;
        crate::operator_contract!(Hold {
            inputs:  {},
            outputs: { out: f32 { -1.0..=1.0, default 0.0, "", lin } },
        });
        /// A node with no Signal output.
        pub struct Hold;
        impl Operator for Hold {
            fn descriptor() -> Descriptor {
                Self::contract()
            }
            fn process(&mut self, _io: &mut Io) {}
            fn spawn(&self) -> Box<dyn Operator> {
                Box::new(Hold)
            }
        }
    }

    fn config() -> AudioConfig {
        AudioConfig::new(RATE, BLOCK)
    }

    /// `/osc` → `/out`, tapped to the master.
    fn osc_plan() -> Plan {
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
        g.tap_output(out, output::OUT_AUDIO);
        Plan::instantiate(g, config()).expect("instantiate")
    }

    fn render(plan: &mut Plan, seconds: f32) {
        let mut r = Renderer::new(plan);
        let mut out = vec![0.0f32; BLOCK];
        for _ in 0..(seconds * RATE) as usize / BLOCK {
            r.render_block(plan, &[], &mut out);
        }
    }

    fn osc_level(plan: &Plan) -> (f32, f32) {
        plan.probes.find("/osc").expect("/osc has a slot").ports[0].read()
    }

    #[test]
    fn a_probed_oscillator_reads_its_level() {
        let mut plan = osc_plan();
        plan.probes.find("/osc").unwrap().set_on(true);
        // Two seconds rather than one: the averaged square settles with a 0.3 s time constant,
        // and after one second it is still 3.6 % short of the sine's 0.5.
        render(&mut plan, 2.0);
        let (peak, mean_square) = osc_level(&plan);
        assert!((peak - 1.0).abs() <= 1e-3, "peak {peak}");
        assert!(
            (mean_square - 0.5).abs() <= 1e-2,
            "mean square {mean_square}"
        );
    }

    #[test]
    fn each_signal_output_meters_into_its_own_port() {
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let pan = g.add("/pan", Pan);
        g.set_value(pan, "pan", &crate::message::Arg::F32(-1.0));
        g.connect(osc, oscillator::OUT_AUDIO, pan, pan::IN_AUDIO);
        g.tap_output(pan, pan::OUT_LEFT);
        g.tap_output(pan, pan::OUT_RIGHT);
        let mut plan = Plan::instantiate(g, config()).expect("instantiate");
        plan.probes.find("/pan").unwrap().set_on(true);
        render(&mut plan, 0.5);

        let probe = plan.probes.find("/pan").unwrap();
        let port = |name: &str| {
            probe
                .ports
                .iter()
                .find(|p| &*p.name == name)
                .unwrap()
                .read()
        };
        // Hard left: the sine on `left`, nothing on `right`.
        assert!(
            (port("left").0 - 1.0).abs() <= 1e-3,
            "left {:?}",
            port("left")
        );
        assert!(port("right").0 <= 1e-3, "right {:?}", port("right"));
        assert_eq!(&*probe.ports[0].name, "left");
        assert_eq!(&*probe.ports[1].name, "right");
    }

    #[test]
    fn an_unprobed_node_is_not_measured() {
        let mut plan = osc_plan();
        render(&mut plan, 1.0);
        assert_eq!(osc_level(&plan), (0.0, 0.0));
    }

    #[test]
    fn probes_do_not_change_what_renders() {
        let mut plain = osc_plan();
        let mut probed = osc_plan();
        for node in &probed.probes.nodes {
            node.set_on(true);
        }
        let (mut r_plain, mut r_probed) = (Renderer::new(&plain), Renderer::new(&probed));
        let (mut a, mut b) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
        for _ in 0..100 {
            r_plain.render_block(&mut plain, &[], &mut a);
            r_probed.render_block(&mut probed, &[], &mut b);
            assert_eq!(
                a.iter().map(|s| s.to_bits()).collect::<Vec<_>>(),
                b.iter().map(|s| s.to_bits()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_blown_up_node_meters_at_the_ceiling_and_recovers() {
        let mut g = Graph::new();
        let bad = g.add("/bad", blowup::BlowUp { blocks: 0 });
        let out = g.add("/out", Output::new());
        g.connect(bad, blowup::OUT_AUDIO, out, output::IN_AUDIO);
        g.tap_output(out, output::OUT_AUDIO);
        let mut plan = Plan::instantiate(g, config()).expect("instantiate");
        plan.probes.find("/bad").unwrap().set_on(true);

        let mut r = Renderer::new(&plan);
        let mut out = vec![0.0f32; BLOCK];
        r.render_block(&mut plan, &[], &mut out);
        let level = |plan: &Plan| plan.probes.find("/bad").unwrap().ports[0].read();
        assert_eq!(level(&plan).0, 1000.0);

        // From the +60 dB ceiling, 12 dB a second takes 11.7 s to reach 1e-4 (-80 dB).
        for _ in 0..(12.0 * RATE) as usize / BLOCK {
            r.render_block(&mut plan, &[], &mut out);
        }
        assert!(level(&plan).0 < 1e-4, "peak {}", level(&plan).0);
    }

    #[test]
    fn a_message_only_node_has_no_slot() {
        let mut g = Graph::new();
        g.add("/hold", hold::Hold);
        let osc = g.add("/osc", Oscillator::new());
        g.tap_output(osc, oscillator::OUT_AUDIO);
        let plan = Plan::instantiate(g, config()).expect("instantiate");
        let probe_of = |address: &str| {
            plan.nodes()
                .iter()
                .find(|n| &*n.address == address)
                .unwrap()
                .probe
        };
        assert_eq!(probe_of("/hold"), None);
        assert!(plan.probes.find("/hold").is_none());
        let osc = probe_of("/osc").expect("/osc has a slot");
        assert_eq!(&*plan.probes.nodes[osc].address, "/osc");
    }

    #[test]
    fn any_on_counts_each_node_once_however_often_it_is_set() {
        let plan = osc_plan();
        let (osc, out) = (
            plan.probes.find("/osc").unwrap(),
            plan.probes.find("/out").unwrap(),
        );
        assert!(!plan.probes.any_on());
        osc.set_on(true);
        osc.set_on(true);
        out.set_on(true);
        osc.set_on(false);
        assert!(plan.probes.any_on());
        out.set_on(false);
        out.set_on(false);
        assert!(!plan.probes.any_on());
    }

    #[test]
    fn turning_on_an_already_on_probe_keeps_its_level() {
        let mut plan = osc_plan();
        plan.probes.find("/osc").unwrap().set_on(true);
        render(&mut plan, 0.5);
        let before = osc_level(&plan);
        assert!(before.0 > 0.0);
        plan.probes.find("/osc").unwrap().set_on(true);
        assert_eq!(osc_level(&plan), before);
        // Rendering on from there continues the meter rather than restarting it.
        render(&mut plan, 0.01);
        assert!((osc_level(&plan).1 - before.1).abs() < before.1 * 0.1);
    }

    #[test]
    fn turning_a_probe_back_on_resets_it_at_the_next_block() {
        let mut plan = osc_plan();
        plan.probes.find("/osc").unwrap().set_on(true);
        render(&mut plan, 2.0);
        let osc = plan.probes.find("/osc").unwrap();
        osc.set_on(false);
        osc.set_on(true);
        // The reader only asks; the old level stands until Render resets it.
        let settled = osc_level(&plan);
        assert!(settled.1 > 0.4);
        render(&mut plan, BLOCK as f32 / RATE);
        // One block of a sine from silence: well under the settled average.
        assert!(osc_level(&plan).1 < settled.1 * 0.1);
    }

    #[test]
    fn an_off_node_is_not_committed() {
        let mut plan = osc_plan();
        let osc = plan.probes.find("/osc").unwrap();
        osc.set_on(true);
        render(&mut plan, 0.1);
        let osc = plan.probes.find("/osc").unwrap();
        osc.set_on(false);
        let held = osc.ports[0].read();
        render(&mut plan, 0.1);
        assert_eq!(plan.probes.find("/osc").unwrap().ports[0].read(), held);
    }
}
