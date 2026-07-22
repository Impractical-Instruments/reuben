#!/usr/bin/env python3
r"""Sample-alias guard — keep the engine's audio element type named in exactly one place.

`crates/reuben-core/src/sample.rs` is the single naming site: it declares `Sample`, `AudioBuffer`
(`&[Sample]`), and `AudioBufferMut` (`&mut [Sample]`). Every genuinely-audio buffer in the render
spine flows through those aliases. This guard fails if a raw `f32` slice (`&[f32]`, `&mut [f32]`,
any `[f32]` slice form) or a raw `f32` vector (`Vec<f32>`) appears anywhere OUTSIDE the naming site
and an explicit, justified allowlist. Fixed-size arrays (`[f32; N]`) and scalars (`f32`) are NOT
flagged — only the buffer forms the alias exists to name.

Why a text linter and not clippy's `disallowed-types`: a primitive slice/Vec of a primitive has no
nameable type *path* to disallow (`[f32]` is not a nominal type). The naming discipline is therefore
enforced textually, in the same spirit as `scripts/check_rules_refs.py` (walk the tree, skip build
dirs, exit non-zero on any violation, stdlib only).

The allowlist is deliberately tight. Each entry is a site where the `f32` is legitimately *not* a
logical audio buffer — a device-native frame, decoded resource data, per-sample DSP arithmetic, a
scalar-capture pool — or a test/bench harness that fabricates raw fixtures. The partition it draws
(audio vs. incidental `f32`) is the point of the exercise, so every entry carries its reason.

Exit non-zero on any violation. Stdlib only. Runs unconditionally in CI (it can catch a raw buffer
in ANY code change, like the reference-linter's always-on `ref-guard`).

Usage: python3 scripts/check_sample_alias.py [root=.]
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

# Same code surface + build-dir skips as the reference-linter.
CODE_EXTS = {".rs", ".py", ".mjs", ".js", ".ts", ".jsx", ".tsx", ".go", ".c", ".h",
             ".cpp", ".hpp", ".java", ".rb", ".sh", ".toml", ".yml", ".yaml"}
SKIP_DIRS = {".git", "target", "node_modules", "dist", "build", "engine"}

# The one place the audio element type is named — exempt by definition.
NAMING_SITE = "crates/reuben-core/src/sample.rs"

# Justified allowlist: paths (files, or dir prefixes ending in "/") where a raw `f32` buffer/Vec is
# legitimate. Kept tight — each entry names *why* the `f32` there is not a logical audio buffer that
# should adopt the alias. A relative POSIX path matches if it equals an entry or begins with a "/"
# dir-prefix entry.
ALLOWLIST = (
    # --- Device layer: OS/cpal-native interleaved frames, not the engine's logical audio buffer.
    "crates/reuben-native/src/audio.rs",   # output callback + device/output-map staging
    "crates/reuben-native/src/input.rs",   # capture ring buffer + resampler; device frames
    # --- Resource decode: audio files decode to planar per-channel `Vec<Vec<f32>>` before they
    #     ever become engine buffers.
    "crates/reuben-native/src/resources.rs",  # symphonia decode -> planar channels
    "crates/reuben-core/src/resources.rs",    # SampleBuffer holds decoded per-channel data
    # --- Operator DSP arithmetic: inside a `process`/kernel the `f32` is the number under the
    #     math (and per-operator unit tests fabricate raw input/expected buffers).
    "crates/reuben-core/src/operators/",       # the whole operator set + their inline tests
    "crates/reuben-core/src/dsp/",             # shared DSP kernels (SVF, ...)
    "crates/reuben-core/src/operator/shell.rs",  # SampleAt: the operand-shape (block vs scalar) helper
    # --- Coordinator / engine <-> device boundary: fill/ramp/duplex staging buffers that marshal
    #     the rendered master out to (and device audio in from) the OS. Genuinely audio, but this
    #     is the device seam, not the render spine; a follow-up may alias it (see PR Findings).
    "crates/reuben-core/src/engine.rs",
    "crates/reuben-core/src/coordinator/",
    # --- Incidental / domain-specific f32 vectors, not audio streams:
    "crates/reuben-core/src/plan.rs",       # `captured`: a snapshot pool of scalar interface Values
    "crates/reuben-core/src/wavetable.rs",  # a precomputed oscillator table's samples
    # --- Bench/test harnesses: fabricate raw fixtures; not shipped audio API surface.
    "crates/reuben-core/src/bench_support.rs",
    "crates/reuben-core/benches/",
    "crates/reuben-core/tests/",
    "crates/reuben-native/tests/",
    # --- The guard itself + its test literally name the forbidden forms in fixtures/messages.
    "scripts/check_sample_alias.py",
    "scripts/test_check_sample_alias.py",
)

# The two buffer forms the alias replaces. Written with `\s*` so this script's own source does not
# contain the bare literals it hunts for (so the guard never flags itself). A trailing `]` right
# after the element excludes fixed-size arrays like `[f32; 32]`.
SLICE_RE = re.compile(r"\[\s*f32\s*\]")
VEC_RE = re.compile(r"\bVec\s*<\s*f32\s*>")


def _allowed(rel: str) -> bool:
    """True when `rel` (a root-relative POSIX path) is the naming site or under the allowlist."""
    if rel == NAMING_SITE:
        return True
    for entry in ALLOWLIST:
        if entry.endswith("/"):
            if rel.startswith(entry):
                return True
        elif rel == entry:
            return True
    return False


def collect_problems(root_arg: str = ".") -> list[str]:
    """Every `file:line: message` violation under `root_arg`, in walk order."""
    root = Path(root_arg).resolve()
    problems: list[str] = []
    for path in root.rglob("*"):
        if not path.is_file() or path.suffix not in CODE_EXTS:
            continue
        parts = set(path.relative_to(root).parts)
        if parts & SKIP_DIRS:
            continue
        rel = path.relative_to(root).as_posix()
        if _allowed(rel):
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        for i, line in enumerate(text.splitlines(), 1):
            if SLICE_RE.search(line):
                problems.append(
                    f"{rel}:{i}: raw f32 slice — use AudioBuffer/AudioBufferMut from "
                    f"reuben_core::sample (or add a justified allowlist entry)"
                )
            if VEC_RE.search(line):
                problems.append(
                    f"{rel}:{i}: raw Vec<f32> — use Vec<Sample> from reuben_core::sample "
                    f"(or add a justified allowlist entry)"
                )
    return problems


def main(root_arg: str = ".") -> int:
    problems = collect_problems(root_arg)
    for p in problems:
        print(p, file=sys.stderr)
    print(f"check_sample_alias: {len(problems)} problem(s)", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:2]))
