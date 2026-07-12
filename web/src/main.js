// The reuben web player app (issue #226, P4): the splash → launcher → player state machine
// over the persistent engine + P3 auto-UI surface. Imports the engine and surface from
// crates/reuben-web/js/ as the source of truth (ADR-0041) — this file owns only the shell.
//
// Engine lifecycle (the #151 "Toy switching" decision): ONE AudioContext + ONE worklet for
// the whole session. createReubenEngine() runs once at boot; picking a Toy calls
// engine.load(id) again on that persistent engine — the worklet reconstructs its engine in
// place, keeping the already-unlocked context (critical for iOS: a second resume gesture
// isn't guaranteed). We never destroy()/recreate between Toys.
//
// The two taps to music (the revised AC): tap Start on the splash = the audio unlock
// (ctx.resume(), the P1/#223 iOS pattern — the ONLY thing the gesture does), then tap a Toy
// card. groovebox's assets are prefetched during the splash so that first pick is instant.

import "./app.css";
import iiBadge from "./assets/ii-badge.png";
import { registerSW } from "virtual:pwa-register";
import { createReubenEngine } from "../../crates/reuben-web/js/reuben-engine.mjs";
import {
  applySnapshotDefaults,
  buildSurface,
  initial,
  SURFACE_CANDIDATES,
  validateSurfaceDoc,
  VALUE_KINDS,
} from "../../crates/reuben-web/js/surface/widget-model.mjs";
import { renderSurface, sendInitialDefaults } from "../../crates/reuben-web/js/surface/render.mjs";
import { decodeControl, encodeControl } from "../../crates/reuben-web/js/codec.mjs";
import { decodeBundle, encodeBundle, ShareError } from "../../crates/reuben-web/js/share.mjs";
import manifest from "../toys.json";
import { h } from "./dom.js";
import { chatEnabled } from "./chat/flag.js";
import { createSpine } from "./chat/spine.js";
import { nodeOfControl } from "./chat/board.js";

const REPO_URL = "https://github.com/Impractical-Instruments/reuben";

// Staged assets are served from the app's base (import.meta.env.BASE_URL) — '/' on the
// Cloudflare Pages root, a sub-path on a PR preview. Every fetch goes through asset() so a
// sub-path deploy needs no code change.
const asset = (p) => `${import.meta.env.BASE_URL}${p}`;

const TOYS = [...(manifest.toys ?? [])].sort((a, b) => a.order - b.order);
const DEFAULT_TOY = manifest.default ?? TOYS[0]?.id;

const app = document.getElementById("app");

// --- engine + shared state ----------------------------------------------------------------

let engine = null; // set once createReubenEngine resolves
let enginePromise = null; // in-flight (or settled) engine creation; awaited by Start
let loadToken = 0; // bumped per openToy so a stale in-flight load can't clobber a newer one
let currentToy = null; // id of the Toy currently loaded on the engine (null before first)
let currentInstrument = null; // document `instrument` name of the current player (Toy or link)
let currentSurface = null; // the surface of the current player screen, read at Share time
let currentSurfaceDocText = null; // the curated surface doc's verbatim text (null = auto-derived), embedded at Share time
let currentBanner = null; // the launcher banner text (null = none), read by the test hook
let currentShareBtn = null; // the current player's Share button, for its "Copied!" flash
let currentShareSlot = null; // the current player's slot for the hand-copy fallback textarea
let lastShareUrl = null; // the URL minted by the most recent Share (test hook)
let currentSpine = null; // the co-presence spine handle when the chat flag routes there (#355)
let currentSpineEngine = null; // the engine the current spine's board is bound to (test hook)

// The control JOURNAL: address -> the args last SENT for it (issue #228 Share). engine.send is
// wrapped once (instrumentJournal) to record into this; it's CLEARED at the start of every load
// so it reflects only the current instrument. Share diffs it against each widget's load-time
// default to capture ONLY the controls the player moved (the sidecar snapshot).
const journal = new Map();

// err.code -> the dismissible banner copy (issue #228 failure taxonomy classes B–E′). The codec
// switches on the code, never the message; main.js owns the user-facing strings here.
const SHARE_ERROR_BANNER = {
  future: "This link was made by a newer version of reuben.",
  damaged: "This link is damaged.",
  "too-large": "This link is too large.",
  sample: "This instrument uses audio samples, which can't be shared as links yet.",
};

// Create the engine (adds the worklet module, fetches + compiles the wasm). Safe while the
// context is still suspended — it never resumes; that's the Start gesture's job. Kicked off
// at boot so the wasm is compiling while the player reads the splash.
function ensureEngine() {
  if (!enginePromise) {
    enginePromise = createReubenEngine({
      assetBase: asset("instruments"),
      wasmUrl: asset("reuben_web.wasm"),
    })
      .then((e) => {
        e.onLog = (t) => console.log(t);
        engine = e;
        return e;
      })
      .catch((err) => {
        // Don't cache a rejection: a transient failure (fetch/compile/addModule) must let the
        // splash "try again" actually re-attempt engine creation, not return the dead promise.
        enginePromise = null;
        throw err;
      });
  }
  return enginePromise;
}

// Surface resolution (the ADR-0043 §5 order for target `web`): SURFACE_CANDIDATES and the
// validateSurfaceDoc version predicate are shared with the README link generator via
// widget-model.mjs — one owner of the candidate order. validateSurfaceDoc refuses at PARSE
// (not resolve) to keep the fallthroughs alive: an unusable .web.json must not shadow a valid
// .json, and an unusable embedded surface must not shadow the origin's.
//
// Returns {doc, text} — the parsed doc plus its verbatim text, kept so Share can embed the
// surface byte-for-byte into the link (ADR-0042 amendment) — or null when no candidate works.
// A 404 falls through to the next candidate SILENTLY — most Toys ship only the base doc, and a
// doc-less Toy is fine by design. Any other failure (network drop, bad JSON) logs and also
// falls through: a broken surface file degrades rather than killing the load (ADR-0016).
async function fetchSurfaceDoc(id) {
  for (const candidate of SURFACE_CANDIDATES(id)) {
    try {
      const r = await fetch(asset(candidate));
      if (r.status === 404) continue; // no such surface doc — try the next candidate
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const text = await r.text();
      return { doc: validateSurfaceDoc(JSON.parse(text)), text };
    } catch (err) {
      console.warn(`[player] surface doc ${candidate} unusable, falling through:`, err);
    }
  }
  return null;
}

// A bundle's embedded surface text → {doc, text}, or null when absent/unusable (warn and fall
// through to the next resolution rung — the same dark-degrade policy as fetchSurfaceDoc).
function parseEmbeddedSurface(surfaceText) {
  if (surfaceText == null) return null;
  try {
    return { doc: validateSurfaceDoc(JSON.parse(surfaceText)), text: surfaceText };
  } catch (err) {
    console.warn("[player] embedded surface doc unusable, falling through:", err);
    return null;
  }
}

