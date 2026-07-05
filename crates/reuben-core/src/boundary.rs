//! Boundary — the OSC ⇄ [`Arg`] conversion at the external edge (ADR-0007, ADR-0026, ADR-0030).
//!
//! The native layer decodes a UDP datagram into an address plus a flat list of **primitive**
//! `Arg`s (the OSC atoms `F32`/`I32`/`Str`) and, on the way out, encodes the same. The two
//! conversion functions here are the *typed* half in between: [`osc_in_arg`] turns that flat
//! list into the single `Arg` a destination port carries, and [`osc_out_args`] expands one
//! internal `Arg` back into the flat list to send.
//!
//! **Dest-port-type-driven** (ADR-0030, Q10a). External OSC routes by address to a node/port; the
//! **port's declared [`PortType`]** drives [`osc_in_arg`]. A primitive port wraps the single arg; a
//! vocab enum resolves it via its [`EnumMeta`](crate::descriptor::EnumMeta); a struct vocab type
//! unpacks the flat form via the converter it registered with [`register_osc_form!`] (its
//! [`OscArg::from_osc`], keyed by the port's declared type name — port-authority, epic #146). A
//! [`Buffer`](Arg::F32Buffer) port has no OSC form, so audio cannot cross, and a struct type
//! that registers no form (`Harmony`; its wire form is issue #209) cannot either — the opt-out
//! is by construction / by omission.
//!
//! **The converter registry** ([`OscForm`], issues #204/#205): struct vocab converters
//! self-register at their definition site and are collected by `inventory` into a link-time
//! slice (the ADR-0024 pattern). [`osc_form_by_name`] serves the inbound decode; [`has_form`]
//! serves [`has_osc_form`]'s capability key. Only those two sides are registry-backed:
//! outbound ([`osc_out_args`]) stays a **closed exhaustive match** over [`Arg`] — see
//! [`OscForm`]'s docs for why — with the
//! `has_osc_form_matches_what_the_drain_can_send` test pinning the name-keyed registry to the
//! variant-keyed drain.

use crate::descriptor::{Port, PortType};
use crate::message::{Arg, OscArg};

/// A compile-time OSC-form registration for a **struct vocab type** (issue #204, epic #146),
/// submitted at the type's definition site via [`register_osc_form!`] and collected by
/// `inventory` into a link-time slice — the same self-registration pattern as the operator
/// registry's [`OpReg`](crate::registry::OpReg) (ADR-0024). Keyed by
/// [`PortType::Vocab`]'s `name`, the inbound + capability authority: [`osc_form_by_name`]
/// serves [`osc_in_arg`]'s struct decode and [`has_form`] serves [`has_osc_form`]'s
/// capability key, so a struct type registers its external form once instead of editing
/// hand-maintained matches here.
///
/// Inbound + capability only, deliberately: [`osc_out_args`] dispatches on the **closed**
/// [`Arg`] enum — a struct type's own variant is a central-enum addition regardless — so the
/// outbound side stays an exhaustive match whose struct arms are one-line
/// [`OscArg::to_osc`] delegations, never a runtime registry.
pub struct OscForm {
    /// The vocab type's name — the [`PortType::Vocab`] `name` this form is looked up by.
    pub type_name: &'static str,
    /// Build the single internal [`Arg`] from the flat external OSC arg list, or `None`
    /// when the args don't fit (malformed forms drop, never default-fill).
    pub from_osc: fn(&[Arg]) -> Option<Arg>,
}

inventory::collect!(OscForm);

/// Register a struct vocab type's external OSC form at compile time (issue #204, mirroring
/// [`register_operator!`](crate::registry) — ADR-0024).
///
/// Invoke **by path** next to the type's [`OscArg`] impl: `crate::register_osc_form!(Note);`.
/// The submitted entry wraps `<T as OscArg>::from_osc(args).map(Arg::from)`, so it requires
/// `T: OscArg` and an `Arg` variant (via the `ArgValue`-derived `From<T>`). The macro name is
/// the greppable census of boundary-crossing struct forms.
macro_rules! register_osc_form {
    ($t:ty) => {
        inventory::submit! {
            $crate::boundary::OscForm {
                type_name: ::core::stringify!($t),
                from_osc: |args| {
                    <$t as $crate::message::OscArg>::from_osc(args)
                        .map($crate::message::Arg::from)
                },
            }
        }
    };
}
// Re-export at the crate root (via `lib.rs`) so vocab modules can call
// `crate::register_osc_form!(..)` regardless of source order (macro_rules visibility is lexical).
pub(crate) use register_osc_form;

