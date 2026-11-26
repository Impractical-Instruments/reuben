//! Voicer — hosts N voice sub-patches and plays incoming notes across them.
//!
//! A **voice is a standalone Instrument patch** referenced by path (an instrument-resource)
//! with a declared `interface` boundary (`freq`/`gate` in, `audio`/`active` out). The loader
//! builds the patch `voices` times and binds the graphs via [`Operator::bind_voices`]; at
//! [`Operator::on_instantiate`] (where the [`AudioConfig`] is fixed) the Voicer turns each into a
//! sub-[`Plan`] plus its own pre-allocated arena. Each block the Voicer:
//!
//! 1. runs note allocation (assign / steal-oldest / release) over the incoming `notes`, resolving
//!    [`Degree`](crate::vocab::pitch::Pitch::Degree) pitches through the `harmony` context — the
//!    musical brain, kept from the old Lane-fan-out Voicer;
//! 2. drives each voice's `freq`/`gate` interface inputs with a sparse change-list of [`Message`]s at
//!    their exact frames (the sub-render block-slices at those frames, so note-ons stay
//!    sample-accurate);
//! 3. renders every voice with the re-entrant [`render_plan`] over that voice's own arena;
//! 4. sums the voices' `audio` into its single audio output, in fixed voice-index order.
//!
//! There is no Lane fan-out: the Voicer is an ordinary operator whose polyphony lives in the hosted
//! sub-plans (supersedes the retired per-Lane replication model).
//!
//! - input 0: `notes` (`Note`) — note events. Velocity 0 is a note-off.
//! - input 1: `harmony` (`Harmony`, held) — the tonal context degree notes resolve against.
//! - output 0: `audio` (`f32_buffer`) — the summed audio of all hosted voices.
//! - constant 0: `voices` (`i32`, `1..=32`) — voice-pool size (read by the loader to decide how many
//!   sub-patches to build).
//! - resource `voice` — the voice patch (instrument-resource).
//!
//! Every voice measures into one probe slot set, exposed by [`Operator::hosted_probes`] and
//! committed once per `process`. A Voicer inside a voice patch exposes only the inner slot set of
//! the outer Voicer's voice 0, so its voice meters show that voice alone and freeze while it is
//! idle.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, PortType};
use crate::graph::Graph;
use crate::coordinator::retire::retire_to;
use crate::message::{Arg, Message};
use crate::operator::{Io, Operator};
use crate::plan::{Plan, PlanError};
use crate::probe::ProbeSlots;
use crate::render::{render_plan, RenderScratch, SerialExecutor};
use crate::vocab::pitch::Pitch;

// Single-source contract: `notes` is a `Note` event port, `harmony` a held
// `Harmony`; the one output `audio` is the summed voice mix. `voices` sizes the hosted voice pool —
// the loader reads it to decide how many voice sub-patches to build, so it is the operator's
// instantiate-time `Constant`, declared via `constant: voices`. Polyphony is hosted
// internally (N voice sub-plans summed into `audio`), not fanned out across engine Lanes — the Lane
// model is gone.
crate::operator_contract!(Voicer {
    inputs:  { notes: note, harmony: harmony },
    outputs: { audio: f32_buffer },
    constants: { voices: i32 { 1..=32, default 8 } },
    resources: { voice },
});

/// Do two pitches denote the same note for note-off matching? Degrees match by degree; absolute
/// notes by MIDI. (A degree and an absolute never match — distinct identities.)
fn same_note(a: Pitch, b: Pitch) -> bool {
    match (a, b) {
        (Pitch::Degree(x), Pitch::Degree(y)) => x == y,
        (Pitch::Absolute(x), Pitch::Absolute(y)) => x == y,
        _ => false,
    }
}

/// One entry in the note-allocation pool — the **musical** state of a voice (its held pitch /
/// gate / assignment age), independent of the rendered sub-plan it drives. Kept separate from
/// [`VoiceSlot`] so the allocation brain is unit-testable without instantiating any graph.
#[derive(Clone, Copy)]
struct Voice {
    /// Symbolic pitch this voice holds — a degree (resolved through the context, re-spells live) or
    /// an absolute MIDI note. Frequency is derived from it each block via the current context.
    pitch: Pitch,
    /// Whether the voice is currently holding a note (gate high).
    on: bool,
    /// Whether the voice is still producing sound: `true` through the release tail,
    /// `false` once fully idle. Fed back post-render from the voice's `active` interface output (or,
    /// for a patch without one, falls back to `on`). Keeps a tailing voice out of the free pool.
    active: bool,
    /// Assignment stamp; higher = more recently assigned (for steal-oldest).
    age: u64,
}

