import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / the M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js so this suite can never drift from the per-ticket specs or the card's own gate
// (finding 4). Whole-word, case-insensitive — so "port" never trips on "important".
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";

// ============================================================================================
// THE ACCEPTANCE BAR (issue #362) — epic #350's TERMINAL acceptance gate.
//
// This is the LAST ticket: it VERIFIES the whole web-chat loop against the spec's §9 acceptance
// criteria and records the ADR-0052 §3 ship gate as satisfied. It builds NO product feature —
// every behavior it exercises was built by #351–#361 and already has its own focused per-ticket
// spec (gallery, spine, change-card, restrike, failure, keep, share, mic, pwa). Those prove each
// §9 criterion in ISOLATION; this suite proves the three things a slice-by-slice pass cannot:
//
//   1. THE WHOLE LOOP, END TO END, in one run, at BOTH phone and desktop viewports —
//      cold-start pick → play → reshape (parameter → send, structural → re-strike) → keep →
//      reload restore. The spec §9 loop as one continuous flow, not seven disjoint fragments.
//   2. THE CONSOLIDATED LEXICON GATE (spec §1 / §9.1, the HARD gate) — one sweep of the live
//      DOM/transcript across ALL states in a single session (cold-start, happy path, thinking,
//      failure, keep). The per-ticket specs each scan their own slice; this proves the surface
//      stays clean as the user walks the whole loop. (The real-model live-eval narration is a
//      SOFT/logged surface by a maintainer decision — agent-policy-eval.test.mjs; the automated
//      ACCEPTANCE guard is this DOM/transcript one, which must be clean.)
//   3. THE SHIP GATE (ADR-0052 §3 / spec §9.7) recorded explicitly: Keep is present AND wired
//      into the loop — the chat window does not ship before that, and here it is.
//
// The PERCEPTUAL halves of §9 — the re-strike landing honestly BY EAR (declicked, replay-from-top)
// and the transcript's tone read-through — are a scripted human pass in docs/rituals/
// web-chat-demo-bar.md (the epic's demo bar, analogous to the native #220/#324 bar). This suite
// automates everything the DOM reaches; that runbook covers the ear.
//
// Flag ON via `?chat=1` (chat/flag.js) throughout — the OFF-path launcher is smoke.spec.js's.
// ============================================================================================

const PHONE = { width: 390, height: 844 };
const DESKTOP = { width: 1280, height: 800 };

const manifest = JSON.parse(
  readFileSync(fileURLToPath(new URL("../toys.json", import.meta.url)), "utf8"),
);
// The demo-bar instrument is groovebox (order 1, self-playing → a live sound to re-strike against,
// and the app's default — its assets are prefetched, so the first pick is instant). It is the first
// gallery card, so `.toy-card` first == groovebox (asserted below, so the demo bar can't silently
// re-order out from under this suite).
const GROOVEBOX = [...manifest.toys].sort((a, b) => a.order - b.order)[0];

// Boot the built app into the co-presence spine at `viewport` by walking the REAL cold-start:
// Start gesture → gallery → pick the first card (groovebox). Returns once a control has rendered on
// the node-identity board and audio is running (the pick rode the Start gesture, so a structural
// re-strike has a live output to duck). Mirrors the per-ticket specs' bootSpine.
async function coldStartToSpine(page, viewport) {
  await page.setViewportSize(viewport);
  await page.goto("/?chat=1");
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");
  await page.locator(".toy-card").first().click();
  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
}

// Assert `text` carries no forbidden engine word (spec §1). Whole-word, case-insensitive.
function assertLexiconClean(text, where) {
  for (const word of FORBIDDEN) {
    const hit = new RegExp(`\\b${word}\\b`, "i").test(text);
    expect(hit, `forbidden word "${word}" found in ${where}:\n${text}`).toBe(false);
  }
}

// The full user-visible chat chrome — the transcript sheet + the pinned input dock — NOT the
// instrument's own control labels (which belong to the surface, not the chat lexicon; every
// per-ticket spec scopes its gate the same way).
async function chatChrome(page) {
  return page.evaluate(() => {
    const dock = document.querySelector(".spine-dock");
    const input = document.querySelector(".reshape-input");
    return [
      dock?.innerText ?? "",
      input?.getAttribute("placeholder") ?? "",
      input?.getAttribute("aria-label") ?? "",
    ].join("\n");
  });
}

