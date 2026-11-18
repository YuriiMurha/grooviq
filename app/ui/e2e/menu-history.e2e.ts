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

for (const tag of ["input", "textarea", "div"]) {
  test(`text focus survives an unrelated live render: ${tag}`, async ({ app, mock }) => {
    await clickCell(app, 0, 0); await clickCell(app, 0, 4);
    await mock.eval("m => m.pauseLive(true)");
    await app.evaluate((tag) => {
      const field=document.createElement(tag); field.id="render-history-field";
      if(tag==="div") field.contentEditable="true";
      field.style.cssText="position:fixed;top:0;left:0;z-index:99999";
      document.body.append(field); field.focus();
    },tag);
    const field=app.locator("#render-history-field");
    const value=()=>field.evaluate(el=>el instanceof HTMLInputElement||el instanceof HTMLTextAreaElement?el.value:el.textContent);
    await app.keyboard.type("hello");
    for(const redo of [false,true]) {
      await field.evaluate(el=>(el as HTMLElement).blur());
      await mock.eval(`m => m.injectLive(${!redo})`);
      await expect(app.getByRole("button",{name:"Play",exact:true})).toHaveAttribute("aria-pressed",String(!redo));
      await app.evaluate(()=>new Promise<void>(resolve=>requestAnimationFrame(()=>requestAnimationFrame(()=>resolve()))));
      await mock.eval(`m => m.emit("edit-history",${redo})`);
      await expect.poll(value).toBe(redo?"hello":"");
      await expect.poll(()=>sessionNotes(mock)).toHaveLength(2);
    }
  });
}
