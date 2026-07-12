import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

// The gallery-first cold start (issue #357 verification, spec §2 + §I). Chat-flag-on ONLY (the
// OFF-path launcher is smoke.spec.js's, unchanged). Asserts:
//   1. the gallery shows ALL toys.json entries, in `order` (§2.2, show-all, no auto-derivation);
//   2. a card tap starts sound AND fires a proactive turn one carrying the authored chips (§2.3);
//   3. a chip tap posts its exact string as a user turn — and re-typing it does the same (§2.3's
//      "what you said is what happened");
//   4. a Toy with no `chips` yields a greeting-only turn one, no generic filler (§I);
//   5. the describe bar routes free text into the build path and lands EXPANDED (§2.4/§3.3).

const manifest = JSON.parse(
  readFileSync(fileURLToPath(new URL("../toys.json", import.meta.url)), "utf8"),
);
const TOYS_IN_ORDER = [...manifest.toys].sort((a, b) => a.order - b.order);

// Boot the built app to the gallery (chat flag on, via the real Start gesture).
async function bootGallery(page) {
  await page.goto("/?chat=1");
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");
}

// --- 1. show-all, in order (spec §2.2) ---------------------------------------------------------
test("gallery shows every toys.json entry, in `order`", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootGallery(page);

  const cards = page.locator(".toy-card");
  await expect(cards).toHaveCount(TOYS_IN_ORDER.length);
  const ids = await cards.evaluateAll((els) => els.map((el) => el.dataset.toy));
  expect(ids).toEqual(TOYS_IN_ORDER.map((t) => t.id));

  // The persistent describe bar is present alongside the gallery, always visible (§2.1).
  await expect(page.locator(".gallery-describe input")).toBeVisible();

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. pick -> playing + proactive turn one carrying the authored chips (spec §2.3) -----------
test("tapping a gallery card starts audio and fires a turn one carrying the authored chips", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  const toy = TOYS_IN_ORDER.find((t) => (t.chips ?? []).length > 0);
  expect(toy, "fixture assumption: at least one toy has authored chips").toBeTruthy();

  await bootGallery(page);
  await page.locator(`.toy-card[data-toy="${toy.id}"]`).click();

  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
  await expect
    .poll(() => page.evaluate(() => window.reubenPlayer.engineState()))
    .toBe("running");
  // Picking lands COLLAPSED-to-bar (spec §3.3) — the newcomer picked something to PLAY.
  expect(await page.evaluate(() => window.reubenChat.arrival())).toBe("picked");
  expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(false);

  await page.evaluate(() => window.reubenChat.toggleSheet(true));

  // The greeting names what's playing.
  const greeting = page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').first();
  await expect(greeting).toContainText(toy.title);

  // The authored chips render VERBATIM as tappable buttons.
  const chipButtons = page.locator(".tx-chips .tx-chip");
  await expect(chipButtons).toHaveCount(toy.chips.length);
  expect(await chipButtons.allTextContents()).toEqual(toy.chips);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. a chip tap posts its EXACT string as a user turn; re-typing it does the same -----------
test("a chip tap posts its exact string as a user turn, same as typing it", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  const toy = TOYS_IN_ORDER.find((t) => (t.chips ?? []).length > 0);
  await bootGallery(page);
  await page.locator(`.toy-card[data-toy="${toy.id}"]`).click();
  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await page.evaluate(() => window.reubenChat.toggleSheet(true));

  const chipText = toy.chips[0];
  await page.locator(".tx-chips .tx-chip", { hasText: chipText }).first().click();

  const youEntries = page.locator('.transcript .tx-entry[data-role="you"] .tx-text');
  await expect(youEntries).toHaveCount(1);
  await expect(youEntries.first()).toHaveText(chipText);

  // Re-typing the identical phrase and sending it goes through the SAME submit path (chat/
  // spine.js `submitTurn`) — proving "what you said is what happened" (spec §2.3): a chip is not
  // a distinct hidden action, it's the same turn a typed line would be.
  await page.locator(".reshape-input").fill(chipText);
  await page.locator(".reshape-send").click();
  await expect(youEntries).toHaveCount(2);
  await expect(youEntries.nth(1)).toHaveText(chipText);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. no chips -> greeting-only turn one, no generic filler (spec §I) ------------------------
test("a Toy with no chips yields a greeting-only turn one (tailored-or-nothing)", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootGallery(page);
  // Drive `pickToy` directly with a synthetic Toy carrying NO chips (issue #357 test hook) — a
  // real Toy id so the engine load is real, but the chip-authoring input is deliberately absent,
  // exercising the fallback the real "not yet authored" case will hit.
  const base = TOYS_IN_ORDER[0];
  await page.evaluate(
    (toy) => window.reubenPlayer.pickToy(toy),
    { id: base.id, title: base.title, chips: [] },
  );

  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await page.evaluate(() => window.reubenChat.toggleSheet(true));

  await expect(page.locator(".transcript .tx-entry")).toHaveCount(1);
  const greeting = page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').first();
  await expect(greeting).toContainText(base.title);
  // No filler inviting a tap that doesn't exist.
  await expect(greeting).not.toContainText("try one of these");
  await expect(page.locator(".tx-chips")).toHaveCount(0);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 5. describe bar -> build path -> lands EXPANDED (spec §2.4/§3.3) --------------------------
test("the describe bar routes text into the build path and lands expanded", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootGallery(page);

  const text = "a warm pad that swells";
  await page.locator(".gallery-describe input").fill(text);
  await page.locator(".gallery-describe .reshape-send").click();

  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
  expect(await page.evaluate(() => window.reubenChat.arrival())).toBe("made");
  // "Made" lands EXPANDED, symmetric with the pick landing (spec §3.3).
  expect(await page.evaluate(() => window.reubenChat.sheetExpanded())).toBe(true);

  // The user's words are the first message, verbatim.
  const entries = page.locator(".transcript .tx-entry");
  await expect(entries.first()).toHaveAttribute("data-role", "you");
  await expect(entries.first().locator(".tx-text")).toHaveText(text);

  // And reuben lands the turn by naming what it made.
  await expect
    .poll(async () => (await entries.count()) >= 2)
    .toBe(true);
  await expect(entries.nth(1)).toHaveAttribute("data-role", "reuben");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});
