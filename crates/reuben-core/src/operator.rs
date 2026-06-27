//! Operator — the authoring contract (ADR-0010, ADR-0030).
//!
//! An Operator is single-Lane: the author writes one mono, single-Voice stream a
//! (sub)block at a time, and the engine fans it out across Lanes with per-Lane state.
//! The process function is allocation-free and sees held values constant for the whole call
//! (the engine block-slices at Message boundaries, ADR-0011), so the author simply reads "my
//! current value".
//!
//! Reads are **two typed verbs** over the one Message model (ADR-0030): [`Io::last`] — the
//! held (zero-order-hold) value on a port — and [`Io::stream`] — the sparse, frame-stamped
//! Messages on a port this (sub)block. Both are generic over the payload type `T` (a
//! [`FromArg`] impl: an OSC primitive, a `&[f32]` buffer, or a *vocab* type). Writes are two
//! verbs: [`Io::emit`] — append one Message to an output port — and [`Io::signal_mut`] — fill
//! this node's own dense output buffer in place.

use std::sync::Arc;

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::{Arg, Emit, Event, FromArg};
use crate::resources::{ResolvedRefs, ResourceStore};

/// A typed, frame-stamped payload yielded by [`Io::stream`] — one decoded Message on a port
/// (ADR-0030). `frame` is segment-relative; `payload` is the Message's [`Arg`] decoded to the
/// requested `T` via [`FromArg`].
#[derive(Debug, Clone, Copy)]
pub struct Stamped<T> {
    /// Sample offset within the current (sub)block at which this Message applies.
    pub frame: usize,
    /// The decoded payload.
    pub payload: T,
}

