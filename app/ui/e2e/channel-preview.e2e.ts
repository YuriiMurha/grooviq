import { test, expect, project, type Mock } from "./fixture";

test.use({ seed: { project: project(["Bass"]) } });

const commands = async (mock: Mock) => (await mock.calls("channel_preview")).map((c) => c.args.on);
const held = (mock: Mock) => mock.eval<number>("m => m.state.presses.size");

for (const focused of [false, true]) {
  for (const terminal of ["inside", "outside", "cancel", "capture", "window", "unmount"] as const) {
    test(`${focused ? "focused" : "unfocused"} pointer hold ends on ${terminal}`, async ({ app, mock }) => {
      const name = app.getByRole("button", { name: /^Bass / });
      if (focused) await name.focus();
      else await app.getByRole("button", { name: "Stop", exact: true }).focus();
      await name.evaluate((el) => el.addEventListener("pointerdown", (event) => {
        if (event instanceof PointerEvent) el.setAttribute("data-pointer", String(event.pointerId));
      }));
      await name.hover();
      await app.mouse.down();
      await expect.poll(() => commands(mock)).toEqual([true]);
      // Allow native focus/blur and capture delivery to finish while the pointer stays down.
      await app.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
      expect(await commands(mock)).toEqual([true]);
      expect(await held(mock)).toBe(1);
      if (terminal === "outside") await app.mouse.move(900, 700);
      if (terminal === "inside" || terminal === "outside") await app.mouse.up();
      if (terminal === "cancel") await name.evaluate((el) => el.dispatchEvent(new PointerEvent("pointercancel", { bubbles: true, pointerId: Number(el.getAttribute("data-pointer")) })));
      if (terminal === "capture") {
        // Deliver gotpointercapture before explicitly releasing the established capture.
        await app.mouse.move(900, 700);
        await name.evaluate((el) => el.releasePointerCapture(Number(el.getAttribute("data-pointer"))));
        await app.mouse.move(901, 700);
      }
      if (terminal === "window") await app.evaluate(() => window.dispatchEvent(new Event("blur")));
      if (terminal === "unmount") await app.keyboard.press("F6");
      await expect.poll(() => commands(mock)).toEqual([true, false]);
      await expect.poll(() => held(mock)).toBe(0);
      await app.mouse.up();
      expect(await commands(mock)).toEqual([true, false]);
    });
  }
}

test("Enter repeats once and focus leaving ends its own press", async ({ app, mock }) => {
  const name = app.getByRole("button", { name: /^Bass / });
  await name.focus();
  await app.keyboard.down("Enter");
  await app.keyboard.down("Enter");
  await expect.poll(() => commands(mock)).toEqual([true]);
  expect(await held(mock)).toBe(1);
  await app.getByRole("button", { name: "Stop", exact: true }).focus();
  await expect.poll(() => commands(mock)).toEqual([true, false]);
  expect(await held(mock)).toBe(0);
  await app.keyboard.up("Enter");
  expect(await commands(mock)).toEqual([true, false]);
});

test("secondary pointers cannot replace or release the captured hold", async ({ app, mock }) => {
  const name = app.getByRole("button", { name: /^Bass / });
  await name.evaluate((el) => el.addEventListener("pointerdown", (event) => {
    if (event instanceof PointerEvent) el.setAttribute("data-pointer", String(event.pointerId));
  }));
  await name.hover();
  await app.mouse.down();
  await expect.poll(() => commands(mock)).toEqual([true]);
  await name.evaluate((el) => {
    const pointerId = Number(el.getAttribute("data-pointer")) + 10;
    const realCapture = el.setPointerCapture.bind(el);
    // Simulate the first touch while a mouse already owns capture. Both devices
    // have a primary pointer; this synthetic touch has no browser capture.
    el.setPointerCapture = (id) => {
      if (id !== pointerId) realCapture(id);
    };
    for (const type of ["pointerup", "pointercancel", "lostpointercapture", "pointerdown"]) {
      el.dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId, pointerType: "touch", button: 0, isPrimary: true }));
    }
  });
  expect(await commands(mock)).toEqual([true]);
  expect(await held(mock)).toBe(1);
  await app.mouse.up();
  await expect.poll(() => held(mock)).toBe(0);
});

test("capture failure starts no voice", async ({ app, mock }) => {
  const name = app.getByRole("button", { name: /^Bass / });
  await name.evaluate((el) => {
    el.setPointerCapture = () => { throw new DOMException("Capture unavailable"); };
  });
  await name.hover();
  await app.mouse.down();
  await app.mouse.move(900, 700);
  await app.mouse.up();
  expect(await commands(mock)).toEqual([]);
  expect(await held(mock)).toBe(0);
});

test("nonprimary and secondary-button presses start no preview", async ({ app, mock }) => {
  const name = app.getByRole("button", { name: /^Bass / });
  await name.dispatchEvent("pointerdown", { pointerId: 42, button: 0, isPrimary: false });
  await name.click({ button: "right" });
  expect(await commands(mock)).toEqual([]);
  expect(await held(mock)).toBe(0);
});
