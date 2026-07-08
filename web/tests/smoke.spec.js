import { expect, test } from "@playwright/test";

// The reuben web player CI smoke (issue #226, scope item 7). The pipeline-is-live assertion:
// boot the built app headless, tap Start (the audio unlock), open the launcher, load the
// default Toy, switch to one more, and assert the engine reports `running` and a surface DOM
// rendered — on the SAME AudioContext (no re-unlock between Toys, the #151 switching
// contract). Audio isn't audible headless; that the whole chain runs is the point.

test("boot → unlock → load groovebox → switch, on one persistent context", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await page.goto("/");

  // Splash: the Start CTA is present and the app reports the splash screen.
  await expect(page.locator("#start")).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("splash");

  // Tap Start = ctx.resume() unlock → launcher grid of all 9 Toys.
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await expect(page.locator(".toy-card")).toHaveCount(9);

  // The context is unlocked by the Start gesture.
  await expect
    .poll(() => page.evaluate(() => window.reubenPlayer.engineState()))
    .toBe("running");

  // Pick the default Toy (groovebox) → player screen; assert it loaded and a surface rendered.
  await page.locator('.toy-card[data-toy="groovebox"]').click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe("groovebox");
  // The auto-UI surface mounted with at least one control (groovebox has step + knob widgets).
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();
  // Engine still running on the same context after the load.
  expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  // Switch to a second Toy WITHOUT re-unlocking: back to launcher, pick djfilter-demo (also
  // self-playing, and it declares control blocks so the switched-in surface has widgets too;
  // several self-playing Toys — vibrato, sequence — declare none and render an empty surface).
  await page.locator(".back").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await page.locator('.toy-card[data-toy="djfilter-demo"]').click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe("djfilter-demo");
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();

  // Still running on the same persistent context — no second unlock gesture was needed.
  expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});
