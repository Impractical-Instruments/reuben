//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.

pub mod add;
pub mod chord;
pub mod clock;
pub mod delay;
pub mod differentiate;
pub mod djfilter;
pub mod edge;
pub mod envelope;
pub mod euclid;
pub mod filter;
pub mod harmony;
pub mod integrate;
pub mod lfo;
pub mod m2s;
pub mod map;
pub mod mul;
pub mod noise;
pub mod osc_out;
pub mod oscillator;
pub mod output;
pub mod pan;
pub mod power;
pub mod reverb;
pub mod sample;
pub mod sequencer;
pub mod snap;
pub mod strum;
pub mod transpose;
pub mod voicer;

/// Shared test helpers for the generated number operators (issue #104, ADR-0033).
#[cfg(test)]
pub mod math_test;

pub use add::{AddF32Signal, AddF32Value};
pub use chord::Chord;
pub use clock::Clock;
pub use delay::Delay;
pub use differentiate::DifferentiateF32Signal;
pub use djfilter::Djfilter;
pub use envelope::Envelope;
pub use euclid::Euclid;
pub use filter::Filter;
pub use harmony::HarmonyOp;
pub use integrate::IntegrateF32Signal;
pub use lfo::Lfo;
pub use m2s::M2s;
pub use map::Map;
pub use mul::{MulF32Signal, MulF32Value};
pub use noise::Noise;
pub use osc_out::OscOut;
pub use oscillator::Oscillator;
pub use output::Output;
pub use pan::Pan;
pub use power::value::PowerF32Value;
pub use power::PowerF32Signal;
pub use reverb::Reverb;
pub use sample::SamplePlayer;
pub use sequencer::Sequencer;
pub use snap::Snap;
pub use strum::Strum;
pub use transpose::Transpose;
pub use voicer::Voicer;
