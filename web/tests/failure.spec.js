import { expect, test } from "@playwright/test";
// The forbidden lexicon (spec §1 / M1 lexicon gate) — imported from its ONE source of truth in
// change-card.js rather than re-declared, so this spec's gate can't drift from the card's (finding
// 4). Whole-word, case-insensitive.
import { FORBIDDEN_LEXICON as FORBIDDEN } from "../src/chat/change-card.js";

// The ambiguity & failure spec (issue #361 verification, spec §5). The user-facing failure surface
// has essentially ONE shape, and only ambiguity acts. Boots the BUILT app into the co-presence
// spine (the same gallery→pick path the other chat specs use) and drives CRAFTED turn envelopes +
// chat-turns through the `window.reubenChat` / `window.reubenPlayer` hooks (the seam #354's real
// agent loop will drive identically). Asserts the §5 observables:
//   1. ambiguous → best-effort change PLAYS + a "how I read it" line + tappable alt chips; a chip
//      tap flips the reading (posts the other reading verbatim) — §5.1 case 1;
//   2. unsatisfiable → a plain chat turn with a tappable "nearest thing" action, NO change-card, NO
//      surface change, NO engine word — §5.1 case 2 / §5.2;
//   3. a {ok:false} with a Diag is repaired silently — the Diag never reaches the DOM — §5.1 case 3;
//   4. empty / off-topic → a gentle re-orient with starter chips, sound untouched — §5.1 case 4;
//   5. the one phase divergence (§5.3): a reshape terminal failure KEEPS the prior sound; a
//      first-creation terminal failure lands back at the gallery.

const DESKTOP = { width: 1280, height: 800 };

// Boot into the spine (arrival "picked" → tap the first gallery card), sheet EXPANDED so chat turns
// + card chrome are visible + tappable. Returns once a control has rendered on the board.
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

const controlNodes = (page) => page.evaluate(() => window.reubenChat.controlNodes());

// The chat chrome (transcript + pinned input dock) — NOT the instrument's own control labels, which
// belong to the surface, not the chat lexicon (mirrors spine.spec.js's gate).
async function chatChrome(page) {
  return page.evaluate(() => {
    const dock = document.querySelector(".spine-dock");
    return dock?.innerText ?? "";
  });
}
function assertLexiconClean(text, where) {
  for (const word of FORBIDDEN) {
    const hit = new RegExp(`\\b${word}\\b`, "i").test(text);
    expect(hit, `forbidden word "${word}" found in ${where}:\n${text}`).toBe(false);
  }
}

// --- 1. ambiguous but actionable (spec §5.1 case 1) --------------------------------------------
test("an ambiguous turn plays a best-effort change, shows a 'how I read it' line + alt chips, and a chip tap flips the reading", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);
  expect(node, "the board has at least one control to sweep").toBeTruthy();

  // Act-then-react: the best-effort change resolves (plays) WITH its reading + alternatives.
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Warming up the tone."));
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

  // The best-effort change PLAYED: the control swept on the surface (§5.1 "plays immediately").
  const swept = await page.evaluate(() => window.reubenChat.lastHighlight());
  expect(swept.changed).toContain(node);

  // The "how I read it" line rides the card, lexicon-clean.
  const reading = await page.evaluate(() => window.reubenChat.cardReading());
  expect(reading).toContain("warmer");
  assertLexiconClean(reading, "the reading line");

  // 1–2 tappable alternative-interpretation chips (§5.1).
  const alts = await page.evaluate(() => window.reubenChat.cardAlternatives());
  expect(alts.length).toBeGreaterThanOrEqual(1);
  expect(alts.length).toBeLessThanOrEqual(2);
  expect(alts).toContain("more of a slow fade-in");

  // A chip tap re-reshapes toward the OTHER reading — posts its label VERBATIM as the user's turn
  // (spec §2.3 / §5.1: a wrong guess is one tap from fixed, no typing).
  await page.evaluate(() => window.reubenChat.tapAltChip(0));
  await expect(
    page.locator('.transcript .tx-entry[data-role="you"] .tx-text', { hasText: "more of a slow fade-in" }),
  ).toHaveCount(1);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. unsatisfiable → "nearest thing", chat-only, no engine word (spec §5.1 case 2 / §5.2) ---
