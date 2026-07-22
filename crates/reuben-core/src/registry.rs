//! Registry — maps an operator's stable type name to a constructor + descriptor.
//!
//! The instrument loader ([`crate::format`]) and the `describe` projections
//! ([`crate::describe`]) both need to turn a type-name string (from a JSON document) into a
//! live operator and to enumerate every operator's self-description. [`Registry::builtin`] holds the MVP
//! operator set; [`Registry::register`] lets an embedder add its own operator types
//! (the seam for the "agents author new Operators in Rust" goal).

use std::collections::BTreeMap;

use crate::descriptor::Descriptor;
use crate::operator::Operator;

/// A compile-time operator registration, submitted at each operator's definition site via
/// [`register_operator!`] and collected by `inventory` into a link-time slice. This
/// replaces the hand-maintained `builtin()` list that every new operator used to edit — the
/// merge-conflict magnet — so an operator self-registers where it is defined.
pub struct OpReg {
    /// Construct a fresh instance with default state.
    pub make: fn() -> Box<dyn Operator>,
    /// The operator's self-description (non-const: holds `Vec`s), hence a fn pointer not a value.
    pub descriptor: fn() -> Descriptor,
}

inventory::collect!(OpReg);

/// Register an operator type with the built-in [`Registry`] at compile time.
///
/// Invoke **by path** at the operator's definition site, after its `impl Operator`:
/// `crate::register_operator!(MyOp);`. The macro name is the greppable census of built-ins —
/// `grep -rn 'register_operator!' src/operators/` enumerates every built-in operator.
macro_rules! register_operator {
    ($t:ty) => {
        inventory::submit! {
            $crate::registry::OpReg {
                make: || Box::new(<$t>::new()),
                descriptor: <$t>::descriptor,
            }
        }
    };
}
// Re-export at the crate root so operator modules can call `crate::register_operator!(..)`
// regardless of source order (macro_rules visibility is lexical without this).
pub(crate) use register_operator;

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

    /// The built-in operator set, gathered from every [`register_operator!`] submission across
    /// the crate. Each operator self-registers at its definition site, so adding one
    /// no longer edits any central list. Iteration here is in link order; the `BTreeMap` re-keys
    /// by `type_name` for deterministic output. Panics on a duplicate `type_name` — a build-time
    /// assertion that two operators don't claim the same name (the override seam stays in
    /// [`register`](Self::register), which still last-writer-wins for embedders).
    pub fn builtin() -> Self {
        let mut r = Self::new();
        for reg in inventory::iter::<OpReg> {
            let descriptor = (reg.descriptor)();
            assert!(
                r.get(descriptor.type_name).is_none(),
                "duplicate operator type_name {:?} — two operators registered the same name",
                descriptor.type_name
            );
            r.register(reg.make, descriptor);
        }
        r
    }

    /// Register an operator type. Keyed by its descriptor's `type_name`.
    ///
    /// Panics on the reserved name `"pipe"`: interface pipes are **loader-built**
    /// — declared through `interface.inputs` entries, never as document nodes — and save
    /// (`NormalizedDoc::from_graph`) identifies pipe nodes by that type name — a registered
    /// `"pipe"` operator's nodes would silently vanish on save. Fail loudly at registration
    /// (a programming error in the embedder, not a document error).
    pub fn register(&mut self, make: fn() -> Box<dyn Operator>, descriptor: Descriptor) {
        assert_ne!(
            descriptor.type_name, "pipe",
            "operator type name \"pipe\" is reserved: interface pipes are loader-built \
             and save identifies their nodes by this name"
        );
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

    // The built-in set is no longer an enumerated list (it self-registers), so these
    // are churn-free invariants over whatever the `inventory` slice gathered, plus a small canary
    // that fails loudly if the linker ever dead-strips the submissions.

    #[test]
    #[should_panic(expected = "reserved")]
    fn registering_the_reserved_pipe_name_panics() {
        // Pipes are loader-built; an embedder-registered "pipe" operator's nodes
        // would silently vanish on save (`from_graph` drops nodes by this type name).
        use crate::operator::Operator;
        let mut r = Registry::new();
        r.register(
            || {
                Box::new(crate::operators::pipe::Pipe::new(
                    crate::plan::PortKind::Value,
                ))
            },
            crate::operators::pipe::Pipe::descriptor(),
        );
    }

    #[test]
    fn builtin_is_nonempty() {
        // If self-registration silently produced nothing (dead-stripped slice), this trips first.
        assert!(
            Registry::builtin().type_names().count() >= 3,
            "builtin() gathered no operators — inventory submissions may have been dropped"
        );
    }

    #[test]
    fn builtin_contains_the_load_bearing_ops() {
        // Anti-dead-strip canary: a few operators no instrument can do without. Names, not count,
        // so it doesn't churn when operators are added — but it still proves registration ran.
        let r = Registry::builtin();
        for name in ["oscillator", "output", "voicer"] {
            assert!(r.get(name).is_some(), "built-in {name:?} not registered");
        }
    }

    // Every integer control port converted in #556 PR 2 (#565) is an `i32` value port. One
    // central assertion so each converted port — not only `euclid.steps` — is pinned to its type;
    // a silent regression of any one back to `f32` would restore the round-in-`process` dance and
    // reopen the `F32 -> I32` wire the migration closes.
    #[test]
    fn the_converted_integer_control_ports_are_i32() {
        use crate::descriptor::PortType;
        let r = Registry::builtin();
        let is_i32 = |type_name: &str, port: &str| {
            let d = &r.get(type_name).expect("registered").descriptor;
            let p = d
                .inputs
                .iter()
                .find(|p| p.name == port)
                .unwrap_or_else(|| panic!("{type_name} has no input {port:?}"));
            assert!(
                matches!(p.ty, PortType::I32 { meta: Some(_) }),
                "{type_name}.{port} should be an i32 value port, got {:?}",
                p.ty
            );
        };
        for port in ["steps", "pulses", "rotation"] {
            is_i32("euclid", port);
        }
        for port in [
            "root", "degrees", "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10",
            "s11",
        ] {
            is_i32("harmony", port);
        }
        is_i32("clock", "division");
        is_i32("chord", "size");
        is_i32("sample", "channel");
        is_i32("granulator", "channel");
    }

    #[test]
    fn every_entry_round_trips() {
        // Each registered entry's stored descriptor name matches its map key, and `make` yields a
        // live operator without panicking — the registration wired its constructor consistently.
        // (`descriptor()` is static/`Sized`, so a boxed `dyn Operator` can't re-report its name;
        // the key↔descriptor identity below is what proves the entry is self-consistent.)
        let r = Registry::builtin();
        for name in r.type_names() {
            let entry = r.get(name).expect("type_names yields registered keys");
            assert_eq!(
                entry.descriptor.type_name, name,
                "key vs descriptor mismatch"
            );
            let _op = (entry.make)();
        }
    }

    #[test]
    fn type_names_are_snake_case() {
        let r = Registry::builtin();
        for name in r.type_names() {
            assert!(!name.is_empty(), "empty type_name");
            let mut chars = name.chars();
            assert!(
                chars.next().is_some_and(|c| c.is_ascii_lowercase()),
                "{name:?} must start with a lowercase letter"
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{name:?} must be snake_case [a-z0-9_]"
            );
        }
    }

    #[test]
    fn register_overrides_an_existing_type_name_last_writer_wins() {
        // builtin() panics on a duplicate type_name (two built-ins claiming
        // one name is a build error), but register() is the *embedder override seam* — a
        // re-registration under an existing name must replace the entry, not panic and not keep
        // the old one. That asymmetry is the contract; a "harden the invariant" refactor moving
        // builtin()'s duplicate assert down into register() would break every embedder override.
        let builtins = Registry::builtin();
        let osc = builtins.get("oscillator").expect("oscillator registered");
        // A distinguishable override descriptor under the same name: one extra input port
        // (cloned from the original, so the fixture doesn't care what oscillator's ports are).
        let mut second = osc.descriptor.clone();
        second.inputs.push(second.inputs[0].clone());

        let mut r = Registry::new();
        r.register(osc.make, osc.descriptor.clone());
        r.register(osc.make, second.clone()); // must not panic, must replace

        let entry = r
            .get("oscillator")
            .expect("still registered after override");
        assert_eq!(
            entry.descriptor.inputs.len(),
            second.inputs.len(),
            "descriptor replaced, not kept"
        );
        assert_ne!(
            entry.descriptor.inputs.len(),
            osc.descriptor.inputs.len(),
            "fixture: the override must be distinguishable from the original"
        );
        assert_eq!(
            r.type_names().filter(|n| *n == "oscillator").count(),
            1,
            "replace, not accumulate"
        );
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(Registry::builtin().get("nope").is_none());
    }
}