// ============================================================================================
// 1. THE WHOLE LOOP, END TO END — cold-start → play → reshape (param + structural) → keep →
//    reload restore. At BOTH phone and desktop (spec §3.7 one responsive layout; §9's loop).
// ============================================================================================
for (const [name, viewport] of [["phone", PHONE], ["desktop", DESKTOP]]) {
  test(`the whole loop end to end at ${name} width: cold-start → play → reshape (param + structural) → keep → reload restore`, async ({
    page,
    context,
  }) => {
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);

    // --- COLD START → PLAY (spec §2, §9.2) ---------------------------------------------------
    // The demo-bar fixture can't silently re-order: the first gallery card IS groovebox.
    await page.goto("/?chat=1");
    await expect(page.locator("#start")).toBeVisible();
    await page.locator("#start").click();
    await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");
    expect(await page.locator(".toy-card").first().getAttribute("data-toy")).toBe(GROOVEBOX.id);

    await page.setViewportSize(viewport);
    await page.locator(".toy-card").first().click();
    await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
    await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
    // One tap → PLAYING sound: the self-playing groovebox is audible and the engine is running.
    expect(await page.evaluate(() => window.reubenPlayer.instrument())).toBe(GROOVEBOX.id);
    expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

    // THE SHIP GATE (ADR-0052 §3, spec §9.7): Keep is present AND wired into this loop from the
    // moment the spine mounts — the window does not ship without it, and here it is.
    expect(await page.evaluate(() => window.reubenChat.keepWired())).toBe(true);
    expect(await page.evaluate(() => window.reubenChat.keepState())).toBe("Not kept yet");
    // An untouched gallery pick is re-findable → not diverged, no pulse yet.
    expect(await page.evaluate(() => window.reubenChat.keepDiverged())).toBe(false);

    const nodes = await page.evaluate(() => window.reubenChat.controlNodes());
    expect(nodes.length, "groovebox exposes ≥2 controls to reshape").toBeGreaterThan(1);
    const [paramNode, structNode] = nodes;

    // --- RESHAPE, PART A: PARAMETER ("darker") → send, live, NO phase reset (spec §6.1, §9.6) ---
    await page.evaluate(() => window.reubenChat.reshapeBegin());
    await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Darkening the tone."));
    await page.evaluate(
      (n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }),
      paramNode,
    );
    // The control swept live on the surface; the transport did NOT restart (a param reshape is a
    // gapless send, ADR-0048 §6.1) and it carries no restart line.
    expect((await page.evaluate(() => window.reubenChat.lastHighlight())).changed).toContain(paramNode);
    expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Reshaped a control"]);
    expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(0);
    expect(await page.evaluate(() => window.reubenChat.cardHonesty())).toBe("");
    // The first landed reshape DIVERGED the instrument → Keep pulsed exactly once (spec §7.5).
    expect(await page.evaluate(() => window.reubenChat.keepDiverged())).toBe(true);
    expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1);

    // --- RESHAPE, PART B: STRUCTURAL ("add a shimmer") → re-strike (spec §6.2, §9.6) ----------
    // The declicked duck resolves when the sound returns; await the whole gesture. The envelope
    // carries the first-run-only honesty line (#356's once-per-session gate produces it).
    const HONESTY = "Here's the new version, from the top.";
    expect(await page.evaluate(() => window.reubenChat.loadingChromeCount())).toBe(0);
    await page.evaluate(() => window.reubenChat.reshapeBegin());
    await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Adding a shimmering layer."));
    await page.evaluate(
      (a) => window.reubenChat.reshapeRestrike({ added: [a.node], changed: [], removed: [] }, a.line),
      { node: structNode, line: HONESTY },
    );
    // Read the SECOND (structural) card specifically — the loop now holds two cards, so the
    // single-card `cardState`/`cardRows`/`cardHonesty` hooks (first/all cards) don't isolate it.
    const structCard = await page.evaluate(() => {
      const cards = document.querySelectorAll(".tx-card");
      const card = cards[cards.length - 1];
      return {
        state: card?.dataset.cardState ?? null,
        rows: [...card.querySelectorAll(".tx-card-row .tx-card-row-text")].map((r) => r.textContent),
        honesty: card.querySelector(".tx-card-honesty")?.textContent ?? "",
      };
    });
    // Co-timed cause (§6.2.1): the card committed AT the drop. Replay-from-top (§6.2.2): the
    // playhead visibly returned to the start exactly once. NO spinner over the gap (§6.2.3).
    expect(structCard.state).toBe("resolved");
    expect(structCard.rows).toEqual(["Added a new layer"]);
    expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(1);
    expect(await page.evaluate(() => window.reubenChat.loadingChromeCount())).toBe(0);
    // The one-time honesty line shows on the first structural restart (§6.4).
    expect(structCard.honesty).toBe(HONESTY);

    // --- KEEP (spec §7, §9.7) → hash write + copy the link -----------------------------------
    await page.evaluate(() => window.reubenChat.tapKeep());
    await expect.poll(() => page.evaluate(() => window.reubenChat.keepState())).toBe("Kept ✓");
    expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(true);
    await expect.poll(() => page.evaluate(() => location.hash)).toMatch(/^#r1\./);
    // The link was copied to the clipboard and equals the hash-persisted URL (spec §7.6).
    const clip = await page.evaluate(() => navigator.clipboard.readText());
    expect(clip).toContain(await page.evaluate(() => location.hash));
    // Kept work is not at risk: the leave-guard is disarmed.
    expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(false);

    // --- RELOAD RESTORE (spec §7.6): the written fragment reboots the instrument on one tap ----
    await page.reload();
    await expect(page.locator("#start")).toBeVisible();
    await page.locator("#start").click();
    await expect.poll(() => page.evaluate(() => window.reubenPlayer.instrument())).toBe(GROOVEBOX.id);

    expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
  });
}

