//! Throwaway spike (issue #223): prove `reuben-core` renders inside a browser audio thread.
//!
//! Compiled to `wasm32-unknown-unknown` as a raw C-ABI `cdylib` (no `wasm-bindgen` — its JS
//! glue assumes a Window/Worker global and fights `AudioWorkletGlobalScope`). The worklet
//! (`web/worklet.js`) instantiates this module, runs the static ctors (`_initialize` /
//! `__wasm_call_ctors` — that's what populates the `inventory`-backed operator registry,
//! ADR-0024), then calls `init` once and `render` once per 128-frame quantum.
//!
//! `block_size = 128` = one engine block per worklet quantum, so there is no drain adapter
//! here at all — that logic is already unit-tested in `reuben-native/src/engine.rs` and is
//! not this spike's risk.
//!
//! Failure is loud by design: on this target a panic is effectively `panic=abort` (a WASM
//! trap that kills the processor and stops audio *silently*), so `init` is fully fallible
//! (status + `error_ptr`/`error_len` message) and a custom panic hook ships the panic
//! message out through the imported `log` *before* the trap. One known gap: the hook is
//! installed as `init`'s first statement, but the static ctors run in every export's
//! LLD-synthesized prologue — i.e. *before* anything in this module can run — so a panic
//! **inside a ctor** still reaches the host only as an opaque `RuntimeError: unreachable`
//! (caught and surfaced by worklet.js, but without the message). Unfixable in-module.

use std::cell::UnsafeCell;

use reuben_core::message::Message;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, Plan, Registry, Renderer};

/// One engine block per worklet render quantum (the Web Audio spec fixes it at 128).
const BLOCK: usize = 128;
/// The worklet node is constructed with a stereo output; core's master floor is also
/// stereo (`AudioConfig::MIN_CHANNELS`), so both channels always exist.
const CHANNELS: usize = 2;

/// The pass/fail gate: a self-playing, sample-free Toy (zero `resources`, so the
/// resolver is never hit on this path).
const VIBRATO_JSON: &str = include_str!("../../../instruments/vibrato.json");
/// The opportunistic stretch: exercises the resolver + voicer-host path P2 leans on.
const SEQUENCE_JSON: &str = include_str!("../../../instruments/sequence.json");
const SEQUENCE_VOICE_JSON: &str = include_str!("../../../instruments/voices/sequence-voice.json");

#[link(wasm_import_module = "env")]
extern "C" {
    /// Host-provided diagnostics channel (backed by `port.postMessage` → `console.error`
    /// on the page). The only import this module has.
    fn log(ptr: *const u8, len: usize);
}

fn log_str(s: &str) {
    // SAFETY: ptr/len describe a live &str; the host only reads within them.
    unsafe { log(s.as_ptr(), s.len()) }
}

/// Single-threaded interior mutability for module state. `wasm32-unknown-unknown` inside
/// one AudioWorkletProcessor has exactly one thread of execution, so a bare `UnsafeCell`
/// is sound; the wrapper exists only because a `static` requires `Sync`.
struct SpikeCell<T>(UnsafeCell<T>);
// SAFETY: see type comment — no second thread ever exists to alias with.
unsafe impl<T> Sync for SpikeCell<T> {}

struct SpikeEngine {
    plan: Plan,
    renderer: Renderer,
    /// Planar master scratch, `plan.config.channels` × `BLOCK`, preallocated at init and
    /// reused so `render` is allocation-free in steady state (same discipline as
    /// `tests/rt_safe.rs` asserts for the core).
    master: Vec<Vec<f32>>,
    /// Outbound Message sink `render_block_multi` requires; the Toys have no `osc_out`,
    /// so it stays empty. Preallocated + cleared per block.
    outbound: Vec<Message>,
}

static ENGINE: SpikeCell<Option<SpikeEngine>> = SpikeCell(UnsafeCell::new(None));
/// Last `init` failure, readable by the host via `error_ptr`/`error_len`.
static ERROR: SpikeCell<String> = SpikeCell(UnsafeCell::new(String::new()));
/// The planar output the host copies from: `[ch0 × BLOCK, ch1 × BLOCK]`. A `static` (not
/// a heap Vec) so its offset into linear memory is fixed for the module's lifetime — the
/// host re-wraps a `Float32Array` over `memory.buffer` each `process()` (heap growth
/// detaches old views) but never needs to re-ask for the pointer.
static OUT: SpikeCell<[f32; CHANNELS * BLOCK]> =
    SpikeCell(UnsafeCell::new([0.0; CHANNELS * BLOCK]));

/// Resolves the embedded Toys' instrument-resources in memory (the `EmbeddedVoices`
/// precedent in `reuben-native/src/rigs.rs`): no filesystem, no fetch, no samples.
struct EmbeddedResources;

