import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { defineConfig } from "vite";
import { VitePWA } from "vite-plugin-pwa";

// Vite config for the reuben web player (issue #226, PWA layer #227). Root is this dir (web/);
// the app imports the engine + surface from crates/reuben-web/js/ (ADR-0041: the source of
// truth stays where P2/P3 built it), which lives OUTSIDE this root — so the dev server's fs
// allow-list is widened to the repo root. `new URL('./worklet.js', import.meta.url)` inside
// reuben-engine.mjs and `new URL('./reuben_web.wasm', ...)` are handled by Vite's asset
// pipeline; the wasm itself is staged into public/ by scripts/stage-assets.mjs, not resolved
// from the crate. A relative base keeps the built bundle path-agnostic (root domain on
// Cloudflare Pages, sub-path on a PR preview) — every fetch() uses import.meta.env.BASE_URL.

// The offline-precache list (issue #227, scope item 2). stage-assets.mjs writes exactly what it
// staged — the wasm, schema, every Toy doc + its transitive voices/subpatches/samples — each with
// a content revision, and we hand that verbatim to Workbox as additionalManifestEntries. Precache
// is therefore GENERATED FROM the same transitive discovery that produces the payload: it cannot
// ship an asset it didn't cache, or cache one it didn't ship. These .wasm/.json/.wav URLs are
// deliberately absent from workbox.globPatterns below, so the two lists never double-precache the
// same file.
//
// `npm run build`/`dev` both run `stage` first, so the file exists then. Only a MISSING file
// (ENOENT) falls back to [] — the documented "bare `vite`/`vite preview` with no staging" case,
// which precaches just the app shell. A present-but-corrupt list is a real build defect and
// rethrows, rather than silently shipping a shell-only SW that offline-fails only at runtime.
function precacheEntries() {
  const path = resolve(import.meta.dirname, ".pwa-precache.json");
  let raw;
  try {
    raw = readFileSync(path, "utf8");
  } catch (err) {
    if (err.code === "ENOENT") return [];
    throw err;
  }
  return JSON.parse(raw); // malformed JSON throws — a corrupt precache list must not build green
}

export default defineConfig({
  base: "./",
  plugins: [
    VitePWA({
      // Ship-and-forget: a new deploy's SW installs, precaches the new revisions, and takes
      // over on next load — no update-prompt UI for a single-screen toy player. Combined with
      // the content revisions in the precache list, only changed assets are re-fetched.
      registerType: "autoUpdate",
      // We register the SW ourselves from src/main.js (virtual:pwa-register); `false` stops the
      // plugin from ALSO injecting a registration into index.html (which would double-register).
      injectRegister: false,
      // Derive every PWA icon size from the single master asset (issue #227, scope item 3 +
      // human prerequisite: web/src/assets/icon.png, 512×512). The minimal-2023 preset emits the
      // 64/192/512 `any` icons, the 512 maskable, the 180 apple-touch, and favicon.ico, injects
      // the <link rel="icon"/apple-touch-icon"> tags into index.html, and (overrideManifestIcons)
      // populates the manifest `icons` array — so the manifest + html icon wiring is generated,
      // not hand-maintained.
      pwaAssets: {
        // stage-assets.mjs copies the committed master (src/assets/icon.png) to public/icon.png;
        // the generator resolves its output dir relative to publicDir, so pointing it at the
        // public/ copy lands the derived sizes in the built payload (dist root) rather than
        // escaping back into src/. See the MASTER_ICON note in stage-assets.mjs.
        image: "public/icon.png",
        preset: "minimal-2023",
        overrideManifestIcons: true,
      },
      manifest: {
        name: "reuben — zero to music",
        short_name: "reuben",
        description:
          "Open a URL, tap once, make music. A browser instrument built from composable operators.",
        // standalone = launched-from-home-screen opens chrome-less (the #227 install AC).
        display: "standalone",
        // Both = the storybook splash's sky ground (#6a9db1): the OS status bar / cold-launch
        // splash match the first screen the player sees, so install + launch read as one surface.
        theme_color: "#6a9db1",
        background_color: "#6a9db1",
        // Relative so the installed scope follows wherever the path-agnostic bundle is served
        // (root on Cloudflare production, a preview subpath on a PR) — same rationale as base.
        start_url: ".",
        scope: ".",
        orientation: "any",
        categories: ["music", "entertainment"],
      },
      workbox: {
        // App shell only: the hashed JS/CSS Vite emits, index.html, and the generated icons.
        // NOT webmanifest — vite-plugin-pwa already injects manifest.webmanifest into the
        // precache itself, so globbing it too lists it twice with different revisions and
        // Workbox aborts the whole precache (add-to-cache-list-conflicting-entries). Likewise the
        // payload proper (wasm/json/wav) is owned by additionalManifestEntries and kept out of
        // these globs, so nothing is precached twice.
        globPatterns: ["**/*.{js,css,html,svg,png,ico,woff2}"],
        // icon.png is the master the icon set is derived FROM (stage-assets copies it into
        // public/ for the generator); Vite copies it to dist too, but nothing references it —
        // the manifest + html use the derived pwa-*.png. Keep the raw master out of the precache.
        globIgnores: ["icon.png"],
        additionalManifestEntries: precacheEntries(),
        // Single-screen app: any offline navigation (a reload, a cold home-screen launch) is
        // served the precached shell so the player always boots offline.
        navigateFallback: "index.html",
        cleanupOutdatedCaches: true,
        // Take control of the page on first activate (autoUpdate emits skipWaiting but not
        // clientsClaim). Without it the SW controls nothing until the next navigation, so the
        // very first visit couldn't go offline mid-session — with it, the payload is precached
        // AND controlling by the time the splash is up, so even a first-session network drop is
        // covered. Pairs with skipWaiting so a new deploy's SW takes over without a manual reload.
        clientsClaim: true,
      },
      // Default (false): the SW is built only by `vite build`, so `vite preview` (what the
      // Playwright smoke drives) serves a real SW while `vite dev` stays SW-free. Left explicit
      // so nobody wonders why `npm run dev` has no service worker.
      devOptions: { enabled: false },
    }),
  ],
  server: {
    fs: {
      // Allow serving the imported engine/surface modules from the repo root above web/.
      allow: [resolve(import.meta.dirname, "..")],
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    // Fail the build if a staged asset is missing rather than silently shipping a broken
    // bundle — the staging script is the drift guard, this is the belt.
    emptyOutDir: true,
  },
});
