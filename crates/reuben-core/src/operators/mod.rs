//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.
//!
//! The [`operator_census!`](crate::registry::operator_census) block below **is** the built-in
//! operator set: one line per module, declaring the module, re-exporting its types, and
//! registering them. Adding an operator is that one line.
//! see rules: composition-operators

// Modules that contribute no built-in operator, so they sit outside the census.
// `pipe` is an operator but deliberately unregistered: pipes are loader-built from
// `interface.inputs` and never named as document nodes.
pub mod edge;
/// Shared test helpers for the generated number operators.
#[cfg(test)]
pub mod math_test;
pub mod pipe;
/// The total arithmetic the generated number operators' scalar fns are written over.
pub mod pointwise;
/// The rounding the generated converter operators cross number types with.
pub mod rounding;

// `m::*` — a macro-generated family (`number_operator_contract!` / `unpack_op!`); the module
// carries its own `OPERATORS` array, so adding a variant stays a one-line edit *there*.
// `m::{A, B}` — hand-written operator types, named.
crate::operator_census! {
    abs::*,
    add::*,
    ceil::*,
    chord::{Chord},
    clamp::*,
    clock::{Clock},
    compressor::{Compressor},
    delay::{Delay},
    differentiate::{DifferentiateF32Signal},
    div::*,
    djfilter::{Djfilter},
    envelope::{Envelope},
    euclid::{Euclid},
    filter::{Filter},
    floor::*,
    granulator::{Granulator},
    harmony::{HarmonyOp},
    integrate::{IntegrateF32Signal},
    lfo::{Lfo},
    m2s::{M2s},
    map::*,
    max::*,
    min::*,
    modulo::*,
    mul::*,
    negate::*,
    noise::{Noise},
    osc_out::{OscOut},
    oscillator::{Oscillator},
    output::{Output},
    pan::{Pan},
    pitch2freq::{Pitch2Freq},
    power::*,
    reciprocal::*,
    resonator::{Resonator},
    reverb::{Reverb},
    round::*,
    sample::{SamplePlayer},
    saturator::{Saturator},
    sequencer::{Sequencer},
    snap::{Snap},
    strum::{Strum},
    sub::*,
    subpatch::{Subpatch},
    transpose::{Transpose},
    trunc::*,
    unpack::*,
    voicer::{Voicer},
}