impl Default for Voice {
    fn default() -> Self {
        // Idle pitch = A4, so an unplayed voice reads 440 Hz.
        Self {
            pitch: Pitch::from_midi(69.0),
            on: false,
            active: false,
            age: 0,
        }
    }
}

/// The fixed-size note-allocation pool: assign prefers a **truly free** voice
/// (gate-off *and* its release tail finished — `!on && !active`) and otherwise steals the oldest;
/// release clears the oldest voice holding the note. Keying free-ness on `active` (not just gate)
/// stops a still-ringing voice being stolen while a silent one exists. The musical brain, carried
/// over from the Lane-fan-out Voicer but now indexable so the host can drive a sub-plan per voice.
#[derive(Default)]
struct VoicePool {
    voices: Vec<Voice>,
    /// Monotonic assignment counter, for steal-oldest ordering.
    counter: u64,
}

impl VoicePool {
    fn new(n: usize) -> Self {
        Self {
            voices: vec![Voice::default(); n],
            counter: 0,
        }
    }

    /// Assign `pitch` to a voice — a free one (lowest index), else steal the oldest — and return its
    /// index.
    fn assign(&mut self, pitch: Pitch) -> usize {
        let idx = self
            .voices
            .iter()
            .position(|v| !v.on && !v.active)
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        self.counter += 1;
        // A freshly assigned voice is sounding immediately (gate rising this block); `active` is
        // refreshed from its render below, but seed it `true` so a same-block second note doesn't
        // treat it as free before its first render.
        self.voices[idx] = Voice {
            pitch,
            on: true,
            active: true,
            age: self.counter,
        };
        idx
    }

    /// Release the oldest voice currently holding `pitch`, returning its index (or `None`).
    fn release(&mut self, pitch: Pitch) -> Option<usize> {
        let idx = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.on && same_note(v.pitch, pitch))
            .min_by_key(|(_, v)| v.age)
            .map(|(i, _)| i)?;
        self.voices[idx].on = false;
        Some(idx)
    }
}

/// One hosted voice's render state: its sub-[`Plan`] and the edge-buffer arena it renders into.
/// Parallel by index to [`VoicePool::voices`].
struct VoiceSlot {
    plan: Plan,
    /// This voice's own edge-buffer arena (independent per-voice signal state, persists across
    /// blocks). Sized at [`Operator::on_instantiate`]; never grown on the hot path.
    arena: Vec<Vec<f32>>,
}

#[derive(Default)]
pub struct Voicer {
    /// Voice patch graphs bound at load; drained into `slots` at `on_instantiate`.
    graphs: Vec<Graph>,
    /// Note-allocation pool (musical state), parallel by index to `slots`.
    pool: VoicePool,
    /// Per-voice render state (sub-plan + arena), parallel by index to `pool.voices`.
    slots: Vec<VoiceSlot>,
    /// The one probe slot set every voice plan measures into; committed at the end of each
    /// `process`, so a node's level folds every voice's samples and its ballistics advance once.
    probes: Option<Arc<ProbeSlots>>,
    /// Shared per-block render scratch, sized to the (uniform) voice plan. Reused across voices.
    scratch: Option<RenderScratch>,
    /// Shared per-channel master scratch for a sub-render; `master[0]` is the voice's audio.
    master: Vec<Vec<f32>>,
    /// Shared throwaway outbound sink (a voice patch sends nothing past the boundary).
    outbound: Vec<Message>,
    executor: SerialExecutor,
    /// Reusable per-voice render-message buffer (`freq`/`gate` edges). Grown only during warmup;
    /// reused (clear + rewrite addresses in place) so steady-state `process` never allocates.
    msg_buf: Vec<Message>,
    /// Reusable allocation change-list `(voice, frame, freq, gate)`. Cleared per block.
    changes: Vec<(usize, usize, f32, bool)>,
    /// Reusable note-event snapshot `(frame, delivery index, on, pitch)`. Cleared per block.
    events: Vec<(usize, usize, bool, Pitch)>,
    /// Per-voice "got an event this block" flag (parallel to `pool.voices`), so a voice toggled
    /// on→off within one block still renders its blip even though it ends idle. Cleared per block.
    touched: Vec<bool>,
    /// Arena buffer index of each voice plan's `audio` interface output, resolved once from voice
    /// 0 and then used to index *every* voice's arena. That is only sound because the voices are
    /// built from one patch and Instantiate is a pure function of the graph, so each independently
    /// built plan lands its boundary output on the same slot — asserted at `on_instantiate`, since
    /// a mismatch would read another voice's audio silently rather than fail.
    /// `None` ⇒ no such output; fall back to `master[0]`.
    audio_buf: Option<usize>,
    /// [`Plan::captured`](crate::plan::Plan::captured) slot of each voice plan's `active` interface
    /// output. `None` ⇒ the patch declares no `active`; liveness then falls back to gate (`on`), and
    /// idle-voice skipping is disabled (we cannot know the release tail).
    active_cap: Option<usize>,
    /// Resolved interface message addresses for the voice patch's `freq`/`gate` inputs
    /// (`/<node>/<port>`), empty if the patch declares no such interface input.
    freq_addr: String,
    gate_addr: String,
    /// This node's address, bound before `on_instantiate` so a refused voice patch names its host.
    address: String,
}

