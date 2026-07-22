//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.

pub mod abs;
pub mod add;
pub mod ceil;
pub mod chord;
pub mod clamp;
pub mod clock;
pub mod compressor;
pub mod delay;
pub mod differentiate;
pub mod div;
pub mod djfilter;
pub mod edge;
pub mod envelope;
pub mod euclid;
pub mod filter;
pub mod floor;
pub mod granulator;
pub mod harmony;
pub mod integrate;
pub mod lfo;
pub mod m2s;
pub mod map;
/// Shared test helpers for the generated number operators (issue #104).
#[cfg(test)]
pub mod math_test;
pub mod max;
pub mod min;
pub mod modulo;
pub mod mul;
pub mod negate;
pub mod noise;
pub mod osc_out;
pub mod oscillator;
pub mod output;
pub mod pan;
pub mod pipe;
pub mod pitch2freq;
/// The total arithmetic the generated number operators' scalar fns are written over (issue #556).
pub mod pointwise;
pub mod power;
pub mod reciprocal;
pub mod resonator;
pub mod reverb;
pub mod round;
/// The rounding the generated converter operators cross number types with (issue #556).
pub mod rounding;
pub mod sample;
pub mod saturator;
pub mod sequencer;
pub mod snap;
pub mod strum;
pub mod sub;
pub mod subpatch;
pub mod transpose;
pub mod trunc;
pub mod unpack;
pub mod voicer;

pub use abs::{AbsF32Signal, AbsF32Value};
pub use add::{AddF32Signal, AddF32Value};
pub use ceil::{CeilF32Signal, CeilF32Value};
pub use chord::Chord;
pub use clamp::{ClampF32Signal, ClampF32Value};
pub use clock::Clock;
pub use compressor::Compressor;
pub use delay::Delay;
pub use differentiate::DifferentiateF32Signal;
pub use div::{DivF32Signal, DivF32Value};
pub use djfilter::Djfilter;
pub use envelope::Envelope;
pub use euclid::Euclid;
pub use filter::Filter;
pub use floor::{FloorF32Signal, FloorF32Value};
pub use granulator::Granulator;
pub use harmony::HarmonyOp;
pub use integrate::IntegrateF32Signal;
pub use lfo::Lfo;
pub use m2s::M2s;
pub use map::{MapF32Signal, MapF32Value};
pub use max::{MaxF32Signal, MaxF32Value};
pub use min::{MinF32Signal, MinF32Value};
pub use modulo::{ModuloF32Signal, ModuloF32Value};
pub use mul::{MulF32Signal, MulF32Value};
pub use negate::{NegateF32Signal, NegateF32Value};
pub use noise::Noise;
pub use osc_out::OscOut;
pub use oscillator::Oscillator;
pub use output::Output;
pub use pan::Pan;
pub use pitch2freq::Pitch2Freq;
pub use power::{PowerF32Signal, PowerF32Value};
pub use reciprocal::{ReciprocalF32Signal, ReciprocalF32Value};
pub use resonator::Resonator;
pub use reverb::Reverb;
pub use round::{RoundF32Signal, RoundF32Value};
pub use sample::SamplePlayer;
pub use saturator::Saturator;
pub use sequencer::Sequencer;
pub use snap::Snap;
pub use strum::Strum;
pub use sub::{SubF32Signal, SubF32Value};
pub use subpatch::Subpatch;
pub use transpose::Transpose;
pub use trunc::{TruncF32Signal, TruncF32Value};
pub use unpack::UnpackNote;
pub use voicer::Voicer;
