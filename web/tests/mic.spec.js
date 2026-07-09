import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

// The mic-affordance coverage (issue #248): duplex instruments (mic-space) load and render
// silently until something calls engine.enableMic(). The player renders an Enable-microphone
// control for any instrument whose load() reports inputChannels > 0, and surfaces the engine's
// verbatim permission copy on the shared .player-status. These tests drive that split — the
// control appears for the input-taking Toy and NOT for a generator — plus each enableMic()
// outcome (denied / no device / live).
//
// getUserMedia is the one nondeterministic boundary here (ADR-0038 §10) and, worse, headless
// Chromium's fake-device support varies by build (a real device isn't guaranteed and some
// builds reject audio capture outright). So the outcome tests stub navigator.mediaDevices
// .getUserMedia at that boundary — the exact seam ADR-0038 sanctions injecting at — rejecting
// with the real DOMException names enableMic() maps, or resolving a real MediaStream synthesized
// from Web Audio (no hardware). This exercises the whole enableMic() → player path and is
// deterministic on any Chromium; nothing here needs a real mic.

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

// Replace getUserMedia before any app script runs (addInitScript fires on the openToy
// navigation, ahead of main.js), so engine.enableMic() sees the stub. `mode` is one of the
// keys below: a rejection with a specific DOMException name, or a resolved real MediaStream.
async function stubGetUserMedia(page, mode) {
  await page.addInitScript((m) => {
    const reject = (name, message) => () => Promise.reject(new DOMException(message, name));
    const stubs = {
      // A real, live audio track with no device: a MediaStreamDestination's stream. enableMic()
      // wires it through ctx.createMediaStreamSource, so it must be an actual MediaStream.
      live: async () => new AudioContext().createMediaStreamDestination().stream,
      denied: reject("NotAllowedError", "Permission denied"),
      noDevice: reject("NotFoundError", "Requested device not found"),
    };
    navigator.mediaDevices.getUserMedia = stubs[m];
  }, mode);
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
  // getUserMedia rejects NotAllowedError (a real permission denial), which enableMic()
  // translates to the finished user-facing copy.
  await stubGetUserMedia(page, "denied");
  await openToy(page, MIC_TOY);

  await page.locator(".mic-enable").click();

  const status = page.locator(".player-status");
  await expect(status).toHaveText("Microphone permission denied — allow mic access and try again");
  await expect(status).toHaveClass(/error/);
  // Denied is retryable: the control returns to a tappable state, not disabled.
  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "denied");
  await expect(page.locator(".mic-enable")).toBeEnabled();
});

test("a device with no input surfaces the no-microphone copy", async ({ page }) => {
  // getUserMedia rejects NotFoundError, exactly what a device with no input throws; enableMic()
  // maps it to the finished "no microphone" copy, surfaced through the same verbatim path.
  await stubGetUserMedia(page, "noDevice");
  await openToy(page, MIC_TOY);

  await page.locator(".mic-enable").click();

  const status = page.locator(".player-status");
  await expect(status).toHaveText("No microphone found on this device");
  await expect(status).toHaveClass(/error/);
  // A missing device is retryable too (plug one in, tap again).
  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "denied");
  await expect(page.locator(".mic-enable")).toBeEnabled();
});

test("granting the mic takes the instrument live", async ({ page }) => {
  // getUserMedia resolves a real MediaStream (no device); enableMic() wires it and the control
  // goes live. Audibility through the reverb can't be asserted headless — the engine's check.mjs
  // duplex passthrough covers that; here we assert the player reflects the live state.
  await stubGetUserMedia(page, "live");
  await openToy(page, MIC_TOY);

  await page.locator(".mic-enable").click();

  await expect(page.locator(".mic-control")).toHaveAttribute("data-mic-state", "live");
  await expect(page.locator(".mic-enable")).toBeDisabled();
});
