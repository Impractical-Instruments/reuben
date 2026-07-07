//! The raw C-ABI boundary (ADR-0040): flat `#[no_mangle]` exports over [`WebShell`], plus the
//! one `log` import. No `wasm-bindgen` — bindgen's generated glue assumes a Window/Worker
//! global and fights `AudioWorkletGlobalScope` (the P1 finding, #223). Every export is a thin
//! shim; the logic lives host-tested in [`crate::shell`].
//!
//! # ABI contract (what the JS side codes against)
//!
//! One module, two instance roles: the main thread's **discovery** instance (runs the
//! fetch-on-miss loop at an arbitrary sample rate to learn the bundle) and the worklet's
//! **persistent** instance (constructs once from the complete bundle and renders). Both drive
//! the identical lifecycle:
//!
//! 1. `alloc(len)` a scratch buffer, write bytes into WASM memory, then call
//!    `set_document(ptr, len)` / `stage_resource(key_ptr, key_len, kind, data_ptr, data_len)`
//!    (kind `0` = text/JSON, `1` = WAV sample bytes — decoded on stage), then `dealloc`.
//! 2. `construct(sampleRate)` → `0` ready · `1` failed (read `error_ptr`/`error_len`) ·
//!    `2` misses: read `miss_count()` / `miss_key_ptr(i)`+`miss_key_len(i)` / `miss_kind(i)`,
//!    fetch `assetBase + key` for each, stage, and construct again.
//! 3. Per quantum: optionally write planar input at `input_ptr()` (`input[ch*128 + f]`,
//!    `input_channels()` wide), call `render(has_input)` (`0` = ok), copy planar output from
//!    `output_ptr()` (`out[ch*128 + f]`, `channels()` wide, `block_size()` == 128 frames).
//!    **Re-wrap the `Float32Array` views every quantum** — memory growth detaches old views;
//!    the pointers themselves are stable (statics never move).
//! 4. Control: pack the flat tagged buffer ([`crate::codec`]) into an `alloc`'d region and
//!    call `queue_control(ptr, len)` (`0` = queued, `1` = rejected + logged).
//! 5. Toy switch: `destroy()`, then stage + construct the next instrument on the same
//!    instance.
//!
//! Panics trap on `wasm32-unknown-unknown`; a hook ships the message through `log` first
//! (installed at every entry point — but a panic inside a static ctor predates any hook and
//! surfaces only as an opaque `RuntimeError`, the known P1 gap).

use std::cell::UnsafeCell;

use crate::shell::WebShell;

#[link(wasm_import_module = "env")]
extern "C" {
    /// Host diagnostics channel: UTF-8 bytes at `ptr..ptr+len`. The worklet posts them to
    /// the main thread; the Node harness prints them.
    fn log(ptr: *const u8, len: usize);
}

fn log_str(s: &str) {
    unsafe { log(s.as_ptr(), s.len()) }
}

/// A `static`-compatible cell for the single-threaded WASM world. An
/// `AudioWorkletProcessor` (and the discovery instance) runs on exactly one thread, so the
/// unsynchronized interior mutability is sound; the wrapper exists only to satisfy
/// `static: Sync`.
struct SingleThreadCell<T>(UnsafeCell<T>);
// SAFETY: wasm32-unknown-unknown has no threads; every export runs on the one instance
// thread (see the type doc).
unsafe impl<T> Sync for SingleThreadCell<T> {}

/// The one shell per instance. A `static` (not a heap box) so the I/O buffers inside sit at
/// fixed linear-memory offsets for the module's lifetime — the host fetches each pointer
/// once and only re-wraps views (P1 finding).
static SHELL: SingleThreadCell<Option<WebShell>> = SingleThreadCell(UnsafeCell::new(None));