// One resolution rung: try the surface entry ({doc, text} | null) against the resolver.
// Returns {surface, surfaceText} when the curated doc actually took — so Share never embeds a
// doc the player isn't showing — or null (warn) when there is no entry or the resolver refuses
// it, letting the CALLER decide what the next rung is (fragmentBoot walks the ADR-0042 A2
// ladder; resolveSurfaceOrDefault drops straight to the derived default).
function tryResolveSurface(doc, entry, name) {
  if (entry == null) return null;
  try {
    return { surface: buildSurface(doc, entry.doc), surfaceText: entry.text };
  } catch (err) {
    console.warn(`[player] surface doc for ${name} rejected, trying the next rung:`, err);
    return null;
  }
}

// Resolve with the surface entry, degrading to the auto-derived default if the resolver
// refuses it — a broken surface file must never kill a Toy that already loaded (the
// fetchSurfaceDoc policy above, enforced at the last seam).
function resolveSurfaceOrDefault(doc, entry, name) {
  return tryResolveSurface(doc, entry, name) ?? { surface: buildSurface(doc, null), surfaceText: null };
}

// Surface resolver warnings (skipped pipes, unknown binds, clamped ranges) surface ONCE per
// load on the console — visible in devtools, never a crash (ADR-0043 dark-degrade).
function logSurfaceWarnings(name, warnings) {
  for (const w of warnings) console.warn(`[surface] ${name}: ${w}`);
}

// Warm the browser HTTP cache for a Toy's document + surface-doc candidates so the first
// engine.load(id) has nothing to fetch over the wire (only the worklet round-trip).
// Best-effort: failures (including the expected 404 on an absent candidate) are swallowed
// here and handled properly by the real load's fetchSurfaceDoc.
function prefetchToy(id) {
  fetch(asset(`instruments/${id}.json`)).catch(() => {});
  for (const candidate of SURFACE_CANDIDATES(id)) fetch(asset(candidate)).catch(() => {});
}

// --- screen plumbing ----------------------------------------------------------------------

// Every screen swap goes through here: it records the screen name on <body data-screen> (CSS
// + the Playwright smoke read it) and replaces the app subtree. Screens are plain functions
// returning a DOM node.
function setScreen(name, node) {
  document.body.dataset.screen = name;
  app.replaceChildren(node);
}

// The vanilla-DOM `h()` element helper now lives in ./dom.js so the chat module (chat/*) and the
// shell build their DOM through the ONE helper (imported above). Behavior is identical.

// --- splash -------------------------------------------------------------------------------

function splashScreen() {
  const start = h(
    "button",
    { class: "cta", id: "start", type: "button", onclick: onStart },
    "Start",
  );
  const hint = h("p", { class: "splash-hint" }, "Tap Start, then pick a Toy.");
  const hero = h(
    "section",
    { class: "splash" },
    iiMark(),
    h("h1", { class: "wordmark" }, "reuben"),
    h("p", { class: "tagline" }, "Open a URL. Tap once. Make music."),
    start,
    hint,
    infoLink(),
  );

  async function onStart() {
    start.disabled = true;
    start.textContent = "Starting…";
    try {
      const e = await ensureEngine();
      // The unlock: resume() alone, on the user gesture. Everything heavy already happened.
      await e.context.resume();
      // Ship gate (acceptance criterion §9(7) + ADR-0052 §3; see chat/flag.js): when the chat
      // flag is OFF (the default) this is the unchanged launcher flow; when ON, the same unlock
      // gesture boots into the gallery-first cold start (spec §2, issue #357) instead — the
      // gallery leads into the co-presence spine (spec §3, issue #355) on a pick or a description.
      // One flag, checked in one place (chat/flag.js).
      // NOTE (M1 routing scope): the flagged-on path covers the splash → gallery → spine loop
      // only. A shared-link fragment boot still mounts the OLD player even when the flag is on
      // (see the note at fragmentBoot) — routing a link into the spine is share-into-spine / Keep
      // (M2) work, not omitted by accident.
      if (chatEnabled()) showGallery();
      else showLauncher();
    } catch (err) {
      start.disabled = false;
      start.textContent = "Start";
      hint.textContent = `Couldn't start audio — ${err.message || err}. Tap Start to try again.`;
      hint.classList.add("error");
    }
  }

  return hero;
}

// --- launcher -----------------------------------------------------------------------------

// The label on a Toy's kind badge (launcher card + player head). Kept in one place so the
// launcher and the player never disagree. "live-input" (issue #248) takes the mic — its player
// screen renders an Enable-microphone control keyed on the load()'s inputChannels, not on this.
function badgeText(kind) {
  if (kind === "self-playing") return "self-playing";
  if (kind === "live-input") return "live input";
  return "tap to play";
}

// A small, dismissible banner (issue #228): the landing surface for every share-link failure
// (classes B–I). The ✕ removes it and clears the recorded text so the test hook reads null.
function bannerElement(message) {
  const dismiss = h(
    "button",
    {
      class: "banner-dismiss",
      type: "button",
      "aria-label": "Dismiss",
      onclick: () => {
        currentBanner = null;
        banner.remove();
      },
    },
    "✕",
  );
  const banner = h(
    "div",
    { class: "banner", role: "alert" },
    h("span", { class: "banner-text" }, message),
    dismiss,
  );
  return banner;
}

// A Toy's card — shared between the OFF-path launcher (openToy) and the ON-path gallery
// (pickToy, issue #357): title, blurb, kind badge. `onClick` is the caller's pick handler, so
// the card itself carries no routing opinion.
function toyCard(toy, onClick) {
  return h(
    "button",
    {
      class: "toy-card",
      type: "button",
      dataset: { toy: toy.id, kind: toy.kind },
      onclick: onClick,
    },
    h("span", { class: "toy-title" }, toy.title),
    h("span", { class: "toy-blurb" }, toy.blurb),
    h("span", { class: `toy-badge ${toy.kind}` }, badgeText(toy.kind)),
  );
}

function launcherScreen(bannerMessage) {
  const grid = h(
    "div",
    { class: "toy-grid" },
    TOYS.map((toy) => toyCard(toy, () => openToy(toy))),
  );
  return h(
    "section",
    { class: "launcher" },
    bannerMessage ? bannerElement(bannerMessage) : null,
    h(
      "header",
      { class: "launcher-head" },
      iiMark(),
      h("h1", { class: "wordmark small" }, "reuben"),
      h("p", { class: "launcher-sub" }, "Pick a Toy."),
    ),
    grid,
    infoLink(),
  );
}

