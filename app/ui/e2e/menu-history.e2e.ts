import { clickCell, expect, test } from "./fixture";
import { at, openRoll, sessionNotes, twoChannels } from "./rollHelpers";

test.use({ seed: { project: twoChannels([]) } });
// Inject the shell event, not a DOM keyboard event. This proves browser routing only;
// AppKit menu selection and native first responder still need an owner hand check.
test("menu history changes grid and roll notes once, preserving browser text history", async ({ app, mock }) => {
  const menu = (redo = false) => mock.eval(`m => m.emit("edit-history", ${redo})`);
  await clickCell(app, 0, 0);
  await clickCell(app, 0, 4);
  await menu();
  await expect.poll(() => sessionNotes(mock)).toEqual([[1, 0, 240, 60]]);
  await menu(true);
  await expect.poll(() => sessionNotes(mock)).toHaveLength(2);
  await openRoll(app);
  const point = await at(app, 1920, 64);
  await app.mouse.click(point.x, point.y);
  await expect.poll(() => sessionNotes(mock)).toHaveLength(3);
  await menu();
  await expect.poll(() => sessionNotes(mock)).toHaveLength(2);
  await menu(true);
  await expect.poll(() => sessionNotes(mock)).toHaveLength(3);

  for (const tag of ["input", "textarea", "div"]) {
    await app.evaluate((tag) => {
      const field = document.createElement(tag);
      field.id = "history-field";
      if (tag === "div") field.contentEditable = "true";
      field.style.cssText = "position:fixed;top:0;left:0;z-index:99999;background:white;color:black";
      document.body.append(field);
      field.focus();
    }, tag);
    const field = app.locator("#history-field");
    await app.keyboard.type("hello");
    const value = () => field.evaluate((el) => el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement ? el.value : el.textContent);
    await expect.poll(value).toBe("hello");
    // Model temporary DOM blur only; this does not synthesize AppKit first responder.
    await field.evaluate((el) => (el as HTMLElement).blur());
    await menu();
    await expect.poll(value).toBe("");
    await expect.poll(() => sessionNotes(mock)).toHaveLength(3);
    await menu(true);
    await expect.poll(value).toBe("hello");
    await expect.poll(() => sessionNotes(mock)).toHaveLength(3);
    await field.evaluate((el) => el.remove());
  }
});