/// Look up a struct vocab type's registered [`OscForm`] by its [`PortType::Vocab`] `name`.
/// `None` means the type has no external OSC form — the boundary opt-out (`Harmony`).
/// A walk of the link-time slice: allocation-free, and the slice is a handful of entries.
pub fn osc_form_by_name(name: &str) -> Option<&'static OscForm> {
    inventory::iter::<OscForm>
        .into_iter()
        .find(|f| f.type_name == name)
}

/// Whether a struct vocab type of this name has a registered external OSC form — the
/// registry-backed half of [`has_osc_form`]'s capability key.
pub fn has_form(name: &str) -> bool {
    osc_form_by_name(name).is_some()
}

/// Convert a flat OSC arg list into the single [`Arg`] a destination port carries, driven by the
/// **destination port** (ADR-0030). `None` when the args don't fit the port (a wrong-typed wire —
/// dropped) or the port has no OSC form (a *bare* [`Buffer`](Arg::F32Buffer): audio never crosses).
///
/// - **F32 / I32 / Str** — wrap the first arg (numeric coercion as for any `Arg`).
/// - **F32Buffer with meta** — a signal control carrying a scalar default (`f32_buffer` + meta,
///   e.g. `djfilter.position`): crosses as a clamped `F32`, materialized ZOH downstream — a control
///   surface can sweep it (ADR-0030/0031). The port's `meta` is what distinguishes it from audio.
/// - **Vocab enum** — resolve the first arg (symbol or index) via the port's resolver.
/// - **Vocab struct** — unpack the flat form via the type's [`OscArg::from_osc`].
/// - **bare F32Buffer** (no meta — audio) — `None` (opt-out).
pub fn osc_in_arg(p: &Port, args: &[Arg]) -> Option<Arg> {
    match &p.ty {
        PortType::F32 => args.first().and_then(Arg::as_f32).map(Arg::F32),
        PortType::I32 { .. } => args
            .first()
            .and_then(|a| match a {
                Arg::I32(i) => Some(*i),
                Arg::F32(f) => Some(f.round() as i32),
                _ => None,
            })
            .map(Arg::I32),
        PortType::Str => args.first().and_then(|a| match a {
            Arg::Str(s) => Some(Arg::Str(s.clone())),
            _ => None,
        }),
        // A signal control with a scalar default (`f32_buffer` + meta, e.g. `djfilter.position`):
        // accept a numeric arg like an `F32` control and cross as `F32` — the ZOH bridge
        // materializes it downstream. Clamp to the port's range here, since the Signal materialize
        // path (unlike a Value's `held_arg`) does no later clamp. `meta` is the audio/control split.
        PortType::F32Buffer if p.meta.is_some() => args
            .first()
            .and_then(Arg::as_f32)
            .map(|v| Arg::F32(p.meta.as_ref().map(|m| m.clamp(v)).unwrap_or(v))),
        // A *bare* Buffer (audio) is not boundary-crossable (ADR-0030): no OSC form.
        PortType::F32Buffer => None,
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => args.first().and_then(|a| e.resolve_arg(a)),
        // A struct vocab type: look up its registered converter ([`OscForm`], issue #205) by the
        // port's declared type name and unpack the flat form. A name with no registration
        // (e.g. `Harmony`) has no OSC form — opt-out by not calling `register_osc_form!`.
        PortType::Vocab {
            enum_meta: None,
            name,
            ..
        } => osc_form_by_name(name).and_then(|f| (f.from_osc)(args)),
        // A type-agnostic pass-through (issue #141, the `osc_out` sink's input): a single atom
        // with a verbatim single-Arg form — numeric or string — crosses as-is: the OSC
        // echo/loopback path (fader/encoder/label feedback). The string atom joined once
        // `Arg::Str` went `Arc<str>`-backed (issues #206/#207): forwarding it through
        // `osc_out.process()` is now a refcount bump, not a heap clone (ADR-0009 holds). A
        // multi-arg list still has no unambiguous single-Arg form (the port names no vocab type
        // to unpack it), so it drops — a typed destination port decodes those.
        PortType::Arg => match args {
            [a @ (Arg::F32(_) | Arg::I32(_) | Arg::Str(_))] => Some(a.clone()),
            _ => None,
        },
    }
}