impl ResourceResolver for EmbeddedResources {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match source {
            "voices/sequence-voice.json" => Ok(SEQUENCE_VOICE_JSON.to_string()),
            other => Err(ResolveError::NotFound(other.to_string())),
        }
    }
}

/// Checkpoint 2 probe, callable before/after ctors: how many operators `inventory`
/// registered. Zero after `_initialize` means life-before-main never ran — the single
/// most likely WASM breakage this spike exists to surface.
#[no_mangle]
pub extern "C" fn registry_count() -> u32 {
    Registry::builtin().entries().count() as u32
}

/// Build the engine for `instrument` (0 = vibrato, the gate; 1 = sequence, the stretch)
/// at the device's real sample rate. Returns 0 on success; nonzero means failure with a
/// human-readable reason at `error_ptr()..error_ptr()+error_len()`.
#[no_mangle]
pub extern "C" fn init(sample_rate: f32, instrument: u32) -> i32 {
    // First thing, before anything can panic: make panics loud. The hook formats the
    // message and ships it through `log` *before* the trap kills the processor.
    std::panic::set_hook(Box::new(|info| {
        log_str(&format!("wasm panic: {info}"));
    }));
    match build_engine(sample_rate, instrument) {
        Ok(engine) => {
            // SAFETY: single-threaded module (see SpikeCell); no live borrow outlives a call.
            unsafe { *ENGINE.0.get() = Some(engine) };
            0
        }
        Err(msg) => {
            log_str(&format!("init failed: {msg}"));
            // SAFETY: as above.
            unsafe { *ERROR.0.get() = msg };
            1
        }
    }
}

fn build_engine(sample_rate: f32, instrument: u32) -> Result<SpikeEngine, String> {
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(format!("bad sample_rate {sample_rate}"));
    }
    let registry = Registry::builtin();
    let ops = registry.entries().count();
    // Checkpoint 2, in-band: the finding must report the inventory-on-WASM verdict.
    log_str(&format!("registry: {ops} operators registered"));
    if ops == 0 {
        return Err(
            "Registry::builtin() is EMPTY inside WASM: inventory ctors never ran — \
             was _initialize()/__wasm_call_ctors() called after instantiation?"
                .to_string(),
        );
    }
    let json = match instrument {
        0 => VIBRATO_JSON,
        1 => SEQUENCE_JSON,
        other => return Err(format!("unknown instrument id {other}")),
    };
    let loaded = load_instrument(json, &registry, &EmbeddedResources)
        .map_err(|e| format!("load_instrument: {e}"))?;
    for w in &loaded.warnings {
        log_str(&format!("load warning: {w}"));
    }
    let plan = Plan::instantiate(loaded.graph, AudioConfig::new(sample_rate, BLOCK))
        .map_err(|e| format!("Plan::instantiate: {e:?}"))?;
    let renderer = Renderer::new(&plan);
    let master = vec![vec![0.0; BLOCK]; plan.config.channels];
    Ok(SpikeEngine {
        plan,
        renderer,
        master,
        outbound: Vec::with_capacity(16),
    })
}

/// Render one 128-frame block into the static output buffer. Returns 0 on success,
/// nonzero if `init` hasn't succeeded yet. Steady state performs no heap allocation
/// (`Renderer::render_block_multi` is the machine-asserted RT-safe path).
#[no_mangle]
pub extern "C" fn render() -> i32 {
    // SAFETY: single-threaded module; this is the only live borrow of ENGINE/OUT.
    let engine = unsafe { &mut *ENGINE.0.get() };
    let Some(e) = engine.as_mut() else {
        return 1;
    };
    e.outbound.clear();
    // No inbound Messages (the control channel is P2's scope) and no input master.
    e.renderer
        .render_block_multi(&mut e.plan, &[], &[], &mut e.master, &mut e.outbound);
    let out = unsafe { &mut *OUT.0.get() };
    for ch in 0..CHANNELS {
        // channels is floored to stereo at instantiate (ADR-0026), so 0 and 1 exist.
        let src = &e.master[ch];
        out[ch * BLOCK..(ch + 1) * BLOCK].copy_from_slice(&src[..BLOCK]);
    }
    0
}

/// Fixed offset of the planar output buffer in linear memory (see [`OUT`]).
#[no_mangle]
pub extern "C" fn output_ptr() -> *const f32 {
    OUT.0.get() as *const f32
}

/// UTF-8 bytes of the last `init` failure message.
#[no_mangle]
pub extern "C" fn error_ptr() -> *const u8 {
    // SAFETY: single-threaded module; read-only view, no &mut outstanding across calls.
    let s: &String = unsafe { &*ERROR.0.get() };
    s.as_ptr()
}

#[no_mangle]
pub extern "C" fn error_len() -> usize {
    // SAFETY: as above.
    let s: &String = unsafe { &*ERROR.0.get() };
    s.len()
}
