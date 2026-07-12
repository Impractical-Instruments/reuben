import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js rather than re-declared, so the gate the card enforces and the gate the test
// asserts can never drift (finding 4). Whole-word, case-insensitive.
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";

// The change-card + surface-highlights spec (issue #358 verification, spec §4). Boots the BUILT app
// into the co-presence spine (the same `?chat=1` → gallery → pick path spine.spec.js uses), then
// drives CRAFTED turn envelopes through the transcript + surface via the `window.reubenChat` reshape
// hooks (the seam #354's real agent loop will drive identically). Asserts the §4 observables:
//   1. a parameter reshape → one card row + a live surface sweep keyed on node identity (§4.1/§4.6);
//   2. a structural reshape → added-pulse / removed-animate + rows (§4.1);
//   3. the plan streams then RESOLVES IN PLACE — one card object, thinking → resolved (§4.2);
//   4. a no-knob change → a row but NO surface echo (§4.3);
//   5. a >5-change diff collapses to a headline + expandable list (§4.4);
//   6. hovering a card row echo-highlights its control (§4.1 linkage);
//   7. NO forbidden engine word in any card row / chrome, even from an engine-ish node name (§4.5).

const DESKTOP = { width: 1280, height: 800 };

// Boot into the spine (arrival "picked" → tap the first gallery card), sheet EXPANDED so the card
// and its rows are visible + hoverable. Returns once a control has rendered on the board.
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
// diff's highlight lands on an on-screen control (see main.js `controlNodes`).
async function controlNodes(page) {
  return page.evaluate(() => window.reubenChat.controlNodes());
}

// --- 1. a parameter reshape: one card row + a live surface sweep (spec §4.1/§4.6) -------------
test("a parameter reshape shows a card row and sweeps its control on the surface", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);
  expect(node, "the board has at least one control to sweep").toBeTruthy();

  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Lowering the tone a touch"));
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);

  // Transcript half: exactly one sensory row for the change, reading as a PURE-GENERIC phrase by
  // change kind (§4.5) — never the node address title-cased (findings 1+2: no engine/theory name).
  await expect.poll(() => page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Reshaped a control"]);
  // Surface half: the reshape swept THIS control, keyed on node identity.
  const swept = await page.evaluate(() => window.reubenChat.lastHighlight());
  expect(swept.changed).toContain(node);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. a structural reshape: added-pulse / removed-animate + rows (spec §4.1) ----------------
test("a structural reshape pulses an added control, animates a removed one out, and lists rows", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const nodes = await controlNodes(page);
  expect(nodes.length).toBeGreaterThan(1);
  const [addNode, removeNode] = nodes; // both back real controls, so both animate on the surface

  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(
    (n) => window.reubenChat.reshapeResolve({ added: [n.add], changed: [], removed: [n.remove] }),
    { add: addNode, remove: removeNode },
  );

  // Two rows: one added, one removed — each a PURE-GENERIC sensory phrase by change kind (§4.5),
  // never the node address (findings 1+2). Added leads, removed last (the §4.1 order).
  const rows = await page.evaluate(() => window.reubenChat.cardRows());
  expect(rows).toEqual(["Added a new layer", "Removed a layer"]);
  // Surface half: the added control pulsed in, the removed one animated out (and left the board).
  const hi = await page.evaluate(() => window.reubenChat.lastHighlight());
  expect(hi.added).toContain(addNode);
  expect(hi.removed).toContain(removeNode);
  await expect.poll(() => controlNodes(page)).not.toContain(removeNode);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. the plan streams then RESOLVES IN PLACE — one card object (spec §4.2) -----------------
test("the plan streams then resolves in place into rows on the SAME card object", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);

  const turnId = await page.evaluate(() => window.reubenChat.reshapeBegin());
  // Thinking: the card is present, streaming, and carries no rows yet.
  expect(await page.evaluate(() => window.reubenChat.cardCount())).toBe(1);
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("thinking");

  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Lowering the tone"));
  const partial = await page.evaluate(() => window.reubenChat.cardPlan());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan(", adding a shimmer"));
  const grown = await page.evaluate(() => window.reubenChat.cardPlan());
  // Streamed incrementally (the plan GREW), not landed as one blob (spec §4.2).
  expect(grown.length).toBeGreaterThan(partial.length);
  expect(grown).toContain("shimmer");
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toHaveLength(0);

  // Resolve: STILL one card, SAME turn id, now resolved into rows — not a second card appended.
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);
  expect(await page.evaluate(() => window.reubenChat.cardCount())).toBe(1);
  expect(await page.evaluate(() => window.reubenChat.cardTurnId())).toBe(turnId);
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toHaveLength(1);
  // The plan stayed as the lead summary through the resolve.
  expect(await page.evaluate(() => window.reubenChat.cardPlan())).toContain("shimmer");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. a no-knob change: a row, but no surface echo (spec §4.3) ------------------------------
