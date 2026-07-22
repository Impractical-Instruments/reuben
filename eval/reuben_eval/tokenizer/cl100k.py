"""A stdlib-only `cl100k_base` encoder — the gate's size proxy. see rules: agent-mcp

This exists so `.github/scripts/` stays stdlib-only (the same rule `bench-dashboard.py` states as
"the runner's system python3 is the whole toolchain"). `tiktoken` would be a Rust extension plus two
transitive deps, and by default it *downloads* the vocabulary at first use — a network fetch inside
a gate job.

**The number this produces is a size proxy, not a billing figure.** It is pinned to cl100k_base
precisely because no rung on the live-tier ladder uses it: the deterministic tier must stay
comparable across years and must not re-baseline when a rung is re-pinned. Both the vocabulary and
the `\\p{L}`/`\\p{N}` tables are hash-checked before a single token is counted — see `pins.json`
and `docs/research/harness-rungs.md` §6.

Correctness is held by `tests/test_tokenizer.py`, which differentially tests this against real
`tiktoken` over the repo's own corpus. That test needs the `dev` extra and never runs in the gate.
"""

from __future__ import annotations

import base64
import functools
import hashlib
import json
import re
import sys
from pathlib import Path

from . import unicode_classes

_HERE = Path(__file__).resolve().parent
_VOCAB = _HERE / "cl100k_base.tiktoken"
_CLASSES = _HERE / "unicode_classes.py"
_PINS = _HERE / "pins.json"

# Possessive quantifiers (`++`, `?+`, `{m,n}+`) landed in Python 3.11. Without them this pattern
# still compiles but backtracks differently, and the split — hence every token count — changes.
MIN_PYTHON = (3, 11)

# The cl100k_base split pattern from `tiktoken_ext/openai_public.py`, with the two Unicode property
# escapes stdlib `re` lacks substituted for the frozen tables. Everything else is verbatim.
_PATTERN_TEMPLATE = (
    r"'(?i:[sdmt]|ll|ve|re)"
    r"|[^\r\n{L}{N}]?+[{L}]++"
    r"|[{N}]{{1,3}}+"
    r"| ?[^\s{L}{N}]++[\r\n]*+"
    r"|\s++$"
    r"|\s*[\r\n]"
    r"|\s+(?!\S)"
    r"|\s"
)


class PinMismatch(RuntimeError):
    """A vendored tokenizer artifact does not match its recorded sha256.

    Fatal by design: a silently-changed vocabulary or character table re-tokenizes every payload and
    resets the trend on the history branch with nothing in the diff to explain it.
    """


def _sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_pins() -> dict[str, str]:
    """Assert both vendored artifacts match `pins.json`. Returns the pins for the run record."""
    if sys.version_info < MIN_PYTHON:
        raise RuntimeError(
            f"cl100k needs Python >= {'.'.join(map(str, MIN_PYTHON))} for possessive quantifiers; "
            f"got {sys.version.split()[0]}"
        )
    pins = json.loads(_PINS.read_text(encoding="utf-8"))
    for name, path in (("cl100k_base.tiktoken", _VOCAB), ("unicode_classes.py", _CLASSES)):
        expected = pins[name]["sha256"]
        actual = _sha256(path)
        if actual != expected:
            raise PinMismatch(
                f"{path.name}: expected sha256 {expected}, got {actual}. "
                "If this change is deliberate, update pins.json and say so in the PR — it resets "
                "the eval trend."
            )
    return {name: meta["sha256"] for name, meta in pins.items()}


@functools.cache
def _splitter() -> re.Pattern[str]:
    return re.compile(
        _PATTERN_TEMPLATE.format(L=unicode_classes.LETTER, N=unicode_classes.NUMBER)
    )


@functools.cache
def _ranks() -> dict[bytes, int]:
    """The vendored BPE vocabulary: `<base64 token> <rank>` per line."""
    ranks: dict[bytes, int] = {}
    with _VOCAB.open("rb") as handle:
        for line in handle:
            if not line.strip():
                continue
            token, rank = line.split()
            ranks[base64.b64decode(token)] = int(rank)
    return ranks


@functools.cache
def _merge(piece: bytes) -> tuple[int, ...]:
    """Byte-pair-merge one pre-token into ranks.

    Repeatedly merges the lowest-ranked adjacent pair, which is the BPE definition. Quadratic in the
    piece length, but pre-tokens are words — and the gate tokenizes a handful of documents per run,
    where the research measured roughly a second per million tokens. Cached because instrument JSON
    is overwhelmingly the same few pieces (`"node"`, `"kind"`, indentation) over and over.
    """
    ranks = _ranks()
    direct = ranks.get(piece)
    if direct is not None:
        return (direct,)

    parts = [bytes([b]) for b in piece]
    while len(parts) > 1:
        best_index = -1
        best_rank = None
        for index in range(len(parts) - 1):
            rank = ranks.get(parts[index] + parts[index + 1])
            if rank is not None and (best_rank is None or rank < best_rank):
                best_index, best_rank = index, rank
        if best_rank is None:
            break
        parts[best_index : best_index + 2] = [parts[best_index] + parts[best_index + 1]]

    # Every single byte is in the vocabulary, so an unmerged remainder still resolves.
    return tuple(ranks[part] for part in parts)


def encode(text: str) -> list[int]:
    """Token ids for `text`, equivalent to `tiktoken.encode_ordinary`.

    Ordinary encoding only: `<|endoftext|>` and friends are treated as literal text. The harness
    counts payloads and prose, never a chat template's control tokens.
    """
    verify_pins()
    tokens: list[int] = []
    for piece in _splitter().findall(text):
        tokens.extend(_merge(piece.encode("utf-8")))
    return tokens


def count(text: str) -> int:
    """Token count for `text` — the only thing the gate actually needs."""
    return len(encode(text))