// Show the launcher, optionally carrying a dismissible banner. Every non-splash landing goes
// through here — a Toy's back button, a link failure, a hashchange to empty — so the banner
// state is owned in exactly one place.
function showLauncher(bannerMessage) {
  currentBanner = bannerMessage ?? null;
  currentSurface = null;
  currentSurfaceDocText = null;
  currentShareBtn = null;
  currentShareSlot = null;
  setScreen("launcher", launcherScreen(currentBanner));
}

// "← Toys": clear the hash FIRST (so a reload from the launcher doesn't resurrect the shared
// instrument — issue #228 hash lifecycle) then land on the launcher. replaceState does not fire
// hashchange, so this can't re-boot a fragment.
function backToLauncher() {
  history.replaceState(null, "", location.pathname);
  showLauncher();
}

// --- gallery (cold start, issue #357, spec §2) ---------------------------------------------

// The first-run screen when the chat flag is on: the show-all `toys.json` gallery (§2.2 — all 5,
// in `order`, no auto-derivation from `instruments/`) plus a PERSISTENT "…or describe your own"
// bar (§2.1). The bar reuses the spine's own reshape-input classes (chat/spine.css) so it reads
// as the SAME input line that continues into the player's pinned input once an instrument is
// playing — one input, cold-start through play. A card tap starts sound + fires the proactive
// turn one (§2.3, `pickToy`); the describe bar routes free text into the build path (§2.4,
// `submitDescribe`).
function galleryScreen(bannerMessage) {
  const grid = h(
    "div",
    { class: "toy-grid" },
    TOYS.map((toy) => toyCard(toy, () => pickToy(toy))),
  );
  const describeInput = h("input", {
    class: "reshape-input",
    type: "text",
    name: "describe",
    autocomplete: "off",
    placeholder: "…or describe your own (e.g. a warm pad that swells)",
    "aria-label": "Describe your own instrument",
  });
  const describeSend = h("button", { class: "reshape-send", type: "submit" }, "Go");
  const describeForm = h(
    "form",
    {
      class: "reshape",
      onsubmit: (ev) => {
        ev.preventDefault();
        const text = describeInput.value.trim();
        if (!text) return; // empty send is the failure ticket's gentle re-orient (#303/E), not ours
        describeInput.value = "";
        submitDescribe(text);
      },
    },
    describeInput,
    describeSend,
  );
  return h(
    "section",
    { class: "gallery" },
    h(
      "div",
      { class: "gallery-scroll" },
      bannerMessage ? bannerElement(bannerMessage) : null,
      h(
        "header",
        { class: "launcher-head" },
        iiMark(),
        h("h1", { class: "wordmark small" }, "reuben"),
        h("p", { class: "launcher-sub" }, "Tap one to play."),
      ),
      grid,
      infoLink(),
    ),
    h(
      "div",
      { class: "gallery-describe" },
      h("p", { class: "gallery-describe-label" }, "…or describe your own"),
      describeForm,
    ),
  );
}

// Show the gallery, optionally carrying a dismissible banner — the chat-flag-on counterpart of
// showLauncher (kept as a separate function/screen name so the OFF-path launcher stays byte-for-
// byte unchanged, per chat/flag.js's contract).
function showGallery(bannerMessage) {
  currentBanner = bannerMessage ?? null;
  currentSurface = null;
  currentSurfaceDocText = null;
  currentShareBtn = null;
  currentShareSlot = null;
  setScreen("gallery", galleryScreen(currentBanner));
}

// --- player -------------------------------------------------------------------------------

// Build the common player-screen chrome shared by a launcher-opened Toy and a fragment-booted
// link: header (back, title, optional blurb, kind badge, Share), an empty share slot for the
// hand-copy fallback, the status line, and a body with a loading skeleton over the surface mount.
// Records the Share button + slot as the current player's so `doShare` (which reads live module
// state, not a closure) targets exactly what's on screen. Returns the pieces the caller fills in.
function buildPlayerScreen({ nameForData, title, blurb, kind }) {
  const surfaceEl = h("div", { class: "surface-mount" });
  const status = h("p", { class: "player-status" }, "Loading…");
  const skeleton = h(
    "div",
    { class: "skeleton", "aria-hidden": "true" },
    h("div", { class: "skeleton-row" }),
    h("div", { class: "skeleton-row" }),
    h("div", { class: "skeleton-row" }),
  );
  const body = h("div", { class: "player-body" }, skeleton, surfaceEl);

  // Disabled until the surface is loaded: currentBundle() is only trustworthy post-load, and a
  // click mid-load would share the PREVIOUS instrument's bundle.
  const shareBtn = h("button", { class: "share", type: "button", disabled: "" }, "Share");
  shareBtn.addEventListener("click", () => doShare());
  const shareSlot = h("div", { class: "share-slot" });

  const heading = h(
    "div",
    { class: "player-heading" },
    h("h1", { class: "player-title" }, title),
    blurb ? h("p", { class: "player-blurb" }, blurb) : null,
  );
  const screen = h(
    "section",
    { class: "player", dataset: { toy: nameForData } },
    h(
      "header",
      { class: "player-head" },
      iiMark(),
      h("button", { class: "back", type: "button", onclick: backToLauncher }, "← Toys"),
      heading,
      h("span", { class: `toy-badge ${kind}` }, badgeText(kind)),
      shareBtn,
    ),
    shareSlot,
    status,
    body,
  );

  currentShareBtn = shareBtn;
  currentShareSlot = shareSlot;
  return { screen, surfaceEl, status, skeleton, body, shareBtn };
}

// Wrap engine.send ONCE so every control message is recorded into the journal (address -> args)
// before being sent. Idempotent — a flag on the engine guards a second wrap across loads.
function instrumentJournal(e) {
  if (e.__journalWrapped) return;
  e.__journalWrapped = true;
  const original = e.send.bind(e);
  e.send = (address, args = []) => {
    journal.set(address, args);
    return original(address, args);
  };
}

// Deep-equal two control arg arrays (bare numbers and {i32} markers). Used to tell a moved
// control from one still resting at its load-time default.
function argsEqual(a, b) {
  if (a === b) return true;
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    const x = a[i];
    const y = b[i];
    if (typeof x === "object" && x !== null) {
      if (typeof y !== "object" || y === null || x.i32 !== y.i32) return false;
    } else if (x !== y) {
      return false;
    }
  }
  return true;
}

