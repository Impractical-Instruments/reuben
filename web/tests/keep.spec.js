import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / the M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js so this spec can't drift from the others (finding 4). Whole-word, case-insensitive.
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";

// THE KEEP GESTURE spec (issue #359 verification, spec §7) — THE SHIP GATE (ADR-0052 §3): the chat
// window ships once Keep is wired into the spine's loop, and not before. All flagged ON via
// `?chat=1` (chat/flag.js). Boots the BUILT app, unlocks audio (the Start gesture), reaches the
// spine through a real gallery interaction, and asserts the §7 observable requirements:
//   1. Keep is PRESENT AND WIRED in the bottom chrome (the ship-gate criterion) and reads "Not kept
//      yet"; a first divergence pulses it exactly once (spec §7.2/§7.5);
//   2. tapping Keep writes `location.hash` (reload restores the instrument) and copies the link
//      (spec §7.6);
//   3. a later diverging reshape flips "Kept ✓" → "Not kept yet" and re-arms the guard, no re-pulse
//      (spec §7.7);
//   4. the leave-guard fires only on diverged-unkept work and NEVER on an untouched gallery pick
//      (spec §7.4);
//   5. asking to save in chat PULSES the control rather than the agent minting a link (spec §7.8);
//   6. all user-visible Keep copy is forbidden-word-clean (spec §1).

const DESKTOP = { width: 1280, height: 800 };

// Boot the built app into the spine, opting the chat flag in with `?chat=1`. The flag routes Start
// into the gallery-first cold start (spec §2), so the spine is reached THROUGH a real gallery
// interaction: a gallery pick (arrival "picked") or the persistent describe bar (arrival "made").
// Returns once a control has rendered on the node-identity board.
async function bootSpine(page, { arrival = "picked" } = {}) {
  await page.setViewportSize(DESKTOP);
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
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
}

// Drive one landing reshape through the change-card lifecycle (spec §4.2 / §6.1): begin → resolve.
// A landed reshape is what diverges the instrument + makes a prior keep stale (spec §7.4/§7.7). Uses
// a REAL board node so the diff resolves against an on-screen control, mirroring change-card.spec.
async function landReshape(page) {
  const [node] = await page.evaluate(() => window.reubenChat.controlNodes());
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(
    (n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }),
    node,
  );
}

// --- 1. THE SHIP GATE: Keep is present + wired, reads "Not kept yet", pulses once on divergence ---
test("ship gate: Keep is present and wired in the bottom chrome, reads 'Not kept yet', and pulses once on first divergence", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);

  // The ship-gate criterion (chat/flag.js): the loop is not shippable until Keep is PRESENT AND
  // WIRED. A real Keep button lives in the reserved bottom-chrome slot (spec §7.3).
  expect(await page.evaluate(() => window.reubenChat.keepWired())).toBe(true);
  await expect(page.locator('[data-slot="keep"] .keep-btn')).toBeVisible();

  // The persistent state teaches volatility passively (spec §7.2/§7.4): it starts "Not kept yet".
  expect(await page.evaluate(() => window.reubenChat.keepState())).toBe("Not kept yet");
  expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(false);

  // An untouched gallery pick has NOT diverged (it's re-findable), so no pulse has fired.
  expect(await page.evaluate(() => window.reubenChat.keepDiverged())).toBe(false);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(0);

  // The FIRST divergence (a reshape lands) pulses the chip exactly once (spec §7.5).
  await landReshape(page);
  expect(await page.evaluate(() => window.reubenChat.keepDiverged())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1);
  // Still "Not kept yet" — a divergence doesn't keep anything, it makes keeping matter.
  expect(await page.evaluate(() => window.reubenChat.keepState())).toBe("Not kept yet");

  // A LATER reshape does NOT re-pulse — proactive exactly once (spec §7.5).
  await landReshape(page);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. tapping Keep writes the hash + copies the link; reload restores the instrument (§7.6) ------
