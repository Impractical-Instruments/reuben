//! Voicer — hosts N voice sub-patches and plays incoming notes across them (ADR-0032).
//!
//! A **voice is a standalone Instrument patch** referenced by path (an instrument-resource, ADR-0032
//! §2) with a declared `interface` boundary (`freq`/`gate` in, `audio`/`active` out). The loader
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
//! sub-plans (ADR-0032 supersedes the retired per-Lane replication model).
//!
//! - input 0: `notes` (`Note`) — note events. Velocity 0 is a note-off (ADR-0030).
//! - input 1: `harmony` (`Harmony`, held) — the tonal context degree notes resolve against.
//! - output 0: `audio` (`f32_buffer`) — the summed audio of all hosted voices.
//! - param 0: `voices` — voice-pool size (read by the loader to decide how many sub-patches to build).
//! - resource `voice` — the voice patch (instrument-resource, ADR-0032 §2).

use crate::config::AudioConfig;
use crate::descriptor::Descriptor;
use crate::graph::Graph;
use crate::message::{Arg, Message};
use crate::operator::{Io, Operator};
use crate::plan::{Plan, PlanError};
use crate::render::{render_plan, RenderScratch, SerialExecutor};
use crate::vocab::pitch::Pitch;

// Single-source contract (ADR-0025/0030/0032): `notes` is a `Note` event port, `harmony` a held
// `Harmony`; the one output `audio` is the summed voice mix. `voices` sizes the hosted voice pool —
// the loader reads it to decide how many voice sub-patches to build, so it is the operator's
// instantiate-time `Constant` (ADR-0028), declared via `constant: voices`. Polyphony is hosted
// internally (N voice sub-plans summed into `audio`), not fanned out across engine Lanes — the Lane
// model is gone (ADR-0032).
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
    /// Whether the voice is still producing sound (ADR-0032 §5): `true` through the release tail,
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

/// The fixed-size note-allocation pool (ADR-0032 §5): assign prefers a **truly free** voice
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
    /// Voice patch graphs bound at load (ADR-0032 §2); drained into `slots` at `on_instantiate`.
    graphs: Vec<Graph>,
    /// Note-allocation pool (musical state), parallel by index to `slots`.
    pool: VoicePool,
    /// Per-voice render state (sub-plan + arena), parallel by index to `pool.voices`.
    slots: Vec<VoiceSlot>,
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
    /// Reusable note-event snapshot `(frame, on, pitch)`. Cleared per block.
    events: Vec<(usize, bool, Pitch)>,
    /// Per-voice "got an event this block" flag (parallel to `pool.voices`), so a voice toggled
    /// on→off within one block still renders its blip even though it ends idle. Cleared per block.
    touched: Vec<bool>,
    /// Arena buffer index of each voice plan's `audio` interface output (ADR-0032 §4), resolved
    /// once (all voice plans are identical copies). `None` ⇒ no such output; fall back to `master[0]`.
    audio_buf: Option<usize>,
    /// [`Plan::captured`](crate::plan::Plan::captured) slot of each voice plan's `active` interface
    /// output. `None` ⇒ the patch declares no `active`; liveness then falls back to gate (`on`), and
    /// idle-voice skipping is disabled (we cannot know the release tail).
    active_cap: Option<usize>,
    /// Resolved interface message addresses for the voice patch's `freq`/`gate` inputs
    /// (`/<node>/<port>`), empty if the patch declares no such interface input.
    freq_addr: String,
    gate_addr: String,
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

    fn on_instantiate(&mut self, config: &AudioConfig) -> Result<(), PlanError> {
        let graphs = std::mem::take(&mut self.graphs);
        if graphs.is_empty() {
            return Ok(());
        }
        // All voice graphs are copies of one patch — resolve the interface addresses once.
        self.freq_addr = iface_input_addr(&graphs[0], "freq").unwrap_or_default();
        self.gate_addr = iface_input_addr(&graphs[0], "gate").unwrap_or_default();

        let block = config.block_size;
        let mut slots = Vec::with_capacity(graphs.len());
        for g in graphs {
            let plan = Plan::instantiate(g, *config)?;
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
        self.pool = VoicePool::new(slots.len());
        self.slots = slots;
        Ok(())
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
        for s in io.read(IN_NOTES) {
            self.events
                .push((s.frame.min(n), s.payload.velocity > 0.0, s.payload.pitch));
        }
        self.events.sort_by_key(|e| e.0);

        // Per-voice change-list: a frame-0 baseline (current held freq/gate, so a re-spell or a
        // cross-block hold lands) plus each allocation change at its event frame.
        self.changes.clear();
        self.touched.iter_mut().for_each(|t| *t = false);
        for (i, v) in self.pool.voices.iter().enumerate() {
            self.changes.push((i, 0, harmony.hz(v.pitch), v.on));
        }
        for k in 0..self.events.len() {
            let (frame, on, pitch) = self.events[k];
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
            ..
        } = self;
        let Some(scratch) = scratch.as_mut() else {
            return;
        };
        // Skip rendering a fully-idle voice (ADR-0032 §5): gate-off, release tail done, untouched
        // this block. Only when `active` is observable — without it we can't know the tail, so we
        // render every voice (today's behaviour) to avoid cutting a release.
        let can_skip = active_cap.is_some();
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
            outbound.clear();
            render_plan(
                &mut slot.plan,
                &mut slot.arena,
                scratch,
                executor,
                &msg_buf[..count],
                n,
                master,
                outbound,
            );
            // Read this voice's audio from its `audio` interface output buffer (ADR-0032 §4),
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
            // Refresh liveness from the voice's `active` interface output (held Value, ADR-0032 §5);
            // a patch without one keys liveness on the gate, the pre-`active` behaviour.
            pool.voices[i].active = match *active_cap {
                Some(c) => slot.plan.captured.get(c).copied().unwrap_or(0.0) > 0.5,
                None => pool.voices[i].on,
            };
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Voicer);

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
}