// The control-state SIDECAR for Share: for every stored-state widget (fader/param-toggle — NOT
// the held note/chord buttons, which carry no resting state) whose journalled args differ from
// its load-time default, capture a verbatim encodeControl() buffer. So a link carries ONLY the
// controls the player actually moved; an untouched instrument mints an empty snapshot.
function captureSnapshot(surface) {
  const snapshot = [];
  for (const widget of surface.widgets ?? []) {
    if (!VALUE_KINDS.has(widget.kind)) continue;
    const moved = journal.get(widget.address);
    if (!moved) continue;
    if (argsEqual(moved, initial(widget).args)) continue;
    snapshot.push(encodeControl(widget.address, moved));
  }
  return snapshot;
}

// The derived kind badge for a fragment-booted instrument (no toys.json entry to read it from):
// a play surface (note/chord buttons) is tap-to-play; else a mic-taking instrument is live-input;
// else it plays itself. Feeds the same badgeText/CSS the launcher Toys use.
function deriveKind(surface, info) {
  if (surface.widgets.some((w) => w.kind === "note-toggle" || w.kind === "chord-button")) {
    return "tap-to-play";
  }
  if (info?.inputChannels > 0) return "live-input";
  return "self-playing";
}

// The player status line once an instrument is loaded and playing. Shared by both entry paths
// (a Toy opened from the launcher and one booted from a link): a mic-taking instrument shows the
// enable prompt (it's silent until the gesture); a tap-to-play surface prompts for taps; a
// surface with faders invites tweaking; a bare self-playing patch just plays.
function playingStatusText(takesInput, kind, hasWidgets) {
  if (takesInput) return "Enable the microphone to play.";
  if (kind === "tap-to-play") return "Ready — tap the buttons to play.";
  return hasWidgets ? "Playing — tweak the controls." : "Playing.";
}

// Share the current player's instrument as a `#r1.…` link (issue #228). Mints a bundle from the
// engine's retained document + resources plus the moved-control snapshot, persists it into the
// hash via replaceState (NOT pushState — repeated Shares must not stack history; replaceState
// also doesn't fire hashchange, so it won't re-boot), then runs the three-step share chain with
// no dead end: Web Share → clipboard → a selected read-only textarea to hand-copy.
async function doShare() {
  if (!engine || !currentSurface) return;
  const bundle = engine.currentBundle();
  if (!bundle) return;

  let fragment;
  try {
    fragment = await encodeBundle({
      docText: bundle.docText,
      resources: bundle.resources,
      snapshot: captureSnapshot(currentSurface),
      surfaceText: currentSurfaceDocText,
    });
  } catch (err) {
    // encodeBundle can only fail on a sample or an over-cap bundle — neither is reachable for a
    // shareable instrument on screen, but degrade to a visible note rather than a silent throw.
    if (currentShareSlot) {
      currentShareSlot.replaceChildren(
        h("p", { class: "share-error" }, `Couldn't make a link — ${err.message || err}.`),
      );
    }
    return;
  }

  const url = location.origin + location.pathname + "#" + fragment;
  lastShareUrl = url;
  history.replaceState(null, "", "#" + fragment);

  // 1. Native share sheet (mobile). A user-cancelled share (AbortError) is quiet; any other
  //    failure falls through to the clipboard rather than dead-ending.
  if (navigator.canShare?.({ url })) {
    try {
      await navigator.share({ url });
      return;
    } catch (err) {
      if (err?.name === "AbortError") return;
    }
  }
  // 2. Clipboard.
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(url);
      flashCopied();
      return;
    } catch {
      // fall through to the hand-copy fallback
    }
  }
  // 3. A selected read-only textarea — always copyable by hand.
  showShareFallback(url);
}

// Flash "Copied!" on the Share button, then restore it.
function flashCopied() {
  const btn = currentShareBtn;
  if (!btn) return;
  btn.textContent = "Copied!";
  setTimeout(() => {
    if (currentShareBtn === btn) btn.textContent = "Share";
  }, 1500);
}

// The no-clipboard fallback: a selected, read-only textarea holding the URL, so it can be copied
// by hand even where the Clipboard API is unavailable.
function showShareFallback(url) {
  if (!currentShareSlot) return;
  const field = h("textarea", { class: "share-fallback", readonly: "", rows: "2" });
  field.value = url;
  currentShareSlot.replaceChildren(
    h("label", { class: "share-fallback-label" }, "Copy this link:"),
    field,
  );
  field.focus();
  field.select();
}

// Open a Toy: show the player screen immediately (skeleton, never a frozen blank), load it on
// the persistent engine, then mount its surface. A loadToken guards against a race where the
// player taps back + opens another Toy while a load is still in flight — only the newest token
// gets to render, and the engine's own one-load-at-a-time guard is respected by awaiting.
// Opening from the launcher does NOT write the hash (issue #228 hash lifecycle) — only Share does.
async function openToy(toy) {
  const token = ++loadToken;
  journal.clear(); // reflect only the instrument about to load
  const { screen, surfaceEl, status, skeleton, body, shareBtn } = buildPlayerScreen({
    nameForData: toy.id,
    title: toy.title,
    kind: toy.kind,
  });
  setScreen("player", screen);

  try {
    const e = await ensureEngine();
    instrumentJournal(e);
    // engine.load is one-at-a-time; if a previous Toy's load is still settling, await it out
    // by retrying once it frees. In practice the back→pick path is sequential, but a fast
    // double-tap shouldn't throw "a load is already in flight" at the player. Its resolved
    // {channels, inputChannels, blockSize} tells us whether this instrument takes live input
    // (inputChannels > 0) — the mic affordance below is driven by that, not by toy.kind (#248).
    const info = await loadWithRetry(e, toy.id);
    if (token !== loadToken) return; // superseded by a newer openToy — drop this render

    currentToy = toy.id;
    const [doc, surfaceEntry] = await Promise.all([
      fetch(asset(`instruments/${toy.id}.json`)).then((r) => {
        if (!r.ok) throw new Error(`fetch ${toy.id}.json: HTTP ${r.status}`);
        return r.json();
      }),
      fetchSurfaceDoc(toy.id),
    ]);
    if (token !== loadToken) return;

    currentInstrument = doc.instrument;
    const { surface, surfaceText } = resolveSurfaceOrDefault(doc, surfaceEntry, toy.id);
    logSurfaceWarnings(toy.id, surface.warnings);
    currentSurface = surface;
    currentSurfaceDocText = surfaceText;
    renderSurface(surface, e, surfaceEl);
    // sendInitialDefaults ONLY after load() resolved (the worklet's `ready`): a control sent
    // before construct is dropped (render.mjs lifecycle note).
    sendInitialDefaults(surface, e);

    skeleton.remove();
    document.body.dataset.state = e.context.state; // "running" — the smoke asserts on this

    // An input-taking instrument (mic-space and any future duplex Toy) loads and renders but
    // plays SILENCE until enableMic() wires a MediaStreamSource into the worklet (#248). Give
    // it the Enable-microphone control — a real user gesture, the only thing iOS accepts for
    // getUserMedia — instead of a "Playing." that lies. Keyed on the load()'s inputChannels so
    // it's instrument-driven, not a per-Toy flag.
    const takesInput = info?.inputChannels > 0;
    if (takesInput) body.prepend(micControl(e, status));

    status.textContent = playingStatusText(takesInput, toy.kind, surface.widgets.length > 0);
    status.classList.remove("error");
    shareBtn.disabled = false; // bundle is now loaded — Share is safe
  } catch (err) {
    if (token !== loadToken) return;
    skeleton.remove();
    // The error text lives on `status` (shown above the body); body holds only the retry
    // affordance — no second copy of the message.
    status.classList.add("error");
    status.textContent = `Couldn't load ${toy.title} — ${err.message || err}.`;
    body.replaceChildren(
      h("button", { class: "cta small", type: "button", onclick: () => openToy(toy) }, "Retry"),
    );
  }
}

