import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  { ignores: ["dist", "dist-e2e", "node_modules", "e2e-results", "playwright-report"] },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: { ecmaVersion: 2022, globals: globals.browser },
    plugins: { "react-hooks": reactHooks, "react-refresh": reactRefresh },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],
    },
  },
  {
    // Run in Node, not the webview: build config and tests.
    files: ["vite.config.ts", "**/*.test.ts"],
    languageOptions: { globals: globals.node },
  },
  {
    // Playwright: the specs run in Node; the mock shell is injected into the page.
    files: ["playwright.config.ts", "e2e/**/*.ts", "bench/**/*.bench.ts"],
    languageOptions: { globals: { ...globals.node, ...globals.browser } },
    rules: { "react-hooks/rules-of-hooks": "off" },
  },
  {
    files: ["**/*.js"],
    extends: [js.configs.recommended],
    languageOptions: { ecmaVersion: 2022, sourceType: "module", globals: globals.node },
  },
);
