import { defineConfig } from "@playwright/test";

// Pure command parity: no app server, rendering or live UI readiness in the critical path.
export default defineConfig({
  testDir: "e2e",
  testMatch: ["session-replay.e2e.ts", "midi-replay.e2e.ts"],
  outputDir: "replay-results",
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"], ["./e2e/replayReporter.ts"]],
  workers: 1,
  use: { trace: "retain-on-failure" },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ],
});