// engine.load is one-at-a-time (its own `loading` guard). If a previous Toy's load is still
// settling when the player switches, POLL until it frees rather than racing a single fixed
// delay — a cold-cache discovery + worklet construct can take well over a frame, and a fixed
// 50 ms retry would spuriously surface an error card while the pipeline is actually healthy.
// The loadToken supersede logic in openToy still ensures only the newest pick renders; this
// just guarantees the newest pick eventually loads. Bounded so a genuinely stuck load still
// surfaces an error instead of hanging forever. Any non-guard error propagates immediately.
async function loadWithRetry(e, id) {
  const deadline = Date.now() + 15_000;
  for (;;) {
    try {
      return await e.load(id);
    } catch (err) {
      const inFlight = /load is already in flight/.test(err.message || "");
      if (!inFlight || Date.now() >= deadline) throw err;
      await new Promise((r) => setTimeout(r, 40));
    }
  }
}

// The Enable-microphone affordance for an input-taking Toy (#248). The whole permission flow
// lives in engine.enableMic() (getUserMedia, worklet wiring, double-invocation + destroy-race
// guards, and the finished user-facing error copy); this is only the button that calls it on a
// user gesture and mirrors the outcome. State runs idle → live, or idle → denied (retryable) —
// enableMic() is idempotent, so a live button is disabled and a denied one stays tappable. The
// headphones note warns about feedback: mic-space is a mic through a reverb with no attenuation,
// so it self-oscillates on a phone speaker the instant the mic goes live.
function micControl(engine, status) {
  const btn = h("button", { class: "mic-enable cta small", type: "button" }, "Enable microphone");
  const note = h(
    "p",
    { class: "mic-note" },
    "🎧 Use headphones — on a speaker this feeds back into the reverb immediately.",
  );
  const wrap = h("div", { class: "mic-control", dataset: { micState: "idle" } }, btn, note);

  btn.addEventListener("click", async () => {
    btn.disabled = true;
    btn.textContent = "Enabling…";
    try {
      await engine.enableMic();
      wrap.dataset.micState = "live";
      btn.textContent = "Microphone live";
      status.textContent = "Microphone live — play into it.";
      status.classList.remove("error");
    } catch (err) {
      // Surface the engine's verbatim copy on the shared status slot (the same one load errors
      // use); leave the button tappable so a denied prompt can be retried after granting access.
      wrap.dataset.micState = "denied";
      btn.disabled = false;
      btn.textContent = "Enable microphone";
      status.textContent = err.message || String(err);
      status.classList.add("error");
    }
  });

  return wrap;
}

// --- spine (chat authoring, issue #355 + #357) ---------------------------------------------

// Open the co-presence spine (spec §3), reached ONLY when the chat flag is on (chat/flag.js) —
// via a gallery pick (§2.3, `pickToy`) or the describe bar (§2.4, `submitDescribe`), both of
// which call this. It loads `toyId` (defaulting to DEFAULT_TOY) onto the persistent engine and
// renders its controls into the spine's node-identity board (spec §3.6), reusing the SAME
// engine-load + surface-resolution path as openToy so the board binds the real, engine-wired
// controls (never a re-derived copy).
//
// `seed` is turn-one content the caller already knows synchronously (a pick's authored greeting
// + chips, or a describe-path's optimistic echo of the user's own words) — passed straight to
// createSpine so it appears the instant the spine mounts, before the (possibly slow) engine load
// resolves (spec §2.3/§2.4 both promise an immediate turn one). `onReady(spine, {id, title})`
// fires once the instrument has actually loaded and rendered — for content that can only be
// known then (a describe-path's "here's what it made" landing line).
async function openSpine({ toyId, arrival = "picked", seed = [], onReady } = {}) {
  const token = ++loadToken;
  journal.clear(); // reflect only the instrument about to load
  // Wire the live engine's declicked duck (spec §6.2.4, issue #360) into the spine's re-strike. The
  // spine is built BEFORE the engine loads, so this closure resolves `engine` at CALL time (by which
  // point a re-strike means the instrument is loaded + playing). Absent an engine it degrades to an
  // immediate pass-through, so the visible re-strike still runs — the spine's own default does the
  // same, this just hands it the real audio fade once one exists.
  const duck = (atSilence) =>
    engine ? engine.restrikeDuck(atSilence) : Promise.resolve(atSilence?.());
  const spine = createSpine({ arrival, seed, duck });
  currentSpine = spine;
  currentSurface = null;
  currentSpineEngine = null;
  setScreen("spine", spine.screen);
  exposeSpineTestHook(spine);

  try {
    const e = await ensureEngine();
    instrumentJournal(e);
    const id = toyId ?? DEFAULT_TOY;
    // The resolved {channels, inputChannels, blockSize} tells us whether this instrument takes
    // live input (inputChannels > 0) — the mic affordance below is driven by that, not by a
    // per-Toy flag, exactly as the player does (#248).
    const info = await loadWithRetry(e, id);
    if (token !== loadToken) return; // superseded by a newer navigation — drop this render
    currentToy = id;

    const [doc, surfaceEntry] = await Promise.all([
      fetch(asset(`instruments/${id}.json`)).then((r) => {
        if (!r.ok) throw new Error(`fetch ${id}.json: HTTP ${r.status}`);
        return r.json();
      }),
      fetchSurfaceDoc(id),
    ]);
    if (token !== loadToken) return;

    currentInstrument = doc.instrument;
    const { surface } = resolveSurfaceOrDefault(doc, surfaceEntry, id);
    logSurfaceWarnings(id, surface.warnings);
    currentSurface = surface;
    currentSpineEngine = e;
    // Render the controls into the node-identity board, then fire the load-time defaults AFTER
    // load() resolved (the render.mjs lifecycle) exactly as the player does.
    spine.board.update(surface.widgets, e);
    sendInitialDefaults(surface, e);
    document.body.dataset.state = e.context.state; // "running" — the spec asserts on this

    // A live-input Toy (Mic Space) loads and renders but plays SILENCE until the user enables the
    // mic on a gesture (#248). Its pick MUST surface the enable control — otherwise the pick is a
    // silent dead end and the greeting's "enable the microphone to play" points at nothing. Mount
    // the SAME micControl the player uses into the spine's mic slot, keyed on inputChannels (not a
    // per-Toy flag), with a small status line for the live/denied feedback the control writes.
    if (info?.inputChannels > 0) {
      const micStatus = h("p", { class: "spine-mic-status" });
      spine.micSlot.replaceChildren(micControl(e, micStatus), micStatus);
    }

    const toy = TOYS.find((t) => t.id === id);
    onReady?.(spine, { id, title: toy?.title ?? currentInstrument, kind: toy?.kind });
  } catch (err) {
    if (token !== loadToken) return;
    // The full failure taxonomy is the ambiguity/failure ticket's (spec §5, #303); the engine
    // reason is NEVER shown to the user (spec §5.1). Log it, show a neutral lexicon-clean line.
    console.error("[spine] instrument load failed:", err);
    spine.transcript.push({
      role: "reuben",
      text: "Something went wrong starting the sound. Give it another try in a moment.",
    });
  }
}

