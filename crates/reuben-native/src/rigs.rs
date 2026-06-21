//! Ready-made instrument graphs.
//!
//! For now there is one: the same Voicer -> Oscillator -> Filter -> Envelope -> Output
//! rig that produced the "first sound". Notes arrive as OSC at `/voicer/note [midi, gate]`.
//! JSON-defined instruments (ADR roadmap) replace this with data later.

use reuben_core::graph::Graph;
use reuben_core::operators::{envelope, oscillator, output, voicer};
use reuben_core::operators::{Envelope, Filter, Oscillator, Output, Voicer};

/// The default monophonic playable rig. Send `/voicer/note [midi, gate]` to play.
pub fn default_rig() -> Graph {
    let mut g = Graph::new();
    let v = g.add("/voicer", Voicer::new());
    let osc = g.add("/osc", Oscillator::new());
    let filt = g.add("/filter", Filter::new());
    let env = g.add("/env", Envelope::new());
    let out = g.add("/out", Output::new());

    g.connect(v, voicer::OUT_FREQ, osc, oscillator::IN_FREQ);
    g.connect(osc, oscillator::OUT_AUDIO, filt, 0);
    g.connect(filt, 0, env, envelope::IN_AUDIO);
    g.connect(v, voicer::OUT_GATE, env, envelope::IN_GATE);
    g.connect(env, envelope::OUT_AUDIO, out, output::IN_AUDIO);
    g.tap_output(out, output::OUT_AUDIO);

    g.set_param(filt, "cutoff", 3_000.0);
    g
}