impl Voicer {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The `/<node>/<port>` message address of a voice patch's named `interface` **input**, used to
/// drive it via routed [`Message`]s. `None` if the patch declares no such interface input.
fn iface_input_addr(g: &Graph, name: &str) -> Option<String> {
    let (key, port) = g.interface.inputs.get(name).copied()?;
    let node = g.nodes.get(key)?;
    let pname = node.descriptor.inputs.get(port)?.name;
    Some(format!("{}/{}", node.address, pname))
}

/// Append/overwrite reusable message slot `*i` with `(addr, value, frame)` — reusing the slot's
/// `String` capacity (no allocation once the buffer is warm), growing only on the first blocks.
fn push_msg(buf: &mut Vec<Message>, i: &mut usize, addr: &str, v: f32, frame: usize) {
    if *i < buf.len() {
        let m = &mut buf[*i];
        m.address.clear();
        m.address.push_str(addr);
        m.frame = frame;
        m.arg = Arg::F32(v);
    } else {
        buf.push(Message::new(addr, Arg::F32(v), frame));
    }
    *i += 1;
}

impl Operator for Voicer {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn bind_voices(&mut self, voices: Vec<Graph>) {
        self.graphs = voices;
    }

    fn bind_address(&mut self, address: &str) {
        self.address = address.into();
    }

    fn on_instantiate(&mut self, config: &AudioConfig) -> Result<(), PlanError> {
        let graphs = core::mem::take(&mut self.graphs);
        if graphs.is_empty() {
            return Ok(());
        }
        // The same rule as the loader's, for a graph that never went through it. A Signal `active`
        // gets no captured slot, so liveness falls back to the gate; any held type but a number
        // would read 0 — idle — and cut every release tail. A key or port the graph does not hold
        // is left for `Plan::instantiate` to refuse with its own error.
        if let Some(&(key, port)) = graphs[0].interface.outputs.get("active") {
            if let Some(out) = graphs[0]
                .nodes
                .get(key)
                .and_then(|n| n.descriptor.outputs.get(port))
            {
                let ty = &out.ty;
                if !(ty.is_buffer() || matches!(ty, PortType::F32 | PortType::I32 { .. })) {
                    return Err(PlanError::NonNumericActive {
                        voicer: self.address.clone(),
                        ty: format!("{ty}"),
                    });
                }
            }
        }
        // All voice graphs are copies of one patch — resolve the interface addresses once.
        self.freq_addr = iface_input_addr(&graphs[0], "freq").unwrap_or_default();
        self.gate_addr = iface_input_addr(&graphs[0], "gate").unwrap_or_default();

        let block = config.block_size;
        let mut slots = Vec::with_capacity(graphs.len());
        // Voice 0's Plan builds the shared slot set, after Instantiate has dissolved the
        // interface pipes out of it, so every slot names a node each voice actually renders.
        let mut probes: Option<Arc<ProbeSlots>> = None;
        for g in graphs {
            // Hosted voice plans get **no input-master plumbing**:
            // the loader's
            // voice-resource pass cleared each copy's channel bindings (hosted inertness is
            // enforced at that one altitude, for every host), so `Plan::instantiate` derives
            // `config.input_channels == 0` and builds no input taps here. Even if a binding
            // slipped through (a programmatic `bind_voices` caller), the Voicer renders every
            // voice with `inputs = &[]`, and an unsupplied channel falls back to the pipe's
            // ordinary message-fed materialize path — hosted bindings stay inert either way.
            let plan = match &probes {
                None => {
                    let mut plan = Plan::instantiate(g, *config)?;
                    plan.hosted = true;
                    probes = Some(plan.probes.clone());
                    plan
                }
                Some(shared) => Plan::instantiate_hosted(g, *config, shared.clone())?,
            };
            let arena = (0..plan.num_buffers).map(|_| vec![0.0; block]).collect();
            slots.push(VoiceSlot { plan, arena });
        }
        self.scratch = Some(RenderScratch::new(&slots[0].plan));
        self.master = (0..slots[0].plan.config.channels)
            .map(|_| vec![0.0; block])
            .collect();
        self.outbound = Vec::with_capacity(64);
        self.msg_buf = Vec::with_capacity(slots.len() * 4);
        self.changes = Vec::with_capacity(slots.len() * 4);
        self.events = Vec::with_capacity(16);
        self.touched = vec![false; slots.len()];
        // All voice plans are copies of one patch — resolve the output boundary indices once.
        self.audio_buf = slots[0].plan.interface_signal_buf("audio");
        self.active_cap = slots[0].plan.interface_value_slot("active");
        // Voice 0's indices are used against every voice's arena and `captured`, so the plans have
        // to agree. They are built independently (a `Graph` is not `Clone`), and the arena slot is
        // now the output of a liveness pass rather than a port ordinal — a quieter function of the
        // same input. Loud in dev, because a drifted index is not a crash but the wrong voice.
        debug_assert!(
            slots
                .iter()
                .all(|s| s.plan.interface_signal_buf("audio") == self.audio_buf
                    && s.plan.interface_value_slot("active") == self.active_cap),
            "voice plans disagree on their boundary slots — one patch must instantiate identically"
        );
        self.pool = VoicePool::new(slots.len());
        self.slots = slots;
        self.probes = probes;
        Ok(())
    }

