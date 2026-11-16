import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// Tauri serves `dist` in the app and this dev server under `tauri dev`.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  // No `data:` URLs: the CSP allows fonts and images from the app itself only.
  build: { target: "es2022", outDir: "dist", emptyOutDir: true, assetsInlineLimit: 0 },
  test: { environment: "node", include: ["src/**/*.test.ts"] },
});