test("a no-knob change shows a row and fires no surface echo", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);

  // A node the surface has no control for (§4.3: added reverb with no knob, an internal reroute).
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() =>
    window.reubenChat.reshapeResolve({ added: ["/deep-space-reverb"], changed: [], removed: [] }),
  );

  // The card is the complete record: the row is present, reading as the PURE-GENERIC added phrase —
  // NOT the node address "/deep-space-reverb" title-cased (findings 1+2: no address ever surfaces).
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Added a new layer"]);
  // ...marked as backing no control...
  await expect(page.locator('.tx-card-row[data-knob="false"]')).toHaveCount(1);
  // ...and no control was highlighted on the surface (nothing to echo).
  expect(await page.evaluate(() => window.reubenChat.lastHighlight())).toEqual({
    added: [],
    changed: [],
    removed: [],
  });
  // Hovering the row echoes nothing.
  await page.locator(".tx-card-row").first().hover();
  expect(await page.evaluate(() => window.reubenChat.echoedCount())).toBe(0);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 5. a >5-change diff collapses to a headline + expandable list (spec §4.4) ----------------
test("a big diff (>5 changes) collapses to a headline with an expandable show-all", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);

  const changed = ["/a", "/b", "/c", "/d", "/e", "/f"]; // 6 > the ~4–5 threshold
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate((c) => window.reubenChat.reshapeResolve({ changed: c, added: [], removed: [] }), changed);

  // Collapsed: a summary headline shows, the enumerated rows are hidden.
  expect(await page.evaluate(() => window.reubenChat.cardHeadline())).toBeTruthy();
  expect(await page.evaluate(() => window.reubenChat.cardRowsExpanded())).toBe(false);
  await expect(page.locator(".tx-card-showall")).toBeVisible();

  // Expanding reveals every row — each a PURE-GENERIC "changed" phrase (§4.5), never a node address
  // like "/a".."/f" title-cased (findings 1+2).
  await page.locator(".tx-card-showall").click();
  expect(await page.evaluate(() => window.reubenChat.cardRowsExpanded())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(Array(6).fill("Reshaped a control"));

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 6. hovering a card row echo-highlights its control (spec §4.1 linkage) -------------------
test("hovering a card row echo-highlights its control on the surface, clearing on leave", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);

  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);

  // Nothing echoed at rest.
  expect(await page.evaluate(() => window.reubenChat.echoedCount())).toBe(0);
  // Hover the row → its control glows.
  await page.locator(".tx-card-row").first().hover();
  await expect.poll(() => page.evaluate(() => window.reubenChat.echoedCount())).toBeGreaterThan(0);
  // Move away → the echo clears (the surface glow is transient; the card persists).
  await page.mouse.move(0, 0);
  await expect.poll(() => page.evaluate(() => window.reubenChat.echoedCount())).toBe(0);
  // The card is still there (§4.1: the card persists after the glow fades).
  expect(await page.evaluate(() => window.reubenChat.cardCount())).toBe(1);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 7. the lexicon gate: no forbidden engine word in any row / chrome (spec §4.5) ------------
test("lexicon gate: no forbidden engine word in a card, even from an engine-ish node name", async ({
  page,
}) => {
  await bootSpine(page);

  // A diff whose node names ARE forbidden words ("/wire", "/tuning", "/note-voice") plus a name
  // that would ALSO be engine/theory vocabulary if title-cased ("/shimmer"). Under the pure-generic
  // fallback (findings 1+2) NONE of these addresses ever becomes user-visible text — the rows read
  // as generic sensory phrases by change kind, so no node/operator/theory name can reach a row.
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Softening the edges, adding a shimmer"));
  await page.evaluate(() =>
    window.reubenChat.reshapeResolve({
      added: ["/shimmer"],
      changed: ["/wire", "/tuning"],
      removed: ["/note-voice"],
    }),
  );

  const cardText = await page.evaluate(() => document.querySelector(".tx-card")?.innerText ?? "");
  for (const word of FORBIDDEN) {
    const hit = new RegExp(`\\b${word}\\b`, "i").test(cardText);
    expect(hit, `forbidden word "${word}" found in card:\n${cardText}`).toBe(false);
  }
  // Every row is a pure-generic phrase by change kind — the node addresses never surface, so a name
  // like "Shimmer" (a title-cased "/shimmer") does NOT leak into a row either (spec §4.5: rows must
  // never show node/operator names; the real "Added Shimmer" copy is agent-supplied, not synthesized).
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual([
    "Added a new layer",
    "Reshaped a control",
    "Reshaped a control",
    "Removed a layer",
  ]);
  // No node name — clean OR engine-ish — appears anywhere in the card chrome (the plan's own lowercase
  // "shimmer" prose is agent copy, distinct from a synthesized "Shimmer" label, which must not exist).
  for (const leaked of ["Shimmer", "Wire", "Tuning", "Note Voice"]) {
    expect(cardText.includes(leaked), `node name "${leaked}" leaked into card:\n${cardText}`).toBe(false);
  }
});
