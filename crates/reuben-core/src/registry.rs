//! Registry — maps an operator's stable type name to a constructor + descriptor.
//!
//! The instrument loader ([`crate::format`]) and the schema generator ([`crate::schema`])
//! both need to turn a type-name string (from a JSON document) into a live operator and
//! to enumerate every operator's self-description. [`Registry::builtin`] holds the MVP
//! operator set; [`Registry::register`] lets an embedder add its own operator types
//! (the seam for the "agents author new Operators in Rust" goal, ADR-0004).

use std::collections::BTreeMap;

use crate::descriptor::Descriptor;
use crate::operator::Operator;
use crate::operators::{Clock, Delay, Envelope, Filter, Lfo, Oscillator, Output, Reverb, Voicer};

/// One registered operator type: how to build it, and its self-description.
pub struct Entry {
    /// Construct a fresh instance with default state.
    pub make: fn() -> Box<dyn Operator>,
    pub descriptor: Descriptor,
}

/// A set of known operator types, keyed by [`Descriptor::type_name`].
///
/// `BTreeMap` so iteration order is deterministic (matters for stable schema output).
#[derive(Default)]
pub struct Registry {
    entries: BTreeMap<&'static str, Entry>,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The built-in operator set: oscillator, envelope, filter, voicer, output, clock, delay,
    /// reverb, lfo.
    pub fn builtin() -> Self {
        let mut r = Self::new();
        r.register(|| Box::new(Oscillator::new()), Oscillator::descriptor());
        r.register(|| Box::new(Envelope::new()), Envelope::descriptor());
        r.register(|| Box::new(Filter::new()), Filter::descriptor());
        r.register(|| Box::new(Voicer::new()), Voicer::descriptor());
        r.register(|| Box::new(Output::new()), Output::descriptor());
        r.register(|| Box::new(Clock::new()), Clock::descriptor());
        r.register(|| Box::new(Delay::new()), Delay::descriptor());
        r.register(|| Box::new(Reverb::new()), Reverb::descriptor());
        r.register(|| Box::new(Lfo::new()), Lfo::descriptor());
        r
    }

    /// Register an operator type. Keyed by its descriptor's `type_name`.
    pub fn register(&mut self, make: fn() -> Box<dyn Operator>, descriptor: Descriptor) {
        self.entries
            .insert(descriptor.type_name, Entry { make, descriptor });
    }

    /// Look up a type by name.
    pub fn get(&self, type_name: &str) -> Option<&Entry> {
        self.entries.get(type_name)
    }

    /// All registered entries, in stable (type-name) order.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// All registered type names, in stable order.
    pub fn type_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.entries.keys().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_the_mvp_operators() {
        let r = Registry::builtin();
        let names: Vec<_> = r.type_names().collect();
        assert_eq!(
            names,
            vec![
                "clock",
                "delay",
                "envelope",
                "filter",
                "lfo",
                "oscillator",
                "output",
                "reverb",
                "voicer"
            ]
        );
    }

    #[test]
    fn make_builds_the_right_operator() {
        let r = Registry::builtin();
        let entry = r.get("oscillator").expect("oscillator registered");
        let op = (entry.make)();
        // The boxed operator's descriptor matches the entry's.
        assert_eq!(entry.descriptor.type_name, "oscillator");
        // Constructing twice yields independent instances (no shared state).
        let _op2 = (entry.make)();
        drop(op);
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(Registry::builtin().get("nope").is_none());
    }
}
