//! The **operator shells** — `process`, written once per carrier.
//!
//! A *stateless pointwise* operator's `process` is pure mechanism: read each operand through its
//! typed handle, call the scalar fn, write the output. `number_operator_contract!` used to emit
//! that body into every generated variant; the shells own it instead, so there is exactly one
//! `process` for the value carrier ([`ValueShell`]) and one for the signal carrier
//! ([`SignalShell`]) however many operators exist.
//!
//! An op supplies only what is genuinely its own: which handles to read ([`ValueOp::HANDLES`]) and
//! the scalar fn ([`ValueOp::apply`]). The handles are the **contract-emitted consts themselves**
//! (`IN_A`, `IN_B`, …), so the port index a shell reads and the port index the descriptor
//! publishes cannot drift — they are one datum, exactly as they were when `process` was emitted
//! alongside them.
//!
//! # Why this beats an emitted `process` on the render thread
//!
//! [`SignalShell`] reads every operand's slice **once, before** the sample loop — legal because
//! [`Io::read`] returns the block lifetime `'a`, not the `&self` borrow. LLVM then proves the
//! iteration space and vectorizes the loop. The emitted body read `io.read(IN_A)[i]` *inside* the
//! loop, where the `Io` accessor blocked hoisting and left a bounds check per operand per sample
//! (issue #556).
//!
//! The binary ops — `add`, `sub`, `mul`, `min`, `max`, `clamp` — were fully scalar because of it;
//! the unary ones and `map`/`div`/`power` vectorized in part regardless, and gain here too. The
//! one op that cannot is `modulo`: `rem_euclid` lowers to a libm call inside the loop, opaque to
//! the vectorizer. It still wins, because hoisting removes the per-sample checks either way.

use std::marker::PhantomData;

use super::form::{Held, SignalF32};
use super::{In, Io, Operator, Out};
use crate::descriptor::Descriptor;
use crate::message::{Arg, FromArg};

/// One operand, read in **block form**: a signal handle yields its slice, a held handle its
/// latched payload. [`at`](Self::at) then projects the block to this sample's value — a slice
/// indexes, a held payload is constant across the block. That projection is what lets one operand
/// tuple serve both carriers.
///
/// The projection lives here, on the *handle*, rather than on the buffer type: an
/// `impl SampleAt for &[f32]` plus a blanket `impl<T: Copy> SampleAt for T` would overlap (a slice
/// is `Copy`), which would force one impl per held payload type — a hand-maintained census of
/// every vocab enum an operand can carry.
pub trait ReadOperand: Copy {
    /// The whole-block form: `&[f32]` for a signal, the payload for a held operand.
    type Buf<'a>: Copy;
    /// This operand's value at one sample.
    type Val;

    /// Read the operand's block from `io`. Returns the block lifetime, so the caller may hoist
    /// this out of the sample loop.
    fn read_block<'a>(self, io: &Io<'a>) -> Self::Buf<'a>;

    /// This operand's value at sample `i`.
    fn at(buf: Self::Buf<'_>, i: usize) -> Self::Val;
}

impl ReadOperand for In<SignalF32> {
    type Buf<'a> = &'a [f32];
    type Val = f32;

    #[inline]
    fn read_block<'a>(self, io: &Io<'a>) -> &'a [f32] {
        io.read(self)
    }

    // The buffer-presence invariant: a Signal input is always exactly `frames` samples, so this
    // indexes directly — and because the slice was read before the loop, the index is the only
    // thing LLVM has to prove.
    #[inline]
    fn at(buf: &[f32], i: usize) -> f32 {
        buf[i]
    }
}

// One impl covers every held payload — `f32`, `i32`, each vocab enum — mirroring the blanket
// `InForm for Held<T>`. A held operand is constant across the block, so `at` ignores the sample.
impl<T> ReadOperand for In<Held<T>>
where
    T: for<'x> FromArg<'x> + Copy,
{
    type Buf<'a> = T;
    type Val = T;

    #[inline]
    fn read_block<'a>(self, io: &Io<'a>) -> T {
        io.read(self)
    }

    #[inline]
    fn at(buf: T, _i: usize) -> T {
        buf
    }
}

/// A whole operand list, read together.
///
/// Rust has no variadic generics; this is the standard `macro_rules` tuple ladder (serde's
/// `impl_tuple!` trick), written **once**, generically — not a per-operator DSL.
///
/// **The ladder below is a hard ceiling of 1..=6 operands, not a description of today's set.**
/// The pre-shell macro emitted a `process` per variant and so had no arity bound at all; a
/// 7-operand (or 0-operand) pointwise op compiled. Now it fails to compile, with the trait bound
/// `(In<..>, ..): Operands` pointing here rather than saying what to do. What to do is add the
/// next line — `impl_operands!(A, B, C, D, E, F, G);` — which costs nothing at runtime. Six is
/// simply where `map` sits today.
pub trait Operands: Copy {
    /// Every operand's block form.
    type Bufs<'a>: Copy;
    /// Every operand's value at one sample — the tuple the scalar fn is called with.
    type Vals;