/// The shell, created on first touch.
#[allow(clippy::mut_from_ref)]
fn shell() -> &'static mut WebShell {
    ensure_panic_hook();
    // SAFETY: single-threaded (see SingleThreadCell); no export holds this borrow across
    // another export call.
    let slot = unsafe { &mut *SHELL.0.get() };
    slot.get_or_insert_with(WebShell::new)
}

static HOOK: SingleThreadCell<bool> = SingleThreadCell(UnsafeCell::new(false));

/// Install the panic-to-`log` hook once, before anything that can panic: a panic on this
/// target is a trap that silently kills the processor, so the message must ship first.
fn ensure_panic_hook() {
    // SAFETY: single-threaded.
    let installed = unsafe { &mut *HOOK.0.get() };
    if !*installed {
        *installed = true;
        std::panic::set_hook(Box::new(|info| {
            log_str(&format!("panic: {info}"));
        }));
    }
}

/// Allocate `len` bytes in linear memory for the host to write into (keys, document text,
/// WAV bytes, control buffers). Returns null for `len == 0`. Pair with [`dealloc`].
#[no_mangle]
pub extern "C" fn alloc(len: u32) -> *mut u8 {
    ensure_panic_hook();
    if len == 0 {
        return std::ptr::null_mut();
    }
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Release a buffer from [`alloc`] (same `len`).
///
/// # Safety
/// `ptr` must come from [`alloc`] with exactly this `len`, and not already freed.
#[no_mangle]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, len: u32) {
    if ptr.is_null() || len == 0 {
        return;
    }
    drop(Vec::from_raw_parts(ptr, 0, len as usize));
}

/// Read a `(ptr, len)` byte region. Null/empty yields an empty slice.
///
/// # Safety
/// `ptr..ptr+len` must be a live region the host wrote (an [`alloc`] buffer).
unsafe fn bytes<'a>(ptr: *const u8, len: u32) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    std::slice::from_raw_parts(ptr, len as usize)
}

/// Number of registered operators — the life-before-main probe (P1 checkpoint): `0` means
/// `inventory` registration failed in this toolchain and nothing will load.
#[no_mangle]
pub extern "C" fn registry_count() -> u32 {
    ensure_panic_hook();
    reuben_core::Registry::builtin().entries().count() as u32
}

/// Stage the top-level instrument document (UTF-8 JSON). `0` ok, `1` bad UTF-8.
///
/// # Safety
/// `ptr..ptr+len` must be a live host-written region.
#[no_mangle]
pub unsafe extern "C" fn set_document(ptr: *const u8, len: u32) -> i32 {
    match std::str::from_utf8(bytes(ptr, len)) {
        Ok(text) => {
            shell().set_document(text);
            0
        }
        Err(_) => {
            log_str("set_document: not UTF-8");
            1
        }
    }
}

/// Stage one fetched resource under its canonical key. `kind`: `0` = text (UTF-8 JSON),
/// `1` = sample (WAV bytes, decoded now). `0` ok, `1` rejected (logged; error readable).
///
/// # Safety
/// Both `(ptr, len)` pairs must be live host-written regions.
#[no_mangle]
pub unsafe extern "C" fn stage_resource(
    key_ptr: *const u8,
    key_len: u32,
    kind: u32,
    data_ptr: *const u8,
    data_len: u32,
) -> i32 {
    let Ok(key) = std::str::from_utf8(bytes(key_ptr, key_len)) else {
        log_str("stage_resource: key not UTF-8");
        return 1;
    };
    let data = bytes(data_ptr, data_len);
    match kind {
        0 => match std::str::from_utf8(data) {
            Ok(text) => {
                shell().stage_text(key, text);
                0
            }
            Err(_) => {
                log_str(&format!("stage_resource {key}: text not UTF-8"));
                1
            }
        },
        1 => match shell().stage_sample_wav(key, data) {
            Ok(()) => 0,
            Err(e) => {
                log_str(&e);
                1
            }
        },
        other => {
            log_str(&format!("stage_resource {key}: unknown kind {other}"));
            1
        }
    }
}

