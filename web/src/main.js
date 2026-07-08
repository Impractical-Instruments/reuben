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
import { createReubenEngine } from "../../crates/reuben-web/js/reuben-engine.mjs";
import { buildSurface, loadParamMeta } from "../../crates/reuben-web/js/surface/widget-model.mjs";
import { renderSurface, sendInitialDefaults } from "../../crates/reuben-web/js/surface/render.mjs";
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
let paramMetaPromise = null; // the parsed schema → param metadata, fetched once and cached
let loadToken = 0; // bumped per openToy so a stale in-flight load can't clobber a newer one
let currentToy = null; // id of the Toy currently loaded on the engine (null before first)

// Create the engine (adds the worklet module, fetches + compiles the wasm). Safe while the
// context is still suspended — it never resumes; that's the Start gesture's job. Kicked off
// at boot so the wasm is compiling while the player reads the splash.
function ensureEngine() {
  if (!enginePromise) {
    enginePromise = createReubenEngine({
      assetBase: asset("instruments"),
      wasmUrl: asset("reuben_web.wasm"),
    }).then((e) => {
      e.onLog = (t) => console.log(t);
      engine = e;
      return e;
    });
  }
  return enginePromise;
}

// The instrument schema drives fader ranges/units in buildSurface. Identical for every Toy,
// so fetch it once and cache the promise (a failure clears the cache so a retry re-fetches).
function getParamMeta() {
  if (!paramMetaPromise) {
    paramMetaPromise = fetch(asset("schema.json"))
      .then((r) => {
        if (!r.ok) throw new Error(`fetch schema.json: HTTP ${r.status}`);
        return r.json();
      })
      .then(loadParamMeta)
      .catch((err) => {
        paramMetaPromise = null;
        throw err;
      });
  }
  return paramMetaPromise;
}

// Warm the browser HTTP cache for a Toy's document + schema so the first engine.load(id) has
// nothing to fetch over the wire (only the worklet round-trip). Best-effort: failures are
// swallowed here and surfaced properly if the real load later hits them.
function prefetchToy(id) {
  fetch(asset(`instruments/${id}.json`)).catch(() => {});
  getParamMeta().catch(() => {});
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

function launcherScreen() {
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
        h(
          "span",
          { class: `toy-badge ${toy.kind}` },
          toy.kind === "self-playing" ? "self-playing" : "tap to play",
        ),
      ),
    ),
  );
  return h(
    "section",
    { class: "launcher" },
    h(
      "header",
      { class: "launcher-head" },
      h("h1", { class: "wordmark small" }, "reuben"),
      h("p", { class: "launcher-sub" }, "Pick a Toy."),
    ),
    grid,
    infoLink(),
  );
}

function showLauncher() {
  setScreen("launcher", launcherScreen());
}

// --- player -------------------------------------------------------------------------------

// Open a Toy: show the player screen immediately (skeleton, never a frozen blank), load it on
// the persistent engine, then mount its surface. A loadToken guards against a race where the
// player taps back + opens another Toy while a load is still in flight — only the newest token
// gets to render, and the engine's own one-load-at-a-time guard is respected by awaiting.
async function openToy(toy) {
  const token = ++loadToken;
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

  const screen = h(
    "section",
    { class: "player", dataset: { toy: toy.id } },
    h(
      "header",
      { class: "player-head" },
      h("button", { class: "back", type: "button", onclick: showLauncher }, "← Toys"),
      h("h1", { class: "player-title" }, toy.title),
      h("span", { class: `toy-badge ${toy.kind}` }, toy.kind === "self-playing" ? "self-playing" : "tap to play"),
    ),
    status,
    body,
  );
  setScreen("player", screen);

  try {
    const e = await ensureEngine();
    // engine.load is one-at-a-time; if a previous Toy's load is still settling, await it out
    // by retrying once it frees. In practice the back→pick path is sequential, but a fast
    // double-tap shouldn't throw "a load is already in flight" at the player. We await purely
    // to sequence + surface errors; the {channels, blockSize} it resolves isn't needed here.
    await loadWithRetry(e, toy.id);
    if (token !== loadToken) return; // superseded by a newer openToy — drop this render

    currentToy = toy.id;
    const [doc, paramMeta] = await Promise.all([
      fetch(asset(`instruments/${toy.id}.json`)).then((r) => {
        if (!r.ok) throw new Error(`fetch ${toy.id}.json: HTTP ${r.status}`);
        return r.json();
      }),
      getParamMeta(),
    ]);
    if (token !== loadToken) return;

    const surface = buildSurface(doc, paramMeta);
    renderSurface(surface, e, surfaceEl);
    // sendInitialDefaults ONLY after load() resolved (the worklet's `ready`): a control sent
    // before construct is dropped (render.mjs lifecycle note).
    sendInitialDefaults(surface, e);

    skeleton.remove();
    document.body.dataset.state = e.context.state; // "running" — the smoke asserts on this
    status.textContent =
      toy.kind === "tap-to-play"
        ? "Ready — tap the buttons to play."
        : surface.widgets.length
          ? "Playing — tweak the controls."
          : "Playing.";
    status.classList.remove("error");
  } catch (err) {
    if (token !== loadToken) return;
    skeleton.remove();
    status.classList.add("error");
    status.textContent = `Couldn't load ${toy.title} — ${err.message || err}.`;
    body.replaceChildren(
      status.cloneNode(true),
      h("button", { class: "cta small", type: "button", onclick: () => openToy(toy) }, "Retry"),
    );
  }
}

// engine.load, but if it rejects with the in-flight guard, wait a beat and try once more.
// Any other error propagates to openToy's catch (error + retry UI).
async function loadWithRetry(e, id) {
  try {
    return await e.load(id);
  } catch (err) {
    if (!/load is already in flight/.test(err.message || "")) throw err;
    await new Promise((r) => setTimeout(r, 50));
    return e.load(id);
  }
}

// --- shared bits --------------------------------------------------------------------------

function infoLink() {
  return h(
    "a",
    { class: "info-link", href: REPO_URL, target: "_blank", rel: "noopener" },
    "What is this?",
  );
}

// --- boot ---------------------------------------------------------------------------------

// A tiny test surface for the Playwright smoke (issue #226 scope 7): current screen, loaded
// Toy, and the live AudioContext state. Not load-bearing for the app itself.
window.reubenPlayer = {
  screen: () => document.body.dataset.screen,
  toy: () => currentToy,
  engineState: () => engine?.context.state ?? "none",
  toys: TOYS,
};

ensureEngine().catch((err) => console.error("[player] engine boot failed:", err));
prefetchToy(DEFAULT_TOY);
setScreen("splash", splashScreen());
