import { chromium, expect, test } from "@playwright/test";
import { mkdtemp, rm } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { join } from "node:path";

// Derive the launcher grid size + default Toy from the manifest (as smoke.spec.js does), so
// editing the Toy list doesn't break these PWA assertions. DEFAULT_TOY is the Toy the offline
// test loads from cache; guard it against the manifest so a renamed default fails loudly here.
const manifest = JSON.parse(
  readFileSync(fileURLToPath(new URL("../toys.json", import.meta.url)), "utf8"),
);
const TOY_COUNT = manifest.toys.length;
const DEFAULT_TOY = manifest.default;
if (!manifest.toys.some((t) => t.id === DEFAULT_TOY)) {
  throw new Error(`pwa test's default Toy "${DEFAULT_TOY}" is not in toys.json`);
}

// The reuben web player PWA smoke (issue #227, scope item 4). Proves the offline/installable
// layer on the BUILT app (vite preview over dist/, which is the only build that emits a real
// service worker — vite dev doesn't). Two guarantees a headless runner CAN certify (the
// touch/install/screen-lock behavior it can't is the human sign-off on the PR):
//
//   1. the SW registers, activates, precaches the payload, and takes control of the page; a
//      full cold reload with the network CUT still boots the app and plays a Toy — i.e. the
//      precache (wired off stage-assets' transitive discovery) actually covers what the engine
//      pulls: wasm, worklet, the Toy doc + its voices, the schema;
//   2. the app is installable — Chrome's own installability check (via CDP) reports no errors,
//      and the manifest parses. (Lighthouse dropped its PWA category in v12, so this asserts
//      the same signal Lighthouse used to, straight from the DevTools protocol.)

// Wait for a Workbox-precaching SW to be active AND controlling this page. `controller` is the
// signal that fetches now route through the SW (so precache is in place); precache completes
// during the SW's install, before it activates + claims, so a non-null controller means the
// payload is cached and offline is safe to exercise.
async function waitForServiceWorkerControl(page) {
  await page.waitForFunction(
    () => navigator.serviceWorker && navigator.serviceWorker.controller != null,
    null,
    { timeout: 30_000 },
  );
}

test("service worker registers, activates, and controls the page", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#start")).toBeVisible();
  await waitForServiceWorkerControl(page);

  const state = await page.evaluate(async () => {
    const reg = await navigator.serviceWorker.getRegistration();
    return { active: !!reg?.active, controller: !!navigator.serviceWorker.controller };
  });
  expect(state.active, "an active service worker registration exists").toBe(true);
  expect(state.controller, "the SW controls this page (fetches route through it)").toBe(true);
});

test("offline cold reload boots and plays a Toy entirely from cache", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  // First online load: register the SW and let it precache the whole payload + take control.
  await page.goto("/");
  await expect(page.locator("#start")).toBeVisible();
  await waitForServiceWorkerControl(page);

  // Cut the network. Everything from here — the shell reload, the wasm + worklet, the Toy doc,
  // its voices, the schema — must be served from the SW precache or the app is not offline-safe.
  await page.context().setOffline(true);

  // Cold reload with no network: the navigation itself is served from cache (precache +
  // navigateFallback), so the splash must render again.
  await page.reload();
  await expect(page.locator("#start")).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer?.screen())).toBe("splash");

  // Tap Start (the unlock) → launcher: the engine boot (wasm + worklet fetches) resolves from
  // cache while offline.
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  await expect(page.locator(".toy-card")).toHaveCount(TOY_COUNT);
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  // Load a Toy offline: its document + transitive voices + the schema all come from precache,
  // and the auto-UI surface renders — the payload is genuinely complete offline.
  await page.locator(`.toy-card[data-toy="${DEFAULT_TOY}"]`).click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.toy())).toBe(DEFAULT_TOY);
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();
  expect(await page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");

  expect(errors, `no uncaught page errors offline: ${errors.join("; ")}`).toEqual([]);
});

test("app is installable (Chrome installability check + manifest parse)", async ({ baseURL }) => {
  // Chrome refuses installation in incognito, and Playwright's default context is incognito-like
  // — it always reports an `in-incognito` installability error that has nothing to do with the
  // app. So this one check runs in a PERSISTENT (on-disk profile) context, which is a normal
  // browsing mode: any installability error it reports is a real defect in the manifest/SW/icons.
  // executablePath mirrors playwright.config.js (a runner's pre-installed Chromium, else the
  // Playwright-managed one when the env var is unset).
  const userDataDir = await mkdtemp(join(tmpdir(), "reuben-pwa-"));
  const context = await chromium.launchPersistentContext(userDataDir, {
    headless: true,
    args: ["--autoplay-policy=no-user-gesture-required"],
    ...(process.env.PW_EXECUTABLE_PATH
      ? { executablePath: process.env.PW_EXECUTABLE_PATH }
      : {}),
  });
  try {
    const page = context.pages()[0] ?? (await context.newPage());
    const client = await context.newCDPSession(page);
    await client.send("Page.enable");

    await page.goto(baseURL + "/");
    await expect(page.locator("#start")).toBeVisible();
    await waitForServiceWorkerControl(page);

    // The manifest parses cleanly (empty `errors`) and declares the icons/display Chrome needs.
    const manifest = await client.send("Page.getAppManifest");
    expect(
      manifest.errors,
      `manifest parsed without errors: ${JSON.stringify(manifest.errors)}`,
    ).toEqual([]);
    expect(manifest.url, "a manifest is linked").toContain("manifest.webmanifest");

    // Installability is evaluated lazily and can report a transient `no-matching-service-worker`
    // (or a restarted pipeline) right after load, so poll until Chrome reports zero errors rather
    // than trusting a single early call. A non-empty steady state is a real installability defect.
    let installabilityErrors = [];
    await expect
      .poll(
        async () => {
          ({ installabilityErrors } = await client.send("Page.getInstallabilityErrors"));
          return installabilityErrors.length;
        },
        { timeout: 20_000, message: "Chrome reports no installability errors" },
      )
      .toBe(0);
  } finally {
    await context.close();
    await rm(userDataDir, { recursive: true, force: true });
  }
});
