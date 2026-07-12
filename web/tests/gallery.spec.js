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
//   5. the describe bar routes free text into the build path and lands EXPANDED (§2.4/§3.3);
//   6. a live-input pick (Mic Space) surfaces the Enable-microphone control and greets it
//      honestly — no false "playing" claim (the epic's honesty gate, "one tap → playing sound");
//   7. the pick greeting is kind-aware: only a self-playing Toy announces sound; tap-to-play and
//      live-input do not (the honesty gate again — never claim a state that doesn't exist).

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

// --- 6. a live-input pick is NOT a silent dead end: mic control + honest greeting (§2.1/honesty)
// The epic's honesty gate: user-facing copy must never claim a state that doesn't exist. A
// live-input Toy (Mic Space) makes NO sound until the mic is enabled on a gesture, so its pick
// must (a) surface an enable control — otherwise "one tap → playing sound" has no reachable sound —
// and (b) greet it honestly, pointing at the mic rather than announcing it's playing.
test("picking a live-input Toy surfaces the mic-enable control and never claims sound", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  const micToy = TOYS_IN_ORDER.find((t) => t.kind === "live-input");
  expect(micToy, "fixture assumption: a live-input Toy exists").toBeTruthy();

  await bootGallery(page);
  await page.locator(`.toy-card[data-toy="${micToy.id}"]`).click();

  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  // The pick is NOT a silent dead end — the Enable-microphone affordance is mounted and visible.
  await expect(page.locator('[data-slot="mic"] .mic-enable')).toBeVisible();
  expect(await page.evaluate(() => window.reubenChat.micEnablePresent())).toBe(true);

  await page.evaluate(() => window.reubenChat.toggleSheet(true));
  const greeting = page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').first();
  await expect(greeting).toContainText(micToy.title);
  // Honesty gate: a mic Toy is silent until the mic is enabled, so the greeting must NOT announce
  // it's "playing" — it must point at the microphone instead.
  await expect(greeting).not.toContainText("playing");
  await expect(greeting).toContainText("microphone");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 7. the pick greeting is kind-aware (honesty gate) ----------------------------------------
// One uniform "…'s playing" greeting lied for tap-to-play (silent until a tap) and live-input
// (silent until the mic). The greeting lead must differ by kind: self-playing announces sound;
// tap-to-play and live-input do not. Drive `pickToy` directly with a real id + kind but NO chips,
// isolating the greeting lead from the chip row.
test("the pick greeting is kind-aware and only self-playing announces sound", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootGallery(page);

  const cases = [
    { kind: "self-playing", claimsSound: true },
    { kind: "tap-to-play", claimsSound: false },
    { kind: "live-input", claimsSound: false },
  ];

  for (const { kind, claimsSound } of cases) {
    const toy = TOYS_IN_ORDER.find((t) => t.kind === kind);
    expect(toy, `fixture assumption: a ${kind} Toy exists`).toBeTruthy();

    await page.evaluate(
      (t) => window.reubenPlayer.pickToy(t),
      { id: toy.id, title: toy.title, kind: toy.kind },
    );
    await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
    await page.evaluate(() => window.reubenChat.toggleSheet(true));

    const greeting = page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').first();
    await expect(greeting, `${kind} greeting names the Toy`).toContainText(toy.title);
    if (claimsSound) {
      await expect(greeting, `${kind} greeting announces sound`).toContainText("playing");
    } else {
      await expect(greeting, `${kind} greeting must not claim it's playing`).not.toContainText(
        "playing",
      );
    }
  }

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
