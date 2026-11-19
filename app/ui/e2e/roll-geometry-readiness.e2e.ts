import type { Page } from "@playwright/test";
import { expect, test } from "./fixture";
import { at, openRoll, sessionNotes, settled, twoChannels } from "./rollHelpers";

test.use({ seed: { project: twoChannels([]) } });

test("healthy geometry delivered after five seconds still permits a real edit", async ({ app, mock }) => {
  await openRoll(app);
  const evaluate = app.evaluate.bind(app);
  let delayed = false;
  const controlled = new Proxy(app, {
    get(target, property) {
      if (property !== "evaluate") return Reflect.get(target, property, target);
      return async (...args: Parameters<Page["evaluate"]>) => {
        const result = await evaluate(...args);
        if (!delayed && String(args[0]).includes("__grooviqRoll")) {
          delayed = true;
          await new Promise((resolve) => setTimeout(resolve, 5500));
        }
        return result;
      };
    },
  });
  await settled(controlled);
  expect(delayed).toBe(true);
  const p = await at(app, 970, 60);
  await app.mouse.click(p.x, p.y);
  await expect.poll(() => sessionNotes(mock)).toEqual([[1, 960, 240, 60]]);
});

for (const state of ["missing", "zero", "width", "height", "nonsettling"] as const) {
  test(`${state} view geometry blocks editing`, async ({ app, mock }) => {
    await openRoll(app);
    await app.evaluate((state) => {
      const roll = (window as unknown as { __grooviqRoll: { view: Record<string, number> } }).__grooviqRoll;
      let live = roll.view;
      let reads = 0;
      Object.defineProperty(roll, "view", {
        configurable: true,
        get: () => {
          reads++;
          if (state === "missing") return undefined;
          if (state === "zero") return { ...live, width: 0, height: 0 };
          return { ...live,
            width: live.width + (state === "width" || (state === "nonsettling" && reads % 2 === 0) ? 10 : 0),
            height: live.height + (state === "height" || (state === "nonsettling" && reads % 2 !== 0) ? 10 : 0),
          };
        },
        set: (value: Record<string, number>) => { live = value; },
      });
    }, state);
    await expect(settled(app, 350)).rejects.toThrow(/roll geometry must match the canvas before editing/);
    expect(await sessionNotes(mock)).toEqual([]);
    expect(await mock.calls("notes_edit")).toHaveLength(0);
  });
}
