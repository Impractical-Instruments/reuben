# Why: `reuben-core` is named by `reuben-api` alone: no other manifest in the workspace declares a dependency on it, dev-dependencies included, and a guard that reads the renamed `package` field as well as the key keeps it that way.

[Rule](../../web-product-process.md#core-is-private-to-the-window)

"Every consumer goes through the window" is the claim the whole boundary rests on, and Rust has no
keyword for it. There is no crate-level visibility, so *private* here cannot mean anything except
**removed from every other crate's manifest** — a crate that cannot name the engine cannot reach past
the window by accident. The guard is that sentence, checked. It runs unconditionally in CI, like the
reference-linter and the sample-alias guard, because a reintroduced dependency edge is one manifest
line and can arrive in any change.

**Dev-dependencies count, and that is the load-bearing part.** Every crate's `src/` stopped naming the
engine phases before its *tests* did. A test that reaches around the window is a report that the window
is missing something, and letting tests keep their own path is exactly how that report stops being
filed. Rewriting the native integration tests to install through the window and drive a render slot —
which is what a device does — made them prove what a device would *play* rather than what the load path
returns.

**A key match is not the check.** Cargo's rename form declares the same edge under a name of the
author's choosing, in one line, and is invisible to a guard that reads dependency keys. So the guard
reads the `package` field too. A guard that *is* the claim has to be checked against the spellings that
would defeat it, not only the one that announces itself. What it deliberately does not flag is the
source *path* — the operator scaffold writes files into the engine's tree — along with doc references
and prose, which name a directory or an idea rather than a build edge.

**The other way to lose a test leaves no edge for any guard to catch**: quietly lowering its claim. One
migrated test had computed its expected content hash by calling the engine directly; replacing that
with a comparison between two *served* responses proves only that the channel agrees with itself, and
would pass a channel that reported the wrong document's hash consistently. It installs a second time
through the window and reads the installed hash instead — off the wire under test, and independent of
it. Migrating a test that reached around the window is not done when it compiles.

Distilled from: ADR-0072