// The lead clause of a pick's proactive turn one, kind-aware so it NEVER claims a state that
// isn't real (the epic's honesty gate). Mirrors the player's per-kind split (playingStatusText):
// a self-playing Toy IS emitting sound; a tap-to-play Toy is silent until a button is tapped; a
// live-input (mic) Toy makes NO sound at all until the mic is enabled on a gesture. Sensory-only,
// forbidden-word-clean. `kind` comes from the Toy manifest (self-playing | tap-to-play |
// live-input); an unknown/absent kind degrades to the self-playing lead.
function greetingLead(title, kind) {
  if (kind === "live-input") return `${title}'s ready — enable the microphone to play`;
  if (kind === "tap-to-play") return `${title}'s ready — tap the buttons to play`;
  return `${title}'s playing`;
}

// Gallery pick → proactive turn one (spec §2.3): starts the Toy (self-playing sounds immediately;
// tap-to-play waits for a tap; live-input waits for the mic) and opens the spine with a short,
// kind-aware greeting that names it honestly + the Toy's authored chips (or greeting-only when
// `chips` is absent/empty — tailored-or-nothing, no generic filler, §I). Lands COLLAPSED-to-bar
// (arrival "picked", spec §3.3) — a newcomer just picked something to PLAY, not to talk about.
function pickToy(toy) {
  const chips = toy.chips ?? [];
  const lead = greetingLead(toy.title, toy.kind);
  const greeting = chips.length
    ? `${lead}. Tell me what to change — or try one of these:`
    : `${lead}. Tell me what to change.`;
  const seed = [{ role: "reuben", text: greeting }];
  if (chips.length) seed.push({ role: "reuben", kind: "chips", chips });
  return openSpine({ toyId: toy.id, arrival: "picked", seed });
}

// Describe-path → echo → build → play → land (spec §2.4): the user's words become their first
// chat message immediately (`seed`, so the echo doesn't wait on the engine), reuben "builds" it,
// it starts playing, then reuben closes the turn by naming what it made + inviting a next change
// — symmetric with `pickToy`'s landing. Lands EXPANDED (arrival "made", spec §3.3): the user was
// just talking, so the transcript stays open.
//
// SCOPE (issue #357): real text → NEW-instrument generation needs the agent host wired
// end-to-end (#354's agent-host.mjs + #356's system prompt + #358's change-card renderer) — none
// of that is wired into the browser yet (this ticket wires the SCREEN + the launcher→player
// continuity, not the agent). So this is a SEAM: it "builds" by loading the default Toy (today's
// closest thing to a first playable instrument). The landing line MUST NOT claim it built what
// the user described (the honesty gate) — until the agent lands, it can't. So it names what's
// actually playing and frames it as a starting point, kind-aware and lexicon-clean. Swap the body
// of this function for a real agent call once the loop is wired in.
function submitDescribe(text) {
  return openSpine({
    toyId: DEFAULT_TOY,
    arrival: "made",
    seed: [{ role: "you", text }],
    onReady: (spine, { title, kind }) => {
      spine.transcript.push({
        role: "reuben",
        text: `To get you started, ${greetingLead(title, kind)}. Tell me what to change.`,
      });
    },
  });
}