/// The per-call I/O view handed to [`Operator::process`] for one (sub)block of one Lane.
///
/// All dense slices are exactly [`Io::frames`] samples long; held values are constant for the
/// call. The port reference lists are collected into inline [`SmallVec`]s, so building an `Io`
/// allocates nothing for the common low-port-count case. The inline input capacity is sized for
/// the widest operator once its former params became inputs — the sequencer is `clock`, `length`,
/// 16 × `step`, `gate_mode`, `pitch` = 20 — because RT-safety (`rt_safe`) depends on not
/// spilling here on the audio thread.
pub struct Io<'a> {
    sample_rate: f32,
    frames: usize,
    /// Dense per-sample buffer per **input** port (a wired [`Buffer`](Arg::F32Buffer) source or a
    /// materialized [`F32`](crate::descriptor::PortType::F32) control), or `None` for a port with
    /// no buffer form. Read per-sample via [`Io::signal`].
    inputs: SmallVec<[Option<&'a [f32]>; 20]>,
    outputs: SmallVec<[&'a mut [f32]; 2]>,
    /// The held (ZOH) [`Arg`] per **input** port — the unified per-port latch (ADR-0030),
    /// collapsing the former Harmony / enum / param lanes. In input-port order; `Copy`-normalized
    /// and constant for this (sub)block (the engine block-slices at held-value changes). Read via
    /// [`Io::last`]; empty when unattached, so `last` then reports `None`.
    latched: &'a [Arg],
    /// The sparse [`Event`]s per **input** port this (sub)block, frames segment-relative
    /// (ADR-0030). In input-port order; zero-copy views borrowed from the Render loop. Read via
    /// [`Io::stream`]; empty when unattached.
    streams: &'a [&'a [Event<'a>]],
    lane: usize,
    lanes: usize,
    /// Sink for Messages this call emits (ADR-0014, ADR-0030), or `None` when this Lane does not
    /// collect emissions. Only Lane 0 collects — emission is single-Lane (pre-fan-out). The former
    /// harmony-publish and outbound sinks fold into this: a context/`osc_out` node simply emits to
    /// the right output port, and routing (the wired edge) carries it.
    emit: Option<&'a mut Vec<Emit>>,
    /// Block-absolute frame of this (sub)block's start, added to an emitted frame so the operator
    /// can work in segment-relative time.
    frame_offset: usize,
    /// Per-input `varying` hint (ADR-0030), in input-port order: `false` when a materialized
    /// input held its value unchanged this block, so a const-folding operator may reuse cached
    /// coefficients. Empty when unattached — `varying()` then conservatively reports `true`.
    varying: &'a [bool],
}

impl<'a> Io<'a> {
    /// Internal constructor used by the Render loop. Defaults to a single Lane (lane 0 of
    /// 1); the engine attaches the latch, streams, lane, and emit sink via the builders.
    ///
    /// `inputs`/`outputs` are taken as iterators so the Render loop can wire ports straight
    /// from the arena without an intermediate heap allocation.
    pub(crate) fn new<I, O>(sample_rate: f32, frames: usize, inputs: I, outputs: O) -> Self
    where
        I: IntoIterator<Item = Option<&'a [f32]>>,
        O: IntoIterator<Item = &'a mut [f32]>,
    {
        Self {
            sample_rate,
            frames,
            inputs: inputs.into_iter().collect(),
            outputs: outputs.into_iter().collect(),
            latched: &[],
            streams: &[],
            lane: 0,
            lanes: 1,
            emit: None,
            frame_offset: 0,
            varying: &[],
        }
    }

    /// Attach the per-input held [`Arg`] latch for this segment (ADR-0030). In input-port order;
    /// read by [`Io::last`]. Unattached ⇒ `last()` reports `None`.
    pub(crate) fn with_latched(mut self, latched: &'a [Arg]) -> Self {
        self.latched = latched;
        self
    }

    /// Attach the per-input [`Event`] streams for this (sub)block (ADR-0030). In input-port order;
    /// read by [`Io::stream`]. Unattached ⇒ `stream()` is empty.
    pub(crate) fn with_streams(mut self, streams: &'a [&'a [Event<'a>]]) -> Self {
        self.streams = streams;
        self
    }

    /// Attach the per-input `varying` hints for this segment (ADR-0030). In input-port order;
    /// read by [`Io::varying`]. Unattached ⇒ `varying()` reports `true`.
    pub(crate) fn with_varying(mut self, varying: &'a [bool]) -> Self {
        self.varying = varying;
        self
    }

    /// Set which Lane (Voice) of how many this call is, for replicated operators.
    pub(crate) fn with_lane(mut self, lane: usize, lanes: usize) -> Self {
        self.lane = lane;
        self.lanes = lanes;
        self
    }

    /// Attach the emit sink and segment frame offset (Lane 0 only). Messages passed to
    /// [`Io::emit`] are collected into `buf` with `frame_offset` added.
    pub(crate) fn with_emit(mut self, buf: &'a mut Vec<Emit>, frame_offset: usize) -> Self {
        self.emit = Some(buf);
        self.frame_offset = frame_offset;
        self
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of samples in this (sub)block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// **Per-sample read of a buffer input** (ADR-0030): the dense block on `port`. A wired
    /// [`Buffer`](Arg::F32Buffer) source, or the engine's materialized buffer for an
    /// [`F32`](crate::descriptor::PortType::F32) control filled from its latched value (mid-block
    /// changes written at their frame). Always `frames` long for a migrated port; an empty slice
    /// for a port with neither a wire nor materialization.
    pub fn signal(&self, port: usize) -> &[f32] {
        self.inputs.get(port).copied().flatten().unwrap_or(&[])
    }

    /// **The held (ZOH) value on `port`** (ADR-0030) — the most-recent Message's payload, decoded
    /// to `T`, constant for this (sub)block (the engine block-slices at held-value changes). The
    /// unifying read for scalars, enums, and the Harmony struct: `io.last::<f32>(CUTOFF)`,
    /// `io.last::<SnapDir>(DIR)`, `io.last::<Harmony>(HARMONY)`. `Some(default)` on an input with a
    /// latched default; `None` when nothing is latchable (unwired, no default) or the wire's type
    /// is not a `T`.
    pub fn last<T: FromArg<'a>>(&self, port: usize) -> Option<T> {
        self.latched.get(port).and_then(|arg| T::from_arg(arg))
    }

    /// **The sparse Messages on `port` this (sub)block** (ADR-0030), each decoded to `T` and
    /// frame-stamped (segment-relative). The unifying read for events — a Voicer iterates
    /// `io.stream::<Note>(NOTES)`. Zero-copy; messages whose payload is not a `T` are skipped.
    pub fn stream<T: FromArg<'a>>(&self, port: usize) -> impl Iterator<Item = Stamped<T>> + 'a {
        let events: &'a [Event<'a>] = self.streams.get(port).copied().unwrap_or(&[]);
        events.iter().filter_map(|e| {
            T::from_arg(e.arg).map(|payload| Stamped {
                frame: e.frame,
                payload,
            })
        })
    }

    /// The `varying` hint for an input (ADR-0030): `false` when a materialized input held its
    /// value unchanged this block (so a const-folding op may reuse cached state), `true` when it
    /// is dense or changed this block. Conservatively `true` when unattached.
    pub fn varying(&self, port: usize) -> bool {
        self.varying.get(port).copied().unwrap_or(true)
    }

    /// **Per-sample write view of a buffer output** (ADR-0030): fill this node's own output buffer
    /// on `port` in place. Length == `frames`.
    pub fn signal_mut(&mut self, port: usize) -> &mut [f32] {
        &mut self.outputs[port][..]
    }

    /// **Emit one Message** onto output `port` at segment-relative `frame` (ADR-0014, ADR-0030).
    /// `addr` is the node-local address carried for OSC shape / debug (e.g. `"notes"`); it is
    /// `&'static str` and `payload` is one [`Arg`], so a wired-edge emit allocates nothing. The
    /// engine delivers it as an [`Event`] to nodes downstream of this one in the same block — and,
    /// for an output port wired to the boundary, drains it past the boundary. A no-op on Lanes that
    /// do not collect emissions (every Lane but 0). Replaces the former `publish_harmony` /
    /// `send_outbound`: publishing a Harmony or sending outbound is just an emit to the right port.
    pub fn emit(&mut self, port: usize, addr: &'static str, payload: impl Into<Arg>, frame: usize) {
        let frame = self.frame_offset + frame;
        if let Some(buf) = self.emit.as_mut() {
            buf.push(Emit {
                port,
                address: addr,
                arg: payload.into(),
                frame,
            });
        }
    }

    /// Which Lane (Voice) this call represents, in `0..lanes()`. Single-Lane operators can
    /// ignore it; an expander like the Voicer uses it to emit just this Voice's output.
    pub fn lane(&self) -> usize {
        self.lane
    }

    /// Total Lane (Voice) count at this point in the graph.
    pub fn lanes(&self) -> usize {
        self.lanes
    }

    /// **Read an input port, dispatched by the payload type `T`** (ADR-0031). `T` *is* the port's
    /// form: `io.input::<&[f32]>(p)` reads a Signal buffer, `io.input::<f32>(p)` (or an enum /
    /// `Harmony`) the held Value, `io.input::<Note>(p)` the Event stream. One verb replaces the
    /// old `signal`/`last`/`stream` trio — there is no runtime `match`; the form is the type.
    pub fn input<T: IoInput<'a>>(&self, port: usize) -> T::Out {
        T::read(self, port)
    }

    /// **Write an output port, dispatched by the payload type `T`** (ADR-0031). `io.output::<&mut
    /// [f32]>(p)` borrows this node's dense Signal buffer to fill in place; `io.output::<f32>(p)`
    /// returns a [`MsgWriter`] for a sparse Value output. One verb replaces `signal_mut`/`emit`.
    pub fn output<T: IoOutput<'a>>(&mut self, port: usize) -> T::Out<'_> {
        T::write(self, port)
    }
}

/// The write side of [`Io::output`] (ADR-0031): each payload type maps to one output form. `&mut
/// [f32]` ⇒ the dense Signal buffer to fill; `f32` ⇒ a [`MsgWriter`] for sparse Value writes. The
/// `Out<'io>` GAT carries the per-call mutable borrow of the `Io`.
pub trait IoOutput<'a>: Sized {
    /// What `io.output::<Self>(port)` returns, borrowing the `Io` for `'io`.
    type Out<'io>
    where
        'a: 'io;
    /// Open `port` of `io` for writing in this type's form.
    fn write<'io>(io: &'io mut Io<'a>, port: usize) -> Self::Out<'io>;
}

