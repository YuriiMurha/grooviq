import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Tauri serves `dist` in the app and this dev server under `tauri dev`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  // No `data:` URLs: the CSP allows fonts and images from the app itself only.
  build: { target: "es2022", outDir: "dist", emptyOutDir: true, assetsInlineLimit: 0 },
  test: {
    // Plain logic runs in Node; anything that renders React runs in jsdom.
    projects: [
      {
        extends: true,
        test: { name: "logic", environment: "node", include: ["src/**/*.test.ts"] },
      },
      {
        extends: true,
        test: {
          name: "ui",
          environment: "jsdom",
          include: ["src/**/*.test.tsx"],
          setupFiles: ["src/test/setup.ts"],
        },
      },
    ],
  },
});
