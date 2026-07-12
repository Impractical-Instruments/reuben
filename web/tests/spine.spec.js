import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js rather than re-declared here, so the spine and change-card specs cannot drift
// (finding 4). Whole-word, case-insensitive — so "port" never trips on "important".
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";
// The deterministic agent-transport stub (issue #397): the real onReshapeSubmit loop now replaces
// spine.js's mock turn, so a turn that must be OBSERVED mid-flight is driven through the stub, held
// open, then released — no key, no network.
import { installChatStub, setTurn } from "./chat-stub.js";

// The co-presence spine spec (issue #355 verification, spec §3). A scripted RESPONSIVE pass at a
// PHONE and a DESKTOP width, all with the chat flag opted in via `?chat=1` (chat/flag.js). Boots
// the BUILT app, unlocks audio (the Start gesture), lands on the spine, and asserts the §3
// observable requirements:
//   1. surface top/center, chat input pinned to the bottom, transcript an upward collapsible sheet
//      that never takes the full screen;
//   2. controls stay hand-live through a MOCK agent turn (no freeze — spec §3.4);
//   3. the surface keys on NODE IDENTITY: an identity-preserving re-render holds positions, while
//      the fresh-sort anti-pattern MOVES them (spec §3.6);
//   4. the lexicon gate: no forbidden engine word reaches the user in the chat chrome (spec §1).
//
// The OFF-path (no `?chat=1`) launcher flow is covered unchanged by smoke.spec.js — this spec
// only exercises the flagged-ON spine.

const PHONE = { width: 390, height: 844 };
const DESKTOP = { width: 1280, height: 800 };


// Boot the built app into the spine at `viewport`, opting the flag in with `?chat=1`. The chat
// flag routes Start into the gallery-first cold start (spec §2, issue #357) rather than straight
// to the spine, so reaching the spine now goes THROUGH a real gallery interaction:
//   - arrival "picked" (the default) → tap the first gallery card (spec §2.3);
//   - arrival "made" → submit the persistent describe bar (spec §2.4).
// Returns once a control has rendered on the board.
async function bootSpine(page, viewport, { arrival = "picked" } = {}) {
  await page.setViewportSize(viewport);
  await page.goto("/?chat=1");
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");

  if (arrival === "made") {
    await page.locator(".gallery-describe input").fill("a warm pad that swells");
    await page.locator(".gallery-describe .reshape-send").click();
  } else {
    await page.locator(".toy-card").first().click();
  }

  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  // The engine loaded the instrument and the node-identity board rendered its controls.
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
}

// --- 1. one responsive layout (spec §3.7): surface top/center, input pinned bottom, sheet ----
for (const [name, viewport] of [["phone", PHONE], ["desktop", DESKTOP]]) {
  test(`spine layout at ${name} width: surface top/center, input pinned bottom, sheet toggles`, async ({
    page,
  }) => {
    const errors = [];
    page.on("pageerror", (e) => errors.push(e.message));

    // Default arrival is "picked" → the sheet lands collapsed-to-bar (spec §3.3).
    await bootSpine(page, viewport);
    expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(false);

    const surfaceBox = await page.locator(".spine-surface").boundingBox();
    const boardBox = await page.locator(".surface-board").boundingBox();
    const inputBox = await page.locator(".reshape-input").boundingBox();

    // Surface owns the TOP / CENTER: its region starts at the top, and the controls render in the
    // upper half (not buried behind the bottom dock).
    expect(surfaceBox.y).toBeLessThan(5);
    expect(boardBox.y).toBeLessThan(viewport.height * 0.5);

    // The reshape input is PINNED TO THE BOTTOM and sits below the surface controls.
    const inputBottom = inputBox.y + inputBox.height;
    expect(inputBottom).toBeGreaterThan(viewport.height * 0.8);
    expect(inputBox.y).toBeGreaterThan(boardBox.y);
    // The input is ALWAYS visible + enabled (never behind a menu; never disabled).
    await expect(page.locator(".reshape-input")).toBeVisible();
    await expect(page.locator(".reshape-input")).toBeEnabled();

    // The transcript is an upward collapsible sheet. Expanding grows the dock upward; the sheet
    // stays CAPPED (never a full-screen takeover) and the surface controls stay visible under it.
    const dockCollapsed = (await page.locator(".spine-dock").boundingBox()).height;
    await page.locator(".sheet-handle").click();
    expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(true);
    await expect
      .poll(async () => (await page.locator(".spine-dock").boundingBox()).height)
      .toBeGreaterThan(dockCollapsed + 20);

    const sheetBox = await page.locator(".transcript").boundingBox();
    expect(sheetBox.height).toBeLessThan(viewport.height * 0.7); // partial occlusion, not full-screen
    await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();

    // And it collapses back to the bar.
    await page.locator(".sheet-handle").click();
    expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(false);
    await expect
      .poll(async () => (await page.locator(".spine-dock").boundingBox()).height)
      .toBeLessThan(dockCollapsed + 20);

    // The bottom-chrome Keep slot (M2, spec §7.3) is present + labeled, awaiting the Keep ticket.
    expect(await page.evaluate(() => window.reubenChat.keepSlotPresent())).toBe(true);

    expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
  });
}