impl<'a> IoOutput<'a> for &'a mut [f32] {
    type Out<'io>
        = &'io mut [f32]
    where
        'a: 'io;
    fn write<'io>(io: &'io mut Io<'a>, port: usize) -> &'io mut [f32] {
        &mut io.outputs[port][..]
    }
}

impl<'a> IoOutput<'a> for f32 {
    type Out<'io>
        = MsgWriter<'io>
    where
        'a: 'io;
    fn write<'io>(io: &'io mut Io<'a>, port: usize) -> MsgWriter<'io> {
        MsgWriter {
            sink: io.emit.as_deref_mut(),
            port,
            frame_offset: io.frame_offset,
            last: None,
        }
    }
}

/// A handle for **sparse Value writes** on one output port, returned by `io.output::<f32>(port)`
/// (ADR-0031). Lowers to today's `Emit → Event → latch`. [`set`](MsgWriter::set) is **deduped** (a
/// no-op change emits nothing, so the wire stays genuinely sparse), **last-write-wins per frame**,
/// and **addressless** (internal wires route by connection). The dedup baseline is writer-local for
/// now — a fresh handle starts with no prior value, so the first `set` of a block always emits; the
/// cross-block held-latch baseline rides in with the operator sweep (ADR-0031 step 5).
pub struct MsgWriter<'io> {
    /// The node's emit sink, or `None` on a Lane that does not collect (every Lane but 0).
    sink: Option<&'io mut Vec<Emit>>,
    port: usize,
    frame_offset: usize,
    /// The most recent value this handle emitted — the dedup baseline.
    last: Option<Arg>,
}

