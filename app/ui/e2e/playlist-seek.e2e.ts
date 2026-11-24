import { test, expect, project } from "./fixture";

test.use({ seed: { project: project([], {}, { playMode: "song", arrangement: {
  tracks: [{ id: 1, name: "Track", color: 0, mute: false }], audioAssets: [], loopRegion: null,
  clips: [{ id: 2, track: 1, name: "Pattern", startTick: 0, muted: false, content: { type: "pattern", pattern: 0, offsetTick: 0, lengthTicks: 15360 } }],
}, nextId: 3 }) } });

test("click seeks without edits; four pixels commits a loop even after returning", async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist loop ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  const box = await ruler.boundingBox(); if (!box) throw new Error("No ruler");
  const before = await mock.view();
  const edits = (await mock.calls("arrangement_edit")).length;
  const reads = (await mock.calls("project_view")).length;
  await app.mouse.move(box.x + 48, box.y + 10); await app.mouse.down();
  await app.mouse.move(box.x + 51, box.y + 10); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_seek")).length).toBe(1);
  expect((await mock.calls("arrangement_seek"))[0].args.tick).toBe(960);
  expect(await mock.calls("arrangement_edit")).toHaveLength(edits);
  expect(await mock.calls("project_view")).toHaveLength(reads);
  expect(await mock.view()).toMatchObject({ dirty: before.dirty, canUndo: before.canUndo, canRedo: before.canRedo });
  await app.mouse.move(box.x + 48, box.y + 10); await app.mouse.down();
  await app.mouse.move(box.x + 52, box.y + 10); await app.mouse.move(box.x + 48, box.y + 10); await app.mouse.up();
  await expect.poll(async () => (await mock.calls("arrangement_edit")).length).toBe(edits + 1);
  expect(await mock.calls("arrangement_seek")).toHaveLength(1);
});

test("keyboard uses live ticks; native failures do not dirty or create history", async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("Space");
  await mock.eval("m => m.hold(1700)");
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist loop ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  await expect.poll(() => mock.eval("m => m.ticks()")).toBe(1700);
  await ruler.focus(); await app.keyboard.press("ArrowRight");
  await expect.poll(async () => (await mock.calls("arrangement_seek"))[0]?.args.tick).toBe(2660);
  await app.keyboard.press("Home");
  await expect.poll(async () => (await mock.calls("arrangement_seek"))[1]?.args.tick).toBe(0);
  const before = await mock.view();
  await mock.eval("m => m.refuseWorkspaceNext('seek admission refused')");
  await app.keyboard.press("ArrowRight");
  await expect(app.getByRole("region", { name: "Playlist", exact: true }).getByRole("alert")).toContainText("seek admission refused");
  await mock.eval("m => { m.state.project.playMode = 'pattern'; }");
  await app.keyboard.press("Home");
  await expect(app.getByRole("region", { name: "Playlist", exact: true }).getByRole("alert")).toContainText("Song mode");
  expect(await mock.view()).toMatchObject({ dirty: before.dirty, canUndo: before.canUndo, canRedo: before.canRedo });
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
});

for (const reason of ["capture loss", "replacement"] as const) test(`held click cancels on ${reason}`, async ({ app, mock }) => {
  await expect(app.getByRole("complementary", { name: "Patterns" })).toHaveAttribute("aria-busy", "false");
  await app.keyboard.press("F5");
  const ruler = app.getByRole("group", { name: "Playlist loop ruler" });
  await expect(ruler).toHaveAttribute("aria-disabled", "false");
  await ruler.evaluate(element => element.addEventListener("gotpointercapture", event => element.setAttribute("data-pointer", String((event as PointerEvent).pointerId)), { once: true }));
  const box = await ruler.boundingBox(); if (!box) throw new Error("No ruler");
  await app.mouse.move(box.x + 48, box.y + 10); await app.mouse.down();
  await app.mouse.move(box.x + 49, box.y + 10);
  if (reason === "capture loss") await ruler.evaluate(element => element.releasePointerCapture(Number(element.getAttribute("data-pointer"))));
  else await app.keyboard.press("ControlOrMeta+n");
  await app.mouse.up();
  expect(await mock.calls("arrangement_seek")).toHaveLength(0);
  expect(await mock.calls("arrangement_edit")).toHaveLength(0);
});
