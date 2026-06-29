//! Operator — the authoring contract (ADR-0010, ADR-0030).
//!
//! An Operator is mono and single-voice: the author writes one stream a (sub)block at a time;
//! polyphony comes from the Voicer hosting voice sub-patches (ADR-0032), not from the operator.
//! The process function is allocation-free and sees held values constant for the whole call
//! (the engine block-slices at Message boundaries, ADR-0011), so the author simply reads "my
//! current value".
//!
//! Reads use one type-dispatched verb (ADR-0031): [`Io::input`] — `io.input::<&[f32]>(p)` reads a
//! Signal buffer, `io.input::<f32>(p)` (or an enum / `Harmony`) the held (zero-order-hold) Value,
//! and `io.input::<Note>(p)` the sparse, frame-stamped Event stream. Writes use [`Io::output`] —
//! `io.output::<&mut [f32]>(p)` fills this node's own dense buffer in place, `io.output::<f32>(p)`
//! (or `<Harmony>`) returns a [`MsgWriter`] for a sparse Value, and `io.output::<Note>(p)` an
//! [`EventWriter`] for events.

use std::sync::Arc;

use smallvec::SmallVec;

use crate::config::AudioConfig;
use crate::descriptor::Descriptor;
use crate::graph::Graph;
use crate::message::{Arg, Emit, Event, FromArg};
use crate::plan::PlanError;
use crate::resources::{ResolvedRefs, ResourceStore};

/// A typed, frame-stamped payload yielded by an [`Io::input`] event stream — one decoded Message on a port
/// (ADR-0030). `frame` is segment-relative; `payload` is the Message's [`Arg`] decoded to the
/// requested `T` via [`FromArg`].
#[derive(Debug, Clone, Copy)]
pub struct Stamped<T> {
    /// Sample offset within the current (sub)block at which this Message applies.
    pub frame: usize,
    /// The decoded payload.
    pub payload: T,
}

/// The per-call I/O view handed to [`Operator::process`] for one (sub)block.
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
    /// no buffer form. Read per-sample via [`Io::input`].
    inputs: SmallVec<[Option<&'a [f32]>; 20]>,
    outputs: SmallVec<[&'a mut [f32]; 2]>,
    /// The held (ZOH) [`Arg`] per **input** port — the unified per-port latch (ADR-0030),
    /// collapsing the former Harmony / enum / param lanes. In input-port order; `Copy`-normalized
    /// and constant for this (sub)block (the engine block-slices at held-value changes). Read via
    /// [`Io::input`]; empty when unattached, so the held read then reports `None`.
    latched: &'a [Arg],
    /// The sparse [`Event`]s per **input** port this (sub)block, frames segment-relative
    /// (ADR-0030). In input-port order; zero-copy views borrowed from the Render loop. Read via
    /// [`Io::input`]; empty when unattached.
    streams: &'a [&'a [Event<'a>]],
    /// Sink for Messages this call emits (ADR-0014, ADR-0030), or `None` when unattached. The former
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
    /// Internal constructor used by the Render loop; the engine attaches the latch, streams,
    /// varying hints, and emit sink via the builders.
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
            emit: None,
            frame_offset: 0,
            varying: &[],
        }
    }

    /// Attach the per-input held [`Arg`] latch for this segment (ADR-0030). In input-port order;
    /// read by [`Io::input`]. Unattached ⇒ the held read reports `None`.
    pub(crate) fn with_latched(mut self, latched: &'a [Arg]) -> Self {
        self.latched = latched;
        self
    }

    /// Attach the per-input [`Event`] streams for this (sub)block (ADR-0030). In input-port order;
    /// read by [`Io::input`]. Unattached ⇒ the event read is empty.
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

    /// Attach the emit sink and segment frame offset. Messages written via
    /// [`Io::output`] are collected into `buf` with `frame_offset` added.
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

    /// The `varying` hint for an input (ADR-0030): `false` when a materialized input held its
    /// value unchanged this block (so a const-folding op may reuse cached state), `true` when it
    /// is dense or changed this block. Conservatively `true` when unattached.
    pub fn varying(&self, port: usize) -> bool {
        self.varying.get(port).copied().unwrap_or(true)
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

impl<'a> IoOutput<'a> for crate::vocab::Harmony {
    // A held `Harmony` is a single Value, so it reuses [`MsgWriter`] — dedup + last-write-wins are
    // the right semantics (publishing the same Harmony twice changes nothing downstream).
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

impl<'a> IoOutput<'a> for crate::vocab::pitch::Note {
    // An Event output is append-only — [`EventWriter`], not [`MsgWriter`].
    type Out<'io>
        = EventWriter<'io>
    where
        'a: 'io;
    fn write<'io>(io: &'io mut Io<'a>, port: usize) -> EventWriter<'io> {
        EventWriter {
            sink: io.emit.as_deref_mut(),
            port,
            frame_offset: io.frame_offset,
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
    /// The node's emit sink, or `None` when no sink is attached.
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
                arg: arg.clone(),
                frame,
            });
        }
        self.last = Some(arg);
    }
}

/// A handle for **Event writes** on one output port, returned by `io.output::<Note>(port)`
/// (ADR-0031). Unlike [`MsgWriter`], it is **append-only**: every [`emit`](EventWriter::emit) pushes
/// a distinct Message — no dedup, no last-write-wins — so a chord's many notes at a single frame all
/// survive and a re-press of the same note is a real second event. Addressless (internal wires route
/// by connection); lowers to today's `Emit → Event`. Replaces the old `emit` verb for events.
pub struct EventWriter<'io> {
    /// The node's emit sink, or `None` when no sink is attached.
    sink: Option<&'io mut Vec<Emit>>,
    port: usize,
    frame_offset: usize,
}