impl MsgWriter<'_> {
    /// Write `value` on this port at segment-relative `frame`. A no-op when `value` equals the last
    /// value this handle wrote (dedup); otherwise emits, replacing any earlier write this handle made
    /// at the same frame (last-write-wins).
    pub fn set(&mut self, frame: usize, value: impl Into<Arg>) {
        let arg = value.into();
        if self.last.as_ref() == Some(&arg) {
            return; // deduped: the held value is unchanged, so the wire stays sparse.
        }
        let frame = self.frame_offset + frame;
        let port = self.port;
        if let Some(sink) = self.sink.as_mut() {
            // Last-write-wins: drop any earlier write this handle made at this frame on this port.
            sink.retain(|e| !(e.port == port && e.frame == frame));
            sink.push(Emit {
                port,
                address: "",
                arg: arg.clone(),
                frame,
            });
        }
        self.last = Some(arg);
    }
}

/// The read side of [`Io::input`] (ADR-0031): each payload type maps to exactly one port form and
/// one return shape. `&[f32]` ⇒ a Signal buffer slice; a scalar / enum / `Harmony` ⇒ the held
/// Value as `Option<T>`; `Note` ⇒ an Event iterator. Resolved at monomorphization, so the call site
/// names a type, never branches on a form.
pub trait IoInput<'a>: Sized {
    /// What `io.input::<Self>(port)` returns.
    type Out;
    /// Read `port` from `io` in this type's form.
    fn read(io: &Io<'a>, port: usize) -> Self::Out;
}

impl<'a> IoInput<'a> for &'a [f32] {
    type Out = &'a [f32];
    fn read(io: &Io<'a>, port: usize) -> &'a [f32] {
        io.inputs.get(port).copied().flatten().unwrap_or(&[])
    }
}

/// The held-Value arm of [`IoInput`]: a scalar / enum / `Harmony` decodes from its latched [`Arg`]
/// to `Option<Self>`. One arm per type (a blanket `impl<T: FromArg>` would collide with the `&[f32]`
/// Signal and `Note` Event arms), minted by this macro as ports migrate.
macro_rules! impl_input_held {
    ($($t:ty),* $(,)?) => {$(
        impl<'a> IoInput<'a> for $t {
            type Out = Option<$t>;
            fn read(io: &Io<'a>, port: usize) -> Option<$t> {
                io.latched.get(port).and_then(<$t>::from_arg)
            }
        }
    )*};
}

impl_input_held!(f32, crate::vocab::FilterMode);

/// The Event-stream arm of [`IoInput`], returned by `io.input::<Note>(port)`: a no-alloc iterator
/// over a port's sparse [`Event`]s, each decoded to `T` and frame-stamped ([`Stamped`]). A *named*
/// type (not `impl Iterator`) so it can be the trait's associated `Out`.
pub struct EventStream<'a, T> {
    events: std::slice::Iter<'a, Event<'a>>,
    _marker: std::marker::PhantomData<T>,
}

impl<'a, T: FromArg<'a>> Iterator for EventStream<'a, T> {
    type Item = Stamped<T>;
    fn next(&mut self) -> Option<Self::Item> {
        for e in self.events.by_ref() {
            if let Some(payload) = T::from_arg(e.arg) {
                return Some(Stamped {
                    frame: e.frame,
                    payload,
                });
            }
        }
        None
    }
}

