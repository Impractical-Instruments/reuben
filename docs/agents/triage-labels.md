# Triage labels

<!-- GENERATED from templates/triage-labels.md + .ii/repo.toml by `python3 "$CLAUDE_PLUGIN_ROOT/generator/ii_generate.py" --write .` — edit the source, not this file. sha256=a567b866f74f4303544c6666eaf6c955ada5ad034c3513486146c6e32a80084d -->

Labels here are **all-repo or one-repo, and nothing in between.** Every family below is all-repo: the same strings with the same meanings in every repo this document reaches, because they are the shared triage vocabulary rather than a per-repo setting. Anything a repo adds for itself is one-repo, and no other repo needs to know it.

Work is triaged first by **state role** — where an issue sits in the path from "someone filed it" to "someone can act on it". When an instruction names a role ("apply the AFK-ready triage label"), use the matching label below.

## The canonical state roles

- **`needs-triage`** — maintainer needs to evaluate this issue.
- **`needs-info`** — waiting on reporter for more information.
- **`ready-for-agent`** — fully specified, ready for an AFK agent.
- **`ready-for-human`** — requires human implementation.
- **`wontfix`** — will not be actioned.

## Charting an initiative

Also all-repo. This family is used when an initiative's shape is not yet known: one issue holds the map, and each child ticket carries the label naming how that piece gets resolved. `issue-tracker.md` beside this file has the operations.

- **`wayfinder:map`** — the initiative map, holding Notes / Decisions-so-far / Fog.
- **`wayfinder:research`** — a child ticket resolved by fact-finding against primary sources.
- **`wayfinder:prototype`** — a child ticket resolved by building a cheap artifact to react to.
- **`wayfinder:grilling`** — a child ticket resolved by stress-testing a decision with a human.
- **`wayfinder:task`** — a child ticket that is straightforward work.

Every family above means the same thing wherever this document is generated, so a label means the same thing in every tracker that carries it. The meanings above are what the label *is*; a label's description field in GitHub is set on the tracker and is not claimed to match.

## Labels this repo adds for itself

The one-repo case. This repo declares these labels because they are useful here; no other repo needs to know them. The tracker may carry others — stock labels, or ones added by hand; this is the declared set, not an inventory of everything on it.

- `post-v1`
- `design-question`
- `someday`
- `epic`
- `triage`
- `perf`

**Label strings are exact.** `gh` matches them literally, spaces and all — a label whose name carries a space is not reachable by the same string without it, and `gh issue edit --add-label` fails rather than guessing. Copy the strings above character for character.