/// Construct the Engine from the staged document + bundle at `sample_rate` (one 128-frame
/// block per quantum). `0` ready · `1` failed (`error_ptr`) · `2` misses (`miss_count`…).
#[no_mangle]
pub extern "C" fn construct(sample_rate: f32) -> i32 {
    shell().construct(sample_rate, &mut log_str) as i32
}

/// Misses recorded by the last construct attempt.
#[no_mangle]
pub extern "C" fn miss_count() -> u32 {
    shell().misses().len() as u32
}

/// UTF-8 pointer of miss `i`'s canonical key (null if out of range).
#[no_mangle]
pub extern "C" fn miss_key_ptr(i: u32) -> *const u8 {
    shell()
        .misses()
        .get(i as usize)
        .map_or(std::ptr::null(), |m| m.key.as_ptr())
}

/// Byte length of miss `i`'s key (`0` if out of range).
#[no_mangle]
pub extern "C" fn miss_key_len(i: u32) -> u32 {
    shell().misses().get(i as usize).map_or(0, |m| m.key.len()) as u32
}

/// Kind of miss `i`: `0` = text, `1` = sample (see [`crate::resolver::ResourceKind`]).
#[no_mangle]
pub extern "C" fn miss_kind(i: u32) -> u32 {
    shell()
        .misses()
        .get(i as usize)
        .map_or(0, |m| m.kind as u32)
}

/// Queue one control message (flat tagged buffer, [`crate::codec`]). `0` queued, `1`
/// rejected (diagnostic logged).
///
/// # Safety
/// `ptr..ptr+len` must be a live host-written region.
#[no_mangle]
pub unsafe extern "C" fn queue_control(ptr: *const u8, len: u32) -> i32 {
    match shell().queue_control(bytes(ptr, len)) {
        Ok(()) => 0,
        Err(e) => {
            log_str(&e);
            1
        }
    }
}

/// Render one 128-frame quantum into the planar output buffer. `has_input != 0` means the
/// host wrote one quantum of planar input at [`input_ptr`] first. `0` ok, `1` = no live
/// Engine (keep emitting silence).
#[no_mangle]
pub extern "C" fn render(has_input: i32) -> i32 {
    if shell().render(has_input != 0) {
        0
    } else {
        1
    }
}

/// Fixed pointer to the planar output quantum (`out[ch * 128 + f]`, [`channels`] wide).
#[no_mangle]
pub extern "C" fn output_ptr() -> *const f32 {
    shell().out().as_ptr()
}

/// Fixed pointer to the planar input staging quantum (`input[ch * 128 + f]`,
/// [`input_channels`] wide). The host writes it before a `render(1)`.
#[no_mangle]
pub extern "C" fn input_ptr() -> *mut f32 {
    shell().input_mut().as_mut_ptr()
}

/// Logical output channels of the live Engine (`0` before a successful construct; at most 2).
#[no_mangle]
pub extern "C" fn channels() -> u32 {
    shell().channels() as u32
}

/// Logical input channels of the live Engine (`0` = no input path).
#[no_mangle]
pub extern "C" fn input_channels() -> u32 {
    shell().input_channels() as u32
}

/// Frames per render quantum (always 128 — exported so JS never hardcodes it).
#[no_mangle]
pub extern "C" fn block_size() -> u32 {
    crate::shell::BLOCK as u32
}

/// Tear down for a toy switch: drop the Engine and the staged bundle. The instance stays
/// reusable — stage + construct the next instrument.
#[no_mangle]
pub extern "C" fn destroy() {
    shell().destroy();
}

/// UTF-8 pointer of the last failure message (empty when the last operation succeeded).
#[no_mangle]
pub extern "C" fn error_ptr() -> *const u8 {
    shell().error().as_ptr()
}

/// Byte length of the last failure message.
#[no_mangle]
pub extern "C" fn error_len() -> u32 {
    shell().error().len() as u32
}