// The Playwright test surface for the spine (issue #355 verification) — analogous to
// window.reubenPlayer. Exposes the arrival state, the sheet + turn controls, the node-identity
// board readout, and the two re-render paths the node-identity test contrasts: an identity-
// preserving reshape (positions MUST stay stable) versus the fresh-sort anti-pattern (positions
// MUST move). Not load-bearing for the app. A deterministic reversal stands in for a "shuffle" —
// for any surface with ≥2 controls a reversed order is guaranteed different from the original.
function exposeSpineTestHook(spine) {
  const reversedWidgets = () => [...(currentSurface?.widgets ?? [])].reverse();
  // The change-card controller a driver (a test, or #354's real loop) is currently steering. Held
  // so the id-based reshape hooks below (begin → appendPlan → resolve) act on ONE card, mirroring
  // how the agent host drives a single turn envelope through its lifecycle.
  let reshapeCard = null;
  window.reubenChat = {
    screen: () => document.body.dataset.screen,
    arrival: () => spine.screen.dataset.arrival,
    sheetExpanded: () => spine.sheetExpanded(),
    toggleSheet: (force) => spine.toggleSheet(force),
    turnInFlight: () => spine.turnInFlight(),
    beginMockTurn: (t) => spine.beginMockTurn(t ?? "make it brighter"),
    endMockTurn: () => spine.endMockTurn(),
    boardNodes: () => spine.board.nodes(),
    keepSlotPresent: () => !!spine.screen.querySelector('[data-slot="keep"]'),
    // The Enable-microphone affordance a live-input pick mounts (#248/#357) — present ONLY when
    // the loaded instrument takes the mic, so its absence for other kinds is meaningful.
    micEnablePresent: () => !!spine.screen.querySelector('[data-slot="mic"] .mic-enable'),
    // Identity-preserving re-render: the SAME nodes in a DIFFERENT input order. Survivors hold
    // position (spec §3.6), so the board order MUST be unchanged after this.
    reshapePreserveIdentity: () => {
      if (currentSurface && currentSpineEngine) {
        spine.board.update(reversedWidgets(), currentSpineEngine);
      }
    },
    // The negative control: the fresh-sort anti-pattern §3.6 forbids. Positions follow the
    // (reversed) input, so the board order MUST change — proving the stability assertion has teeth.
    resortRebuild: () => {
      if (currentSurface && currentSpineEngine) {
        spine.board.__resortRebuild(reversedWidgets(), currentSpineEngine);
      }
    },

    // --- the change-card + surface highlights (spec §4, issue #358) --------------------------
    // The NODE addresses the current board's controls back (control "/cutoff/in" → node "/cutoff").
    // A test picks a REAL one so a crafted diff's highlight lands on an on-screen control (the live
    // surface sweep), and a made-up one to exercise the no-knob path (§4.3).
    controlNodes: () => spine.board.nodes().map((n) => nodeOfControl(n.control)),

    // Drive one change-card through its lifecycle (spec §4.2): begin (thinking) → appendPlan (stream)
    // → resolve (rows + surface highlight). Id-based so a Playwright test can step it across
    // page.evaluate calls, exactly as #354's agent host will step the turn envelope.
    reshapeBegin: () => {
      reshapeCard = spine.beginReshapeCard();
      return reshapeCard.turn.id;
    },
    reshapeAppendPlan: (text) => reshapeCard?.appendPlan(text),
    // §6.1 param-only resolve: live sweep, no gap, no restart line (a param reshape never restarts).
    reshapeResolve: (diff) => reshapeCard?.resolve(diff),
    // §6.2 structural re-strike: the declicked duck + co-timed commit + replay-from-top. Returns the
    // duck's promise so a Playwright test can await the gap. `honesty` is the first-run-only restart
    // line the envelope carries (#356's content/gate); `sounding=false` exercises §6.4's build-ready
    // path (nothing playing → no duck, no reset, no line). Async, so the driver awaits the gesture.
    reshapeRestrike: (diff, honesty, opts) => reshapeCard?.restrike(diff, honesty, opts),

    // Card introspection (transcript half). All read the ONE card element, so "same object across
    // resolve" is observable: the count stays 1 through thinking → resolved.
    cardCount: () => document.querySelectorAll(".tx-card").length,
    cardState: () => document.querySelector(".tx-card")?.dataset.cardState ?? null,
    cardTurnId: () => document.querySelector(".tx-card")?.dataset.turnId ?? null,
    cardPlan: () => document.querySelector(".tx-card .tx-card-plan")?.textContent ?? "",
    cardRows: () =>
      [...document.querySelectorAll(".tx-card .tx-card-row .tx-card-row-text")].map(
        (r) => r.textContent,
      ),
    cardHeadline: () => document.querySelector(".tx-card .tx-card-headline")?.textContent ?? null,
    cardRowsExpanded: () =>
      document.querySelector(".tx-card .tx-card-rows")?.dataset.expanded === "true",
    cardHonesty: () => document.querySelector(".tx-card .tx-card-honesty")?.textContent ?? "",

    // Surface half: which controls a landed reshape actually swept/pulsed/removed (node identity),
    // and the live count of controls currently echo-highlighted by a hovered row (§4.1).
    lastHighlight: () => spine.board.lastHighlight(),
    echoedCount: () => document.querySelectorAll('.board-cell[data-echo="on"]').length,

    // --- the re-strike (spec §6, issue #360) -------------------------------------------------
    // The transport's replay-from-top counter: it increments once per structural re-strike, so a
    // test proves the playhead visibly reset (spec §6.2.2) exactly when — and only when — one fired.
    // (The honesty-slot text is read via the existing `cardHonesty` hook above.)
    transportRestrikeSeq: () => spine.transport.restrikeSeq(),
    // §6.2.3 negative control: any loading/spinner chrome anywhere in the spine. Must stay 0 — the
    // gap is a beat, not a wait, so the re-strike shows NO spinner. Matches a spinner class, an
    // aria-busy region, or literal "loading" text.
    loadingChromeCount: () => {
      const spinners = spine.screen.querySelectorAll('.spinner, [role="progressbar"], [aria-busy="true"]').length;
      const loadingText = /\bloading\b/i.test(spine.screen.textContent || "") ? 1 : 0;
      return spinners + loadingText;
    },
  };
}

// --- fragment boot (share links, issue #228) ----------------------------------------------

