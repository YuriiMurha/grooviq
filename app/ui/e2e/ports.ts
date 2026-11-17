import { createHash } from "node:crypto";

// Each checkout serves its builds on ports of its own (E2E_PORT overrides): agents run the e2e
// tests in parallel worktrees, and a shared port would let one run test another checkout's build.
const ui = new URL("../", import.meta.url).pathname;

/** The e2e-mode build (`dist-e2e`): the roll's counters on `window`, the bench page. */
export const PORT = Number(process.env.E2E_PORT) || 4100 + (parseInt(createHash("sha1").update(ui).digest("hex").slice(0, 6), 16) % 800);
/** The production build (`dist-prod`), as the app ships it. */
export const PROD_PORT = PORT + 1000;
export const PROD_URL = `http://127.0.0.1:${PROD_PORT}`;
