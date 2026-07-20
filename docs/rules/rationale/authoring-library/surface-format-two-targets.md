# Why: One surface format and resolver projects to both the live web renderer and the disposable TouchOSC .tosc over a superset widget vocabulary, each target rendering its subset and skipping the rest loudly.

[Rule](../../authoring-library.md#surface-format-two-targets)

There are two consumers of a surface — a rich web renderer and an external OSC controller — and the
lesson from the pre-decouple era was that letting each grow its own resolver produces two
hand-ported copies that drift (the corrections in one never reach the other). So there is **one
surface format and one resolver semantics**, projected to two targets. The widget vocabulary is a
**superset of TouchOSC's**, deliberately, so the web player is never capped by TouchOSC's ceiling:
shipped kinds are `fader`, `radial`, `param-toggle`, `note-toggle`, `chord-button`, with richer
names (`xy-pad`, `grid`, `visualizer`, `keyboard`) reserved in the format so it stays stable when
they land, though nothing builds them yet.

The web renderer consumes the surface doc live and renders every shipped kind. The **TouchOSC
target is a disposable projection** — the instrument JSON is the source of truth, a `.tosc` is a
scratch playing surface you regenerate when the instrument changes (this is the one durable piece of
the original control-surface framing, now scoped to the projection only). The emitter renders its subset and
**skips web-only or reserved widgets loudly**, a warning naming each skipped control, exactly as it
already skips enum inputs. File resolution is per-target and layered:
`surfaces/<instrument>.<target>.json` ?? `surfaces/<instrument>.json` ?? the auto-derived default
([default-surface](default-surface.md)) — a per-target file earned only when the control *set*
genuinely diverges, not merely its geometry.

Keeping the *semantics* shared while each target keeps a tiny native resolver (JS for web, Python
for TouchOSC) is the balance point: the projection logic is one contract, the rendering is
per-platform, and the format's superset ceiling means adding a web-rich widget never requires a
TouchOSC change beyond skipping it.

Distilled from: ADR-0043, ADR-0018
