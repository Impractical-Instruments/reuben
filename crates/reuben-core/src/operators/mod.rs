//! The MVP operator set.
//!
//! Each operator lives in its own file with frozen ports/params (declared here in Stage
//! A) and is filled in test-first in Stage B. Port/param indices are part of the contract
//! the rig builder wires against — see each module's descriptor.

pub mod clock;
pub mod delay;
pub mod envelope;
pub mod filter;
pub mod oscillator;
pub mod output;
pub mod voicer;

pub use clock::Clock;
pub use delay::Delay;
pub use envelope::Envelope;
pub use filter::Filter;
pub use oscillator::Oscillator;
pub use output::Output;
pub use voicer::Voicer;