/// Whether a wire of this declared [`PortType`] can ever cross the outbound boundary — the
/// **capability key** for legality into a type-agnostic [`Arg`](PortType::Arg) pass-through input
/// (issue #141). This is the single statement of "has an external OSC form": the load-time
/// compat check (`format.rs`) and the plan-time form check (`plan.rs`) both consume it, so
/// legality and [`osc_out_args`] cannot drift — a type is wireable into the pass-through **iff**
/// the drain produces a non-empty form for it. A wire that could never send anything is a
/// patching mistake, rejected loud at load/plan (the same philosophy as the Signal opt-out).
pub fn has_osc_form(ty: &PortType) -> bool {
    match ty {
        // The OSC atoms cross verbatim.
        PortType::F32 | PortType::I32 { .. } | PortType::Str => true,
        // A type-erased vocab enum leaves as its bare index today; symbol-on-the-wire is
        // issue #147 (drain-side source-port resolution).
        PortType::Vocab {
            enum_meta: Some(_), ..
        } => true,
        // A struct vocab type has a form iff it registered a converter ([`OscForm`], the same
        // registry `osc_in_arg`'s struct arm decodes through). `Harmony` opts out by not
        // registering; the `has_osc_form_matches_what_the_drain_can_send` test guards the
        // name-keyed registry against drifting from the variant-keyed drain (`osc_out_args`).
        PortType::Vocab {
            enum_meta: None,
            name,
            ..
        } => has_form(name),
        // Audio never crosses (ADR-0026/0030), and a pass-through names no type of its own.
        PortType::F32Buffer | PortType::Arg => false,
    }
}

