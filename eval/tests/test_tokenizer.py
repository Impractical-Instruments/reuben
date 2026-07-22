"""Differential test: the stdlib encoder must agree with real `tiktoken`, token for token.

**Never runs in the gate job.** `tiktoken` is a Rust extension with two transitive deps that
downloads its vocabulary over the network at first use — exactly what `eval/reuben_eval/tokenizer/`
exists to avoid on a CI runner. It lives in the `dev` extra and is skipped when absent, so
`python3 -m unittest` still passes on a bare interpreter.

    pip install -e 'eval[dev]' && python3 -m unittest discover eval/tests

The corpus is this repo's own text, because that is what the gate actually tokenizes: instrument
JSON, tool descriptions, and the authoring guide. Plus a seeded Unicode fuzz, because the whole
reason `unicode_classes.py` is frozen is that the character tables are where a silent drift would
hide.
"""

from __future__ import annotations

import random
import unittest
from pathlib import Path

from reuben_eval.tokenizer import cl100k

try:
    import tiktoken

    REFERENCE = tiktoken.get_encoding("cl100k_base")
except Exception:  # pragma: no cover - the whole module is skipped
    REFERENCE = None

REPO = Path(__file__).resolve().parent.parent.parent


@unittest.skipIf(REFERENCE is None, "tiktoken not installed (dev extra) — differential test skipped")
class TestAgainstTiktoken(unittest.TestCase):
    def assert_same(self, text: str, label: str) -> None:
        self.assertEqual(cl100k.encode(text), REFERENCE.encode_ordinary(text), f"mismatch in {label}")

    def test_repo_corpus(self) -> None:
        """Markdown, Rust and instrument JSON — the shapes the gate meets in practice."""
        corpus = [
            *sorted(REPO.glob("docs/**/*.md")),
            *sorted(REPO.glob("instruments/**/*.json")),
            *sorted(REPO.glob("crates/**/*.rs"))[:150],
        ]
        self.assertGreater(len(corpus), 50, "corpus went missing — the test would pass vacuously")
        total = 0
        for path in corpus:
            text = path.read_text(encoding="utf-8", errors="strict")
            self.assert_same(text, str(path.relative_to(REPO)))
            total += len(REFERENCE.encode_ordinary(text))
        self.assertGreater(total, 100_000, f"only {total} tokens compared")

    def test_unicode_fuzz(self) -> None:
        """Seeded random Unicode, surrogates excluded — where a drifted `\\p{L}` table would show."""
        rng = random.Random(0x5EED)
        for _ in range(3000):
            text = "".join(
                chr(cp)
                for cp in (rng.randint(1, 0x2FFFF) for _ in range(rng.randint(1, 40)))
                if not 0xD800 <= cp <= 0xDFFF
            )
            self.assert_same(text, repr(text))

    def test_json_and_whitespace_shapes(self) -> None:
        """Indentation runs and punctuation clusters — the payload the gate is built to watch."""
        for text in (
            '{\n  "instrument": "x",\n  "nodes": [\n    {"node": "/osc", "kind": "sine"}\n  ]\n}',
            "\n\n\n\t\t  \r\n  ",
            "/voice1/cutoff -0.5 1e-9 [1,2,3]",
            "don't we've it'll they're",
        ):
            self.assert_same(text, repr(text))


class TestPins(unittest.TestCase):
    """The pins are the point: without them a silent artifact change resets the trend."""

    def test_pins_verify(self) -> None:
        pins = cl100k.verify_pins()
        self.assertEqual(
            pins["cl100k_base.tiktoken"],
            "223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7",
            "the vendored vocab must stay the file tiktoken itself pins",
        )

    def test_pin_mismatch_is_fatal(self) -> None:
        original = cl100k._VOCAB.read_bytes()
        try:
            cl100k._VOCAB.write_bytes(original + b"\n")
            with self.assertRaises(cl100k.PinMismatch):
                cl100k.verify_pins()
        finally:
            cl100k._VOCAB.write_bytes(original)


if __name__ == "__main__":
    unittest.main()
