// Sampler marker grips that meet (at an edge or close together) must each stay grabbable: a press
// on a grip or its tab reaches that marker, never the one stacked next to it.
import type { Page } from "@playwright/test";
import { expect, project, soundsFor, test } from "./fixture";

const editor = (page: Page) => page.getByRole("dialog", { name: "Kick sampler" });

/** Which marker a press at the centre of each grip and of each tab reaches. */
const hits = (page: Page, ids: string[]) =>
  editor(page).evaluate((dialog, ids) => {
    const reached = (x: number, y: number) =>
      document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-param]")?.dataset.param ?? null;
    const centre = (el: Element) => {
      const r = el.getBoundingClientRect();
      return [r.left + r.width / 2, r.top + r.height / 2] as const;
    };
    return ids.map((id) => {
      const grip = dialog.querySelector(`.wmark[data-param="${id}"]`);
      const tab = grip?.querySelector(".wmark-tab");
      if (!grip || !tab) return { id, grip: null, tab: null };
      return { id, grip: reached(...centre(grip)), tab: reached(...centre(tab)) };
    });
  }, ids);

const CASES = [
  { name: "both at the left edge", sampler: { start: 0, end: 0.00001 } },
  { name: "both at the right edge", sampler: { start: 0.99999, end: 1 } },
  { name: "both at the left edge, reversed", sampler: { start: 0, end: 0.00001, reverse: 1 } },
  { name: "both at the right edge, reversed", sampler: { start: 0.99999, end: 1, reverse: 1 } },
  { name: "close together mid-waveform", sampler: { start: 0.5, end: 0.52 } },
  { name: "start, end and loop at the left edge", sampler: { start: 0, end: 0.00001, loop: 1, loop_start: 0 } },
] as const;

for (const { name, sampler } of CASES) {
  test.describe(`marker grips ${name}`, () => {
    const p = project(["Kick"]);
    p.channels[0].frames = 48_000;
    p.channels[0].sampler = sampler;
    test.use({ seed: { project: p, files: soundsFor(["Kick"]) } });

    test("each grip and tab reaches its own marker", async ({ app }) => {
      await app.getByRole("button", { name: "Edit the Kick sampler" }).click();
      await expect(editor(app).getByRole("img", { name: "Kick waveform" })).toBeVisible();
      const ids = "loop" in sampler ? ["start", "end", "loop_start"] : ["start", "end"];
      await expect.poll(() => hits(app, ids)).toEqual(ids.map((id) => ({ id, grip: id, tab: id })));
    });
  });
}
