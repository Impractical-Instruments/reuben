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
import { buildSurface, initial } from "../../crates/reuben-web/js/surface/widget-model.mjs";
import { renderSurface, sendInitialDefaults } from "../../crates/reuben-web/js/surface/render.mjs";
import { encodeControl } from "../../crates/reuben-web/js/codec.mjs";
import { decodeBundle, encodeBundle, ShareError } from "../../crates/reuben-web/js/share.mjs";
import manifest from "../toys.json";

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
let currentBanner = null; // the launcher banner text (null = none), read by the test hook
let currentShareBtn = null; // the current player's Share button, for its "Copied!" flash
let currentShareSlot = null; // the current player's slot for the hand-copy fallback textarea
let lastShareUrl = null; // the URL minted by the most recent Share (test hook)

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

// The ADR-0043 §5 resolution order for target `web`:
//   surfaces/<id>.web.json ?? surfaces/<id>.json ?? null (⇒ buildSurface auto-derives from
//   the instrument's interface pipes).
// A 404 falls through to the next candidate SILENTLY — most Toys ship only the base doc, and a
// doc-less Toy is fine by design. Any other failure (network drop, bad JSON) logs and also
// falls through: a broken surface file degrades to the auto-derived default rather than
// killing the load (dark-degrade, ADR-0016).
const SURFACE_CANDIDATES = (id) => [`surfaces/${id}.web.json`, `surfaces/${id}.json`];

async function fetchSurfaceDoc(id) {
  for (const candidate of SURFACE_CANDIDATES(id)) {
    try {
      const r = await fetch(asset(candidate));
      if (r.status === 404) continue; // no such surface doc — try the next candidate
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      return await r.json();
    } catch (err) {
      console.warn(`[player] surface doc ${candidate} unusable, falling through:`, err);
    }
  }
  return null;
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

function h(tag, props = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (k === "class") el.className = v;
    else if (k === "dataset") Object.assign(el.dataset, v);
    else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else if (v != null) el.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c == null) continue;
    el.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return el;
}

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
      showLauncher();
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

function launcherScreen(bannerMessage) {
  const grid = h(
    "div",
    { class: "toy-grid" },
    TOYS.map((toy) =>
      h(
        "button",
        {
          class: "toy-card",
          type: "button",
          dataset: { toy: toy.id, kind: toy.kind },
          onclick: () => openToy(toy),
        },
        h("span", { class: "toy-title" }, toy.title),
        h("span", { class: "toy-blurb" }, toy.blurb),
        h("span", { class: `toy-badge ${toy.kind}` }, badgeText(toy.kind)),
      ),
    ),
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
    if (widget.kind !== "fader" && widget.kind !== "param-toggle") continue;
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
    const [doc, surfaceDoc] = await Promise.all([
      fetch(asset(`instruments/${toy.id}.json`)).then((r) => {
        if (!r.ok) throw new Error(`fetch ${toy.id}.json: HTTP ${r.status}`);
        return r.json();
      }),
      fetchSurfaceDoc(toy.id),
    ]);
    if (token !== loadToken) return;

    currentInstrument = doc.instrument;
    const surface = buildSurface(doc, surfaceDoc);
    logSurfaceWarnings(toy.id, surface.warnings);
    currentSurface = surface;
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

// --- fragment boot (share links, issue #228) ----------------------------------------------

// A shared link (`#r1.…`) IS an instrument: decode it, then boot it on ONE tap (iOS needs the
// gesture). decode needs no audio, so it runs at boot; a decode failure lands on the launcher
// with the mapped banner (classes B–E′), never a splash the reader can't leave.
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
      // A shared bundle carries no surface file — buildSurface(doc, null) auto-derives the
      // default surface from the document's interface pipes (ADR-0043 §3).
      const surface = buildSurface(doc, null);
      logSurfaceWarnings(doc.instrument, surface.warnings);
      currentSurface = surface;
      const kind = deriveKind(surface, info);

      const { screen, surfaceEl, status, skeleton, body, shareBtn } = buildPlayerScreen({
        nameForData: doc.instrument,
        title: doc.instrument,
        blurb: doc.doc,
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
  toys: TOYS,
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
  else showLauncher();
});

const bootHash = location.hash.slice(1);
if (isFragment(bootHash)) fragmentBoot(bootHash);
else setScreen("splash", splashScreen());
