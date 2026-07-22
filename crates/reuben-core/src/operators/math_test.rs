//! Shared test helpers for the generated number operators (issue #104).
//!
//! Every `number_operator_contract!` op tests the same two shapes — drive a value-carrier op and
//! read its emitted scalar, or drive a signal-carrier op and read its output buffer. The extractor
//! and the two drivers live here so each op's test file carries **only** its math assertions; the
//! contract-derived `defaults_are_data` test is emitted by the macro itself.

use crate::message::{Arg, Emit};
use crate::op_driver::OpDriver;
use crate::operator::{Operator, PortIndex};

/// The sample rate every math-op test renders at.
pub const SR: f32 = 48_000.0;

/// The F32 value carried by an emit (panics on any other `Arg` — a number op emits F32).
pub fn f32_emit(e: &Emit) -> f32 {
    match &e.arg {
        Arg::F32(v) => *v,
        other => panic!("expected an F32 emit, got {other:?}"),
    }
}

/// Drive a **value**-carrier op: configure its held inputs via `setup`, render a block, and return
/// the emitted F32 value(s). `setup` calls `d.set(IN_*, ..)` for each operand it wires.
pub fn value_emits<O: Operator + 'static>(op: O, setup: impl FnOnce(&mut OpDriver)) -> Vec<f32> {
    let mut d = OpDriver::for_type(op, SR);
    setup(&mut d);
    d.render(64).emits().iter().map(f32_emit).collect()
}

/// The I32 value carried by an emit (panics on any other `Arg` — an `i32` value op emits I32).
pub fn i32_emit(e: &Emit) -> i32 {
    match &e.arg {
        Arg::I32(v) => *v,
        other => panic!("expected an I32 emit, got {other:?}"),
    }
}

/// Drive an **`i32` value**-carrier op — [`value_emits`]'s integer twin (issue #556), returning the
/// emitted `i32` value(s) rather than `f32`. The emitted `Arg` variant is itself the assertion that
/// the operator's ports are typed `i32`: an operator that only *named* itself `i32` would emit
/// `Arg::F32` here and panic in [`i32_emit`].
///
/// Note `OpDriver::set` writes the latch directly, so it bypasses the range clamp a wired or OSC
/// value goes through. That is deliberate: it lets a test hand the arithmetic operands the clamp
/// would never allow, which is the only way to reach the saturation path from a test.
pub fn i32_value_emits<O: Operator + 'static>(
    op: O,
    setup: impl FnOnce(&mut OpDriver),
) -> Vec<i32> {
    let mut d = OpDriver::for_type(op, SR);
    setup(&mut d);
    d.render(64).emits().iter().map(i32_emit).collect()
}

/// Drive a **signal**-carrier op: configure its inputs via `setup`, render `n` frames, and return
/// the `out` buffer. `setup` calls `d.drive(IN_*, buf)` / `d.set(IN_*, ..)` for each operand.
pub fn signal_out<O: Operator + 'static>(
    op: O,
    out: impl PortIndex,
    n: usize,
    setup: impl FnOnce(&mut OpDriver),
) -> Vec<f32> {
    let mut d = OpDriver::for_type(op, SR);
    setup(&mut d);
    d.render(n).output(out).to_vec()
}