test("an unsatisfiable ask is a chat-only 'nearest thing' with a tappable action and no engine word", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const before = await page.evaluate(() => window.reubenChat.boardNodes());

  // The agent reframes to the nearest achievable move (no change-card, no surface change — §5.2).
  await page.evaluate(() =>
    window.reubenChat.chatReply({
      text: "I can't bring a live saxophone in, but I can make the lead breathier and more reedy — want that?",
      chips: ["make the lead breathier and reedy"],
    }),
  );

  // Container rule (§5.2): NO change-card — it is a plain chat turn.
  await expect(page.locator(".tx-card")).toHaveCount(0);
  // The sound is unchanged: nothing highlighted, the board is untouched.
  expect(await page.evaluate(() => window.reubenChat.lastHighlight())).toEqual({
    added: [],
    changed: [],
    removed: [],
  });
  expect(await page.evaluate(() => window.reubenChat.boardNodes())).toEqual(before);

  // The reply is present, and the nearest move is a single tappable action chip. (Scope to the LAST
  // chips row — the gallery pick's turn one already seeded its own authored quick-change chips.)
  await expect(page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').last()).toContainText(
    "breathier",
  );
  const actionChips = page.locator(".transcript .tx-chips-entry").last();
  await expect(actionChips.locator(".tx-chip")).toHaveCount(1);

  // The engine reason is NEVER exposed: no forbidden engine word anywhere in the chat chrome.
  assertLexiconClean(await chatChrome(page), "the unsatisfiable chat turn");

  // Tapping the nearest-thing action posts it verbatim as the user's next turn.
  await actionChips.locator(".tx-chip").first().click();
  await expect(
    page.locator('.transcript .tx-entry[data-role="you"] .tx-text', {
      hasText: "make the lead breathier and reedy",
    }),
  ).toHaveCount(1);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. a {ok:false} with a Diag is repaired silently — no Diag reaches the DOM (§5.1 case 3) ---
test("an injected {ok:false} Diag is repaired silently and never reaches the DOM", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const [node] = await controlNodes(page);

  // The agent's OWN mistaken document fails validation: a node/port-addressed Diag comes back. It is
  // agent-internal fuel (ADR-0048 §3) — recorded on the envelope's toolLog, never rendered.
  await page.evaluate(() => window.reubenChat.reshapeBegin());
  await page.evaluate(() => window.reubenChat.reshapeAppendPlan("Adding a touch of shimmer."));
  await page.evaluate(() =>
    window.reubenChat.reshapeRecordTool({
      id: "tu-1",
      name: "swap",
      input: {},
      isError: false,
      result: {
        ok: false,
        errors: [{ node: "/osc", port: "freq", message: "unknown operator type 'oscilllator'" }],
        warnings: [],
      },
    }),
  );
  // ...the agent repairs within the turn; the user sees only the eventual success (a normal card).
  await page.evaluate((n) => window.reubenChat.reshapeResolve({ changed: [n], added: [], removed: [] }), node);

  // The card resolved normally — the user never saw the failure.
  expect(await page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  expect(await page.evaluate(() => window.reubenChat.cardRows())).toEqual(["Reshaped a control"]);

  // NONE of the Diag's text — the node address, the port, the raw message, or the internal type —
  // appears anywhere in the rendered card.
  const cardText = await page.evaluate(() => document.querySelector(".tx-card")?.innerText ?? "");
  for (const leak of ["/osc", "freq", "unknown operator", "oscilllator", "message"]) {
    expect(cardText.includes(leak), `Diag fragment "${leak}" leaked into the card:\n${cardText}`).toBe(false);
  }
  assertLexiconClean(cardText, "the silently-repaired card");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. empty / off-topic → gentle re-orient with starter chips, sound untouched (§5.1 case 4) --
test("empty and off-topic sends re-orient with starter chips and leave the sound untouched", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const before = await page.evaluate(() => window.reubenChat.boardNodes());
  const entriesBefore = await page.evaluate(() => window.reubenChat.transcriptEntryCount());

  // An EMPTY send is a no-op (the form drops it) — nothing is added to the transcript.
  await page.locator(".reshape-input").fill("");
  await page.locator(".reshape-send").click();
  expect(await page.evaluate(() => window.reubenChat.transcriptEntryCount())).toBe(entriesBefore);

  // An OFF-TOPIC ask gets a light redirect + tappable starter directions (§5.1 case 4).
  await page.evaluate(() =>
    window.reubenChat.chatReply({
      text: "I make sounds — want to try making this brighter?",
      chips: ["make it brighter", "add a beat"],
    }),
  );

  await expect(page.locator(".tx-card")).toHaveCount(0); // not an error, not a change-card
  // The starter directions (scope to the LAST chips row — turn one seeded its own chips).
  await expect(page.locator(".transcript .tx-chips-entry").last().locator(".tx-chip")).toHaveCount(2);
  // The sound is untouched.
  expect(await page.evaluate(() => window.reubenChat.lastHighlight())).toEqual({
    added: [],
    changed: [],
    removed: [],
  });
  expect(await page.evaluate(() => window.reubenChat.boardNodes())).toEqual(before);
  assertLexiconClean(await chatChrome(page), "the re-orient chat turn");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 5a. reshape terminal failure KEEPS the prior sound (spec §5.3) ----------------------------
test("a reshape terminal failure keeps the prior sound: a plain chat turn, no card, no re-strike", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const boardBefore = await page.evaluate(() => window.reubenChat.boardNodes());
  const restrikeBefore = await page.evaluate(() => window.reubenChat.transportRestrikeSeq());

  // Exhausted → collapse into the "can't" shape (§5.1 case 3 → §5.3): a plain line, no card. The
  // prior sound keeps playing (ADR-0048 §5: {ok:false} installs nothing).
  await page.evaluate(() =>
    window.reubenChat.chatReply({
      text: "I couldn't make that change stick — the sound's still going as it was.",
    }),
  );

  await expect(page.locator(".tx-card")).toHaveCount(0);
  // The board did not change and the transport never re-struck — the old sound survives untouched.
  expect(await page.evaluate(() => window.reubenChat.boardNodes())).toEqual(boardBefore);
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(restrikeBefore);
  await expect(page.locator('.transcript .tx-entry[data-role="reuben"] .tx-text').last()).toContainText(
    "still going",
  );
  assertLexiconClean(await chatChrome(page), "the reshape terminal-failure turn");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 5b. first-creation terminal failure lands back at the gallery (spec §5.3) -----------------
test("a first-creation terminal failure lands back at the gallery with a warm nudge", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);

  // Nothing built (describe-path from cold-start) → return the user to the gallery/cold-start.
  await page.evaluate(() => window.reubenPlayer.firstCreationTerminalFailure());

  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("gallery");
  await expect(page.locator(".toy-card").first()).toBeVisible(); // the gallery is back
  // The warm, lexicon-clean nudge is shown.
  const banner = await page.evaluate(() => window.reubenPlayer.banner());
  expect(banner).toContain("start from one of these");
  assertLexiconClean(banner, "the first-creation failure banner");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});
