// Sampler marker grips that meet (at an edge or close together) must each stay grabbable: a press
// on a grip or its tab reaches that marker, never the one stacked next to it.
import type { Page } from "@playwright/test";
import { expect, project, soundsFor, test } from "./fixture";

const editor = (page: Page) => page.getByRole("dialog", { name: "Kick sampler" });

/** Per marker: whether it can be dragged, whether its grip and tab boxes are non-empty and inside
 * the waveform well, which marker a press at each centre reaches, and how much grip area it
 * shares with any other grip. */
const grips = (page: Page, ids: string[]) =>
  editor(page).evaluate((dialog, ids) => {
    const well = dialog.querySelector(".sample-wave")!.getBoundingClientRect();
    const inside = (r: DOMRect) =>
      r.width > 0 && r.height > 0 &&
      r.left >= well.left - 0.5 && r.right <= well.right + 0.5 && r.top >= well.top - 0.5 && r.bottom <= well.bottom + 0.5;
    const reached = (r: DOMRect) =>
      document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2)?.closest<HTMLElement>("[data-param]")?.dataset.param ?? null;
    const boxes = ids.map((id) => dialog.querySelector<HTMLElement>(`.wmark[data-param="${id}"]`));
    const overlap = (a: DOMRect, b: DOMRect) =>
      Math.max(0, Math.min(a.right, b.right) - Math.max(a.left, b.left)) * Math.max(0, Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top));
    return ids.map((id, i) => {
      const grip = boxes[i];
      const tab = grip?.querySelector(".wmark-tab");
      if (!grip || !tab) return { id, missing: true };
      const g = grip.getBoundingClientRect();
      const t = tab.getBoundingClientRect();
      const shared = boxes.reduce((sum, other, j) => (j === i || !other ? sum : sum + overlap(g, other.getBoundingClientRect())), 0);
      return {
        id,
        enabled: grip.getAttribute("aria-disabled") === "false",
        inWell: inside(g) && inside(t),
        grip: reached(g),
        tab: reached(t),
        shared: Math.round(shared * 100) / 100,
      };
    });
  }, ids);

// 0.0001 of 48,000 frames is 4.8 frames, so both markers stay movable at the edges.
const CASES = [
  { name: "both at the left edge", sampler: { start: 0, end: 0.0001 } },
  { name: "both at the right edge", sampler: { start: 0.9999, end: 1 } },
  { name: "both at the left edge, reversed", sampler: { start: 0, end: 0.0001, reverse: 1 } },
  { name: "both at the right edge, reversed", sampler: { start: 0.9999, end: 1, reverse: 1 } },
  { name: "close together mid-waveform", sampler: { start: 0.5, end: 0.52 } },
  // Just past the point where the grips stop stacking: side by side, they must not touch.
  { name: "just apart mid-waveform", sampler: { start: 0.5, end: 0.543 } },
  { name: "start, end and loop at the left edge", sampler: { start: 0, end: 0.0001, loop: 1, loop_start: 0 } },
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
      await expect
        .poll(() => grips(app, ids))
        .toEqual(ids.map((id) => ({ id, enabled: true, inWell: true, grip: id, tab: id, shared: 0 })));
    });
  });
}