impl<'a> IoInput<'a> for crate::vocab::pitch::Note {
    type Out = EventStream<'a, crate::vocab::pitch::Note>;
    fn read(io: &Io<'a>, port: usize) -> Self::Out {
        let events: &'a [Event<'a>] = io.streams.get(port).copied().unwrap_or(&[]);
        EventStream {
            events: events.iter(),
            _marker: std::marker::PhantomData,
        }
    }
}

/// A unit of behavior. Authored single-Lane; replicated across Lanes by the engine.
pub trait Operator: Send {
    /// Static self-description (ports + param metadata). Drives serialization,
    /// connection checking, good-button controls, and AI grounding.
    fn descriptor() -> Descriptor
    where
        Self: Sized;

    /// Process exactly one (sub)block for one Lane. Must not allocate.
    fn process(&mut self, io: &mut Io);

    /// Make a fresh-state instance of the same operator type, for the engine to use as
    /// another Voice's Lane. Params are applied by the engine separately, so this only
    /// needs to reset per-Lane state (typically `Box::new(Self::new())`). An operator that
    /// holds a resource binding (see [`Operator::bind_resources`]) must carry it forward
    /// here while resetting playback state, so every Voice shares the decoded data.
    fn spawn(&self) -> Box<dyn Operator>;

    /// Receive decoded resources after construction, before Plan fan-out (ADR-0016). The
    /// loader calls this on every node that declares a resource slot in its descriptor,
    /// handing the shared [`ResourceStore`] (clone the `Arc` to hold it) and the node's
    /// [`ResolvedRefs`] (resolved handles by slot name). Default no-op — the two-phase
    /// init pattern for a type-erased registry, so operators with no resources ignore it.
    fn bind_resources(&mut self, _store: &Arc<ResourceStore>, _refs: &ResolvedRefs) {}
}

#[cfg(test)]
mod new_io_api {
    //! ADR-0031 step 3 — the two return-type-dispatched verbs `io.input::<T>` / `io.output::<T>`,
    //! built test-first additively alongside the old verbs. `T` *is* the form: `&[f32]` ⇒ Signal,
    //! a scalar/enum/`Harmony` ⇒ Value (held), `Note` ⇒ Event.
    use super::*;

    /// Tracer bullet: `input::<&[f32]>` reads the dense per-sample buffer on a Signal input —
    /// the new spelling of the old `signal` verb, dispatched purely by the `&[f32]` type.
    #[test]
    fn input_reads_a_signal_buffer_slice() {
        let buf = [1.0_f32, 2.0, 3.0];
        let io = Io::new(
            48_000.0,
            3,
            [Some(&buf[..])],
            std::iter::empty::<&mut [f32]>(),
        );
        assert_eq!(io.input::<&[f32]>(0), &buf[..]);
    }

    /// `input::<f32>` reads the held (ZOH) Value from the latch as `Option<f32>` — the new spelling
    /// of `last::<f32>`. Dispatched by the scalar type, not a verb.
    #[test]
    fn input_reads_a_held_scalar_value() {
        let latch = [Arg::F32(440.0)];
        let io =
            Io::new(48_000.0, 1, [None], std::iter::empty::<&mut [f32]>()).with_latched(&latch);
        assert_eq!(io.input::<f32>(0), Some(440.0));
        // An unlatched port reports absence.
        let bare = Io::new(48_000.0, 1, [None], std::iter::empty::<&mut [f32]>());
        assert_eq!(bare.input::<f32>(0), None);
    }

    /// `input::<T>` reads a held vocab Value too — an enum (`FilterMode`) decodes from its latched
    /// `Arg` the same way a scalar does. The held arm spans every `FromArg` Value type.
    #[test]
    fn input_reads_a_held_enum_value() {
        use crate::vocab::FilterMode;
        let latch = [Arg::FilterMode(FilterMode::Bp)];
        let io =
            Io::new(48_000.0, 1, [None], std::iter::empty::<&mut [f32]>()).with_latched(&latch);
        assert_eq!(io.input::<FilterMode>(0), Some(FilterMode::Bp));
    }

