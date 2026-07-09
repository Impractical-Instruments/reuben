import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

import {
  encodeBundle,
  decodeBundle,
  ENVELOPE_PREFIX,
  CAPS,
} from "../../crates/reuben-web/js/share.mjs";

// Share-link coverage (issue #228): the web player boots a whole instrument out of a `#r1.…` URL
// fragment — no origin fetch, one tap to sound — and every malformed link lands the reader on the
// launcher with the right banner instead of crashing the tab. This spec MINTS its fixtures at test
// time from the real instrument documents on disk (encodeBundle, the app's own codec) and FORGES
// adversarial payloads the way share.test.mjs does, so it exercises the true wire format end to
// end rather than a stand-in. Follows the smoke/mic precedent: the built app over vite preview
// (playwright.config.js), audio unlocked by clicking the Start/Play button, state read off
// window.reubenPlayer.

// --- fixtures minted from the real instruments/ documents --------------------------------

const readInstrument = (rel) =>
  readFileSync(fileURLToPath(new URL(`../../instruments/${rel}`, import.meta.url)), "utf8");
const readInstrumentBytes = (rel) =>
  new Uint8Array(readFileSync(fileURLToPath(new URL(`../../instruments/${rel}`, import.meta.url))));

const VIBRATO_DOC = readInstrument("vibrato.json"); // self-playing, no resources, no controls
const EUCLIDEAN_DOC = readInstrument("euclidean-drums.json"); // self-playing, faders, no resources
const GROOVEBOX_DOC = readInstrument("groovebox.json"); // self-playing, three voice resources

// groovebox's transitive TEXT resources, keyed by the canonical root-relative keys the engine's
// discovery reports (loader.mjs readMisses) — the exact shape loadBundle resolves against.
const GROOVEBOX_RESOURCES = [
  "voices/kick-voice.json",
  "voices/snare-voice.json",
  "voices/hat-voice.json",
].map((key) => ({ key, kind: 0, bytes: readInstrumentBytes(key) }));

// --- helpers -----------------------------------------------------------------------------

// Forge a fragment from ARBITRARY TLV bytes (deflate-raw + base64url, no prefix logic) — the
// share.test.mjs idiom — so we can craft payloads the encoder would never mint (a truncated TLV,
// a kind=1 sample resource), the true shape of a hostile link.
async function forge(tlvBytes, prefix = ENVELOPE_PREFIX) {
  const cs = new CompressionStream("deflate-raw");
  const w = cs.writable.getWriter();
  w.write(tlvBytes);
  w.close();
  const comp = new Uint8Array(await new Response(cs.readable).arrayBuffer());
  return prefix + Buffer.from(comp).toString("base64url");
}

// Read the address out of a raw control buffer (codec.mjs v1: u32 LE addr length + UTF-8 bytes).
function controlAddress(buf) {
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const len = view.getUint32(0, true);
  return new TextDecoder().decode(buf.subarray(4, 4 + len));
}

const startFragmentBoot = (page, fragment) => page.goto(`/#${fragment}`);

// --- AC 1 / 2: a shared link boots and plays after one tap -------------------------------

test("a shared vibrato link boots and plays after one tap", async ({ page }) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  const fragment = await encodeBundle({ docText: VIBRATO_DOC });
  await startFragmentBoot(page, fragment);

  // The fragment-boot splash offers a single Start/Play gesture (iOS: one tap to sound).
  await expect(page.locator("#start")).toBeVisible();
  await page.locator("#start").click();

  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("player");
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.instrument())).toBe("vibrato");
  // Engine running on the unlocked context, and the auto-UI surface mounted (vibrato is a
  // controlless drone, so we assert the surface rendered, not a specific widget).
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");
  await expect(page.locator(".surface-mount")).toHaveClass(/reuben-surface/);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- AC 3: origin independence — a multi-resource bundle boots without any origin fetch ----

