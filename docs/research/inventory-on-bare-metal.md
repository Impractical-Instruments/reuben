# Does `inventory` self-registration survive `thumbv7em-none-eabihf`?

**Date:** 2026-08-12 (all sources read and all builds run this day) ·
**Ticket:** [#737](https://github.com/Impractical-Instruments/reuben/issues/737)
(`wayfinder:research`), an input to the port-shape decision
[#734](https://github.com/Impractical-Instruments/reuben/issues/734).

**Research question:** `reuben-core`'s two registries — operators
([`registry.rs`](../../crates/reuben-core/src/registry.rs)) and OSC forms
([`boundary.rs`](../../crates/reuben-core/src/boundary.rs)) — self-register through the
`inventory` crate. Does that keep working when the core is built for a bare-metal Cortex-M7
(`thumbv7em-none-eabihf`, no OS, external SDRAM), or must it be replaced with `linkme`?

The reasoning on record says `linkme` is "the fallback if the core ever goes `no_std`". That
premise looked stale: **`inventory` 0.3.24 is itself `#![no_std]`.** `no_std` is necessary but
not sufficient — the section still has to survive the link *and* something has to run it — so
this was tested, not inferred.

**Method:** the vendored `inventory` 0.3.24 source and its own README (primary), the vendored
`cortex-m-rt` 0.7.6 source and its generated `link.x` (primary), the upstream `dtolnay/inventory`
and `dtolnay/linkme` repositories — release notes, CI config, issue threads (primary), the Rust
Reference on `#[used]` (primary), and **a throwaway spike crate built for the target seven ways**
and inspected with `readelf`/`rust-nm`/`rust-objdump` (the load-bearing evidence). Every claim is
marked **OBSERVED** (read in source or official docs, or read off a linked artifact produced
today) or **INFERRED** (reasoning over observations). No hardware and no emulator were available;
§7 states exactly what that leaves unconfirmed.

---

## 0. Verdict

**`inventory` does not work on `thumbv7em-none-eabihf` as things stand, and the reason is not
`no_std`.** It registers through **ELF `.init_array` constructors**, and nothing on this target
runs them. `#![no_std]` removed a *compilation* obstacle and left the *startup* obstacle exactly
where it was.

The two failure modes the ticket asked to separate:

| Failure mode | Result |
|---|---|
| **(a) registration never happens** | **This is what happens.** The constructor pointers are in the image; no code calls them; the registry head stays null; iteration yields **zero** entries. |
| **(b) the linker discards the section under release codegen** | **Ruled out.** Under `lto = "fat"`, `opt-level = "s"`, `codegen-units = 1` and `--gc-sections`, all five submissions survive — *even with no `KEEP` in the linker script*, because `#[used]` marks the section `SHF_GNU_RETAIN`. That is true on this repo's pinned toolchain and would not have been true before rustc 1.89 (§5). |

So the silent-corruption mode the ticket most feared is the one that is *not* in play, and
ADR-0024's reasoning that `inventory` "leans less on `--gc-sections` behavior" is **confirmed**.
What is stale is only the trigger condition written next to it.

Consequences, most useful first:

1. **The fallback stands, but its trigger is wrong on the record.** The trigger is not "the core
   goes `no_std`", it is "the target's startup does not run `.init_array`". An embedder told the
   wrong condition diagnoses the wrong thing. Corrections in §8.
2. **Upstream says so in its own words.** `inventory`'s README names its supported platforms and
   then: *"Beyond this, other platforms will simply find that no plugins have been registered."*
   The same README points at `linkme` as "a different approach … that does not involve
   life-before-main", and dtolnay has twice closed `no_std` requests by redirecting to `linkme`
   (§3). This is not a gap we discovered; it is the documented contract.
3. **There is a third option the record does not mention:** keep `inventory` and have the
   embedder's startup walk `.init_array` before it calls the engine. It links today at a cost of
   **36 bytes of `.text`** (§4, variant G). Whether that is a reasonable thing to *ask* of an
   embedder is a #734 judgement, not a fact this note settles.
4. **The existing dead-strip canary does not cover this target.** Both canaries in `registry.rs`
   are `#[cfg(test)]` host tests, and `cargo test` does not run on `thumbv7em-none-eabihf`. On the
   bare-metal target the empty registry is currently *undetected* — §6 describes the trap.
5. **`linkme`'s cost on this target is small and bounded, and upstream CI-tests it there.**
   `linkme` has a `thumbv7m-none-eabi` QEMU job asserting a slice's length. Its price is two
   hand-written linker-script lines *per slice*, named after the slice identifier — four lines for
   reuben's two registries (§7).

---

## 1. What `inventory` 0.3.24 actually does — OBSERVED

Read at `~/.cargo/registry/src/index.crates.io-*/inventory-0.3.24/src/lib.rs` (582 lines, the
whole crate; no `build.rs`). This is the version `Cargo.lock` pins:
`inventory 0.3.24`, checksum
`a4f0c30c76f2f4ccee3fe55a2435f691ca00c0e4bd87abe4f4a851b1d4dac39b`.

`#![no_std]` is real — line 151. It is also not the mechanism question.

Each `submit!` expands, via `__do_submit!` (lines 511–582), to **a static node plus a constructor
pointer in a platform init-array section**:

```rust
static __INVENTORY: Node = Node {
    value: &{ /* the submitted value */ },
    next: UnsafeCell::new(None),
};

unsafe extern "C" fn __ctor() {
    unsafe { ErasedNode::submit(__INVENTORY.value, &__INVENTORY) }
}

#[used]                                  // the default arm, lines 576-581
#[cfg_attr(
    all(not(target_family = "wasm"), any(
        target_os = "linux", /* … */ target_os = "none",
    )),
    link_section = ".init_array",
)]
static __CTOR: unsafe extern "C" fn() = __ctor;
```

Three things matter.

**The section holds function pointers, not values.** One pointer per `submit!`.

**`target_os = "none"` is on the `.init_array` allowlist** (line 548). So `inventory` does emit a
section on this target, and emits the *ELF hosted-platform* one — the same treatment as Linux.
There is no bare-metal-specific path and no object-format-driven path: the `cfg` tree branches
only on `target_os` and `target_family`, never on `target_arch`, `target_env`, or
`target_vendor`. `rustc --print cfg --target thumbv7em-none-eabihf` gives `target_os="none"`,
`target_vendor="unknown"`, and *no* `target_family` key at all, so
`not(target_family = "wasm")` is satisfied and the branch matches.

`target_os = "none"` was added in **0.3.15** (PR #70, commit `b273ebc72`, 2024-01-26). The
motivating use case in that thread was **seL4 userspace, not bare-metal Cortex-M** — and the PR
records why the mechanism is an OS allowlist rather than a format decision: Clang picks sections
from target-object-format information *"that a crate like `inventory` does not have structured
access to"*.

**The registry is a runtime-built intrusive linked list, not a link-time slice** (lines 174–263):

```rust
pub struct Registry { head: AtomicPtr<Node> }

unsafe fn submit(&'static self, new: &'static Node) {
    let mut head = self.head.load(Ordering::Relaxed);
    loop {
        unsafe { *new.next.get() = head.as_ref(); }
        let new_ptr = ptr::addr_of!(*new).cast_mut();
        match self.head.compare_exchange(head, new_ptr, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return,
            Err(prev) => head = prev,
        }
    }
}
```

with `iter` starting from `T::registry().head.load(Ordering::Acquire)` (line 332), and the
registry itself a plain `static` behind a `const fn new()` — no `Once`, no lazy init, no
allocator (`core` imports only, no `extern crate alloc`).

**INFERRED, then confirmed in §4:** because the list is *built by the constructors*, a target
where `.init_array` is never executed has a null head and an empty iteration — no matter how
perfectly the section survived the link. That is the whole finding.

Incidentally: `submit` needs pointer-width `compare_exchange` and `iter` an `Acquire` load.
ARMv7E-M has `ldrex`/`strex`, and the target advertises `target_has_atomic="ptr"`/`"32"`; no
64-bit atomic is used anywhere (`AtomicBool` appears only under `target_family = "wasm"`).
**OBSERVED:** the spike links with no `compiler_builtins` atomic-emulation fallback, and the
compiled iteration contains a real `dmb sy` (§4).

### The `no_std` history — the premise moved long before 0.3.24

**OBSERVED** (upstream release notes and tag diffs):

| Version | Date | What changed |
|---|---|---|
| 0.1.6 | 2020-04-08 | `#![no_std]` added (PR #16) — but *with* `extern crate alloc`. |
| 0.2.0 | 2021-11-10 | `alloc` dropped; submissions became const-constructible + `Sync`. Genuinely no-alloc from here. |
| 0.3.6 | 2023-05-15 | The `ctor` crate dependency replaced with inventory's own `link_section` attributes (PR #60). |
| 0.3.8 | 2023-07-03 | `no-std::no-alloc` added to the crate's own categories. |
| 0.3.15 | 2024-01-26 | `target_os = "none"` added to the `.init_array` branch (PR #70, seL4). |
| 0.3.24 | — | VxWorks target support (PR #89). Nothing about `no_std`. |

So the ticket's "0.3.24 is itself `#![no_std]`" is **true but misleadingly framed**: `no_std`
landed in 0.1.6, five years ago, and the crate has been no-alloc since 0.2.0. The stale comment
was arguably never accurate — the blocker was always life-before-main, which `no_std` never
addressed. `inventory` has never claimed otherwise; its README has always said so.

## 2. What `cortex-m-rt` 0.7.6 does at startup — OBSERVED

`grep -n 'init_array|__libc_init|__attribute__((constructor|preinit'` over
`cortex-m-rt-0.7.6/src/lib.rs` and `link.x.in`: **zero hits.** The runtime neither runs
`.init_array` nor mentions it, and defines no `__init_array_start`/`__init_array_end`.

Its generated `link.x` has output sections for `.vector_table`, `.text`, `.rodata`, `.data`,
`.gnu.sgstubs`, `.bss`, `.uninit`, `.got`, and a `/DISCARD/` for `.ARM.exidx`/`.ARM.extab`.
**`.init_array` appears in neither the placement list nor the discard list.**

The reset handler, disassembled out of a linked artifact
(`rust-objdump -d --disassemble-symbols=Reset`), does exactly four things:

```
08000400 <Reset>:
 8000400: bl   __pre_init
 8000404: …    zero .bss  (__sbss .. __ebss)
 8000412: …    copy .data from __sidata to .data
 8000422: …    CPACR |= 0xF00000   (enable the FPU)
 8000438: bl   main
```

No init-array walk, no `__libc_init_array`. **INFERRED:** any `inventory` submission is inert
under this runtime — and because `Registry::head` lands in `.bss`, which `Reset` zeroes, it is
inert as a *guaranteed null*, not as undefined behaviour.

## 3. Upstream's own posture — OBSERVED

- **The crate documents this failure.** `README.md`: *"Inventory is built on runtime
  initialization functions similar to `__attribute__((constructor))` in C … This registration
  happens dynamically as part of life-before-main for statically linked elements. … Platform
  support includes Linux, macOS, iOS, FreeBSD, Android, Windows, WebAssembly, and a few others.*
  **Beyond this, other platforms will simply find that no plugins have been registered.**"
- **And points at the alternative:** *"For a different approach to plugin registration that does
  not involve life-before-main, see the `linkme` crate."*
- **dtolnay has twice redirected `no_std` users to `linkme`.** Issue #26 ("no_std support",
  2020-11-27): *"I would prefer not to do this. The `linkme` crate is a more appropriate way to
  handle a no_std use case."* Issue #13 ("Rewrite to use data segments instead of ctor?"): *"I
  have an implementation of your approach already in … linkme … it may be a better fit for your
  use case than inventory."*
- **`inventory` has no embedded CI.** Its `ci.yml` matrix is ubuntu / macos / windows-gnu /
  windows-msvc, plus MSRV check, docs, clippy. No `--target`, no cross-compile, no QEMU, no
  `thumb*`.
- **No upstream issue mentions Cortex-M or `thumbv*`** across all 42 issues (5 open, 37 closed).
  The nearest bare-metal items are PR #66 (closed unmerged, "Add feature to force `.init_array`
  and `.text.startup`") and PR #70 (merged, seL4).

**INFERRED:** this target is outside `inventory`'s stated support envelope, and upstream's answer
for it is already on record as `linkme`. That does not settle #734 — the third option in §0 is
still available and cheap — but it does mean choosing `inventory` here would be running against
the grain of the crate's own documentation, untested by its CI.

## 4. The spike

Throwaway crate, kept entirely outside the reuben workspace (built under `/tmp`, never a
workspace member). One source file, built both for the host — as a control, since registration is
known to work there — and for the target.

It mirrors the shape of reuben's registries rather than a toy: a struct of **function pointers**
(`OpReg { name: &'static str, arity: fn() -> u32 }` — the same non-`const`-value reason
`registry.rs` uses fn pointers), one `collect!`, and **five `submit!`s at five separate module
"definition sites"**. Iteration computes three things and writes them through `write_volatile` to
three `#[no_mangle]` statics, so no profile can fold or drop them:

- `SPIKE_COUNT` — how many entries iteration yielded (expect 5)
- `SPIKE_ARITY_OR` — OR of every entry's `arity()`, powers of two, so **31 iff all five were
  seen**, and the set bits say *which*
- `SPIKE_NAME_HASH` — a hash over each entry's name, so a non-zero value proves the submitted
  *values* were reachable, not merely that some node count came back

Bare-metal side: `#![no_std]`, `#![no_main]`, `cortex-m-rt` 0.7.6 `#[entry]`, a `udf` panic
handler, `memory.x` with a generic 128K flash / 128K RAM Cortex-M7 layout. Release profile set to
the embedder's codegen: `lto = "fat"`, `opt-level = "s"`, `codegen-units = 1`. Toolchain pinned to
the repo's own `1.96.0`.

**Host control — OBSERVED, by running it:**

```
debug:                            host control: count=5 arity_or=31 name_hash=910379675
release (lto=fat, opt-level=s):   host control: count=5 arity_or=31 name_hash=3021659819
```

Five entries under both profiles, all five arity bits. The spike is a correct instrument. (The
`name_hash` differing between profiles is iteration order changing with link order — an
incidental confirmation that link order is not stable, which is exactly why `registry.rs` re-keys
through a `BTreeMap`.)

### 4.1 The target, seven ways — OBSERVED

`--gc-sections` is not optional here: rustc passes it to `rust-lld` for this target by default
(it appears in the captured link line of every build below).

**A, B — stock `cortex-m-rt` linker script: the link fails outright.** Debug **and** release
(`lto = "fat"`, `opt-level = "s"`) fail identically:

```
rust-lld: error: section .init_array virtual address range overlaps with .text
  >>> .init_array range is [0x8000400, 0x8000413]
  >>> .text range is [0x8000400, 0x8000DD7]
rust-lld: error: section .init_array load address range overlaps with .text
```

`.init_array` is placed by no output-section rule and discarded by none (§2), so it becomes an
orphan and lld drops it at the start of FLASH, on top of `.text`. **No artifact is produced.**
`0x8000413 - 0x8000400` is 20 bytes = five 4-byte function pointers: all five submissions are
present at the moment the link fails.

*(This corrects a natural guess: one might expect lld to quietly park the orphan somewhere
harmless. On this linker script it does not — it collides and the build stops. Loud, not silent.)*

**C–F — the section placed by the embedder: it survives everything.** The natural fix is to add
an output section. Two variants, each at both profiles: **with** `KEEP(*(.init_array))` and
**without**.

| Variant | Profile | `KEEP`? | Links | `.init_array` size | `__CTOR` syms | All 5 name strings |
|---|---|---|---|---|---|---|
| A | debug | *(unplaced)* | **no** | — | — | — |
| B | release | *(unplaced)* | **no** | — | — | — |
| C | debug | yes | yes | **0x14 = 20 B = 5 ptrs** | 5 | present |
| D | release | yes | yes | **0x14 = 20 B = 5 ptrs** | 5 | present |
| E | debug | **no** | yes | **0x14 = 20 B = 5 ptrs** | 5 | present |
| F | release | **no** | yes | **0x14 = 20 B = 5 ptrs** | 5 | present |
| G | release | yes | yes | **0x14 = 20 B = 5 ptrs** | 5 | present |

`readelf -SW` on variant D (release, `lto = "fat"`, `opt-level = "s"`) says why the no-`KEEP`
column is not a typo:

```
  [Nr] Name          Type         Addr     Off    Size   ES Flg
  [ 2] .text         PROGBITS     08000400 010400 0001d0 00  AX
  [ 3] .rodata       PROGBITS     080005d0 0105d0 000098 00   A
  [ 4] .init_array   INIT_ARRAY   08000668 010668 000014 00  AR
  [ 5] .data         PROGBITS     20000000 020000 000048 00  WA
  [ 7] .bss          NOBITS       20000048 030048 000004 00  WA
```

Flag **`R` is `SHF_GNU_RETAIN`** (readelf's own key: "R (retain)"), emitted because `inventory`'s
macro puts `#[used]` on `__CTOR`. The linker is instructed not to garbage-collect it, so the
script's `KEEP` is belt to that braces. **Failure mode (b) is ruled out by observation, and the
mechanism that rules it out is identified** — see §5 for the toolchain-version caveat on that.

`rust-nm --print-size` on the same artifact places every piece:

```
08000668  4  r  spike::op_a::_::__CTOR      ← .init_array, 5 × 4 B
0800066c  4  r  spike::op_b::_::__CTOR
08000670  4  r  spike::op_c::_::__CTOR
08000674  4  r  spike::op_d::_::__CTOR
08000678  4  r  spike::op_e::_::__CTOR
2000000c 12  d  spike::op_a::_::__INVENTORY ← .data, 5 × 12 B  (the Node, mutated in place)
…
20000048  4  b  <OpReg as Collect>::registry::REGISTRY  ← .bss: the list head
```

### 4.2 The proof that the constructors never run

Two independent observations on variant D, the release build at the embedder's codegen.

**One — no code materialises the section's start address.** The only instruction anywhere in the
image carrying the value `0x0800067c` is the literal at `0x8000450`, inside `Reset`, which is
`__sidata` (the load address of `.data`) — numerically equal to `__einit_array` only because
`.data`'s LMA sits immediately after `.init_array`. `__sinit_array` (`0x08000668`) is referenced
by **nothing**.

**Two — the compiled iteration has a null-head fast path, and it is the only reachable one:**

```
 800049a: movw r0, #0x48 ; movt r0, #0x2000   ← &REGISTRY = 0x20000048, in .bss
 80004a2: ldr.w r8, [r0]                      ← head.load(Acquire)
 80004a6: dmb  sy
 80004aa: cmp.w r8, #0x0
 80004ae: beq  0x80004e8                      ← taken: head is null
 …
 80004e8: movs r4, #0x0 ; mov.w r9, #0x0 ; movs r6, #0x0
 80004f0: …  str r6 -> SPIKE_COUNT
 80004fa: …  str r9 -> SPIKE_ARITY_OR
 8000506: …  str r4 -> SPIKE_NAME_HASH
 8000510: nop ; b .-6
```

`REGISTRY` is in `.bss`, `Reset` zeroes `.bss`, and no code writes it because no code calls the
constructors. The branch at `0x80004ae` is therefore taken unconditionally, and the three statics
are stored as `0`, `0`, `0`.

**This is failure mode (a), and it is established statically rather than by running the image.**
The count is a genuinely runtime quantity — the section's contents are a link-time fact the
optimiser cannot fold — so the honest static form of "iteration yields zero" is exactly this: the
head is provably null at `main`, and the zero-count path is the only path out.

### 4.3 G — an embedder that runs `.init_array`

Variant D plus a hand-rolled constructor walk at the top of the entry point, the way a hosted
libc's crt does it, over `__sinit_array`/`__einit_array` symbols the linker script provides. Links
clean at release codegen. `.text` grows `0x1d0 → 0x1f4`, **36 bytes**, and the loop is in the
disassembly, this time materialising *both* bounds:

```
 800049a: movw r4, #0x6a0 ; movt r4, #0x800   ← __einit_array = 0x080006a0
 800049e: movw r0, #0x68c ; movt r0, #0x800   ← __sinit_array = 0x0800068c
 80004aa: cmp  r0, r4
 80004ac: bhs  …                              ← skip if empty
 80004ae: movw r5, #0x68c ; movt r5, #0x800
 80004b6: ldr  r0, [r5], #4                   ← next ctor pointer
 80004ba: blx  r0                             ← call it
 80004bc: cmp  r5, r4
 80004be: blo  0x80004b6
 80004c0: … then the same sweep as D
```

**OBSERVED:** it links, the walk is present, it costs 36 bytes.
**INFERRED (not confirmed on hardware):** executing it would submit all five nodes before the
sweep, which would then yield 5 / 31. `blx` through a Rust `unsafe extern "C" fn()` value handles
the Thumb bit without manual masking, so no interworking hazard.

## 5. Why failure mode (b) is ruled out — and the version floor under that

**OBSERVED** — the Rust Reference is explicit that `#[used]` alone is historically not a link-time
guarantee (`abi.used.intro`):

> "The `used` attribute forces a static to be kept in the output object file … even if it's never
> used or referenced by any other item in the crate. **The linker, however, is still free to
> remove it.**"

**OBSERVED** — that gap was closed at the compiler level: rust-lang/rust **PR #140872, "Make
`#[used(linker)]` the default on ELF too"**, merged 2025-06-06, milestone **1.89.0**. Which is
why §4's `readelf` shows `SHF_GNU_RETAIN` from a plain `#[used]`.

**INFERRED, and it matters for #734:** the "(b) is ruled out" result is a property of the
*toolchain*, not only of `inventory`. This repo pins 1.96.0, comfortably past 1.89, and
`rust-lld`'s bundled LLD is far past the LLD 13 / binutils 2.36 floor for honouring
`SHF_GNU_RETAIN`. But a rollback of the toolchain pin below 1.89 would put mode (b) back on the
table — for `inventory` *and* for `linkme`, which leans on it harder (§7). Worth naming as a
constraint rather than assuming it away.

## 6. Why this is a trap worth writing down — INFERRED

The failure sequence an embedder walks:

1. Build with a stock `cortex-m-rt` linker script → **loud link error** (§4.1 A/B). Good: nothing
   silent yet.
2. Fix the error the obvious way, by giving `.init_array` an output section → **links clean, and
   the registry is silently empty.** Every instrument fails to load, for a reason that points
   nowhere near the linker script that "fixed" it.

Step 2 is the expensive one, and the repo's protection against exactly this is `registry.rs`'s two
canaries — `builtin_is_nonempty` and `builtin_contains_the_load_bearing_ops`. Both are
`#[cfg(test)]`, and `cargo test` does not run on `thumbv7em-none-eabihf`. **On the bare-metal
target the canary is absent.** Whatever #734 decides about the mechanism, the canary needs a form
that holds *on the target* — a build-time or startup-time assertion, or a test run under emulation
— or the loud red test the ADR relies on is a host-only guarantee. `linkme`'s own upstream reached
the same conclusion and answered it with a QEMU CI job (§7).

Second, smaller input to #734: `inventory` costs **12 bytes of writable RAM per registered entry**
(the `Node`, whose `next` is mutated in place — observed in §4.1's `nm` output) plus 4 bytes per
registry for the head. Those nodes *cannot* live in flash. `linkme` puts its table in read-only
flash and needs no RAM and no startup step. On a target where SRAM is the scarce resource and
flash is not, that is a real difference independent of the startup question.

## 7. What `linkme` would cost here — OBSERVED, but unspiked

`linkme` was **not** spiked; these are primary-source facts about it, not measurements.

- **Mechanism, and no life-before-main.** `README.md`: *"The implementation is based on
  `link_section` attributes and platform-specific linker support. **It does not involve
  life-before-main or any other runtime initialization on any platform.** This is a zero-cost safe
  abstraction that operates entirely during compilation and linking."* `src/lib.rs:136` is
  `#![no_std]`. Latest release 0.3.37 (2026-07-18); not vendored on this machine.
- **`target_os = "none"` is first-class in its ELF branch** (`impl/src/declaration.rs:136-158`) —
  listed *first*, ahead of linux.
- **Sections and encapsulation symbols.** On ELF: elements in `linkme_{IDENT}`, bounded by
  `__start_linkme_{IDENT}` / `__stop_linkme_{IDENT}`, plus a parallel `linkm2_*` section for
  duplicate detection.
- **It is CI-tested on Cortex-M.** `.github/workflows/ci.yml` has a `cortex` job: nightly,
  `target: thumbv7m-none-eabi`, installs `qemu-system-arm`, runs `cargo run --release` and
  `cargo run --release --features used_linker` in `tests/cortex` with
  `RUSTFLAGS: -C link-arg=-Tlink.x`. The test asserts `SHENANIGANS.len() == 3` under QEMU against
  `cortex-m-rt` 0.7 — i.e. the exact assertion this note could not make for `inventory`.
- **The price is per-slice linker-script lines.** `tests/cortex/memory.x`:

  ```
  SECTIONS {
    linkme_SHENANIGANS : { *(linkme_SHENANIGANS) } > FLASH
    linkm2_SHENANIGANS : { *(linkm2_SHENANIGANS) } > FLASH
    linkme_EMPTY : { *(linkme_EMPTY) } > FLASH
    linkm2_EMPTY : { *(linkm2_EMPTY) } > FLASH
  }
  INSERT AFTER .rodata
  ```

  Two lines per slice, named after the slice identifier, hand-maintained. **INFERRED:** reuben has
  two slices (`OpReg`, `OscForm`), so four lines — bounded, and it does not grow with the number of
  operators. Note there is no `KEEP()`; retention rides on `#[used]` as in §5.
- **Its `--gc-sections` exposure is the sharper one, and is the same 1.89 story.** linkme issue #49
  ("The encapsulation symbol needs to be retained under `--gc-sections` properly") is still open:
  lld defaults to eager `-z start-stop-gc`, which *"will remove all the element sections, since
  they are not referenced by any code, but only accessed via the encapsulation symbols"*. The
  recommended fixes are `-Wl,-znostart-stop-gc` or the `used_linker` feature, and the thread
  concludes *"I would consider this issue fixed as soon as `#[used]` becomes `#[used(linker)]` on
  ELF"* — which 1.89 did, confirmed in-thread as reproducible on 1.87.0 and not on 1.89.0-beta.1.
- **And this class of silent failure has bitten `linkme` on this target family before.** Issue #40,
  "Cortex-M build is failing: 0 vs 3" (closed) — elements vanished and the slice length came back
  0 instead of 3, bisected to a rustc change.

**INFERRED:** the swap is not a free upgrade in robustness. It trades a *startup* dependency for a
*linker-script and section-GC* dependency, which is precisely ADR-0024's original objection. What
has changed since is that rustc 1.89 removed most of the teeth from the GC half, and that upstream
`linkme` now tests the target family under emulation while `inventory` does not test it at all.

## 8. What is not confirmed

- **No hardware run and no emulator.** `qemu-system-arm` is not installed on this machine, so no
  variant was executed on the target. Every target-side claim is read off a **linked ELF** produced
  today — section tables, symbol tables, disassembly — not off a running image.
- The **negative** claim is strong under that limit: the head is in `.bss`, `Reset` zeroes `.bss`,
  no instruction materialises `__sinit_array`, and the disassembled sweep's null-head branch stores
  zeroes. Little room is left for it to be wrong. It also agrees with `inventory`'s own README.
- The **positive** claim about variant G is weaker: it links and the walk is in the image, but
  *that it yields 5 at runtime* is inference from the host control plus the same code path. **Do
  not quote variant G as a proven fix.** Installing `qemu-system-arm` would close it in minutes,
  and would close the `linkme` half too.
- **`linkme` was not spiked here at all.** §7 is documentation and upstream CI config, not
  measurement. Nothing above is evidence that `linkme` works in *this* repo on *this* target.
- Only `cortex-m-rt` 0.7.6 was tested as the startup layer. A different runtime, or a
  vendor-supplied C startup that *does* run `.init_array`, changes the answer — which is precisely
  why §9 rewrites the trigger as a property of the *startup*, not of `no_std`.
- The mode-(b) result carries the rustc ≥ 1.89 floor from §5.

## 9. The two stale comments, and what they should say

Exactly two places in the repo carry the `no_std`-trigger claim.

**`Cargo.toml:29`** — currently:

```toml
# Compile-time operator self-registration. linkme is the no_std fallback.
inventory = "0.3"
```

Proposed:

```toml
# Compile-time operator self-registration. see rules: composition-operators
inventory = "0.3"
```

The `linkme` sentence is a *tradeoff under a condition* — rationale by the repo's own comment
rule, so it belongs in the rules tree rather than restated at a dependency line where it went
stale. Pointing at the topic keeps it findable and keeps one copy.

**`docs/rules/rationale/composition-operators/operator-self-registration.md:25–26`** —
currently:

> a silent gap into a loud red test. `inventory` was chosen over `linkme` because it leans less on
> `--gc-sections` behavior; `linkme` is the documented fallback if the core ever goes `no_std`.

Proposed:

> a silent gap into a loud red test — though both canaries are host tests, so a target that cannot
> run them is covered by neither. `inventory` was chosen over `linkme` because it leans less on
> `--gc-sections` behavior, and that has held: `#[used]` marks its `.init_array` `SHF_GNU_RETAIN`,
> and every submission survives `lto = "fat"` + `opt-level = "s"` + `--gc-sections` on the pinned
> toolchain. The fallback stands, but **not** for the reason first written down. The trigger is not
> the core going `no_std` — `inventory` has been `#![no_std]` since 0.1.6 and no-alloc since 0.2.0
> — it is a target whose startup never runs ELF `.init_array`, which is how `inventory` submits.
> On such a target the submissions sit in the image, nothing calls them, and iteration yields
> nothing; `inventory`'s own README says as much. The escape is either `linkme`'s link-time table
> or an embedder startup that walks `.init_array` before it touches the engine.

Note what does **not** change: the `--gc-sections` clause. That half of the original reasoning was
tested and is correct. Only the trigger condition was wrong.

## 10. Reproducing

The spike is throwaway and deliberately not in the workspace. To rebuild it:

- one crate, `inventory = "0.3.24"`, plus `cortex-m` / `cortex-m-rt` behind
  `[target.'cfg(target_os = "none")'.dependencies]`
- `[profile.release]` `lto = "fat"`, `opt-level = "s"`, `codegen-units = 1`, `debug = true`
- `#![cfg_attr(target_os = "none", no_std)]` + `no_main`; one `collect!`, five `submit!`s in five
  modules; results written via `write_volatile` to three `#[no_mangle]` statics
- pin the toolchain to this repo's channel *and* add the target — a `rust-toolchain.toml` with
  `targets = ["thumbv7em-none-eabihf"]`, or `rustup target add thumbv7em-none-eabihf` against the
  pinned toolchain rather than the default one
- `RUSTFLAGS="-C link-arg=-Tlink.x"` for the stock variants, plus
  `-C link-arg=-Tinit-array-keep.x` (or `…-nokeep.x`) for the placed ones, where that script is
  `SECTIONS { .init_array : ALIGN(4) { __sinit_array = .; KEEP(*(.init_array));
  KEEP(*(.init_array.*)); __einit_array = .; } > FLASH } INSERT AFTER .rodata;`
- inspect with `readelf -SW`, `rust-nm -C --print-size`, and
  `rust-objdump -d --disassemble-symbols=<sym>`

**Versions used:** rustc 1.96.0 (ac68faa20 2026-05-25), this repo's pinned toolchain ·
`inventory` 0.3.24 · `cortex-m-rt` 0.7.6 · `cortex-m` 0.7.8 · linker `rust-lld -flavor gnu`.
