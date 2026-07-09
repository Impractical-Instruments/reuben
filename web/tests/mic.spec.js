import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

// The mic-affordance coverage (issue #248): duplex instruments (mic-space) load and render
// silently until something calls engine.enableMic(). The player renders an Enable-microphone
// control for any instrument whose load() reports inputChannels > 0, and surfaces the engine's
// verbatim permission copy on the shared .player-status. These tests drive that split — the
// control appears for the input-taking Toy and NOT for a generator — and the denied path, all
// against the real wasm with a Chromium fake device (no hardware, no real permission prompt).

const manifest = JSON.parse(
  readFileSync(fileURLToPath(new URL("../toys.json", import.meta.url)), "utf8"),
);
// The input-taking Toy under test and a plain generator as the negative control. Guard both
// against the manifest so a rename fails loudly here instead of as an opaque locator timeout.
const MIC_TOY = "mic-space";
const GENERATOR_TOY = "groovebox";
for (const id of [MIC_TOY, GENERATOR_TOY]) {
  if (!manifest.toys.some((t) => t.id === id)) {
    throw new Error(`mic test references Toy "${id}" but it is not in toys.json`);
  }
}

// Boot → Start (the audio unlock) → launcher → open the given Toy on the persistent engine.
async function openToy(page, id) {
  await page.goto("/");
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await page.locator(`.toy-card[data-toy="${id}"]`).click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe(id);
}

test("input-taking Toy shows the Enable-microphone control and headphones note", async ({
  page,
}) => {
  await openToy(page, MIC_TOY);
  // The control mounts in the idle state with the feedback warning alongside it.
  await expect(page.locator(".mic-enable")).toBeVisible();
  await expect(page.locator(".mic-note")).toBeVisible();
  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "idle");
});

test("a generator Toy shows no mic control", async ({ page }) => {
  await openToy(page, GENERATOR_TOY);
  // The surface mounted (the generator loaded) but no mic affordance appears.
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();
  await expect(page.locator(".mic-enable")).toHaveCount(0);
  await expect(page.locator(".mic-note")).toHaveCount(0);
});

test("denying the mic surfaces the engine copy and stays retryable", async ({ page }) => {
  // The context grants no 'microphone' permission, and the launch flags (playwright.config.js)
  // are fake-device + --deny-permission-prompts: getUserMedia({audio}) hits a present device and
  // an auto-denied prompt, rejecting NotAllowedError deterministically — no hardware, no dangling
  // prompt — which enableMic() translates to the finished user-facing copy.
  await openToy(page, MIC_TOY);

  await page.locator(".mic-enable").click();

  const status = page.locator(".player-status");
  await expect(status).toHaveText("Microphone permission denied — allow mic access and try again");
  await expect(status).toHaveClass(/error/);
  // Denied is retryable: the control returns to a tappable state, not disabled.
  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "denied");
  await expect(page.locator(".mic-enable")).toBeEnabled();
});

test("granting the mic takes the instrument live", async ({ page, context }) => {
  await context.grantPermissions(["microphone"]);
  await openToy(page, MIC_TOY);

  await page.locator(".mic-enable").click();

  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "live");
  await expect(page.locator(".mic-enable")).toBeDisabled();
});