impl EventWriter<'_> {
    /// Push one Event `payload` on this port at segment-relative `frame`. Always appends.
    pub fn emit(&mut self, frame: usize, payload: impl Into<Arg>) {
        let frame = self.frame_offset + frame;
        if let Some(sink) = self.sink.as_mut() {
            sink.push(Emit {
                port: self.port,
                arg: payload.into(),
                frame,
            });
        }
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

impl_input_held!(
    f32,
    crate::vocab::FilterMode,
    crate::vocab::Waveform,
    crate::vocab::GateMode,
    crate::vocab::M2sMode,
    crate::vocab::MapCurve,
    crate::vocab::GrainWindow,
    crate::vocab::SnapDir,
    crate::vocab::SnapTarget,
    crate::vocab::Harmony,
);

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

/// A unit of behavior. Authored mono and single-voice; polyphony is hosted by the Voicer (ADR-0032).
pub trait Operator: Send {
    /// Static self-description (ports + param metadata). Drives serialization,
    /// connection checking, good-button controls, and AI grounding.
    fn descriptor() -> Descriptor
    where
        Self: Sized;

    /// Process exactly one (sub)block. Must not allocate.
    fn process(&mut self, io: &mut Io);

    /// Make a fresh-state copy of the same operator type. Params are applied by the engine
    /// separately, so this only needs to reset per-instance state (typically
    /// `Box::new(Self::new())`). An operator that holds a resource binding (see
    /// [`Operator::bind_resources`]) must carry it forward here while resetting playback state,
    /// so every copy shares the decoded data.
    fn spawn(&self) -> Box<dyn Operator>;

    /// Receive decoded resources after construction, before instantiate (ADR-0016). The
    /// loader calls this on every node that declares a resource slot in its descriptor,
    /// handing the shared [`ResourceStore`] (clone the `Arc` to hold it) and the node's
    /// [`ResolvedRefs`] (resolved handles by slot name). Default no-op — the two-phase
    /// init pattern for a type-erased registry, so operators with no resources ignore it.
    fn bind_resources(&mut self, _store: &Arc<ResourceStore>, _refs: &ResolvedRefs) {}

    /// Receive the resolved **instrument-resource** sub-graphs for this node (ADR-0032 §2). The
    /// loader calls this on a node whose descriptor declares an instrument-resource slot (the
    /// Voicer), handing the voice patch built `voices` times — one independent [`Graph`] per voice,
    /// each with its own state and resolved `interface` boundary. Building happens at **load** (where
    /// the registry + resolver live, so nested `sample` resources resolve); the operator stashes the
    /// graphs and turns them into per-voice sub-plans later, at [`Operator::on_instantiate`] (which
    /// has the [`AudioConfig`]). Default no-op — only the Voicer hosts sub-patches.
    fn bind_voices(&mut self, _voices: Vec<Graph>) {}

    /// Construct any config-dependent runtime state, after the engine fixes the [`AudioConfig`]
    /// (ADR-0032 §3). Called once per node from [`Plan::instantiate`](crate::plan::Plan::instantiate)
    /// — the one place with the resolved config — **before** the node enters the execution image, so
    /// every allocation here is off the hot path (RT-safe by construction, ADR-0012). The Voicer
    /// instantiates each bound voice [`Graph`] into a sub-`Plan` + pre-allocated arena here. May fail
    /// (a voice sub-plan can be malformed); the error aborts the whole instantiate. Default `Ok(())`.
    fn on_instantiate(&mut self, _config: &AudioConfig) -> Result<(), PlanError> {
        Ok(())
    }
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
            Event { arg: &n0, frame: 0 },
            Event {
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
        // Addresslessness is now type-enforced — `Emit` has no address field (ADR-0031 step 7).
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

    /// `output::<Note>(port).emit(frame, note)` pushes one Event Message on that port — addressless
    /// (internal wires route by connection; ADR-0031), the new spelling of `emit` for events.
    #[test]
    fn output_event_emit_pushes_one_addressless_message() {
        use crate::vocab::pitch::{Note, Pitch};
        let mut sink = Vec::new();
        emitting_io(&mut sink, 0)
            .output::<Note>(0)
            .emit(2, Note::new(Pitch::from_midi(60.0), 1.0));
        assert_eq!(sink.len(), 1);
        assert_eq!(sink[0].port, 0);
        assert_eq!(sink[0].frame, 2);
        assert_eq!(
            sink[0].arg,
            Arg::Note(Note::new(Pitch::from_midi(60.0), 1.0))
        );
        // Addresslessness is now type-enforced — `Emit` has no address field (ADR-0031 step 7).
    }

    /// An Event writer is **append-only** — unlike `MsgWriter`, repeated equal payloads are NOT
    /// deduped and same-frame writes are NOT collapsed (a chord lands many notes at one frame).
    #[test]
    fn output_event_emit_appends_without_dedup_or_last_write_wins() {
        use crate::vocab::pitch::{Note, Pitch};
        let mut sink = Vec::new();
        {
            let mut io = emitting_io(&mut sink, 0);
            let mut w = io.output::<Note>(0);
            // Two chord tones at the SAME frame — both must survive (no last-write-wins).
            w.emit(0, Note::new(Pitch::Degree(0), 1.0));
            w.emit(0, Note::new(Pitch::Degree(2), 1.0));
            // The same payload again — must NOT dedup (re-press is a real second event).
            w.emit(4, Note::new(Pitch::Degree(0), 1.0));
            w.emit(4, Note::new(Pitch::Degree(0), 1.0));
        }
        assert_eq!(sink.len(), 4);
    }

    /// The Event writer adds the segment frame offset, exactly like the old `emit` and `MsgWriter`.
    #[test]
    fn output_event_emit_adds_the_segment_frame_offset() {
        use crate::vocab::pitch::{Note, Pitch};
        let mut sink = Vec::new();
        emitting_io(&mut sink, 100)
            .output::<Note>(0)
            .emit(2, Note::new(Pitch::from_midi(60.0), 1.0));
        assert_eq!(sink[0].frame, 102);
    }

    /// A held `Harmony` output reuses `MsgWriter` (`output::<Harmony>(port).set(...)`): dedup +
    /// last-write-wins are the right semantics for a single held Value.
    #[test]
    fn output_harmony_uses_msgwriter_dedup() {
        use crate::vocab::Harmony;
        let mut sink = Vec::new();
        {
            let mut io = emitting_io(&mut sink, 0);
            let mut w = io.output::<Harmony>(0);
            w.set(0, Harmony::default()); // emits
            w.set(4, Harmony::default()); // unchanged → deduped
        }
        assert_eq!(sink.len(), 1);
    }
}
