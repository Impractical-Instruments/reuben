//! Registry — maps an operator's stable type name to a constructor + descriptor.
//!
//! The instrument loader (`reuben-document`'s `format`) and the `describe` projections
//! (`reuben-document`'s `describe`) both need to turn a type-name string (from a JSON document) into a
//! live operator and to enumerate every operator's self-description. [`Registry::builtin`] holds the MVP
//! operator set; [`Registry::register`] lets an embedder add its own operator types
//! (the seam for the "agents author new Operators in Rust" goal).
//!
//! The built-in set is an ordinary array the compiler can see — [`operator_census!`], invoked in
//! `operators/mod.rs` — not a link-time collection.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::descriptor::Descriptor;
use crate::operator::Operator;

/// One operator's entry in the built-in census: how to build it, and how to ask it to describe
/// itself. A plain `const`-constructible value, so a census is an ordinary array the compiler
/// sees.
pub(crate) struct OpReg {
    /// Construct a fresh instance with default state.
    pub make: fn() -> Box<dyn Operator>,
    /// The operator's self-description (non-const: holds `Vec`s), hence a fn pointer not a value.
    pub descriptor: fn() -> Descriptor,
}

/// One [`OpReg`] **value** for an operator type, as an expression.
///
/// The type needs an inherent `new()` and an `impl Operator`. `descriptor` is reached through a
/// qualified trait path so a caller owes no `use` of its own — the census sites are module files
/// and macro expansions that name the type and nothing else.
macro_rules! op_reg {
    ($t:ty) => {
        $crate::registry::OpReg {
            make: || $crate::__alloc::Box::new(<$t>::new()),
            descriptor: <$t as $crate::operator::Operator>::descriptor,
        }
    };
}
pub(crate) use op_reg;

/// Declare the built-in operator set: one line per module, folding that module's `pub mod`
/// declaration, its flat re-export, and its registration into a single entry.
///
/// Invoked once, in `operators/mod.rs`, over an alphabetical list of two entry forms:
///
/// ```ignore
/// crate::operator_census! {
///     abs::*,               // a macro-generated family: splice the module's own `OPERATORS`
///     chord::{Chord},       // hand-written types, named
/// }
/// ```
///
/// It emits `pub mod <m>;`, the re-export (`pub use <m>::*;` / `pub use <m>::<T>;`), and a
/// `CENSUS: &[&[OpReg]]` whose entries are `<m>::OPERATORS` for the `*` form and an inline array
/// for the named form. A module the census does not name is not a built-in operator — `pipe` is
/// declared outside it deliberately.
macro_rules! operator_census {
    ( $($entries:tt)* ) => {
        $crate::registry::operator_census_step!(@parse [] [] $($entries)*);
    };
}
pub(crate) use operator_census;

/// The token muncher behind [`operator_census!`]: one entry per step, accumulating the items to
/// emit and the `CENSUS` elements in the two bracketed lists. Separate from the entry point
/// because a `macro_rules!` arm cannot both consume a list and accumulate two outputs in one pass.
macro_rules! operator_census_step {
    (@parse [$($items:tt)*] [$($slices:tt)*]) => {
        $($items)*

        /// The built-in operator set, as data: one `&[OpReg]` per module the census names.
        ///
        /// A slice of slices rather than one flat array because a `const` cannot concatenate
        /// slices — a macro-generated family contributes its module's whole `OPERATORS` array,
        /// a hand-written module contributes an inline one. [`Registry::builtin`] flattens it.
        pub(crate) const CENSUS: &[&[$crate::registry::OpReg]] = &[ $($slices)* ];
    };

    // `m::*,` — the module's own `OPERATORS`, emitted by whichever macro generated its operators.
    (@parse [$($items:tt)*] [$($slices:tt)*] $m:ident :: * , $($rest:tt)*) => {
        $crate::registry::operator_census_step!(@parse
            [$($items)* pub mod $m; pub use $m::*;]
            [$($slices)* $m::OPERATORS,]
            $($rest)*
        );
    };

    // `m::{A, B},` — hand-written types, named.
    (@parse [$($items:tt)*] [$($slices:tt)*] $m:ident :: { $($t:ident),+ $(,)? } , $($rest:tt)*) => {
        $crate::registry::operator_census_step!(@parse
            [$($items)* pub mod $m; $( pub use $m::$t; )+]
            [$($slices)* &[ $( $crate::registry::op_reg!($m::$t) ),+ ],]
            $($rest)*
        );
    };
}
pub(crate) use operator_census_step;

/// One registered operator type: how to build it, and its self-description.
pub struct Entry {
    /// Construct a fresh instance with default state.
    pub make: fn() -> Box<dyn Operator>,
    /// The type's self-description, behind a shared handle. Every field of a [`Descriptor`] is a
    /// function of the operator **type**, not the instance, and nothing mutates one after
    /// registration — so each node built from this entry clones the handle rather than the port
    /// lists, and one registry costs one descriptor per registered type however many nodes name it.
    pub descriptor: Arc<Descriptor>,
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