test("a shared groovebox link boots entirely from the bundle, fetching no instruments/", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  // The link carries the document PLUS its three voice patches, so booting it must never fetch
  // those resources from the origin — that's the whole point: it plays where the voices/ files are
  // NOT served. Record any fetch of a bundle-carried resource key to prove none happens. (The app
  // separately warms the DEFAULT Toy's document, instruments/groovebox.json, at boot — a
  // speculative launcher prefetch unrelated to this fragment boot, which reads the doc from the
  // link, so we scope the assertion to the transitive resources that must ride in the bundle.)
  const RESOURCE_KEYS = GROOVEBOX_RESOURCES.map((r) => r.key);
  const resourceFetches = [];
  page.on("request", (r) => {
    if (RESOURCE_KEYS.some((key) => r.url().includes(`/instruments/${key}`))) {
      resourceFetches.push(r.url());
    }
  });

  const fragment = await encodeBundle({ docText: GROOVEBOX_DOC, resources: GROOVEBOX_RESOURCES });
  await startFragmentBoot(page, fragment);
  await page.locator("#start").click();

  await expect.poll(() => page.evaluate(() => window.reubenPlayer.instrument())).toBe("groovebox");
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.engineState())).toBe("running");
  // groovebox has faders + step lanes — its surface has widgets.
  await expect(page.locator(".surface-mount .surface-widget").first()).toBeVisible();

  expect(
    resourceFetches,
    `booted from the bundle without fetching any voice resource, saw: ${resourceFetches.join(", ")}`,
  ).toEqual([]);
  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// --- AC 2: round-trip — move a control, Share, and the link carries the doc + the move ------

test("moving a control then Share round-trips the document and the moved control", async ({
  page,
  context,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  // Force the clipboard branch of the share chain: with Web Share suppressed, Share writes the URL
  // to the clipboard (the desktop path we can read back deterministically).
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "canShare", { configurable: true, value: undefined });
    Object.defineProperty(navigator, "share", { configurable: true, value: undefined });
  });

  const fragment = await encodeBundle({ docText: EUCLIDEAN_DOC });
  await startFragmentBoot(page, fragment);
  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("player");

  // Move the first fader (the /clock/tempo radial) to its maximum so its journalled value differs
  // from the load-time default — the snapshot must then carry exactly this control.
  const fader = page.locator(".surface-mount input[type=range]").first();
  await expect(fader).toBeVisible();
  await fader.evaluate((el) => {
    el.value = "1";
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });

  await page.locator(".share").click();

  // The clipboard now holds the share URL (the flash "Copied!" confirms the branch fired).
  await expect
    .poll(async () => (await page.evaluate(() => navigator.clipboard.readText())) || "")
    .toContain(`#${ENVELOPE_PREFIX}`);
  const url = await page.evaluate(() => navigator.clipboard.readText());

  const decoded = await decodeBundle(url.slice(url.indexOf("#") + 1));
  // The untouched document round-trips byte-identically (AC 2).
  expect(decoded.docText).toBe(EUCLIDEAN_DOC);
  // The sidecar carries the one control we moved, and no more.
  expect(decoded.snapshot.length).toBeGreaterThan(0);
  const addrs = decoded.snapshot.map(controlAddress);
  expect(addrs).toContain("/clock/tempo");
});

// --- AC 4: every failure class lands on the launcher with the right banner, no crash --------

// Class A is special: a non-r1 hash (#about) is NOT a share link, so the app takes the ordinary
// splash flow SILENTLY (no banner) — deliverable 1.1. One tap still reaches the launcher, still
// with no banner, and nothing crashes.
test("class A — a non-share hash (#about) is silent: splash, no banner, no crash", async ({
  page,
}) => {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));

  await page.goto("/#about");
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("splash");
  expect(await page.evaluate(() => window.reubenPlayer.banner())).toBe(null);

  await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  expect(await page.evaluate(() => window.reubenPlayer.banner())).toBe(null);

  expect(errors, `no uncaught page errors: ${errors.join("; ")}`).toEqual([]);
});

// Classes B–I: each fixture is one malformed/unbuildable link. Decode-time failures (B–E′) land on
// the launcher at boot with no gesture; document failures (F–I) surface only after loadBundle runs,
// so those need the Start tap. In every case the tab does not crash and the banner text is asserted.
async function bootAndLand(page, fragment, { gesture = false } = {}) {
  const errors = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await startFragmentBoot(page, fragment);
  if (gesture) await page.locator("#start").click();
  await expect.poll(() => page.evaluate(() => window.reubenPlayer.screen())).toBe("launcher");
  const banner = await page.evaluate(() => window.reubenPlayer.banner());
  return { banner, errors };
}