    fn hosted_probes(&self) -> Option<Arc<ProbeSlots>> {
        self.probes.clone()
    }

    fn swap_hosted_probes(&mut self, slots: &mut Option<Arc<ProbeSlots>>) {
        debug_assert!(
            match (&self.probes, &*slots) {
                (Some(a), Some(b)) => a.nodes.len() == b.nodes.len(),
                _ => true,
            },
            "hosted slot sets from different voice patches"
        );
        core::mem::swap(&mut self.probes, slots);
        if let Some(probes) = &self.probes {
            for slot in &mut self.slots {
                slot.plan.probes = probes.clone();
            }
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        if self.slots.is_empty() {
            // No voice patch bound (e.g. driven bare): output silence.
            let out = io.write(OUT_AUDIO);
            out[..n].iter_mut().for_each(|s| *s = 0.0);
            return;
        }

        // Current context (constant this segment; the engine slices at context changes, so a held
        // degree re-spells at the change frame). Default when unconnected.
        let harmony = io.read(IN_HARMONY);

        // Snapshot note events for this (sub)block, sorted by frame. (Can't read the stream while an
        // output borrow is live, so snapshot first.)
        self.events.clear();
        for (k, s) in io.read(IN_NOTES).enumerate() {
            self.events
                .push((s.frame.min(n), k, s.payload.velocity > 0.0, s.payload.pitch));
        }
        // Not `sort_by_key`: a stable sort allocates its merge buffer once a burst outgrows the
        // stack scratch. Keying on the delivery index keeps same-frame events in delivery order.
        self.events.sort_unstable_by_key(|e| (e.0, e.1));

        // Per-voice change-list: a frame-0 baseline (current held freq/gate, so a re-spell or a
        // cross-block hold lands) plus each allocation change at its event frame.
        self.changes.clear();
        self.touched.iter_mut().for_each(|t| *t = false);
        for (i, v) in self.pool.voices.iter().enumerate() {
            self.changes.push((i, 0, harmony.hz(v.pitch), v.on));
        }
        for k in 0..self.events.len() {
            let (frame, _, on, pitch) = self.events[k];
            let idx = if on {
                self.pool.assign(pitch)
            } else {
                match self.pool.release(pitch) {
                    Some(i) => i,
                    None => continue,
                }
            };
            self.touched[idx] = true;
            let v = self.pool.voices[idx];
            self.changes.push((idx, frame, harmony.hz(v.pitch), v.on));
        }

        // Render each voice into its own arena and sum its audio (fixed index order — determinism).
        let retire = io.retire_queue();
        let out = io.write(OUT_AUDIO);
        out[..n].iter_mut().for_each(|s| *s = 0.0);
        let Voicer {
            pool,
            slots,
            scratch,
            master,
            outbound,
            executor,
            msg_buf,
            changes,
            touched,
            freq_addr,
            gate_addr,
            audio_buf,
            active_cap,
            probes,
            ..
        } = self;
        // Set together with `slots` at `on_instantiate`; a `None` here would skip the commit below.
        debug_assert!(scratch.is_some(), "a Voicer with voices has no render scratch");
        let Some(scratch) = scratch.as_mut() else {
            return;
        };
        // Skip rendering a fully-idle voice: gate-off, release tail done, untouched
        // this block. Only when `active` is observable — without it we can't know the tail, so we
        // render every voice (today's behaviour) to avoid cutting a release.
        let can_skip = active_cap.is_some();
        let mut counted = crate::operator::OperatorCounts::default();
        for (i, slot) in slots.iter_mut().enumerate() {
            let v = pool.voices[i];
            if can_skip && !v.on && !v.active && !touched[i] {
                continue;
            }
            let mut count = 0usize;
            for &(vi, frame, freq, gate) in changes.iter() {
                if vi != i {
                    continue;
                }
                if !freq_addr.is_empty() {
                    push_msg(msg_buf, &mut count, freq_addr, freq, frame);
                }
                if !gate_addr.is_empty() {
                    push_msg(
                        msg_buf,
                        &mut count,
                        gate_addr,
                        if gate { 1.0 } else { 0.0 },
                        frame,
                    );
                }
            }
            // A voice's `osc_out` sends go nowhere past the boundary. Their args are retired; each
            // Message's address `String` is freed here, as the echo path frees one.
            for m in outbound.drain(..) {
                retire_to(retire, m.arg);
            }
            render_plan(
                &mut slot.plan,
                &mut slot.arena,
                scratch,
                executor,
                &msg_buf[..count],
                n,
                // No input master for a hosted plan — and none exists: the
                // voice graph's channel bindings were cleared at `on_instantiate`.
                &[],
                master,
                outbound,
                retire,
            );
            counted += slot.plan.take_operator_counts();
            // Read this voice's audio from its `audio` interface output buffer,
            // falling back to the master tap for a patch without one (e.g. driven bare).
            let audio = match *audio_buf {
                Some(b) => slot.arena.get(b),
                None => master.first(),
            };
            if let Some(ch0) = audio {
                for (o, s) in out[..n].iter_mut().zip(ch0[..n].iter()) {
                    *o += *s;
                }
            }
            // Refresh liveness from the voice's `active` interface output (held Value);
            // a patch without one keys liveness on the gate, the pre-`active` behaviour.
            pool.voices[i].active = match *active_cap {
                Some(c) => slot.plan.captured.get(c).copied().unwrap_or(0.0) > 0.5,
                None => pool.voices[i].on,
            };
        }
        *io.counts() += counted;
        // Every block, sounding voices or none, so an idle Voicer's meters still fall.
        if let Some(probes) = probes.as_deref().filter(|p| p.any_on()) {
            probes.commit(n);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        // The bound voice graphs are this operator's resource binding, and [`Operator::spawn`]
        // carries a binding forward: a copy that dropped them would host an empty pool and render
        // silence. Each voice is copied in turn rather than shared, so a copy's pool starts as
        // independent of its template's as the template's voices are of each other.
        //
        // After `on_instantiate` has taken the graphs there is nothing here to carry — by then the
        // pool is live sub-plans, which no copy may share.
        Box::new(Self {
            graphs: self.graphs.iter().map(Graph::spawn_copy).collect(),
            ..Self::new()
        })
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn abs(midi: f32) -> Pitch {
        Pitch::Absolute(midi)
    }

    // --- note-allocation pool (the musical brain; no rendering needed) ---

    #[test]
    fn assign_fills_free_voices_in_order() {
        let mut p = VoicePool::new(3);
        assert_eq!(p.assign(abs(60.0)), 0);
        assert_eq!(p.assign(abs(64.0)), 1);
        assert_eq!(p.assign(abs(67.0)), 2);
        assert!(p.voices.iter().all(|v| v.on));
    }

    #[test]
    fn release_clears_the_matching_voice_only() {
        let mut p = VoicePool::new(2);
        p.assign(abs(60.0));
        p.assign(abs(64.0));
        assert_eq!(p.release(abs(60.0)), Some(0));
        assert!(!p.voices[0].on);
        assert!(p.voices[1].on);
        // A note-off for a pitch no voice holds is a no-op.
        assert_eq!(p.release(abs(72.0)), None);
    }

    #[test]
    fn out_of_voices_steals_the_oldest() {
        let mut p = VoicePool::new(2);
        p.assign(abs(60.0)); // voice 0, age 1
        p.assign(abs(64.0)); // voice 1, age 2
                             // Pool full: the third note steals voice 0 (oldest) and plays 67.
        let idx = p.assign(abs(67.0));
        assert_eq!(idx, 0);
        assert!(same_note(p.voices[0].pitch, abs(67.0)));
        assert!(same_note(p.voices[1].pitch, abs(64.0)));
    }

    #[test]
    fn assign_skips_a_tailing_voice_for_a_truly_free_one() {
        // v0 played + released but still ringing (active true through its tail); v1 never played.
        let mut p = VoicePool::new(2);
        p.assign(abs(60.0));
        p.release(abs(60.0)); // on=false, active stays true (tail not yet finished)
        assert!(!p.voices[0].on && p.voices[0].active);
        // A new note must take the free v1, not steal the ringing v0.
        assert_eq!(p.assign(abs(67.0)), 1);
        assert!(p.voices[0].active, "tailing voice left untouched");
    }

    #[test]
    fn steals_oldest_when_every_voice_is_still_sounding() {
        // Both voices released but still tailing (active) — no truly-free voice — so steal oldest.
        let mut p = VoicePool::new(2);
        p.assign(abs(60.0)); // age 1
        p.assign(abs(64.0)); // age 2
        p.release(abs(60.0));
        p.release(abs(64.0));
        assert!(p.voices.iter().all(|v| !v.on && v.active));
        assert_eq!(p.assign(abs(67.0)), 0); // oldest
    }

    #[test]
    fn an_idle_voice_clears_of_active_is_reused_first() {
        // Once a voice's tail finishes (active cleared, mimicking the render feedback), it becomes
        // free again and is preferred over a tailing one.
        let mut p = VoicePool::new(2);
        p.assign(abs(60.0)); // v0
        p.assign(abs(64.0)); // v1
        p.release(abs(60.0));
        p.voices[0].active = false; // v0 tail finished
                                    // v1 still held (on). v0 free. New note -> v0.
        assert_eq!(p.assign(abs(67.0)), 0);
    }

    #[test]
    fn degree_and_absolute_never_match_for_note_off() {
        assert!(!same_note(Pitch::Degree(0), Pitch::Absolute(60.0)));
        assert!(same_note(Pitch::Degree(2), Pitch::Degree(2)));
        assert!(same_note(Pitch::Absolute(60.0), Pitch::Absolute(60.0)));
    }

    // --- probes: every voice of one Voicer folds into one slot set ---

    mod probes {
        use alloc::sync::Arc;

        use super::*;
        use crate::message::Arg;
        use crate::operators::envelope::{self, Envelope};
        use crate::operators::mul::mul_f32_signal::{self, MulF32Signal};
        use crate::descriptor::Port;
        use crate::graph::NodeKey;
        use crate::operators::chord::{self, Chord};
        use crate::operators::filter::Filter;
        use crate::operators::harmony::{self, HarmonyOp};
        use crate::operators::oscillator::{self, Oscillator};
        use crate::operators::pipe::Pipe;
        use crate::plan::PortKind;
        use crate::probe::ProbeSlots;
        use crate::render::Renderer;
        use crate::vocab::pitch::Note;

        const RATE: f32 = 48_000.0;
        const BLOCK: usize = 128;

        /// `/osc` → `/vca` ← `/env.cv`, `/vca` the voice's `audio`. Sustain at 1 so a held note
        /// peaks at the sine's 1.0; release at its 1 ms floor so `/vca` falls silent at once. With
        /// `active` the Voicer skips a voice once its envelope idles; without it, every voice
        /// renders every block.
        fn voice_patch(active: bool) -> Graph {
            let mut g = Graph::new();
            let osc = g.add("/osc", Oscillator::new());
            let env = g.add("/env", Envelope::new());
            let vca = g.add("/vca", MulF32Signal::new());
            g.set_value(env, "sustain", &Arg::F32(1.0));
            g.set_value(env, "release", &Arg::F32(0.001));
            g.connect(osc, oscillator::OUT_AUDIO, vca, mul_f32_signal::IN_A);
            g.connect(env, envelope::OUT_CV, vca, mul_f32_signal::IN_B);
            let iface = &mut g.interface;
            iface
                .inputs
                .insert("freq".into(), (osc, oscillator::IN_FREQ.index()));
            iface
                .inputs
                .insert("gate".into(), (env, envelope::IN_GATE.index()));
            iface
                .outputs
                .insert("audio".into(), (vca, mul_f32_signal::OUT_OUT.index()));
            if active {
                iface
                    .outputs
                    .insert("active".into(), (env, envelope::OUT_ACTIVE.index()));
            }
            g
        }

        fn voicer(patch: &Graph, voices: usize) -> Voicer {
            let mut v = Voicer::new();
            v.bind_voices((0..voices).map(|_| patch.spawn_copy()).collect());
            v
        }

        /// One Voicer per address, each with two voices of `patch`, summed to the master.
        fn plan(patch: &Graph, addresses: &[&str]) -> Plan {
            plan_of(patch, addresses, 2)
        }

        fn plan_of(patch: &Graph, addresses: &[&str], voices: usize) -> Plan {
            let mut g = Graph::new();
            for address in addresses {
                let v = g.add(address, voicer(patch, voices));
                g.tap_output(v, OUT_AUDIO);
            }
            Plan::instantiate(g, AudioConfig::new(RATE, BLOCK)).expect("instantiate")
        }

        fn hosted(plan: &Plan, address: &str) -> Arc<ProbeSlots> {
            plan.nodes()
                .iter()
                .find(|n| &*n.address == address)
                .expect("the Voicer node")
                .op
                .hosted_probes()
                .expect("a Voicer with voices hosts a slot set")
        }

        fn peak(slots: &ProbeSlots, address: &str) -> f32 {
            slots.find(address).expect("a slot").ports[0].read().0
        }

        fn note(address: &str, velocity: f32) -> Message {
            let notes = format!("{address}/notes");
            Message::new(&notes, Arg::Note(Note::new(Pitch::Absolute(69.0), velocity)), 0)
        }

        fn render(r: &mut Renderer, plan: &mut Plan, messages: &[Message], seconds: f32) {
            let mut out = vec![0.0f32; BLOCK];
            r.render_block(plan, messages, &mut out);
            for _ in 1..(seconds * RATE) as usize / BLOCK {
                r.render_block(plan, &[], &mut out);
            }
        }

        fn db(ratio: f32) -> f32 {
            20.0 * ratio.log10()
        }

        #[test]
        fn voices_fold_into_one_meter_that_decays_once_per_block() {
            let mut plan = plan(&voice_patch(false), &["/v"]);
            let slots = hosted(&plan, "/v");
            slots.find("/osc").unwrap().set_on(true);
            slots.find("/vca").unwrap().set_on(true);
            let mut r = Renderer::new(&plan);

            render(&mut r, &mut plan, &[note("/v", 1.0), note("/v", 1.0)], 0.5);
            let held = peak(&slots, "/osc");
            assert!((held - 1.0).abs() <= 1e-3, "two voices peak {held}, not 2.0");

            // Both voices keep rendering, silent past their 1 ms release: two renders a block, one
            // commit.
            render(&mut r, &mut plan, &[note("/v", 0.0), note("/v", 0.0)], 0.1);
            let before = peak(&slots, "/vca");
            render(&mut r, &mut plan, &[], 1.0);
            let fell = db(peak(&slots, "/vca") / before);
            assert!((fell + 12.0).abs() <= 0.5, "fell {fell} dB in 1 s");
        }

        /// Voices fold as more samples of one block, so two identical voices in phase sum their
        /// squares: twice one voice's mean square, √2 its RMS.
        #[test]
        fn in_phase_voices_sum_their_mean_squares() {
            let mean_square = |voices: usize| {
                let mut plan = plan_of(&voice_patch(false), &["/v"], voices);
                let slots = hosted(&plan, "/v");
                slots.find("/osc").unwrap().set_on(true);
                let mut r = Renderer::new(&plan);
                // Without `active`, every voice's `/osc` runs from the same phase, note or none.
                render(&mut r, &mut plan, &[], 2.0);
                let (_, mean_square) = slots.find("/osc").unwrap().ports[0].read();
                mean_square
            };
            let (one, two) = (mean_square(1), mean_square(2));
            assert!((two / one - 2.0).abs() <= 0.02, "one {one}, two {two}");
        }

        #[test]
        fn a_reenabled_voice_probe_reads_silence_while_the_voicer_is_idle() {
            let mut plan = plan(&voice_patch(true), &["/v"]);
            let slots = hosted(&plan, "/v");
            let osc = slots.find("/osc").unwrap();
            osc.set_on(true);
            let mut r = Renderer::new(&plan);
            render(&mut r, &mut plan, &[note("/v", 1.0)], 0.5);
            render(&mut r, &mut plan, &[note("/v", 0.0)], 0.1);
            assert!(osc.ports[0].read().0 > 0.5);

            osc.set_on(false);
            osc.set_on(true);
            // Every voice idle: no voice renders `/osc`, so only the commit can apply the reset.
            render(&mut r, &mut plan, &[], BLOCK as f32 / RATE);
            assert_eq!(osc.ports[0].read(), (0.0, 0.0));
        }

        #[test]
        fn a_silent_voicer_still_decays_its_meters() {
            let mut plan = plan(&voice_patch(true), &["/v"]);
            let slots = hosted(&plan, "/v");
            slots.find("/osc").unwrap().set_on(true);
            let mut r = Renderer::new(&plan);

            render(&mut r, &mut plan, &[note("/v", 1.0), note("/v", 1.0)], 0.5);
            render(&mut r, &mut plan, &[note("/v", 0.0), note("/v", 0.0)], 0.1);
            // Every voice idle: from here the Voicer renders none of them.
            let before = peak(&slots, "/osc");
            assert!(before > 0.5, "peak {before}");
            render(&mut r, &mut plan, &[], 1.0);
            let fell = db(peak(&slots, "/osc") / before);
            assert!((fell + 12.0).abs() <= 0.5, "fell {fell} dB in 1 s");
        }

        #[test]
        fn two_voicers_on_one_voice_patch_meter_separately() {
            let mut plan = plan(&voice_patch(true), &["/a", "/b"]);
            let (a, b) = (hosted(&plan, "/a"), hosted(&plan, "/b"));
            assert!(!Arc::ptr_eq(&a, &b));
            a.find("/osc").unwrap().set_on(true);
            b.find("/osc").unwrap().set_on(true);
            let mut r = Renderer::new(&plan);

            render(&mut r, &mut plan, &[note("/a", 1.0)], 0.5);
            assert!(peak(&a, "/osc") > 0.9);
            assert_eq!(b.find("/osc").unwrap().ports[0].read(), (0.0, 0.0));
        }

        /// `voice_patch` with its `active` output fed from `port` of a node `add` puts in it.
        fn active_from(add: impl FnOnce(&mut Graph) -> (NodeKey, usize)) -> Graph {
            let mut g = voice_patch(false);
            let active = add(&mut g);
            g.interface.outputs.insert("active".into(), active);
            g
        }

        fn instantiate(patch: &Graph) -> Result<Plan, PlanError> {
            let mut g = Graph::new();
            let v = g.add("/v", voicer(patch, 2));
            g.tap_output(v, OUT_AUDIO);
            Plan::instantiate(g, AudioConfig::new(RATE, BLOCK))
        }

        /// A graph built without the document loader still meets the loader's rule: a held
        /// `active` that is not a number would read as idle and cut every release tail, so the
        /// Voicer refuses it at Instantiate.
        #[test]
        fn a_non_numeric_held_active_is_refused_at_instantiate() {
            let note = active_from(|g| {
                let ch = g.add("/ch", Chord::new());
                (ch, chord::OUT_DEGREES.index())
            });
            let enumerated = active_from(|g| {
                let mode = Filter::descriptor()
                    .inputs
                    .into_iter()
                    .find(|p| p.name == "mode")
                    .expect("the filter's mode port");
                let mut desc = Pipe::descriptor();
                desc.inputs = vec![Port { name: "in", ..mode.clone() }];
                desc.outputs = vec![Port { name: "out", ..mode }];
                let pipe = Box::new(Pipe::new(PortKind::Value));
                (g.add_boxed("/mode", pipe, Arc::new(desc)), 0)
            });
            let harmony = active_from(|g| {
                let h = g.add("/h", HarmonyOp::default());
                (h, harmony::OUT_HARMONY.index())
            });
            for (label, patch) in [("note", note), ("enum", enumerated), ("harmony", harmony)] {
                match instantiate(&patch) {
                    Err(PlanError::NonNumericActive { voicer, .. }) => {
                        assert_eq!(voicer, "/v", "{label}")
                    }
                    Err(e) => panic!("{label}: refused for another reason: {e:?}"),
                    Ok(_) => panic!("{label}: a non-numeric `active` must not instantiate"),
                }
            }
        }

        /// A Signal `active` has no captured slot, so liveness keys on the gate: it instantiates.
        #[test]
        fn a_signal_active_instantiates() {
            let patch = active_from(|g| {
                let vca = g
                    .interface
                    .outputs
                    .get("audio")
                    .expect("the voice's audio")
                    .0;
                (vca, mul_f32_signal::OUT_OUT.index())
            });
            instantiate(&patch).expect("a Signal `active` instantiates");
        }
    }
}
