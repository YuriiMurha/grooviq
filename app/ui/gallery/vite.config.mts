import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL(".", import.meta.url));
export default defineConfig({
  root,
  plugins: [react()],
  cacheDir: `${root}/.runtime/vite`,
  server: { host: "127.0.0.1", strictPort: true },
  build: { outDir: `${root}/.runtime/dist`, assetsInlineLimit: 0 },
});
