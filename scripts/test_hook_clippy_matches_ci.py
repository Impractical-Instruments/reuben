"""The pre-push clippy check runs CI's main clippy command, flags included.

Text comparison, not a YAML parse: the runner's python3 has no YAML library, and the only thing
compared is one command line. CI's main clippy step is the one `run: cargo clippy` that names no
`-p` package; the editor job and the bare-metal pair all scope themselves with `-p` and stay
CI-only by design.
"""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HOOK = ROOT / "scripts" / "hooks" / "pre-push.d" / "10-rust-clippy"
CI = ROOT / ".github" / "workflows" / "ci.yml"


def ci_main_clippy():
    commands = [
        m.group(1).strip()
        for m in re.finditer(r"^\s*run:\s*(cargo clippy\b.*)$", CI.read_text(), re.M)
    ]
    unscoped = [c for c in commands if not re.search(r"(^|\s)-p\s", c)]
    return commands, unscoped


class TestHookClippyMatchesCi(unittest.TestCase):

    def test_ci_has_exactly_one_unscoped_clippy_step(self):
        commands, unscoped = ci_main_clippy()
        self.assertTrue(commands, f"no `run: cargo clippy` step found in {CI}")
        self.assertEqual(len(unscoped), 1, f"expected one unscoped clippy step, got {unscoped}")

    def test_the_hook_runs_and_prints_cis_command(self):
        _, unscoped = ci_main_clippy()
        hook = HOOK.read_text()
        invoked = re.findall(r"^if ! (cargo clippy\b.*?);\s*then$", hook, re.M)
        hinted = re.findall(r"Inspect:\s*(cargo clippy\b[^\"]*)\"", hook)
        self.assertEqual(invoked, unscoped, "the hook's clippy invocation drifted from CI's")
        self.assertEqual(hinted, unscoped, "the hook's Inspect hint drifted from CI's")


if __name__ == "__main__":
    unittest.main()
