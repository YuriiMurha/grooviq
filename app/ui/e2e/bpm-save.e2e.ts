import { test, expect, project, soundsFor } from './fixture';
test.use({ seed: { project: project(['Kick'], {}), files: soundsFor(['Kick']) } });

test('a refused tempo can be retried with the same value', async ({ app, mock }) => {
  const bpm = app.getByRole('textbox', { name: 'BPM' });
  await mock.eval("m => m.refuseNext('project_edit', 'tempo refused')");
  await bpm.fill('137'); await bpm.press('Enter');
  await expect(app.getByRole('status', { name: 'Messages' })).toContainText('tempo refused');
  await bpm.fill('137'); await bpm.press('Enter');
  await expect.poll(() => mock.view().then(v => v.bpm)).toBe(137);
});
for (const saveAs of [false, true]) {
  test(`focused draft waits for acceptance before save, saveAs=${saveAs}`, async ({ app, mock }) => {
    await mock.eval("m => m.queueDialog('/qa/bpm.grooviq')");
    await app.keyboard.press('ControlOrMeta+s');
    await expect.poll(() => mock.view().then(v => v.path)).toBe('/qa/bpm.grooviq');
    const bpm = app.getByRole('textbox', { name: 'BPM' });
    await bpm.fill('143.25');
    await mock.eval("m => m.holdCommand('project_edit')");
    const command = saveAs ? 'project_save_as' : 'project_save';
    const before = (await mock.calls(command)).length;
    if (saveAs) await mock.eval("m => m.queueDialog('/qa/bpm.grooviq')");
    await app.keyboard.press(saveAs ? 'ControlOrMeta+Shift+s' : 'ControlOrMeta+s');
    await expect.poll(async () => (await mock.calls('project_edit')).length).toBeGreaterThan(0);
    expect((await mock.calls(command)).length).toBe(before);
    await mock.eval("m => m.releaseCommand('project_edit')");
    await expect.poll(async () => (await mock.calls(command)).length).toBe(before + 1);
    expect(await mock.eval("m => m.file('/qa/bpm.grooviq').project.bpm")).toBe(143.25);
  });
}
test('refused draft prevents save and a deliberate retry saves accepted tempo', async ({ app, mock }) => {
  await mock.eval("m => m.queueDialog('/qa/refused.grooviq')");
  await app.keyboard.press('ControlOrMeta+s');
  await expect.poll(() => mock.view().then(v => v.path)).toBe('/qa/refused.grooviq');
  const before = (await mock.calls('project_save')).length;
  await app.getByRole('textbox', { name: 'BPM' }).fill('151');
  await mock.eval("m => m.refuseNext('project_edit', 'tempo refused')");
  await app.keyboard.press('ControlOrMeta+s');
  await expect(app.getByRole('status', { name: 'Messages' })).toContainText('tempo refused');
  expect((await mock.calls('project_save')).length).toBe(before);
  expect(await mock.eval("m => m.file('/qa/refused.grooviq').project.bpm")).not.toBe(151);
  await app.getByRole('textbox', { name: 'BPM' }).fill('151');
  await app.keyboard.press('ControlOrMeta+s');
  await expect.poll(async () => (await mock.calls('project_save')).length).toBe(before + 1);
  expect(await mock.eval("m => m.file('/qa/refused.grooviq').project.bpm")).toBe(151);
});
test('invalid draft is discarded and close-prompt save uses a valid draft', async ({ app, mock }) => {
  await app.getByRole('textbox', { name: 'BPM' }).fill('NaN');
  await mock.eval("m => m.queueDialog('/qa/close.grooviq')");
  await app.keyboard.press('ControlOrMeta+s');
  await expect.poll(() => mock.view().then(v => v.path)).toBe('/qa/close.grooviq');
  expect(Number.isFinite(await mock.eval<number>("m => m.file('/qa/close.grooviq').project.bpm"))).toBe(true);
  await app.getByRole('textbox', { name: 'BPM' }).fill('130');
  await app.getByRole('textbox', { name: 'BPM' }).press('Enter');
  await expect.poll(() => mock.view().then(v => v.bpm)).toBe(130);
  await app.getByRole('textbox', { name: 'BPM' }).fill('155');
  await mock.eval('m => m.requestClose()');
  await app.getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(async () => mock.eval("m => m.file('/qa/close.grooviq').project.bpm")).toBe(155);
});
test('Escape cancels a draft, unchanged text avoids an edit, and numeric bounds remain enforced', async ({ app, mock }) => {
  const bpm = app.getByRole('textbox', { name: 'BPM' });
  const before = (await mock.calls('project_edit')).length;
  const initial = (await mock.view()).bpm;
  await bpm.fill('175'); await bpm.press('Escape');
  await bpm.fill(String(initial)); await bpm.press('Enter');
  expect((await mock.calls('project_edit')).length).toBe(before);
  for (const [text, expected] of [['999', 400], ['1', 20], ['123,456', 123.46]] as const) {
    await bpm.fill(text); await bpm.press('Enter');
    await expect.poll(() => mock.view().then(v => v.bpm)).toBe(expected);
  }
  await bpm.focus(); await bpm.press('Shift+ArrowUp');
  await expect.poll(() => mock.view().then(v => v.bpm)).toBe(123.56);
});
