//! MIDI input for `play`: each MIDI message is translated to an OSC-shaped message on the
//! `/midi/<ch>/…` address scheme, which the instrument's MIDI map (`surfaces/<stem>.midi.json`)
//! routes to its interface inputs. Nothing here runs on the render thread; what reaches the engine
//! is an ordinary `/<name>/in` send.

pub mod devices;
pub mod router;
pub mod routes;
pub mod translate;
pub mod watch;