/// Expand one internal [`Arg`] into the flat OSC arg list to send out (ADR-0026, ADR-0030),
/// appending primitive `Arg`s to `out`. The inverse of [`osc_in_arg`], dispatched on the `Arg`
/// variant (the closed central enum). A primitive forwards verbatim; a struct vocab type packs via
/// [`OscArg::to_osc`]; a type-erased [`Enum`](Arg::Enum) goes out as its bare index (the boundary
/// has no port to resolve a symbol — see the arm).
///
/// Returns whether anything was appended. A no-OSC-form `Arg` ([`Harmony`](Arg::Harmony),
/// [`Buffer`](Arg::F32Buffer)) expands to nothing and returns `false`: **no OSC form → emit
/// nothing** — the caller must skip the datagram, never encode zero args onto the wire. Legality
/// checks ([`has_osc_form`]) keep such wires out of a plan, so a `false` here is belt-and-braces,
/// but the rule lives with the expansion.
#[must_use = "false means the Arg had no OSC form and nothing was appended — skip the datagram"]
pub fn osc_out_args(arg: &Arg, out: &mut Vec<Arg>) -> bool {
    let before = out.len();
    match arg {
        Arg::F32(_) | Arg::I32(_) | Arg::Str(_) => out.push(arg.clone()),
        Arg::Note(n) => n.to_osc(out),
        // A type-erased vocab enum (`Arg::Enum`) goes out as its bare **index**: at the boundary
        // there is no port context to recover the symbol from (type identity lives in the port, not
        // the value). Symbol-on-the-wire for outbound enums needs the sink's wired *source-port*
        // `enum_meta` resolved at the engine drain — issue #147, not here.
        Arg::Enum(i) => out.push(Arg::I32(*i as i32)),
        // No external OSC form.
        Arg::Harmony(_) | Arg::F32Buffer(_) => {}
    }
    out.len() > before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Curve, F32Meta, Port};
    use crate::vocab::harmony::SnapDir;
    use crate::vocab::pitch::{Note, Pitch};

    /// A bare port of the given type (no meta) — the fixture for the type-driven arms.
    fn port(ty: PortType) -> Port {
        Port {
            name: "p",
            ty,
            meta: None,
        }
    }

    #[test]
    fn f32_port_wraps_first_arg() {
        let p = port(PortType::F32);
        assert_eq!(osc_in_arg(&p, &[Arg::F32(0.5)]), Some(Arg::F32(0.5)));
        // Int coerces to the numeric port.
        assert_eq!(osc_in_arg(&p, &[Arg::I32(3)]), Some(Arg::F32(3.0)));
    }

    #[test]
    fn bare_buffer_port_never_crosses() {
        // A *bare* `f32_buffer` (audio, no meta) has no OSC form.
        assert_eq!(
            osc_in_arg(&Port::f32_buffer("audio"), &[Arg::F32(1.0)]),
            None
        );
    }

    #[test]
    fn signal_control_buffer_crosses_and_clamps() {
        // A signal control carrying a scalar default (`f32_buffer` + meta, e.g. `djfilter.position`)
        // DOES cross — a control surface can sweep it — and clamps to the port's range.
        let pos = Port::f32_buffer_meta(F32Meta {
            name: "position",
            min: -1.0,
            max: 1.0,
            default: 0.0,
            unit: "",
            curve: Curve::Linear,
        });
        assert_eq!(osc_in_arg(&pos, &[Arg::F32(0.5)]), Some(Arg::F32(0.5)));
        // Out-of-range clamps to the knob's bounds.
        assert_eq!(osc_in_arg(&pos, &[Arg::F32(9.0)]), Some(Arg::F32(1.0)));
        assert_eq!(osc_in_arg(&pos, &[Arg::F32(-9.0)]), Some(Arg::F32(-1.0)));
    }

    #[test]
    fn enum_port_resolves_symbol_and_index() {
        let p = Port::enumerated(SnapDir::enum_meta("dir"));
        let up = SnapDir::from_symbol("Up").unwrap();
        // Both inbound forms normalize to the type-erased `Arg::Enum(index)` the port declares.
        assert_eq!(
            osc_in_arg(&p, &[Arg::Str("Up".into())]),
            Some(Arg::from(up))
        );
        // Index fallback.
        assert_eq!(
            osc_in_arg(&p, &[Arg::I32(up.to_index() as i32)]),
            Some(Arg::from(up))
        );
    }

    #[test]
    fn note_port_unpacks_flat_form() {
        let p = port(PortType::Vocab {
            name: "Note",
            is_event: true,
            enum_meta: None,
        });
        // Float pitch → absolute MIDI; second arg is velocity.
        let got = osc_in_arg(&p, &[Arg::F32(69.0), Arg::F32(0.8)]);
        assert_eq!(got, Some(Arg::Note(Note::new(Pitch::Absolute(69.0), 0.8))));
        // Int pitch → scale degree; missing velocity defaults to 1.0.
        let got = osc_in_arg(&p, &[Arg::I32(2)]);
        assert_eq!(got, Some(Arg::Note(Note::new(Pitch::Degree(2), 1.0))));
    }

    /// ADR-0028 at the runtime boundary: external OSC that doesn't fit the destination port
    /// **drops** (`None`) — it never snaps to a default. Load time pins this for documents
    /// (`unknown_symbol_errors` in `format.rs`); this is the counterpart for live input, the
    /// hardening surface against arbitrary external OSC (a control surface sending a typo'd
    /// symbol or a stale index). A regression making the derived enum resolver fall back to
    /// variant 0 on garbage, or `osc_in_arg` default-fill a malformed Note, fails here.
    #[test]
    fn boundary_drops_args_that_do_not_fit_the_port() {
        // Enum port: an unknown symbol, an out-of-range index, and a negative index all drop.
        let dir = Port::enumerated(SnapDir::enum_meta("dir"));
        assert_eq!(osc_in_arg(&dir, &[Arg::Str("Nope".into())]), None);
        assert_eq!(osc_in_arg(&dir, &[Arg::I32(99)]), None);
        assert_eq!(osc_in_arg(&dir, &[Arg::I32(-1)]), None);
        // F32 port: a string atom has no numeric coercion.
        let f32p = port(PortType::F32);
        assert_eq!(osc_in_arg(&f32p, &[Arg::Str("loud".into())]), None);
        // Str port: a numeric atom is not a string.
        let strp = port(PortType::Str);
        assert_eq!(osc_in_arg(&strp, &[Arg::F32(1.0)]), None);
        // Note port: an empty list and a non-numeric pitch atom are malformed flat forms —
        // neither default-fills into a note-on.
        let note = port(PortType::Vocab {
            name: "Note",
            is_event: true,
            enum_meta: None,
        });
        assert_eq!(osc_in_arg(&note, &[]), None);
        assert_eq!(osc_in_arg(&note, &[Arg::Str("A4".into())]), None);
    }

    #[test]
    fn note_round_trips_through_osc() {
        let n = Note::new(Pitch::Absolute(60.0), 0.5);
        let mut flat = Vec::new();
        assert!(osc_out_args(&Arg::Note(n), &mut flat));
        assert_eq!(flat, vec![Arg::F32(60.0), Arg::F32(0.5)]);
        let p = port(PortType::Vocab {
            name: "Note",
            is_event: true,
            enum_meta: None,
        });
        assert_eq!(osc_in_arg(&p, &flat), Some(Arg::Note(n)));
    }

    /// A type-erased outbound enum serializes as its bare index (symbol-on-the-wire lands with
    /// issue #147's drain-side source-port resolution). The boundary has no port context, so it
    /// cannot recover the variant symbol from a bare `Arg::Enum`.
    #[test]
    fn enum_out_sends_index() {
        let up = SnapDir::from_symbol("Up").unwrap();
        let mut flat = Vec::new();
        assert!(osc_out_args(&Arg::from(up), &mut flat));
        assert_eq!(flat, vec![Arg::I32(up.to_index() as i32)]);
    }

    /// The type-agnostic pass-through port (issue #141, `osc_out.in`): a **single numeric or
    /// string** atom crosses verbatim — the OSC echo/loopback path (the string atom joined in
    /// issue #207, once `Arc<str>` backing made its forward a refcount bump, issue #206) — while
    /// a multi-arg list drops (no vocab type to unpack it into one Arg).
    #[test]
    fn arg_passthrough_port_crosses_a_single_numeric_or_string_atom_verbatim() {
        let p = Port::arg("in");
        assert_eq!(osc_in_arg(&p, &[Arg::F32(0.5)]), Some(Arg::F32(0.5)));
        assert_eq!(osc_in_arg(&p, &[Arg::I32(3)]), Some(Arg::I32(3)));
        // A string atom crosses too: forwarding it is an `Arc` clone, no render-thread alloc.
        assert_eq!(
            osc_in_arg(&p, &[Arg::Str("Up".into())]),
            Some(Arg::Str("Up".into()))
        );
        // Multi-arg lists have no unambiguous single-Arg form.
        assert_eq!(osc_in_arg(&p, &[Arg::F32(1.0), Arg::F32(2.0)]), None);
        assert_eq!(osc_in_arg(&p, &[]), None);
    }

    /// Inbound string echo (issue #207): a single `Str` atom round-trips through an `arg` port —
    /// in via [`osc_in_arg`] (an `Arc` clone, RT-safe since issue #206), back out via
    /// [`osc_out_args`] as the same single flat atom. Multi-arg lists — string ones included —
    /// still drop on the way in: without a typed destination port there is no unambiguous
    /// single-`Arg` form.
    #[test]
    fn str_atom_round_trips_through_an_arg_passthrough_port() {
        let p = Port::arg("in");
        let crossed = osc_in_arg(&p, &[Arg::Str("hello".into())]).expect("single Str crosses");
        assert_eq!(crossed, Arg::Str("hello".into()));
        // Outbound: the crossed Arg expands back to the identical single-atom flat form.
        let mut flat = Vec::new();
        assert!(osc_out_args(&crossed, &mut flat));
        assert_eq!(flat, vec![Arg::Str("hello".into())]);
        // Multi-arg still drops, whatever the atom types.
        assert_eq!(
            osc_in_arg(&p, &[Arg::Str("a".into()), Arg::Str("b".into())]),
            None
        );
        assert_eq!(osc_in_arg(&p, &[Arg::F32(1.0), Arg::Str("x".into())]), None);
    }

    /// The capability key (issue #141): a type is wireable into the pass-through **iff**
    /// [`osc_out_args`] produces a non-empty external form for it. The second half locks the
    /// "cannot drift" claim in [`has_osc_form`]'s docs: for a value of every `Arg` variant, the
    /// drain reports a form (`true`) exactly where the key grants one.
    #[test]
    fn has_osc_form_matches_what_the_drain_can_send() {
        use crate::vocab::FilterMode;
        assert!(has_osc_form(&PortType::F32));
        assert!(has_osc_form(&PortType::I32 { meta: None }));
        assert!(has_osc_form(&PortType::Str));
        // A vocab enum leaves as its index (symbols: issue #147).
        assert!(has_osc_form(
            &Port::enumerated(FilterMode::enum_meta("mode")).ty
        ));
        // Note packs its registered flat form; Harmony registers none — the boundary opt-out
        // (its wire form is deferred to issue #209).
        assert!(has_osc_form(&PortType::Vocab {
            name: "Note",
            is_event: true,
            enum_meta: None,
        }));
        assert!(!has_osc_form(&PortType::Vocab {
            name: "Harmony",
            is_event: false,
            enum_meta: None,
        }));
        // Audio never crosses; a pass-through names no type of its own.
        assert!(!has_osc_form(&PortType::F32Buffer));
        assert!(!has_osc_form(&PortType::Arg));

        // Key ↔ drain: `osc_out_args` emits iff the corresponding type has a form.
        let mut flat = Vec::new();
        assert!(osc_out_args(&Arg::F32(1.0), &mut flat));
        assert!(osc_out_args(&Arg::I32(3), &mut flat));
        assert!(osc_out_args(&Arg::Str("s".into()), &mut flat));
        assert!(osc_out_args(
            &Arg::from(FilterMode::from_symbol("Lp").unwrap()),
            &mut flat,
        ));
        assert!(osc_out_args(
            &Arg::Note(Note::new(Pitch::Absolute(60.0), 0.5)),
            &mut flat,
        ));
        assert!(!osc_out_args(
            &Arg::Harmony(crate::vocab::harmony::Harmony::default()),
            &mut flat,
        ));
        assert!(!osc_out_args(
            &Arg::F32Buffer(crate::message::Signal::default()),
            &mut flat,
        ));
    }

    /// The converter registry (issue #204): `Note` self-registers its flat form via
    /// `register_osc_form!`, so the lookup finds it by its `PortType::Vocab` name; `Harmony`
    /// (no `OscArg` impl — the boundary opt-out) is absent by omission.
    #[test]
    fn registry_finds_note_by_name_and_omits_harmony() {
        assert!(osc_form_by_name("Note").is_some());
        assert!(has_form("Note"));
        assert!(osc_form_by_name("Harmony").is_none());
        assert!(!has_form("Harmony"));
        // Anti-dead-strip canary + uniqueness: the link-time slice gathered at least Note, and
        // no two types registered the same name (the same guard `Registry::builtin` asserts).
        let mut names: Vec<&str> = inventory::iter::<OscForm>
            .into_iter()
            .map(|f| f.type_name)
            .collect();
        assert!(!names.is_empty(), "no OscForm submissions gathered");
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate OscForm type_name");
    }

    /// Consistency (issue #204): the registry's `from_osc` agrees with the hand-baked `"Note"`
    /// arm for representative flat args — degree (int pitch), absolute (float pitch), missing
    /// velocity, and malformed forms (empty list, non-numeric pitch atom). Locks the additive
    /// registry to the behavior the dispatch rewire (issue #205) must preserve.
    #[test]
    fn registered_note_form_agrees_with_the_hand_baked_arm() {
        let form = osc_form_by_name("Note").expect("Note registered");
        let cases: &[&[Arg]] = &[
            &[Arg::I32(2)],                          // degree, missing velocity → defaults 1.0
            &[Arg::F32(69.0), Arg::F32(0.8)],        // absolute pitch + velocity
            &[Arg::I32(-3), Arg::F32(0.0)],          // degree + note-off velocity
            &[Arg::F32(60.0)],                       // absolute, missing velocity
            &[],                                     // malformed: empty
            &[Arg::Str("A4".into())],                // malformed: non-numeric pitch atom
            &[Arg::Str("A4".into()), Arg::F32(0.5)], // malformed pitch, well-formed velocity
        ];
        for args in cases {
            assert_eq!(
                (form.from_osc)(args),
                Note::from_osc(args).map(Arg::Note),
                "registry vs hand-baked arm diverged for {args:?}"
            );
        }
    }

    /// No OSC form → emit nothing: the expansion appends nothing and says so (`false`), which is
    /// what tells the sender to skip the datagram.
    #[test]
    fn harmony_and_buffer_have_no_osc_form() {
        let mut flat = Vec::new();
        assert!(!osc_out_args(
            &Arg::Harmony(crate::vocab::harmony::Harmony::default()),
            &mut flat,
        ));
        assert!(!osc_out_args(
            &Arg::F32Buffer(crate::message::Signal::default()),
            &mut flat,
        ));
        assert!(flat.is_empty());
    }
}
