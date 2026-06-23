//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.

pub mod chord;
pub mod clock;
pub mod context;
pub mod delay;
pub mod djfilter;
pub mod envelope;
pub mod filter;
pub mod lfo;
pub mod m2s;
pub mod math;
pub mod noise;
pub mod oscillator;
pub mod output;
pub mod reverb;
pub mod sample;
pub mod sequencer;
pub mod snap;
pub mod strum;
pub mod voicer;

pub use chord::Chord;
pub use clock::Clock;
pub use context::ContextOp;
pub use delay::Delay;
pub use djfilter::Djfilter;
pub use envelope::Envelope;
pub use filter::Filter;
pub use lfo::Lfo;
pub use m2s::M2s;
pub use math::{Add, Differentiate, Integrate, Map, Mul};
pub use noise::Noise;
pub use oscillator::Oscillator;
pub use output::Output;
pub use reverb::Reverb;
pub use sample::SamplePlayer;
pub use sequencer::Sequencer;
pub use snap::Snap;
pub use strum::Strum;
pub use voicer::Voicer;
