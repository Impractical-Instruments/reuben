import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

// The reuben web player CI smoke (issue #226, scope item 7). The pipeline-is-live assertion:
// boot the built app headless, tap Start (the audio unlock), open the launcher, load the
// default Toy, switch to one more, and assert the engine reports `running` and a surface DOM
// rendered — on the SAME AudioContext (no re-unlock between Toys, the #151 switching
// contract). Audio isn't audible headless; that the whole chain runs is the point.

// The launcher grid renders exactly the Toys in the manifest, so derive the expected count
// from toys.json rather than hardcoding it — editing the Toy list no longer breaks this test.
const manifest = JSON.parse(
  readFileSync(fileURLToPath(new URL("../toys.json", import.meta.url)), "utf8"),
);
const TOY_COUNT = manifest.toys.length;

// The default Toy is loaded first; SWITCH_TOY is the second Toy the test tabs to. SWITCH_TOY
// must be self-playing AND declare interface pipes (bound by its surface doc, or auto-derived
// — ADR-0043) so its switched-in surface renders a widget (line ~44). Guard both ids against
// the manifest so a Toy renamed/removed out from under the test fails loudly here instead of
// as an opaque locator timeout below.
const DEFAULT_TOY = manifest.default;
const SWITCH_TOY = "euclidean-drums";
for (const id of [DEFAULT_TOY, SWITCH_TOY]) {
  if (!manifest.toys.some((t) => t.id === id)) {
    throw new Error(`smoke test references Toy "${id}" but it is not in toys.json`);
  }
}

test("boot → unlock → load default Toy → switch, on one persistent context", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await page.goto("/");

  // Splash: the Start CTA is present and the app reports the splash screen.
  await expect(page.locator("#start")).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("splash");

  // Tap Start = ctx.resume() unlock → launcher grid of every manifest Toy.
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await expect(page.locator(".toy-card")).toHaveCount(TOY_COUNT);

  // The context is unlocked by the Start gesture.
  await expect
    .poll(() => page.evaluate(() => window.reubenPlayer.engineState()))
    .toBe("running");

  // Pick the default Toy → player screen; assert it loaded and a surface rendered.
  await page.locator(`.toy-card[data-toy="${DEFAULT_TOY}"]`).click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe(DEFAULT_TOY);
  // The auto-UI surface mounted with at least one control (the default Toy has widgets).
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();
  // Engine still running on the same context after the load.
  expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  // Switch to a second Toy WITHOUT re-unlocking: back to launcher, pick SWITCH_TOY (also
  // self-playing, and its interface pipes + surface doc give the switched-in surface widgets too).
  await page.locator(".back").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await page.locator(`.toy-card[data-toy="${SWITCH_TOY}"]`).click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe(SWITCH_TOY);
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();

  // Still running on the same persistent context — no second unlock gesture was needed.
  expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});