test("class B — a link from the future (r2.) → newer-version banner", async ({ page }) => {
  const { banner, errors } = await bootAndLand(page, "r2.anything");
  expect(banner).toBe("This link was made by a newer version of reuben.");
  expect(errors).toEqual([]);
});

test("class C — invalid base64url → damaged banner", async ({ page }) => {
  const { banner, errors } = await bootAndLand(page, "r1.@@@");
  expect(banner).toBe("This link is damaged.");
  expect(errors).toEqual([]);
});

test("class D — a deflate bomb over the decompressed cap → too-large banner", async ({ page }) => {
  // A >1 MB document compresses to a few KB (under the fragment cap) but inflates past the 1 MB
  // decompressed cap — the streaming abort fires at boot.
  const bomb = await encodeBundle({ docText: "z".repeat(2 * 1024 * 1024) });
  expect(bomb.length).toBeLessThanOrEqual(CAPS.FRAGMENT_BYTES);
  const { banner, errors } = await bootAndLand(page, bomb);
  expect(banner).toBe("This link is too large.");
  expect(errors).toEqual([]);
});

test("class E — a truncated TLV → damaged banner", async ({ page }) => {
  // docLen=10 declared but only 4 body bytes present: the bounds-checked reader must refuse.
  const tlv = new Uint8Array(4 + 4);
  new DataView(tlv.buffer).setUint32(0, 10, true);
  const { banner, errors } = await bootAndLand(page, await forge(tlv));
  expect(banner).toBe("This link is damaged.");
  expect(errors).toEqual([]);
});

test("class E′ — a kind=1 (sample) resource in the TLV → sample banner", async ({ page }) => {
  // doc="{}", 1 resource, key="a", kind=1 — refused at parse before its data is read.
  const enc = new TextEncoder();
  const key = enc.encode("a");
  const doc = enc.encode("{}");
  const tlv = new Uint8Array(4 + doc.length + 4 + 4 + key.length + 1);
  const v = new DataView(tlv.buffer);
  let p = 0;
  v.setUint32(p, doc.length, true);
  p += 4;
  tlv.set(doc, p);
  p += doc.length;
  v.setUint32(p, 1, true); // resource count
  p += 4;
  v.setUint32(p, key.length, true);
  p += 4;
  tlv.set(key, p);
  p += key.length;
  tlv[p] = 1; // kind = 1 (sample)
  const { banner, errors } = await bootAndLand(page, await forge(tlv));
  expect(banner).toBe("This instrument uses audio samples, which can't be shared as links yet.");
  expect(errors).toEqual([]);
});

test("class F — a document that isn't valid JSON → the engine's verbatim message", async ({
  page,
}) => {
  const fragment = await encodeBundle({ docText: "{ not valid json" });
  const { banner, errors } = await bootAndLand(page, fragment, { gesture: true });
  expect(banner).toMatch(/invalid JSON/i);
  expect(errors).toEqual([]);
});

test("class G — a document from a newer format version → the engine's verbatim message", async ({
  page,
}) => {
  const doc = JSON.stringify({ ...JSON.parse(VIBRATO_DOC), format_version: 999 });
  const { banner, errors } = await bootAndLand(page, await encodeBundle({ docText: doc }), {
    gesture: true,
  });
  expect(banner).toMatch(/format version/i);
  expect(errors).toEqual([]);
});

test("class H — a document with an unknown operator → the engine's verbatim message", async ({
  page,
}) => {
  const doc = JSON.stringify({
    format_version: 2,
    instrument: "broken",
    nodes: [{ type: "not_a_real_operator", address: "/x", inputs: {} }],
  });
  const { banner, errors } = await bootAndLand(page, await encodeBundle({ docText: doc }), {
    gesture: true,
  });
  expect(banner).toMatch(/unknown operator type/i);
  expect(errors).toEqual([]);
});

test("class I — a bundle missing a referenced resource → incomplete banner", async ({ page }) => {
  // The groovebox document with NO resources: discovery misses the voice patches, and a bundle
  // miss is terminal (never an origin fetch) → the incomplete-link banner.
  const fragment = await encodeBundle({ docText: GROOVEBOX_DOC, resources: [] });
  const { banner, errors } = await bootAndLand(page, fragment, { gesture: true });
  expect(banner).toBe("This link is incomplete.");
  expect(errors).toEqual([]);
});
