//! Boundary — the OSC ⇄ [`Arg`] conversion at the external edge (ADR-0007, ADR-0026, ADR-0030).
//!
//! The native layer decodes a UDP datagram into an address plus a flat list of **primitive**
//! `Arg`s (the OSC atoms `F32`/`I32`/`Str`) and, on the way out, encodes the same. These two
//! functions are the *typed* half in between: turning that flat list into the single `Arg` a
//! destination port carries, and expanding one internal `Arg` back into the flat list to send.
//!
//! **Dest-port-type-driven** (ADR-0030, Q10a). External OSC routes by address to a node/port; the
//! **port's declared [`PortType`]** drives [`osc_in_arg`] — there is no separate registry to drift
//! from the descriptor. A primitive port wraps the single arg; a vocab enum resolves it via its
//! [`EnumMeta`](crate::descriptor::EnumMeta); a struct vocab type unpacks the flat form via
//! [`OscArg::from_osc`]. A [`Buffer`](Arg::F32Buffer) port has no OSC form, so audio cannot cross —
//! the opt-out is by construction.

use crate::descriptor::{Port, PortType};
use crate::message::{Arg, OscArg};
use crate::vocab::pitch::Note;

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
        // A struct vocab type: dispatch on its `Arg` variant name to the one type that has an
        // external form. This central match is the boundary's registry for multi-arg vocab
        // structs (mirroring how `Arg` is itself the closed central enum). A name with no arm
        // (e.g. `Harmony`) has no OSC form — opt-out by omission.
        PortType::Vocab {
            enum_meta: None,
            name,
            ..
        } => match *name {
            "Note" => Note::from_osc(args).map(Arg::Note),
            _ => None,
        },
    }
}

/// Expand one internal [`Arg`] into the flat OSC arg list to send out (ADR-0026, ADR-0030),
/// appending primitive `Arg`s to `out`. The inverse of [`osc_in_arg`], dispatched on the `Arg`
/// variant (the closed central enum). A primitive forwards verbatim; a struct vocab type packs via
/// [`OscArg::to_osc`]; a type-erased [`Enum`](Arg::Enum) goes out as its bare index (the boundary
/// has no port to resolve a symbol — see the arm, TODO #141). [`Harmony`](Arg::Harmony) and
/// [`Buffer`](Arg::F32Buffer) have no external form and contribute nothing.
pub fn osc_out_args(arg: &Arg, out: &mut Vec<Arg>) {
    match arg {
        Arg::F32(_) | Arg::I32(_) | Arg::Str(_) => out.push(arg.clone()),
        Arg::Note(n) => n.to_osc(out),
        // A type-erased vocab enum (`Arg::Enum`) goes out as its bare **index**: at the boundary
        // there is no port context to recover the symbol from (type identity lives in the port, not
        // the value). Today this arm is unreachable — no operator declares an enum output and the
        // one sink, `osc_out`, forwards only `Note` — so nothing regresses. Symbol-on-the-wire for
        // outbound enums needs the sink's source-port `enum_meta` resolved at the drain.
        // TODO(#141): once `osc_out` forwards typed args, resolve the index to the enum's symbol.
        Arg::Enum(i) => out.push(Arg::I32(*i as i32)),
        // No external OSC form.
        Arg::Harmony(_) | Arg::F32Buffer(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{Curve, F32Meta, Port};
    use crate::vocab::harmony::SnapDir;
    use crate::vocab::pitch::Pitch;

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

    #[test]
    fn note_round_trips_through_osc() {
        let n = Note::new(Pitch::Absolute(60.0), 0.5);
        let mut flat = Vec::new();
        osc_out_args(&Arg::Note(n), &mut flat);
        assert_eq!(flat, vec![Arg::F32(60.0), Arg::F32(0.5)]);
        let p = port(PortType::Vocab {
            name: "Note",
            is_event: true,
            enum_meta: None,
        });
        assert_eq!(osc_in_arg(&p, &flat), Some(Arg::Note(n)));
    }

    /// A type-erased outbound enum serializes as its bare index today (TODO #141: symbol). The
    /// boundary has no port context, so it cannot recover the variant symbol from a bare `Arg::Enum`.
    #[test]
    fn enum_out_sends_index() {
        let up = SnapDir::from_symbol("Up").unwrap();
        let mut flat = Vec::new();
        osc_out_args(&Arg::from(up), &mut flat);
        assert_eq!(flat, vec![Arg::I32(up.to_index() as i32)]);
    }

    #[test]
    fn harmony_and_buffer_have_no_osc_form() {
        let mut flat = Vec::new();
        osc_out_args(
            &Arg::Harmony(crate::vocab::harmony::Harmony::default()),
            &mut flat,
        );
        osc_out_args(
            &Arg::F32Buffer(crate::message::Signal::default()),
            &mut flat,
        );
        assert!(flat.is_empty());
    }
}