    /// The built-in operator set, built from the [`operator_census!`] array in
    /// [`crate::operators`]. Iteration is in census (source) order; the `BTreeMap` re-keys by
    /// `type_name`, so the census order never reaches an output.
    pub fn builtin() -> Self {
        Self::from_census(crate::operators::CENSUS.iter().copied().flatten())
    }

    /// Build a registry from a census, panicking on a duplicate `type_name` — two built-ins
    /// claiming one name is a build error.
    ///
    /// The duplicate assertion belongs **here and not in [`register`](Self::register)**:
    /// `register` is the embedder override seam and must stay last-writer-wins. Taking the census
    /// as an argument is what lets the determinism test feed the same entries in a different
    /// order and compare (`permuting_the_census_yields_the_same_iteration_order`).
    fn from_census<'a>(regs: impl IntoIterator<Item = &'a OpReg>) -> Self {
        let mut r = Self::new();
        for reg in regs {
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
        self.entries.insert(
            descriptor.type_name,
            Entry {
                make,
                descriptor: Arc::new(descriptor),
            },
        );
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

    // Churn-free invariants over whatever the census names, so adding an operator does not edit
    // a test.

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
        // A directly-referenced array cannot go missing, so this is cheap insurance against an
        // emptied census rather than the load-bearing check it was under link-time collection.
        assert!(
            Registry::builtin().type_names().count() >= 3,
            "builtin() gathered no operators — the census is empty"
        );
    }

    #[test]
    fn builtin_contains_the_load_bearing_ops() {
        // A few operators no instrument can do without. Names, not count, so it doesn't churn
        // when operators are added.
        let r = Registry::builtin();
        for name in ["oscillator", "output", "voicer"] {
            assert!(r.get(name).is_some(), "built-in {name:?} not registered");
        }
    }

    // Determinism is two independent properties, and each is blind to the other's mutation. They
    // are kept as two tests so a failure says which one broke.

    #[test]
    fn permuting_the_census_yields_the_same_iteration_order() {
        // Property one: **output does not depend on census order.** Feed the same entries forward
        // and reversed; the `BTreeMap` re-key by `type_name` is what makes the results identical,
        // so this reds the moment iteration starts leaking the order entries arrived in.
        //
        // Asserting `type_names()` is ascending cannot see this: over a reversed *submission* the
        // map re-keys anyway, so ascending stays trivially true — which is how the earlier attempt
        // shipped an assertion that passed while iteration was deliberately reversed.
        let flat: Vec<&OpReg> = crate::operators::CENSUS.iter().copied().flatten().collect();
        let forward: Vec<&str> = Registry::from_census(flat.iter().copied())
            .type_names()
            .collect();
        let reversed: Vec<&str> = Registry::from_census(flat.iter().copied().rev())
            .type_names()
            .collect();
        assert_eq!(forward, reversed, "iteration order depends on census order");
        assert!(forward.len() > 1, "fixture: needs at least two entries");
    }

    #[test]
    fn enumeration_is_name_ordered_and_the_two_enumerators_agree() {
        // Property two: **output is name-ordered**, which is what makes the generated schema and
        // `describe` byte-stable across builds. The test above is blind to this — reversing the
        // iterator reverses both of its sides equally, so it stays green while every consumer's
        // output silently reorders.
        //
        // Strictly ascending, so it doubles as the uniqueness check on the map's keys. Both
        // enumerators are pinned: `entries()` feeds the schema and `type_names()` feeds `describe`,
        // and a reversal of either one alone would reorder one consumer and not the other.
        let r = Registry::builtin();
        let names: Vec<&str> = r.type_names().collect();
        assert!(
            names.windows(2).all(|w| w[0] < w[1]),
            "type_names() must be strictly ascending: {names:?}"
        );
        let by_entry: Vec<&str> = r.entries().map(|e| e.descriptor.type_name).collect();
        assert_eq!(
            names, by_entry,
            "entries() and type_names() disagree on order"
        );
    }

    // Every integer control port is an `i32` value port. One central assertion so each one — not
    // only `euclid.steps` — is pinned to its type; a silent regression of any one back to `f32`
    // would restore the round-in-`process` dance and reopen the F32→I32 wire that widening exists
    // to avoid.
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
        // Deref-then-clone: the entry hands out a shared handle, and this fixture needs a
        // *divergent* descriptor, so it copies the pointee rather than the pointer.
        let mut second = (*osc.descriptor).clone();
        second.inputs.push(second.inputs[0].clone());

        let mut r = Registry::new();
        r.register(osc.make, (*osc.descriptor).clone());
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
