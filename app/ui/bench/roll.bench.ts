// The piano roll's frame budget (Global Constraints, performance acceptance): run by
// `npm run bench` in the installed Google Chrome on the GPU, never in CI.
import { expect, test } from "@playwright/test";
import type { BenchResult } from "./roll";

test("ten_thousand_notes_edit_within_the_frame_budget", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/bench/roll.html");
  const handle = await page.waitForFunction(
    () => {
      const w = window as unknown as { __bench?: unknown; __benchError?: string };
      return w.__bench ?? (w.__benchError ? { error: w.__benchError } : undefined);
    },
    null,
    { timeout: 100_000 },
  ).catch(async (e: unknown) => {
    throw new Error(`${String(e)}; stalled at ${await page.evaluate(() => JSON.stringify([(window as unknown as { __benchStage?: string }).__benchStage, (window as unknown as { __grooviqRoll?: unknown }).__grooviqRoll]))}`);
  });
  const r = (await handle.jsonValue()) as BenchResult & { error?: string };
  console.log(`piano roll bench: ${JSON.stringify(r, null, 2)}`);
  expect(r.error).toBeUndefined();
  expect(errors).toEqual([]);
  // On the GPU, not a software rasteriser.
  expect(r.renderer).not.toMatch(/swiftshader|llvmpipe|software/i);
  expect(r.selectedForMove).toBeGreaterThan(800);
  expect(r.selectedForResize).toBeGreaterThan(150);
  // The move and the resize each committed once, with every selected note.
  expect(r.updated).toEqual([r.selectedForMove, r.selectedForResize]);
  expect(r.p95).toBeLessThanOrEqual(1.05 * r.refreshMs);
  expect(r.max).toBeLessThanOrEqual(2 * r.refreshMs);
});