test("tapping Keep writes location.hash and copies the link; a reload restores the instrument", async ({
  page,
  context,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);

  await bootSpine(page); // picks groovebox (order 1)
  const instrument = await page.evaluate(() => window.reubenPlayer.instrument());
  expect(instrument).toBe("groovebox");

  // Tap Keep (a real user gesture — the clipboard write rides it, spec §7.8).
  await page.evaluate(() => window.reubenChat.tapKeep());

  // The chip flips to the kept state (spec §7.2) and the hash now carries the `#r1.…` snapshot.
  await expect.poll(() => page.evaluate(() => window.reubenChat.keepState())).toBe("Kept ✓");
  expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(true);
  await expect.poll(() => page.evaluate(() => location.hash)).toMatch(/^#r1\./);

  // The link was copied to the clipboard AND equals the hash-persisted URL (spec §7.6).
  const clip = await page.evaluate(() => navigator.clipboard.readText());
  expect(clip).toContain("#r1.");
  expect(clip).toContain(await page.evaluate(() => location.hash));

  // The non-modal confirm shows, leading with keep-to-not-lose (spec §7.1/§7.6).
  await expect(page.locator(".keep-confirm-text")).toContainText("bookmark this page");

  // Reload RESTORES the instrument from the hash (bookmarking = a store the persona understands).
  // The written fragment boots the player on one tap (the M1 share-link path).
  await page.reload();
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.instrument())).toBe("groovebox");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. staleness: a later reshape flips Kept ✓ → Not kept yet and re-arms the guard (§7.7) --------
test("a later diverging reshape flips 'Kept ✓' back to 'Not kept yet' and re-arms the leave-guard, with no re-pulse", async ({
  page,
  context,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);

  await bootSpine(page);

  // Diverge once (pulse fires), then Keep — now "Kept ✓", guard disarmed.
  await landReshape(page);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1);
  await page.evaluate(() => window.reubenChat.tapKeep());
  await expect.poll(() => page.evaluate(() => window.reubenChat.keepState())).toBe("Kept ✓");
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(false);

  // A LATER diverging reshape makes the keep stale (unsaved-changes model, spec §7.7): the chip
  // flips back to "Not kept yet" and the guard re-arms — but the one-time pulse does NOT re-fire.
  await landReshape(page);
  await expect.poll(() => page.evaluate(() => window.reubenChat.keepState())).toBe("Not kept yet");
  expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(false);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1); // no re-pulse

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. the leave-guard: only diverged-unkept work; NEVER an untouched gallery pick (§7.4) ---------
test("the leave-guard fires only on diverged, unkept work and never on an untouched gallery pick", async ({
  page,
  context,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);

  await bootSpine(page); // an untouched gallery pick — re-findable, still in the gallery

  // Untouched gallery pick → NOT at risk: the guard is disarmed and a beforeunload navigates clean.
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(false);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardBlocks())).toBe(false);

  // A landed reshape DIVERGES it (un-re-findable, unkept) → the guard arms and beforeunload blocks.
  await landReshape(page);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardBlocks())).toBe(true);

  // Keeping it disarms the guard again — kept work is not at risk.
  await page.evaluate(() => window.reubenChat.tapKeep());
  await expect.poll(() => page.evaluate(() => window.reubenChat.keepIsKept())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(false);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardBlocks())).toBe(false);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4b. describe-own (arrival "made") is diverged from the start → the guard arms immediately -----
test("a described-own build (arrival 'made') is diverged from the start: the leave-guard arms immediately", async ({
  page,
}) => {
  await bootSpine(page, { arrival: "made" });

  // No reshape yet — but a described-own instrument is un-re-findable, so it's diverged + unkept
  // from the start (spec §7.4): the guard is armed and the first-divergence pulse has fired once.
  expect(await page.evaluate(() => window.reubenChat.keepDiverged())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.leaveGuardArmed())).toBe(true);
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(1);
});

// --- 5. agent role: asking to save in chat PULSES the control, mints no link (§7.8) ----------------
test("asking to save in chat pulses the Keep control rather than the agent minting a link", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const pulsesBefore = await page.evaluate(() => window.reubenChat.keepPulseCount());
  const hashBefore = await page.evaluate(() => location.hash);
  expect(hashBefore).toBe("");
  expect(await page.evaluate(() => window.reubenPlayer.shareUrl())).toBe(null);

  // The user asks about saving in chat. reuben POINTS to Keep (a pulse) and answers in one line —
  // it mints NOTHING (the browser requires the user's own tap for the clipboard; agent directs).
  await page.evaluate(() => window.reubenChat.askToSave());

  // The control pulsed (the eye is directed to it)...
  expect(await page.evaluate(() => window.reubenChat.keepPulseCount())).toBe(pulsesBefore + 1);
  // ...and NO link was minted: the hash is untouched and no share URL exists.
  expect(await page.evaluate(() => location.hash)).toBe("");
  expect(await page.evaluate(() => window.reubenPlayer.shareUrl())).toBe(null);
  expect(await page.evaluate(() => window.reubenChat.keepIsKept())).toBe(false);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 6. the lexicon gate (spec §1): no forbidden engine word in the Keep copy, in any state --------
test("lexicon gate: no forbidden engine word in the Keep control, its confirm, or the save chat line", async ({
  page,
  context,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await bootSpine(page);

  // Exercise every user-visible Keep surface: tap Keep (confirm) + ask to save (chat line + pulse).
  await page.evaluate(() => window.reubenChat.tapKeep());
  await expect(page.locator(".keep-confirm-text")).toBeVisible();
  await page.evaluate(() => window.reubenChat.askToSave());
  await page.evaluate(() => window.reubenChat.toggleSheet(true)); // reveal the transcript copy

  const copy = await page.evaluate(() => {
    const keep = document.querySelector('[data-slot="keep"]');
    const confirm = document.querySelector(".keep-confirm");
    const transcript = document.querySelector(".transcript");
    return [keep?.innerText ?? "", confirm?.innerText ?? "", transcript?.innerText ?? ""].join("\n");
  });

  for (const word of FORBIDDEN) {
    const hit = new RegExp(`\\b${word}\\b`, "i").test(copy);
    expect(hit, `forbidden word "${word}" found in Keep copy:\n${copy}`).toBe(false);
  }
});
