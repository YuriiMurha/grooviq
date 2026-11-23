import { test, expect, project, soundsFor, clickCell } from './fixture';
import type { Page } from '@playwright/test';
const kit = ['Kick'];
test.use({ seed: { project: project(kit, { 1: [0] }), files: soundsFor(kit) } });
const turns = (app: Page) => app.evaluate(() => new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r()))));

for (const saved of [false, true]) {
  test.describe(saved ? 'existing project' : 'untitled project', () => {
    const command = saved ? 'project_save' : 'project_save_as';
    test.beforeEach(async ({ app, mock }) => {
      if (!saved) return;
      await mock.eval("(m) => m.queueDialog('/qa/projects/existing.grooviq')");
      await app.keyboard.press('ControlOrMeta+s');
      await expect.poll(() => mock.view().then(v => v.path)).not.toBeNull();
      await turns(app);
    });

    test('Save cancels a held step before taking its snapshot', async ({ app, mock }) => {
      const box = (await app.getByLabel('Step grid').boundingBox())!;
      const before = (await mock.calls('project_edit')).length;
      const saves = (await mock.calls(command)).length;
      await app.mouse.move(box.x + 18, box.y + 18);
      await app.mouse.down();
      if (!saved) await mock.eval("(m) => m.queueDialog('/qa/projects/held-save.grooviq')");
      await app.keyboard.press('ControlOrMeta+s');
      await expect.poll(async () => (await mock.calls(command)).length).toBe(saves + 1);
      await app.mouse.up();
      await turns(app);
      expect((await mock.calls('project_edit')).length).toBe(before);
      await clickCell(app, 0, 3);
      await expect.poll(async () => (await mock.calls('project_edit')).length).toBe(before + 1);
    });

    test('Save freezes new step edits while settlement waits', async ({ app, mock }) => {
      const settled = (await mock.calls('pattern_edit')).length;
      const saves = (await mock.calls(command)).length;
      await mock.eval("(m) => m.holdCommand('pattern_edit')");
      if (!saved) await mock.eval("(m) => m.queueDialog('/qa/projects/frozen-save.grooviq')");
      await app.keyboard.press('ControlOrMeta+s');
      await expect.poll(async () => (await mock.calls('pattern_edit')).length).toBe(settled + 1);
      const before = (await mock.calls('project_edit')).length;
      const box = (await app.getByLabel('Step grid').boundingBox())!;
      await app.mouse.click(box.x + 18, box.y + 18);
      await turns(app);
      const during = (await mock.calls('project_edit')).length;
      await mock.eval("(m) => m.releaseCommand('pattern_edit')");
      await expect.poll(async () => (await mock.calls(command)).length).toBe(saves + 1);
      expect(during).toBe(before);
      await clickCell(app, 0, 3);
      await expect.poll(async () => (await mock.calls('project_edit')).length).toBe(before + 1);
    });

    test('Save As cancellation releases the gesture guard', async ({ app, mock }) => {
      const before = (await mock.calls('project_edit')).length;
      const dialogs = (await mock.calls('plugin:dialog|save')).length;
      await mock.eval('(m) => m.queueDialog(null)');
      await app.keyboard.press('ControlOrMeta+Shift+s');
      await expect.poll(async () => (await mock.calls('plugin:dialog|save')).length).toBe(dialogs + 1);
      await turns(app);
      await clickCell(app, 0, 3);
      await expect.poll(async () => (await mock.calls('project_edit')).length).toBe(before + 1);
    });

    test('Save failure releases the gesture guard', async ({ app, mock }) => {
      const before = (await mock.calls('project_edit')).length;
      await mock.eval(`(m) => m.refuseNext('${command}', 'temporary save refusal')`);
      if (!saved) await mock.eval("(m) => m.queueDialog('/qa/projects/failed-save.grooviq')");
      await app.keyboard.press('ControlOrMeta+s');
      await expect(app.getByRole('status', { name: 'Messages' })).toContainText('temporary save refusal');
      await turns(app);
      await clickCell(app, 0, 3);
      await expect.poll(async () => (await mock.calls('project_edit')).length).toBe(before + 1);
    });
  });
}
for (const roll of [false, true]) {
  test(`external recording refuses held double Undo with roll ${roll}`, async ({ app, mock }) => {
    const grid = app.getByLabel('Step grid');
    const box = (await grid.boundingBox())!;
    await app.mouse.click(box.x + 50, box.y + 18);
    await expect.poll(() => mock.eval('(m) => m.notes().length')).toBe(2);
    if (roll) { await app.keyboard.press('F7'); await expect(app.getByLabel('Piano roll notes')).toBeVisible(); }
    await mock.eval("(m) => m.holdCommand('arrangement_history')");
    await app.keyboard.press('ControlOrMeta+z');
    await expect.poll(async () => (await mock.calls('arrangement_history')).length).toBe(1);
    await app.keyboard.press('ControlOrMeta+z');
    await mock.eval('(m) => m.addNote({channel:1,tick:2400,length:240,key:70,velocity:0.8})');
    await mock.eval("(m) => m.releaseCommand('arrangement_history')");
    await expect(app.getByRole('status', {name:'Messages'})).toContainText('Stale arrangement revision');
    await expect(app.getByRole('status', {name:'Messages'})).toContainText('Pending edits were refused');
    // Failed settlement aborts the second intent before another native history call.
    expect(await mock.calls('arrangement_history')).toHaveLength(1);
    expect((await mock.calls('arrangement_history'))[0].args.undo).toBe(true);
    expect(await mock.eval('(m) => m.notes().length')).toBe(3);
    // Let the canonical view/fetch recover before a new deliberate action.
    await app.evaluate(() => new Promise<void>(r => requestAnimationFrame(() => requestAnimationFrame(() => r()))));
    await app.keyboard.press('ControlOrMeta+z');
    await expect.poll(() => mock.eval('(m) => m.notes().length')).toBe(2);
  });
}
