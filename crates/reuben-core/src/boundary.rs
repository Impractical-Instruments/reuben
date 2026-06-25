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
//! [`OscArg::from_osc`]. A [`Buffer`](Arg::Buffer) port has no OSC form, so audio cannot cross —
//! the opt-out is by construction.

use crate::descriptor::PortType;
use crate::message::{Arg, OscArg};
use crate::vocab::harmony::{SnapDir, SnapTarget};
use crate::vocab::pitch::Note;
use crate::vocab::{FilterMode, GateMode, M2sMode, MapCurve, Waveform};

/// Convert a flat OSC arg list into the single [`Arg`] a destination port carries, driven by the
/// **destination port type** (ADR-0030). `None` when the args don't fit the port (a wrong-typed
/// wire — dropped) or the port has no OSC form ([`Buffer`](Arg::Buffer): audio never crosses).
///
/// - **F32 / I32 / Str** — wrap the first arg (numeric coercion as for any `Arg`).
/// - **Vocab enum** — resolve the first arg (symbol or index) via the port's resolver.
/// - **Vocab struct** — unpack the flat form via the type's [`OscArg::from_osc`].
/// - **Buffer** — `None` (opt-out).
pub fn osc_in_arg(ty: &PortType, args: &[Arg]) -> Option<Arg> {
    match ty {
        PortType::F32 => args.first().and_then(Arg::as_f32).map(Arg::F32),
        PortType::I32 => args
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
        // Audio is not boundary-crossable (ADR-0030): a Buffer port has no OSC form.
        PortType::Buffer => None,
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
        } => match *name {
            "Note" => Note::from_osc(args).map(Arg::Note),
            _ => None,
        },
    }
}

/// Expand one internal [`Arg`] into the flat OSC arg list to send out (ADR-0026, ADR-0030),
/// appending primitive `Arg`s to `out`. The inverse of [`osc_in_arg`], dispatched on the `Arg`
/// variant (the closed central enum). A primitive forwards verbatim; an enum sends its **symbol**;
/// a struct vocab type packs via [`OscArg::to_osc`]. [`Harmony`](Arg::Harmony) and
/// [`Buffer`](Arg::Buffer) have no external form and contribute nothing.
pub fn osc_out_args(arg: &Arg, out: &mut Vec<Arg>) {
    match arg {
        Arg::F32(_) | Arg::I32(_) | Arg::Str(_) => out.push(arg.clone()),
        Arg::Note(n) => n.to_osc(out),
        Arg::SnapTarget(v) => out.push(Arg::Str(SnapTarget::VARIANTS[v.to_index()].to_string())),
        Arg::SnapDir(v) => out.push(Arg::Str(SnapDir::VARIANTS[v.to_index()].to_string())),
        Arg::GateMode(v) => out.push(Arg::Str(GateMode::VARIANTS[v.to_index()].to_string())),
        Arg::FilterMode(v) => out.push(Arg::Str(FilterMode::VARIANTS[v.to_index()].to_string())),
        Arg::Waveform(v) => out.push(Arg::Str(Waveform::VARIANTS[v.to_index()].to_string())),
        Arg::M2sMode(v) => out.push(Arg::Str(M2sMode::VARIANTS[v.to_index()].to_string())),
        Arg::MapCurve(v) => out.push(Arg::Str(MapCurve::VARIANTS[v.to_index()].to_string())),
        // No external OSC form.
        Arg::Harmony(_) | Arg::Buffer(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::Port;
    use crate::vocab::pitch::Pitch;

    #[test]
    fn f32_port_wraps_first_arg() {
        let ty = PortType::F32;
        assert_eq!(osc_in_arg(&ty, &[Arg::F32(0.5)]), Some(Arg::F32(0.5)));
        // Int coerces to the numeric port.
        assert_eq!(osc_in_arg(&ty, &[Arg::I32(3)]), Some(Arg::F32(3.0)));
    }

    #[test]
    fn buffer_port_never_crosses() {
        assert_eq!(osc_in_arg(&PortType::Buffer, &[Arg::F32(1.0)]), None);
    }

    #[test]
    fn enum_port_resolves_symbol_and_index() {
        let p = Port::enumerated(SnapDir::enum_meta("dir"));
        let up = SnapDir::from_symbol("Up").unwrap();
        assert_eq!(
            osc_in_arg(&p.ty, &[Arg::Str("Up".into())]),
            Some(Arg::SnapDir(up))
        );
        // Index fallback.
        assert_eq!(
            osc_in_arg(&p.ty, &[Arg::I32(up.to_index() as i32)]),
            Some(Arg::SnapDir(up))
        );
    }

    #[test]
    fn note_port_unpacks_flat_form() {
        let ty = PortType::Vocab {
            name: "Note",
            enum_meta: None,
        };
        // Float pitch → absolute MIDI; second arg is velocity.
        let got = osc_in_arg(&ty, &[Arg::F32(69.0), Arg::F32(0.8)]);
        assert_eq!(got, Some(Arg::Note(Note::new(Pitch::Absolute(69.0), 0.8))));
        // Int pitch → scale degree; missing velocity defaults to 1.0.
        let got = osc_in_arg(&ty, &[Arg::I32(2)]);
        assert_eq!(got, Some(Arg::Note(Note::new(Pitch::Degree(2), 1.0))));
    }

    #[test]
    fn note_round_trips_through_osc() {
        let n = Note::new(Pitch::Absolute(60.0), 0.5);
        let mut flat = Vec::new();
        osc_out_args(&Arg::Note(n), &mut flat);
        assert_eq!(flat, vec![Arg::F32(60.0), Arg::F32(0.5)]);
        let ty = PortType::Vocab {
            name: "Note",
            enum_meta: None,
        };
        assert_eq!(osc_in_arg(&ty, &flat), Some(Arg::Note(n)));
    }

    #[test]
    fn enum_out_sends_symbol() {
        let mut flat = Vec::new();
        osc_out_args(
            &Arg::SnapDir(SnapDir::from_symbol("Up").unwrap()),
            &mut flat,
        );
        assert_eq!(flat, vec![Arg::Str("Up".into())]);
    }

    #[test]
    fn harmony_and_buffer_have_no_osc_form() {
        let mut flat = Vec::new();
        osc_out_args(
            &Arg::Harmony(crate::vocab::harmony::Harmony::default()),
            &mut flat,
        );
        osc_out_args(&Arg::Buffer(crate::message::Signal::default()), &mut flat);
        assert!(flat.is_empty());
    }
}
