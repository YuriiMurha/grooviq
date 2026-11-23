import { defineConfig } from "@playwright/test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
const baseURL = process.env.GROOVIQ_GALLERY_URL;
if (!baseURL || !/^http:\/\/127\.0\.0\.1:\d+\/?$/.test(baseURL)) {
  throw new Error("GROOVIQ_GALLERY_URL must identify the managed loopback gallery server");
}
// Captures are candidates. Approved comparisons can fail on missing references,
// but this fixture must never manufacture or update its own visual approval.
if (process.argv.some((arg) => arg === "-u" || arg.startsWith("--update-snapshots"))) {
  throw new Error("Gallery baseline updates require separate reference approval");
}
export default defineConfig({
  testDir: root, testMatch: "gallery.spec.ts", workers: 1, fullyParallel: false,
  retries: 0, timeout: 30_000, updateSnapshots: "none",
  outputDir: `${root}/.runtime/results`,
  snapshotPathTemplate: `${root}/approved/{projectName}/{arg}{ext}`,
  reporter: [["list"], ["json", { outputFile: `${root}/.runtime/report.json` }]],
  use: { baseURL, viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1, reducedMotion: "reduce" },
  projects: [
    { name: "chromium", use: { browserName: "chromium" } },
    { name: "webkit", use: { browserName: "webkit" } },
  ],
  webServer: [],
});
