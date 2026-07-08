import { defineConfig, devices } from "@playwright/test";

// Playwright config for the reuben web player smoke (issue #226, scope item 7). Boots the
// BUILT app (vite preview over dist/) headless and drives the real pipeline: unlock → load →
// switch. Chromium only — the smoke asserts the pipeline is live, not cross-browser rendering
// (the manual matrix covers Safari/Firefox). The preview server is started by Playwright and
// serves the output of `npm run build` (wasm + staged assets + bundle), so run the build
// before `test:smoke` (CI does; locally `npm run build && npm run test:smoke`).

const PORT = 4173;

export default defineConfig({
  testDir: "./tests",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "list" : "line",
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // Headless has no audio device; allow the context to reach "running" without a
        // hardware sink so the smoke can assert engine state after the Start gesture.
        // PW_EXECUTABLE_PATH lets a runner with a pre-installed Chromium point at it instead
        // of a Playwright-managed download (CI leaves it unset and uses `playwright install`).
        launchOptions: {
          args: ["--autoplay-policy=no-user-gesture-required"],
          ...(process.env.PW_EXECUTABLE_PATH
            ? { executablePath: process.env.PW_EXECUTABLE_PATH }
            : {}),
        },
      },
    },
  ],
  webServer: {
    command: `npm run preview -- --port ${PORT} --strictPort`,
    url: `http://localhost:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 60_000,
  },
});
