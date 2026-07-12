import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js so this spec and the card's own gate can never drift. Whole-word, case-insensitive.
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";

// The re-strike spec (issue #360 verification, spec §6). Boots the BUILT app into the co-presence
// spine (the same `?chat=1` → gallery → pick path change-card.spec.js uses) and drives CRAFTED turn
// envelopes through the reshape hooks (the seam #354's real agent loop will drive identically).
// Asserts the §6 observables:
//   1. a STRUCTURAL reshape re-strikes: the change-card commits, the playhead visibly returns to the
//      start (§6.2.2), and NO spinner appears over the gap (§6.2.3);
//   2. a PARAMETER reshape moves the control live — no gap, NO phase reset (the playhead does not
//      restart) (§6.1);
//   3. the FIRST structural restart per session shows the honesty line and a later one does not —
//      driven by crafted envelopes carrying / omitting the line the #356 gate produces (§6.4);
//   4. a NOT-currently-sounding structural change just builds ready: no playhead reset, no line (§6.4);
//   5. NO forbidden engine word in any re-strike text (§1 lexicon gate).
//
// SCRIPTED-HUMAN (the ear is the instrument — automated here only as far as the DOM reaches):
//   • A structural reshape must DUCK-AND-RETURN with NO AUDIBLE CLICK (spec §6.2.4). The declick
//     SHAPE is asserted automatically in crates/reuben-web/js/declick.test.mjs (raised-cosine, flat
//     tangent at both ends — not a hard cut); the AUDIBILITY is a human step:
//       1. `cd web && npm run dev`, open the app with `?chat=1`, pick a Toy so a sound is playing.
//       2. In the console, drive a structural re-strike:
//            const c = window.reubenChat; c.reshapeBegin();
//            await c.reshapeRestrike({ added: ["/shimmer"], changed: [], removed: [] },
//                                    "Here's the new version, from the top.");
//       3. LISTEN across the ~12ms edges: the output must fade to silence and back — a soft DUCK,
//          never a click/pop. Repeat a few times; no edge should tick.
//   • The re-strike must read as INTENTIONAL, not a glitch (spec §6.2): watch that the change-card
//     resolves and the transport playhead snaps to the start EXACTLY as the sound drops/returns
//     (co-timed cause), and that no "loading…" chrome ever flashes over the gap.

const DESKTOP = { width: 1280, height: 800 };

// Boot into the spine (arrival "picked" → tap the first gallery card), sheet EXPANDED so the card,
// its rows, and the honesty foot are visible. Returns once a control has rendered on the board and
// audio is running (the pick went through the Start gesture, so a real duck has a live output).
async function bootSpine(page) {
  await page.setViewportSize(DESKTOP);
  await page.goto("/?chat=1");
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");
  await page.locator(".toy-card").first().click();
  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
  await page.evaluate(() => window.reubenChat.toggleSheet(true));
}

// The real node addresses the current board's controls back — a test picks from these so a crafted
// diff's highlight lands on an on-screen control.
async function controlNodes(page) {
  return page.evaluate(() => window.reubenChat.controlNodes());
}

// --- 1. a structural reshape re-strikes: commit + replay-from-top + no spinner (spec §6.2) --------
test("a structural reshape commits the card, resets the playhead to start, and shows no spinner", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const nodes = await controlNodes(page);
  expect(nodes.length).toBeGreaterThan(1);
  const [addNode, removeNode] = nodes;

  // No spinner at rest, and the playhead has not reset yet.
  expect(await page.evaluate(() => window.reubenChat.loadingChromeCount())).toBe(0);
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(0);

  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Adding a shimmering layer"));
  // Await the whole re-strike gesture (the declicked duck resolves when the sound returns).
  await page.evaluate(
    (n) => window.reubenChat.reshapeRestrike({ added: [n.add], changed: [], removed: [n.remove] }),
    { add: addNode, remove: removeNode },
  );

  // Co-timed cause (§6.2.1): the card committed into rows AT the drop.
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual([
    "Added a new layer",
    "Removed a layer",
  ]);
  // Replay-from-top (§6.2.2): the transport playhead visibly returned to the start — exactly once.
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(1);
  // No spinner / "loading…" over the gap (§6.2.3): it's a beat, not a wait.
  expect(await page.evaluate(() => window.reubenChat.loadingChromeCount())).toBe(0);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. a parameter reshape moves the control live: no gap, NO phase reset (spec §6.1) ------------
test("a parameter reshape sweeps its control live and does NOT reset the playhead", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);
  expect(node).toBeTruthy();

  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Lowering the tone a touch"));
  // Param-only path: the existing #358 live sweep — resolve, NOT re-strike.
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);

  // The control swept live (node-identity highlight), the card holds its one row...
  const swept = await page.evaluate(() => window.reubenChat.lastHighlight());
  expect(swept.changed).toContain(node);
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Reshaped a control"]);
  // ...and CRUCIALLY the transport did not restart — a parameter reshape has no phase reset (§6.1).
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(0);
  // A param reshape never carries a restart line (§6.4).
  expect(await page.evaluate(() => window.reubenChat.cardHonesty())).toBe("");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. first structural restart per session shows the honesty line; a later one does not (§6.4) --
test("the first structural re-strike shows the honesty line, a later re-strike is wordless", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const nodes = await controlNodes(page);
  const [n1, n2] = nodes;

  // First re-strike: the envelope carries the first-run-only line (the #356 gate produces it once).
  // The slot RENDERS it. (The gating is #356's — this asserts we render the slot the envelope carries.)
  const FIRST_LINE = "Here's the new version, from the top.";
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(
    (a) => window.reubenChat.reshapeRestrike({ added: [a.node], changed: [], removed: [] }, a.line),
    { node: n1, line: FIRST_LINE },
  );
  expect(await page.evaluate(() => window.reubenChat.cardHonesty())).toBe(FIRST_LINE);
  await expect(page.locator('.tx-card-honesty[data-slot="restart-honesty"]').first()).toBeVisible();

  // A later re-strike: the envelope omits the line (once/session already spent), so its card's slot
  // stays EMPTY — wordless on the repeat (§6.4). Assert on the SECOND card specifically.
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate((node) => window.reubenChat.reshapeRestrike({ added: [node], changed: [], removed: [] }), n2);
  const honesties = await page.evaluate(() =>
    [...document.querySelectorAll(".tx-card .tx-card-honesty")].map((el) => el.textContent),
  );
  expect(honesties[0]).toBe(FIRST_LINE);
  expect(honesties[honesties.length - 1]).toBe(""); // the repeat's slot is empty/wordless

  // The line is forbidden-word clean (§1 gate).
  for (const word of FORBIDDEN) {
    expect(new RegExp(`\\b${word}\\b`, "i").test(FIRST_LINE), `"${word}" in honesty line`).toBe(false);
  }

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. a structural change with nothing sounding just builds ready: no reset, no line (§6.4) -----
test("a structural change while NOT sounding builds ready — no playhead reset, no honesty line", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);

  // sounding:false — there is no live sound, so there's no restart to be honest about (§6.4). It
  // commits the card + surface, but skips the duck, the playhead reset, AND the restart line.
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(
    (a) => window.reubenChat.reshapeRestrike({ added: [a.node], changed: [], removed: [] }, a.line, { sounding: false }),
    { node, line: "Here's the new version, from the top." },
  );

  // The card still resolved (the change landed)...
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Added a new layer"]);
  // ...but NO re-strike gesture: the playhead did not reset and no restart line rendered.
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(0);
  expect(await page.evaluate(() => window.reubenChat.cardHonesty())).toBe("");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});