// A shared link (`#r1.…`) IS an instrument: decode it, then boot it on ONE tap (iOS needs the
// gesture). decode needs no audio, so it runs at boot; a decode failure lands on the launcher
// with the mapped banner (classes B–E′), never a splash the reader can't leave.
//
// M1 chat-flag scope (deliberate, not a bug): this path mounts the OLD player even when the chat
// flag is on. The spine (issue #355) covers only the splash → default-instrument path; routing a
// shared link INTO the spine belongs with the share-into-spine / Keep (M2) work, so a link stays
// on the player for now.
async function fragmentBoot(hash) {
  let bundle;
  try {
    bundle = await decodeBundle(hash);
  } catch (err) {
    const code = err instanceof ShareError ? err.code : null;
    showLauncher(SHARE_ERROR_BANNER[code] ?? "This link is damaged.");
    return;
  }

  // One Start/Play button — the single gesture that unlocks audio (ctx.resume) AND boots the
  // instrument. loadBundle+render is wrapped so a bad document lands on the launcher, not here.
  async function onPlay() {
    start.disabled = true;
    start.textContent = "Starting…";
    try {
      const e = await ensureEngine();
      await e.context.resume();
      journal.clear(); // reflect only the instrument about to boot
      instrumentJournal(e);

      const info = await e.loadBundle({ docText: bundle.docText, resources: bundle.resources });
      // Safe to parse — loadBundle already parsed it (a JSON failure would have thrown above).
      const doc = JSON.parse(bundle.docText);

      currentToy = null;
      currentInstrument = doc.instrument;
      // Surface resolution for a link (ADR-0042 A2 / ADR-0043 §5, share target): the bundle's
      // embedded surface ?? the origin's surfaces/<instrument>.web.json ?? surfaces/
      // <instrument>.json (the origin rungs upgrade links minted before surfaces travelled)
      // ?? the auto-derived default. Every rung dark-degrades — including an embedded doc the
      // RESOLVER refuses (not just a parse failure), which falls to the origin rungs rather
      // than shadowing them. `??=` short-circuits, so the origin fetch only runs when needed.
      let resolved = tryResolveSurface(doc, parseEmbeddedSurface(bundle.surfaceText), doc.instrument);
      resolved ??= tryResolveSurface(doc, await fetchSurfaceDoc(doc.instrument), doc.instrument);
      resolved ??= { surface: buildSurface(doc, null), surfaceText: null };
      const { surface, surfaceText } = resolved;
      logSurfaceWarnings(doc.instrument, surface.warnings);
      currentSurface = surface;
      currentSurfaceDocText = surfaceText;
      // Fold the sidecar into the widgets' rest values BEFORE rendering, so the surface shows
      // the state it will be playing (a shared step pattern lights its toggles). A buffer the
      // decoder can't read is skipped — the verbatim engine replay below stays the authority.
      applySnapshotDefaults(
        surface,
        bundle.snapshot.flatMap((buf) => {
          try {
            return [decodeControl(buf)];
          } catch (err) {
            console.warn("[player] undecodable snapshot entry, widgets may not reflect it:", err);
            return [];
          }
        }),
      );
      const kind = deriveKind(surface, info);

      // A shared instrument that IS a launcher Toy reuses its manifest title/blurb — the
      // short, player-facing copy the launcher cards show. An unknown/custom instrument gets
      // title only: the document's `doc` field is the agent-facing spec (ADR refs, pipe
      // addresses), never player copy.
      const toy = TOYS.find((t) => t.id === doc.instrument);
      const { screen, surfaceEl, status, skeleton, body, shareBtn } = buildPlayerScreen({
        nameForData: doc.instrument,
        title: toy?.title ?? doc.instrument,
        blurb: toy?.blurb,
        kind,
      });
      setScreen("player", screen);

      renderSurface(surface, e, surfaceEl);
      // Defaults FIRST, then the snapshot overrides them: a moved control replayed verbatim from
      // the link's sidecar wins over its load-time default.
      sendInitialDefaults(surface, e);
      for (const buf of bundle.snapshot) e.sendRaw(buf);

      skeleton.remove();
      document.body.dataset.state = e.context.state;

      const takesInput = info?.inputChannels > 0;
      if (takesInput) body.prepend(micControl(e, status));
      status.textContent = playingStatusText(takesInput, kind, surface.widgets.length > 0);
      status.classList.remove("error");
      shareBtn.disabled = false;
    } catch (err) {
      // loadBundle failures map to the taxonomy: a share-shaped error (defensive) → its banner;
      // a bundle miss (code "incomplete") → class I; anything else (invalid JSON / version /
      // unknown operator, classes F/G/H) → the engine's VERBATIM message, shown as-is.
      let message;
      if (err instanceof ShareError) message = SHARE_ERROR_BANNER[err.code] ?? "This link is damaged.";
      else if (err?.code === "incomplete") message = "This link is incomplete.";
      else message = err?.message || String(err);
      showLauncher(message);
    }
  }

  const start = h("button", { class: "cta", id: "start", type: "button", onclick: onPlay }, "Play");
  const hero = h(
    "section",
    { class: "splash boot-splash" },
    iiMark(),
    h("h1", { class: "wordmark" }, "reuben"),
    h("p", { class: "tagline" }, "Someone shared an instrument with you."),
    start,
    h("p", { class: "splash-hint" }, "Tap Play to hear it."),
    infoLink(),
  );
  setScreen("splash", hero);
}

// --- shared bits --------------------------------------------------------------------------

function infoLink() {
  return h(
    "a",
    { class: "info-link", href: REPO_URL, target: "_blank", rel: "noopener" },
    "What is this?",
  );
}

// The persistent Impractical Instruments brand mark — the inked lightbulb-press badge, shown in
// every screen's header (and top-left on the splash) so the maker's stamp is always present.
function iiMark() {
  return h(
    "a",
    {
      class: "ii-mark",
      href: "https://impracticalinstruments.com",
      target: "_blank",
      rel: "noopener",
      "aria-label": "Impractical Instruments",
    },
    h("img", { src: iiBadge, alt: "Impractical Instruments", width: "40", height: "40" }),
  );
}

// --- boot ---------------------------------------------------------------------------------

// A tiny test surface for the Playwright smoke (issue #226 scope 7) + share (issue #228):
// current screen, loaded Toy, the live AudioContext state, the launcher banner text, the booted
// instrument name (Toy card OR link), and the last Share URL. Not load-bearing for the app.
window.reubenPlayer = {
  screen: () => document.body.dataset.screen,
  toy: () => currentToy,
  instrument: () => currentInstrument,
  banner: () => currentBanner,
  shareUrl: () => lastShareUrl,
  engineState: () => engine?.context.state ?? "none",
  // The resolved widget list (kind/widget/bind per widget), for launcher-vs-link parity checks.
  surface: () => currentSurface?.widgets.map(({ kind, widget, bind }) => ({ kind, widget, bind })) ?? null,
  toys: TOYS,
  // Gallery / cold-start test hooks (issue #357 verification): drive a pick or a describe-bar
  // submit directly, the SAME functions the gallery's DOM calls. Lets a spec exercise a synthetic
  // Toy (e.g. one with `chips: []`, proving the greeting-only fallback) without editing toys.json.
  pickToy: (toy) => pickToy(toy),
  submitDescribe: (text) => submitDescribe(text),
};

// Register the service worker (issue #227): precache the whole payload on first load so a cold,
// offline, home-screen launch boots and plays. `immediate` registers now rather than waiting for
// the window `load` event — the engine boot below is the heavy work, not the SW install, and an
// early register means the payload is caching while the player reads the splash. `virtual:pwa-
// register` is a build-time no-op under `vite dev` (SW disabled there); it's real under the built
// app the Playwright smoke and Cloudflare serve. autoUpdate (vite.config) swaps in new revisions
// silently, so there's no update-prompt callback to wire here.
registerSW({ immediate: true });

ensureEngine().catch((err) => console.error("[player] engine boot failed:", err));
prefetchToy(DEFAULT_TOY);

// Hash routing (issue #228). A `#r1.…` fragment IS a shared instrument → fragment boot, skipping
// the launcher. Anything else (empty, `#about`) → the ordinary splash flow, SILENTLY (class A, no
// banner). A `hashchange` fires when a link is PASTED/edited into an open tab (Share uses
// history.replaceState, which does NOT fire it — so a Share can't re-boot): a fragment re-boots,
// an empty hash shows the launcher.
function isFragment(hash) {
  return /^r\d+\./.test(hash);
}

window.addEventListener("hashchange", () => {
  const hash = location.hash.slice(1);
  if (isFragment(hash)) fragmentBoot(hash);
  // An empty-hash landing (issue #357): the gallery when the chat flag is on, the OLD launcher
  // otherwise — the same chat-flag branch `onStart` uses, kept in sync here.
  else if (chatEnabled()) showGallery();
  else showLauncher();
});

const bootHash = location.hash.slice(1);
if (isFragment(bootHash)) fragmentBoot(bootHash);
else setScreen("splash", splashScreen());
