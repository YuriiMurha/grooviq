import { defineConfig, devices } from "@playwright/test";
import { PORT, PROD_PORT } from "./e2e/ports";

// End-to-end tests: the production web build (`vite build`, served by `vite preview`) driven by
// Playwright, with the Tauri shell and the engine replaced by the in-memory mock in
// e2e/mockTauri.ts. WebKit is the engine the macOS app runs on; Chromium is Windows' WebView2.
//
// The specs run against an e2e-mode build; a production build is served beside it for the checks
// that must see what ships (e2e/ports.ts).
// The bench (bench/roll.bench.ts) is a project of its own that only `npm run bench` turns on
// (GROOVIQ_BENCH=1): CI never runs it. It drives the installed Google Chrome on the GPU.
const BENCH = process.env.GROOVIQ_BENCH === "1";

export default defineConfig({
  testDir: "e2e",
  testMatch: /.*\.e2e\.ts$/,
  outputDir: "e2e-results",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"], ["./e2e/diagnosticsReporter.ts"], ...(process.env.GROOVIQ_E2E_JSON ? [["json", { outputFile: process.env.GROOVIQ_E2E_JSON }] as [string, { outputFile: string }]] : [])],
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    viewport: { width: 1280, height: 800 },
    trace: "retain-on-failure",
  },
  projects: [
    { name: "webkit", use: { ...devices["Desktop Safari"], viewport: { width: 1280, height: 800 } } },
    { name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1280, height: 800 } } },
    ...(BENCH
      ? [
          {
            name: "bench",
            testDir: "bench",
            testMatch: /.*\.bench\.ts$/,
            timeout: 120_000,
            use: {
              channel: "chrome",
              viewport: { width: 1280, height: 800 },
              launchOptions: { args: ["--headless=new", "--use-angle=metal", "--enable-gpu-rasterization", "--ignore-gpu-blocklist"] },
            },
          },
        ]
      : []),
  ],
  webServer: [
    {
      // `--mode e2e` (into its own folder, never the app's `dist`): the roll's counters are on
      // `window` outside production builds, and the bench page is built too.
      command: `npx vite build --mode e2e --outDir dist-e2e && npx vite preview --outDir dist-e2e --host 127.0.0.1 --port ${PORT} --strictPort`,
      url: `http://127.0.0.1:${PORT}`,
      // Never someone else's server: if the port is taken, fail instead (--strictPort).
      reuseExistingServer: false,
      timeout: 120_000,
    },
    {
      command: `npx vite build --outDir dist-prod && npx vite preview --outDir dist-prod --host 127.0.0.1 --port ${PROD_PORT} --strictPort`,
      url: `http://127.0.0.1:${PROD_PORT}`,
      reuseExistingServer: false,
      timeout: 120_000,
    },
  ],
});
