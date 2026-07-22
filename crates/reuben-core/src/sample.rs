//! Sample — the **one place** the engine's audio element type is named.
//!
//! Every genuinely-audio buffer in the render spine flows as a slice or `Vec` of this element.
//! Naming it once, here, and referring to it everywhere else through the transparent aliases
//! below means a future change to the audio element (say `f32` → a fixed-point or `f64` sample)
//! is a single edit at this site, not a repo-wide grep-and-replace. The aliases are **zero-cost**:
//! [`Sample`] *is* `f32`, [`AudioBuffer`] *is* `&[f32]`, [`AudioBufferMut`] *is* `&mut [f32]`, so
//! adopting them changes no layout, no ABI, and no runtime behavior.
//!
//! **The point is the boundary, not the abstraction.** Forcing every `f32` buffer site to spell
//! out whether it is "permanently audio" (adopt the alias) or "incidental `f32`" (keep the
//! primitive) surfaces a partition the codebase otherwise leaves implicit. The CI guard
//! `scripts/check_sample_alias.py` enforces the rule: a raw `f32` slice or `Vec` may appear only
//! in this file or in an explicit, justified allowlist (the device layer, resource decode, and
//! operator DSP arithmetic — where the `f32` is a device-native frame or a number under the math,
//! not a logical audio buffer). Clippy's `disallowed-types` cannot express this — a primitive has
//! no type path to disallow — which is why the guard is a text linter.

/// The engine's audio element. One block of audio is a run of these.
pub type Sample = f32;

/// A borrowed, read-only audio buffer — the shared-reference render form (`&[Sample]`).
pub type AudioBuffer<'a> = &'a [Sample];

/// A borrowed, writable audio buffer — the exclusive-reference render form (`&mut [Sample]`).
pub type AudioBufferMut<'a> = &'a mut [Sample];
