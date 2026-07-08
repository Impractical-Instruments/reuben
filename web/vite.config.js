import { resolve } from "node:path";
import { defineConfig } from "vite";

// Vite config for the reuben web player (issue #226). Root is this dir (web/); the app
// imports the engine + surface from crates/reuben-web/js/ (ADR-0041: the source of truth
// stays where P2/P3 built it), which lives OUTSIDE this root — so the dev server's fs
// allow-list is widened to the repo root. `new URL('./worklet.js', import.meta.url)` inside
// reuben-engine.mjs and `new URL('./reuben_web.wasm', ...)` are handled by Vite's asset
// pipeline; the wasm itself is staged into public/ by scripts/stage-assets.mjs, not resolved
// from the crate. A relative base keeps the built bundle path-agnostic (root domain on
// Cloudflare Pages, sub-path on a PR preview) — every fetch() uses import.meta.env.BASE_URL.
export default defineConfig({
  base: "./",
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
