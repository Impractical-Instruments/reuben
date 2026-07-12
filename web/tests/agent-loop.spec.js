import { expect, test } from "@playwright/test";
import { installChatStub, setTurn } from "./chat-stub.js";

// The live agent-loop wiring (issue #397): the REAL onReshapeSubmit / generate path — chat-host.mjs
// (wasmIntrospect → tool layer → agent host) driving the change-card — exercised end to end against
// a deterministic stubbed transport (tests/chat-stub.js), no key, no network. This is the merge-
// gating half of #397's verification; the live-model half is the self-gated browser smoke.
//
// Asserts the observables the ticket names:
//   - a param turn drives a real `send` through the in-page tool layer, streams its plan INTO the
//     card (§4.2), sweeps the touched control + glows it (§4.1), and does NOT restart (§6.1);
//   - a structural turn drives a real `swap`, re-strikes (§6.2) — the playhead resets once — and
//     carries the once-per-session restart line (§6.4);
//   - a host/transport drop collapses to a §5.3 terminal reshape failure: the prior sound is KEPT
//     (no restart), a warm line lands, and the reason is never shown;
//   - the turn reads as in-flight the instant it is submitted (§3.4), then settles.

const DESKTOP = { width: 1280, height: 800 };

// Boot the built app into the spine via a gallery pick (arrival "picked"), with the stub installed
// BEFORE load so the host binds it. Returns once a control has rendered on the board.
async function bootSpine(page, { card = 0 } = {}) {
  await page.setViewportSize(DESKTOP);
  await installChatStub(page);
  await page.goto("/?chat=1");
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("gallery");
  await page.locator(".toy-card").nth(card).click();
  await expect.poll(() => page.evaluate(() => window.reubenChat?.screen())).toBe("spine");
  await expect(page.locator(".surface-board .surface-widget").first()).toBeVisible();
  await page.evaluate(() => window.reubenChat.toggleSheet(true)); // sheet open so the card is visible
}

const submit = async (page, text) => {
  await page.locator(".reshape-input").fill(text);
  await page.locator(".reshape-send").click();
};

// --- 1. param reshape: real send → stream into card → sweep + glow, no restart (§4.1/§4.2/§6.1) ---
test("a param turn drives a real send, streams its plan into the card, sweeps the control, and does not restart", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  // Aim the send at a real on-screen control so the sweep + glow land on it.
  const address = await page.evaluate(() => document.querySelector(".board-cell")?.dataset.control);
  expect(address, "the board mounted at least one control").toBeTruthy();
  await setTurn(page, {
    plan: "Warming it up with a rounder, mellower tone.",
    action: { type: "send", address, value: 0.4 },
  });

  await submit(page, "make it warmer");

  // §4.2: the plan streams into THE card, in place.
  await expect(page.locator(".tx-card .tx-card-plan")).toContainText("Warming");
  await expect.poll(() => page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  // §6.1: a param reshape is a live sweep — the transport never re-struck.
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(0);
  // §4.1: the touched control's node glowed (the synthesized param diff drove highlightDiff).
  const highlight = await page.evaluate(() => window.reubenChat.lastHighlight());
  expect(highlight.changed.length, "the swept control's node glowed").toBeGreaterThan(0);
  // Settles back to idle (no freeze, §3.4).
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "idle", { timeout: 3000 });
  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 2. structural reshape: real swap → re-strike + restart-honesty line (§6.2/§6.4) --------------
test("a structural turn drives a real swap, re-strikes once, and carries the restart-honesty line", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  // A real, self-contained instrument document to swap in (euclidean-drums has NO resource refs, so
  // it installs by value with an empty resource set) — the swap validates + loads against the live
  // wasm, producing a genuine structural diff vs the playing groovebox.
  const document = await page.evaluate(() =>
    fetch("/instruments/euclidean-drums.json").then((r) => r.json()),
  );
  await setTurn(page, { plan: "Adding a driving pulse underneath.", action: { type: "swap", document } });

  await submit(page, "add a beat");

  // §6.2.2: the playhead reset exactly once — a structural re-strike fired.
  await expect
    .poll(() => page.evaluate(() => window.reubenChat.transportRestrikeSeq()), { timeout: 5000 })
    .toBe(1);
  await expect.poll(() => page.evaluate(() => window.reubenChat.cardState())).toBe("resolved");
  // §6.4: the first restart of already-playing sound this session carries the honesty line.
  expect((await page.evaluate(() => window.reubenChat.cardHonesty())).trim().length).toBeGreaterThan(0);
  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 3. host/transport drop → §5.3 terminal reshape failure keeps the prior sound -----------------
test("a transport failure keeps the prior sound and lands a warm line, never the host reason", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await bootSpine(page);
  const restrikeBefore = await page.evaluate(() => window.reubenChat.transportRestrikeSeq());
  await setTurn(page, { fail: true });

  await submit(page, "make it darker");

  // A reuben line lands (the §5.3 keep-the-sound copy), and the sound was never re-struck.
  await expect(page.locator('.transcript .tx-entry[data-role="reuben"]').last()).toContainText(
    /lost the thread|try that again/i,
  );
  expect(await page.evaluate(() => window.reubenChat.transportRestrikeSeq())).toBe(restrikeBefore);
  // The host reason ("stub transport down") is NEVER exposed to the user.
  const chrome = await page.evaluate(() => document.querySelector(".transcript")?.innerText ?? "");
  expect(chrome).not.toContain("transport");
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "idle", { timeout: 3000 });
  // The transport throw is CAUGHT (surfaced as a terminal shape, not an uncaught error).
  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- 4. the turn reads as in-flight the instant it is submitted, then settles (§3.4) --------------
test("the turn raises the in-flight stripe immediately and settles once the loop completes", async ({
  page,
}) => {
  await bootSpine(page);
  await setTurn(page, { hold: true, action: { type: "none" } });

  await submit(page, "make it brighter");

  // Held open by the stub: the stripe is up while the loop is mid-flight.
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "in-flight");
  // Release the transport → the turn resolves and the stripe drops.
  await setTurn(page, { hold: false });
  await expect(page.locator(".spine")).toHaveAttribute("data-turn", "idle", { timeout: 3000 });
});