    /// `input::<Note>` iterates the sparse Event stream on a port, each decoded + frame-stamped —
    /// the new spelling of `stream::<Note>`. The `Note` type selects the Event form.
    #[test]
    fn input_iterates_an_event_stream() {
        use crate::vocab::pitch::{Note, Pitch};
        let n0 = Arg::Note(Note::new(Pitch::from_midi(60.0), 1.0));
        let n1 = Arg::Note(Note::new(Pitch::from_midi(64.0), 0.5));
        let events = [
            Event {
                address: "notes",
                arg: &n0,
                frame: 0,
            },
            Event {
                address: "notes",
                arg: &n1,
                frame: 32,
            },
        ];
        let streams: [&[Event]; 1] = [&events];
        let io =
            Io::new(48_000.0, 64, [None], std::iter::empty::<&mut [f32]>()).with_streams(&streams);
        let got: Vec<_> = io
            .input::<Note>(0)
            .map(|s| (s.frame, s.payload.pitch.midi()))
            .collect();
        assert_eq!(got, vec![(0, Some(60.0)), (32, Some(64.0))]);
    }

    /// `output::<&mut [f32]>` hands back this node's own dense output buffer to fill in place — the
    /// new spelling of `signal_mut`. The `&mut [f32]` type selects the Signal-write form.
    #[test]
    fn output_writes_a_signal_buffer() {
        let mut buf = [0.0_f32; 4];
        {
            let mut io = Io::new(
                48_000.0,
                4,
                std::iter::empty::<Option<&[f32]>>(),
                [&mut buf[..]],
            );
            io.output::<&mut [f32]>(0)
                .copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
        }
        assert_eq!(buf, [1.0, 2.0, 3.0, 4.0]);
    }

    /// Build an `Io` with only an emit sink attached — the fixture for the `output::<f32>` /
    /// `MsgWriter` slices.
    fn emitting_io(sink: &mut Vec<Emit>, frame_offset: usize) -> Io<'_> {
        Io::new(
            48_000.0,
            8,
            std::iter::empty::<Option<&[f32]>>(),
            std::iter::empty::<&mut [f32]>(),
        )
        .with_emit(sink, frame_offset)
    }

    /// `output::<f32>(port).set(frame, v)` emits one Message on that port at that frame — addressless
    /// (the internal wire routes by connection, not name; ADR-0031).
    #[test]
    fn output_value_set_emits_one_addressless_message() {
        let mut sink = Vec::new();
        emitting_io(&mut sink, 0).output::<f32>(0).set(2, 1.0);
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].port, 0);
        assert_eq!(sink[0].frame, 2);
        assert_eq!(sink[0].arg, Arg::F32(1.0));
        assert_eq!(
            sink[0].address, "",
            "internal Value write carries no address"
        );
    }

    /// Dedup: a `set` whose value equals the last one this handle wrote emits nothing — the held
    /// value is unchanged, so the wire stays genuinely sparse.
    #[test]
    fn output_value_set_dedups_unchanged_writes() {
        let mut sink = Vec::new();
        {
            let mut io = emitting_io(&mut sink, 0);
            let mut w = io.output::<f32>(0);
            w.set(0, 1.0); // emits
            w.set(4, 1.0); // unchanged → dropped
            w.set(6, 2.0); // changed → emits
        }
        let got: Vec<_> = sink
            .iter()
            .map(|e| (e.frame, e.arg.as_f32().unwrap()))
            .collect();
        assert_eq!(got, vec![(0, 1.0), (6, 2.0)]);
    }

    /// Last-write-wins per frame: two writes at the same frame collapse to the later value (a single
    /// Message at that frame), not two competing Messages.
    #[test]
    fn output_value_set_is_last_write_wins_per_frame() {
        let mut sink = Vec::new();
        {
            let mut io = emitting_io(&mut sink, 0);
            let mut w = io.output::<f32>(0);
            w.set(5, 1.0);
            w.set(5, 2.0); // same frame → overrides
        }
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].frame, 5);
        assert_eq!(sink[0].arg, Arg::F32(2.0));
    }

    /// The segment frame offset is added to the written frame, so an operator works in
    /// segment-relative time while the engine sees block-absolute frames (matches the old `emit`).
    #[test]
    fn output_value_set_adds_the_segment_frame_offset() {
        let mut sink = Vec::new();
        emitting_io(&mut sink, 100).output::<f32>(0).set(2, 1.0);
        assert_eq!(sink[0].frame, 102);
    }
}
