// Legacy trim preservation before an explicit repair is chosen. No native repair API is
// simulated here: these cases exercise the incumbent individual-marker refusal contract.
import { expect, project, soundsFor, test } from "./fixture";

for (const [frames, start, end] of [[1, 0.2, 0.7], [2, 0.6, 0.4]]) {
  test.describe(`legacy ${frames}-frame trim`, () => {
    const p = project(["Tiny"]);
    p.channels[0].frames = frames;
    p.channels[0].sampler = { start: Math.fround(start), end: Math.fround(end), reverse: 1, loop: 1, loop_start: Math.fround(0.37), tune: -3 };
    test.use({ seed: { project: p, files: soundsFor(["Tiny"]) } });

    test("render_resize_keys_and_real_pointer_drags_preserve_the_exact_model", async ({ app, mock }, info) => {
      await app.getByRole("button", { name: "Edit the Tiny sampler" }).click();
      const editor = app.getByRole("dialog", { name: "Tiny sampler" });
      const wave = editor.getByRole("img", { name: "Tiny waveform" });
      await expect(wave).toBeVisible();
      const before = await mock.eval("m => m.view().channels[0].sampler");
      expect(before).toMatchObject(p.channels[0].sampler);
      for (const size of [{ width: 1280, height: 800 }, { width: 900, height: 560 }]) {
        await app.setViewportSize(size);
        for (const id of ["start", "end"]) {
          const marker = editor.locator(`[data-param="${id}"]`);
          await marker.focus();
          for (const key of ["Home", "End", "ArrowLeft", "ArrowRight"]) await app.keyboard.press(key);
          const box = (await marker.boundingBox())!;
          const well = (await wave.boundingBox())!;
          const x = box.x + box.width / 2;
          const y = box.y + box.height / 2;
          // Prove the pointer reaches this actual handle, rather than a nearby loop handle.
          expect(await app.evaluate(({ x, y, id }) => document.elementFromPoint(x, y)?.closest("[data-param]")?.getAttribute("data-param") === id, { x, y, id })).toBe(true);
          await app.mouse.move(x, y);
          await app.mouse.down();
          await app.mouse.move(well.x + well.width * 0.05, y, { steps: 4 });
          await app.mouse.move(well.x + well.width * 0.95, y, { steps: 4 });
          await app.mouse.up();
        }
        await app.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
        expect(await mock.eval("m => m.view().channels[0].sampler")).toEqual(before);
        expect(await mock.calls("project_edit")).toEqual([]);
        expect(await mock.calls("channel_param_live")).toEqual([]);
        await app.screenshot({ path: info.outputPath(`legacy-${frames}-${size.width}.png`) });
      }
    });
  });
}
