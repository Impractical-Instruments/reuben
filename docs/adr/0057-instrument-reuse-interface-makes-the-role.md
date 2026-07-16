# ADR-0057: Instrument reuse — interface makes the role; recipe-role doc lines; the generated library index

## Status

Accepted (2026-07-16). The recipe/library-shape decision of the patch-pipeline streamline effort —
wayfinder ticket [Patch-pipeline/F (reuben-web#87)](https://github.com/Impractical-Instruments/reuben-web/issues/87)
on map [reuben-web#81](https://github.com/Impractical-Instruments/reuben-web/issues/81), resolved in a
grilling session with the repo owner. **Rides on** [ADR-0034](0034-instrument-nesting.md) (`subpatch`
nesting mechanics — untouched; this ADR evolves its *terminology* only), [ADR-0038](0038-interface-pipes-and-the-device-layer.md)
(interface pipes carry the quantity contract), [ADR-0036](0036-instrument-library-and-format-versioning.md)
(the document is the save source of truth), [ADR-0045](0045-whole-document-edit-contract.md)
(whole-document edits — reaffirmed for the web lane by Patch-pipeline/E, and load-bearing for §3 here),
and [ADR-0051](0051-authoring-grounding-single-source.md) (grounding views are generated, never
hand-kept). Grounded on the [idiom mining](https://github.com/Impractical-Instruments/reuben-web/blob/dev/docs/research/patch-pipeline-idioms.md)
(Patch-pipeline/D) and the [count_tokens re-baseline](https://github.com/Impractical-Instruments/reuben-web/pull/97)
(Patch-pipeline/I).

## Context

The web-chat patching agent authors instruments under the whole-document contract, so output cost is
linear in document size. The evidence that shaped this decision:

- **53% of all corpus nodes are CV-plumbing glue**, and it clusters: six recipe candidates cover ≈25%
  of the corpus by tokens. The three drum bodies already exist as `voices/` documents — and
  euclidean-drums re-authored them inline anyway, a live drift pair (~1,100 tokens of duplication,
  already diverged).
- **One euclidean-drums emission measures 81% of the per-round output cap** (8,192 `max_tokens`) —
  the re-baseline corrected the baseline's 44% estimate. Factoring big instruments into referenced
  children is no longer a nicety; it is the pressure valve ADR-0045 §4 named, nearly at its trigger.
- The nesting machinery for reuse **already exists** (`subpatch` node, `resources`, interface pipes,
  inline-dissolve; staged-resource resolution on the web lane). The recursive patching system was
  built for exactly this; the standing preference is to use the engine plumbing, not invent beside it.
- Terminology had begun to fork: "recipe" was being minted as a new kind of thing, "subpatch" as a
  kind of document, `voices/` treated as a category — while the format knows only instruments.

## Decision

### 1. A recipe is nothing but an instrument

The unit of reuse is a **validated instrument document with a defaulted interface-pipe face**,
consumed by-reference via the existing `subpatch` node (ADR-0034). No new format kind, no new
directory semantics, no library mechanism beyond `resources` + staging. Non-nestable idioms
(clock scaffolds, poly scaffolds) stay prompt-side material — the library holds one kind of thing.

**Considered and rejected:** *paste-in exemplars* (few-shot node clusters the model copies inline) —
zero output-token savings, and they re-create exactly the euclidean-drums drift the corpus already
demonstrates; *a recipe format/manifest* — a second kind of document to validate, version, and keep
true, when the instrument format already carries everything (§3).

### 2. One noun — instrument; roles are contextual; interface makes the role

There is one noun: **instrument**. Roles are read off the interface or the use, never off a path,
filename, or kind:

- An instrument hosted by a voicer is a **voice instrument** *while hosted* — what makes it usable
  as one is its face (`freq`/`gate` in, `audio`/`active` out), nothing else.
- An instrument referenced from another is a **nested instrument** *while referenced*.
- **`subpatch` is demoted to a format keyword** — the node type that references a nested instrument.
  It is not a noun for the document. (The wire format keeps the keyword; renaming format surface
  buys nothing.)
- **"Recipe" is demoted to role shorthand** — there is no recipe document, recipe directory, or
  recipe format. What exists is an instrument's **recipe-role**: its reuse story (§3).

This **amends ADR-0034's terminology block** (noun demotion only; every mechanic stands) and evolves
CONTEXT.md's language section in the same commit.

**Considered and rejected:** *role-by-directory or naming conventions* — storage location is
arbitrary and heading server-side; a convention would be a second, silently-drifting encoding of
what the interface already states; *keeping "recipe" as a kind* — the corpus drift happened
precisely because `voices/` was treated as a kind rather than a role.

### 3. The recipe-role: the `doc` first line, authored at creation, kept true by re-emission

An instrument's reuse story — its **recipe-role** — is carried in the document itself: **the first
sentence of its top-level `doc` field** states what it is and when to reach for it, in the domain
language. Authored when the instrument is created (by the seed work for shipped children; by prompt
policy for chat-built ones).

Staleness is handled by the edit contract, not by tooling: under ADR-0045 every reshape re-emits the
whole document, `doc` line included, so the role line is mechanically re-presented for revision on
every edit — prompt policy says *keep `doc` true when you reshape*. (An incremental edit layer would
have made role drift structural; this synergy was an argument in E's reaffirmation.)

**Trust boundary — the role is trusted for selection only.** The face (pipe names, `Arg` types,
defaults, outputs) is always projected mechanically from the `interface` block and enforced by the
loader; no consumer may take face facts from prose. A wrong role line can cost a bad-sounding
attempt (wrong child chosen); it can never mis-wire a document.

**Considered and rejected:** *a dedicated role/format field* — format surface before the convention
has earned structure; if tags/categories ever prove necessary, a field can be minted then, with its
own ADR; *deriving the role by inference per session* — re-paying the reasoning cost the role line
exists to remove.

### 4. Discovery: a generated index over the available-set; no curated list

The library **is the available-set**: every instrument accessible to the session (bundled today;
server-stored per the Vault effort later). Discovery is a **generated index** — one signature line
per instrument, projected mechanically from the document alone:

```
kick-body — pitch-drop kick/tom body; gate-driven. (gate:f32=0, base:f32 Hz=48, sweep:f32 Hz=220, decay:f32 s=0.4) → audio, active
```

name + recipe-role line + face signature. The generator is the same projection family as the compact
`describe_operators` view (reuben-web grounding audit, option 2): a second **generated** view of one
source, staleness-tested like every generated artifact — never a hand-kept digest (ADR-0051). The
full document remains fetchable on demand as the fallback when a role seems off; consumers never
need a child's internals to *use* it — reference id + face is the whole contract.

There is **no curated list**. Curation dissolves into two concerns that already have homes:
*quality* is authoring (give the instrument a face and a role line worth reusing — done once, at
creation), and *availability* is delivery (which documents a session can reference — the Vault
effort's question, out of scope here).

**Considered and rejected:** *a hand-kept curated list* — a drift pair with the documents it
describes, plus an admission process nobody owns; *shipping full documents in grounding* — the
index line is ~30–60 tokens against ~500–2,000 for a document body that grounds nothing the face
doesn't.

### 5. The seed and its acceptance

Seed the library with the idiom mining's top six, in dependency order: **shaped VCA** (the
primitive), then **pitch-drop drum body / snare body / hat tick** built on it (promote the existing
`voices/` drum documents' baked literals to defaulted pipes — do not author new documents), then the
**DJ-filter + level channel strip** and the **Good Button fan-out**. Below-floor idioms (smoothed
level knob, master trim) are excluded — under the ~75–110-token reference overhead they save
nothing; the master-trim fix is an operator-backlog note (a `gain` input on `output`, which also
erases the corpus's 11 constant-`m2s` materializers).

**Acceptance:** re-express euclidean-drums' channels through the seed recipes and assert the
re-expressed document **renders bit-identical** to the current inline version (the ADR-0026
discipline) — repairing the corpus's live drift pair and proving the library on the instrument that
is already at 81% of the output cap.

### 6. Mechanical validation wherever possible

Standing preference, applied here and forward: spend build effort on mechanical checks that cut
inference cost and improve accuracy — the index generator's staleness test, the bit-identical
euclid assertion, and **selection evals** (synthetic dev-session asks against the index: does the
agent pick the right child? — privacy-safe under the no-transcripts posture). Role-line accuracy is
enforced by convention + eval + the fetch-on-demand fallback, not by review process.

**Out of scope, recorded:** the interface-boundary gaps the next recipe tier hits (pipe re-export /
lane sugar, voicer voice face, resource-slot parameters, constants-on-pipes, note→gate, variadic
pipes) are ruled out of the patch-pipeline effort entirely — filed with the quantified case as
[reuben#453](https://github.com/Impractical-Instruments/reuben/issues/453). Flag A (no pipe
re-export) is the **named limit** of this architecture: sequencer-heavy instruments (groovebox's 48
step pipes ≈45% of its document) gain little from recipes until that future effort runs.

## Consequences

- **No engine code changes from this ADR** — the design gate only. The builds it feeds (index
  generator + staleness test, seed face-promotion + euclid re-expression, selection evals, prompt
  policy lines) are sliced by the patch-pipeline spec ticket (Patch-pipeline/H).
- **ADR-0034's terminology evolves**: "subpatch" is a format keyword, not a noun; CONTEXT.md's
  language section is updated in this commit (Subpatch entry reframed, Voice sub-patch → **voice
  instrument**, new **interface makes the role** entry).
- **The `doc` field's first sentence becomes load-bearing** for shipped and chat-built instruments;
  authoring guidance and the web prompt policy must say so (H slices the wording change).
- **The index becomes a grounding ingredient** for the web lane's prefix composition (the
  prompt-architecture ticket owns placement and budget).
- **Delivery is explicitly deferred**: how the available-set and its documents reach a web session
  (bundled vs server-fetched vs staged) belongs to the Vault effort; nothing here presumes an answer.
- **Terminology:** *recipe-role* = an instrument's reuse story: its `doc` first line plus its
  interface face, projected into the index — trusted for selection, never for wiring; *available-set*
  = the instruments a session can reference; *library index* = the generated signature-line
  projection of the available-set.