// ============================================================================================
// 2. THE CONSOLIDATED LEXICON GATE (spec §1 / §9.1, the HARD gate). One session, one sweep of the
//    live DOM/transcript across EVERY state the user walks — cold-start, happy path, thinking,
//    failure, keep. No forbidden engine word may surface in ANY of them.
// ============================================================================================
test("the lexicon gate holds across every state in one session: cold-start, happy path, thinking, failure, keep", async ({
  page,
  context,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);

  await coldStartToSpine(page, DESKTOP);
  await page.evaluate(() => window.reubenChat.toggleSheet(true)); // reveal transcript copy

  // --- STATE: cold-start / turn-one greeting + authored chips (happy path, spec §2.3) ---------
  assertLexiconClean(await chatChrome(page), "the cold-start greeting + chips");

  const [node] = await page.evaluate(() => window.reubenChat.controlNodes());

  // --- STATE: THINKING — a change-card mid-flight, plan streaming in (spec §4.2) ---------------
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Warming and rounding the tone."));
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("thinking");
  assertLexiconClean(await chatChrome(page), "the thinking state (card mid-flight)");

  // --- STATE: AMBIGUOUS — best-effort card with a 'how I read it' line + alt chips (spec §5.1) -
  await page.evaluate(() =>
    window.reubenChat.reshapeSetReading("I read 'warmer' as a rounder, mellower tone."),
  );
  await page.evaluate(() =>
    window.reubenChat.reshapeSetAlternatives([
      { id: "fade", label: "more of a slow fade-in" },
      { id: "room", label: "cozier, roomier space" },
    ]),
  );
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);
  assertLexiconClean(await chatChrome(page), "the resolved ambiguous card (reading + alternatives)");

  // --- STATE: FAILURE — a chat-turn carrying engine words is DROPPED, never rendered unclean ---
  // The §1 gate is an acceptance gate in EVERY state: a tripping line/chip is dropped, a clean one
  // alongside still renders. This exercises the failure surface's worst case (a leak attempt).
  await page.evaluate(() =>
    window.reubenChat.chatReply({
      text: "I re-wired the patch and swapped the operator for you.",
      chips: ["route it through the voicer", "make it brighter"],
    }),
  );
  const chromeAfterFailure = await chatChrome(page);
  assertLexiconClean(chromeAfterFailure, "the failure/chat-turn state");
  // Proof the gate had teeth: the tripping line was dropped and only the clean chip survived.
  expect(chromeAfterFailure.includes("re-wired")).toBe(false);
  await expect(page.locator(".transcript .tx-chip", { hasText: "make it brighter" })).toHaveCount(1);
  await expect(page.locator(".transcript .tx-chip", { hasText: "route it through" })).toHaveCount(0);

  // --- STATE: KEEP — the Keep confirm + the ask-to-save chat line (spec §7) --------------------
  await page.evaluate(() => window.reubenChat.tapKeep());
  await expect(page.locator(".keep-confirm-text")).toBeVisible();
  await page.evaluate(() => window.reubenChat.askToSave());
  assertLexiconClean(await chatChrome(page), "the keep state (confirm + ask-to-save)");
  // The Keep chrome itself (the button + its state + the confirm) is clean too.
  const keepCopy = await page.evaluate(() => {
    const keep = document.querySelector('[data-slot="keep"]');
    const confirm = document.querySelector(".keep-confirm");
    return [keep?.innerText ?? "", confirm?.innerText ?? ""].join("\n");
  });
  assertLexiconClean(keepCopy, "the Keep control + confirm");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// ============================================================================================
// 3. THE SHIP GATE, recorded explicitly (ADR-0052 §3 / spec §9.7). The chat window does not ship
//    to users before a Keep gesture is wired into its loop. This asserts the gate is SATISFIED:
//    Keep is present AND wired (a real button in the reserved bottom-chrome slot) at spine arrival,
//    and it performs — a tap writes the durable snapshot to the hash. If this test ever goes red,
//    the epic is NOT shippable.
// ============================================================================================
test("SHIP GATE (ADR-0052 §3): Keep is present, wired into the loop, and performs — the gate is satisfied", async ({
  page,
  context,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);

  await coldStartToSpine(page, DESKTOP);

  // PRESENT AND WIRED: a real Keep button lives in the reserved bottom-chrome slot (spec §7.3).
  expect(await page.evaluate(() => window.reubenChat.keepWired())).toBe(true);
  await expect(page.locator('[data-slot="keep"] .keep-btn')).toBeVisible();

  // PERFORMS: the user's tap writes the durable snapshot to location.hash (the keep gesture every
  // kept swap pairs with, ADR-0052 §3). Before the tap the hash is empty; after, it carries `#r1.`.
  expect(await page.evaluate(() => location.hash)).toBe("");
  await page.evaluate(() => window.reubenChat.tapKeep());
  await expect.poll(() => page.evaluate(() => location.hash)).toMatch(/^#r1\./);
  expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(true);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// ============================================================================================
// 4. REGISTER (spec §8 / §9.8): the session starts `plain` and all STATIC starter content is
//    plain — the binary never demotes and only ratchets up on unprompted user theory vocabulary,
//    which is a live-model behavior (agent-policy-eval.test.mjs, the soft surface). What is
//    DOM-observable and static here is the cold-start greeting + authored chips: they must lead
//    with sensory language, carrying none of the theory-side vocabulary of §1.2's plain↔theory
//    pairs. (This is a REPRESENTATIVE sample of theory jargon, not a source of truth — the
//    authoritative pair list lives in crates/reuben-web/proxy/system-prompt.mjs PLAIN_THEORY_PAIRS;
//    the point is that plain starter copy leads sensory, never with key/tempo/BPM/etc.)
// ============================================================================================
test("register (spec §8.4): the static cold-start starter content is plain — no theory-side jargon", async ({
  page,
}) => {
  await coldStartToSpine(page, DESKTOP);
  await page.evaluate(() => window.reubenChat.toggleSheet(true));

  const starter = await page.evaluate(() => {
    const greetings = [...document.querySelectorAll('.transcript .tx-entry[data-role="reuben"] .tx-text')]
      .map((el) => el.textContent)
      .join("\n");
    const chips = [...document.querySelectorAll(".transcript .tx-chip")].map((el) => el.textContent).join("\n");
    return `${greetings}\n${chips}`;
  });

  // The theory SIDE of §1.2's plain↔theory pairs (a representative sample). Plain starter content
  // leads with the sensory side and must carry none of these.
  const THEORY_JARGON = [
    "tempo", "bpm", "key", "chord", "scale", "arpeggio", "octave", "semitone", "cutoff",
    "resonance", "envelope", "lfo", "oscillator", "reverb", "filter", "frequency",
  ];
  for (const term of THEORY_JARGON) {
    const hit = new RegExp(`\\b${term}\\b`, "i").test(starter);
    expect(hit, `theory jargon "${term}" in plain starter content:\n${starter}`).toBe(false);
  }
  // And it is engine-lexicon clean too (the §1 gate applies to static content as much as dynamic).
  assertLexiconClean(starter, "the static starter content");
});