// --- 2. modeless co-presence (spec §3.4): controls stay live through a mock agent turn --------
test("controls stay hand-live while a mock agent turn is in flight (no freeze)", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page, DESKTOP);

  // Begin a MOCK turn (the #354 seam). The turn is in flight...
  await page.evaluate(() => window.reubenChat.beginMockTurn("make it brighter"));
  expect(await page.evaluate(() => window.reubenChat.turnInFlight())).toBe(true);
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "in-flight");

  // ...and the surface control STILL responds to the user's hand (no dead zone). A groovebox step
  // is a latching toggle: clicking it flips its pressed state even mid-turn.
  const step = page.locator(".surface-board .surface-widget.param-toggle").first();
  const before = await step.getAttribute("aria-pressed");
  await step.click();
  await expect(step).not.toHaveAttribute("aria-pressed", before ?? "");
  // The input is never disabled during a turn either.
  await expect(page.locator(".reshape-input")).toBeEnabled();

  // End the turn — state clears, nothing was frozen.
  await page.evaluate(() => window.reubenChat.endMockTurn());
  expect(await page.evaluate(() => window.reubenChat.turnInFlight())).toBe(false);
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "idle");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2b. the real user submit path: form submit → clears → in-flight → auto-settles -----------
// The test above drives the turn state directly; this exercises the ACTUAL path a user takes —
// typing a reshape and hitting Send routes through the form's onsubmit → mockReshape (the #354
// seam), which pushes the "you" line, marks the turn in flight, and auto-settles.
test("submitting the reshape form clears the input, records the turn, and settles", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await installChatStub(page);
  await bootSpine(page, DESKTOP);
  // Hold the turn open so its in-flight window is observable (a real turn against the stub would
  // otherwise resolve within a tick); the action is a no-op so nothing on the surface changes.
  await setTurn(page, { hold: true, action: { type: "none" } });

  const input = page.locator(".reshape-input");
  await input.fill("make it warmer");
  await page.locator(".reshape-send").click(); // the real submit gesture

  // The input clears, and the turn is now in flight (set synchronously by the submit handler).
  await expect(input).toHaveValue("");
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "in-flight");

  // The user's words were pushed to the transcript as their line (spec §3.3 / §2.4 echo).
  await page.evaluate(() => window.reubenChat.toggleSheet(true));
  await expect(page.locator('.transcript .tx-entry[data-role="you"] .tx-text')).toHaveText(
    "make it warmer",
  );

  // ...then, once the loop completes, it settles back to idle (no freeze — spec §3.4).
  await setTurn(page, { hold: false });
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "idle", { timeout: 3000 });

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. node-identity layout (spec §3.6): survivors hold position; a re-sort moves them --------
test("surface layout keys on node identity: identity-preserving re-render is stable, re-sort is not", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page, DESKTOP);

  const before = await page.evaluate(() => window.reubenChat.boardNodes());
  expect(before.length).toBeGreaterThan(2); // need ≥2 controls for order to be meaningful
  const orderBefore = before.map((n) => n.control);
  const uidBefore = Object.fromEntries(before.map((n) => [n.control, n.uid]));

  // Identity-preserving re-render (the SAME nodes in a DIFFERENT input order). Survivors HOLD
  // POSITION → the board order is unchanged and every cell is the SAME element (uid preserved).
  await page.evaluate(() => window.reubenChat.reshapePreserveIdentity());
  const afterPreserve = await page.evaluate(() => window.reubenChat.boardNodes());
  expect(afterPreserve.map((n) => n.control)).toEqual(orderBefore);
  for (const n of afterPreserve) expect(n.uid).toBe(uidBefore[n.control]);

  // The fresh-sort ANTI-PATTERN §3.6 forbids: positions follow the (reversed) input, so the
  // controls MOVE and the cells are rebuilt. This is what "a shuffled re-sort FAILS the test"
  // means — the stability assertion above genuinely distinguishes the two.
  await page.evaluate(() => window.reubenChat.resortRebuild());
  const afterResort = await page.evaluate(() => window.reubenChat.boardNodes());
  expect(afterResort.map((n) => n.control)).not.toEqual(orderBefore);
  expect(afterResort.map((n) => n.control)).toEqual([...orderBefore].reverse());
  for (const n of afterResort) expect(n.uid).not.toBe(uidBefore[n.control]);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3.3 arrival default: made → expanded, picked → collapsed ----------------------------------
test("arrival default (spec §3.3): 'made' lands expanded, 'picked' lands collapsed-to-bar", async ({
  page,
}) => {
  await bootSpine(page, DESKTOP, { arrival: "made" });
  expect(await page.evaluate(() => window.reubenChat.arrival())).toBe("made");
  expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(true);

  await bootSpine(page, DESKTOP, { arrival: "picked" });
  expect(await page.evaluate(() => window.reubenChat.arrival())).toBe("picked");
  expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(false);
});

// --- 4. lexicon gate (spec §1): no forbidden engine word in the chat chrome --------------------
test("lexicon gate: no forbidden engine word in the chat chrome, in any state", async ({ page }) => {
  await bootSpine(page, DESKTOP);
  // Expand the sheet so the transcript copy is included in the visible text we scan.
  await page.evaluate(() => window.reubenChat.toggleSheet(true));

  // Gather ONLY the chat chrome this ticket introduces — the sheet + the pinned input dock — not
  // the instrument's own control labels (which belong to the surface, not the chat lexicon).
  const chrome = await page.evaluate(() => {
    const dock = document.querySelector(".spine-dock");
    const input = document.querySelector(".reshape-input");
    return [
      dock?.innerText ?? "",
      input?.getAttribute("placeholder") ?? "",
      input?.getAttribute("aria-label") ?? "",
    ].join("\n");
  });

  for (const word of FORBIDDEN) {
    const hit = new RegExp(`\\b${word}\\b`, "i").test(chrome);
    expect(hit, `forbidden word "${word}" found in chat chrome:\n${chrome}`).toBe(false);
  }
});