    /// Read every operand's block from `io`.
    fn read_blocks<'a>(self, io: &Io<'a>) -> Self::Bufs<'a>;

    /// Project every operand's block to sample `i`.
    fn at(bufs: Self::Bufs<'_>, i: usize) -> Self::Vals;
}

macro_rules! impl_operands {
    ($($n:ident),+) => {
        #[allow(non_snake_case)]
        impl<$($n: ReadOperand),+> Operands for ($($n,)+) {
            type Bufs<'a> = ($($n::Buf<'a>,)+);
            type Vals = ($($n::Val,)+);

            #[inline]
            fn read_blocks<'a>(self, io: &Io<'a>) -> Self::Bufs<'a> {
                let ($($n,)+) = self;
                ($($n.read_block(io),)+)
            }

            #[inline]
            fn at(bufs: Self::Bufs<'_>, i: usize) -> Self::Vals {
                let ($($n,)+) = bufs;
                ($($n::at($n, i),)+)
            }
        }
    };
}

impl_operands!(A);
impl_operands!(A, B);
impl_operands!(A, B, C);
impl_operands!(A, B, C, D);
impl_operands!(A, B, C, D, E);
impl_operands!(A, B, C, D, E, F);

/// A stateless pointwise operator on the **value** carrier: held operands in, one held Value out,
/// written once per (sub)block. The engine block-slices at every change, so writing at frame 0 of
/// each slice is sample-accurate.
pub trait ValueOp: Send + 'static {
    /// The contract-emitted input handle consts, in declaration order.
    type Handles: Operands;
    /// The output payload.
    type Value: Into<Arg> + Copy;

    /// The handles themselves — `(IN_A, IN_B)`.
    const HANDLES: Self::Handles;
    /// The output handle — `OUT_OUT`.
    const OUT: Out<Held<Self::Value>>;

    /// The op's arithmetic: this block's operand values in, the output value out.
    fn apply(vals: <Self::Handles as Operands>::Vals) -> Self::Value;

    /// The operator's contract — `Self::contract()`, emitted by `operator_contract!`.
    fn descriptor() -> Descriptor;
}

/// A stateless pointwise operator on the **signal** carrier: per-sample buffers in, one buffer
/// out. Held operands (a vocab enum mode — enums have no buffer form) may appear alongside
/// buffers; [`ReadOperand::at`] flattens the distinction.
pub trait SignalOp: Send + 'static {
    /// The contract-emitted input handle consts, in declaration order.
    type Handles: Operands;

    /// The handles themselves.
    const HANDLES: Self::Handles;
    /// The output buffer handle.
    const OUT: Out<SignalF32>;

    /// The op's arithmetic, called once per sample.
    fn apply(vals: <Self::Handles as Operands>::Vals) -> f32;

    /// The operator's contract — `Self::contract()`, emitted by `operator_contract!`.
    fn descriptor() -> Descriptor;
}

/// The value-carrier [`Operator`]: reads each held operand, calls [`ValueOp::apply`], writes the
/// result once.
///
/// A zero-sized wrapper rather than a blanket `impl<T: ValueOp> Operator for T`, which would
/// overlap every hand-written `impl Operator` (the compiler cannot prove a given operator is
/// *not* a `ValueOp`).
pub struct ValueShell<Op>(PhantomData<fn() -> Op>);

impl<Op> ValueShell<Op> {
    /// A fresh instance. Stateless — the shell holds nothing but the op's identity.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Op> Default for ValueShell<Op> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Op: ValueOp> Operator for ValueShell<Op> {
    fn descriptor() -> Descriptor {
        Op::descriptor()
    }

    #[inline]
    fn process(&mut self, io: &mut Io) {
        // Every operand is held, so the block *is* the value — `at(.., 0)` is the identity read.
        let vals = <Op::Handles as Operands>::at(Op::HANDLES.read_blocks(io), 0);
        let out = Op::apply(vals);
        io.write(Op::OUT).set(0, out);
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

/// The signal-carrier [`Operator`]: hoists every operand's block out of the loop, then fills the
/// output buffer sample by sample.
pub struct SignalShell<Op>(PhantomData<fn() -> Op>);

impl<Op> SignalShell<Op> {
    /// A fresh instance. Stateless — the shell holds nothing but the op's identity.
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Op> Default for SignalShell<Op> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Op: SignalOp> Operator for SignalShell<Op> {
    fn descriptor() -> Descriptor {
        Op::descriptor()
    }

    #[inline]
    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // Hoisted: `Io::read` returns the block lifetime `'a`, not the `&self` borrow, so these
        // slices stay live across the `&mut io` write below. This is the whole vectorization win —
        // reading inside the loop leaves a bounds check per operand per sample.
        let bufs = Op::HANDLES.read_blocks(io);
        // Sliced to `n` rather than `.take(n)`: a short output buffer must panic (as the emitted
        // `io.write(OUT)[i]` did) instead of silently rendering a partial block, and the fixed
        // length is what lets LLVM size the vector loop.
        let out = &mut io.write(Op::OUT)[..n];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = Op::apply(<Op::Handles as Operands>::at(bufs, i));
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}
