import { test } from 'node:test';
import assert from 'node:assert/strict';
import { validate } from './e2e-browser-results.mjs';
const meta = { head: 'a'.repeat(40), workflow: 'b'.repeat(40), run: '123', attempt: '1' };
const report = (project) => ({ errors: [], suites: [{ title: 'file', specs: [{ id: project, title: 'case', file: 'case.ts', tests: [{ projectName: project, expectedStatus: 'passed', status: 'expected', annotations: [], results: [{ status: 'passed', retry: 0 }] }] }] }] });
const fixture = () => ['webkit','chromium'].map(project => ({ ...meta, project, outcome: 'success', inventory: { errors: [], suites: [report('webkit').suites[0], report('chromium').suites[0]] }, report: report(project) }));
test('both projects complete', () => assert.doesNotThrow(() => validate(fixture(), meta, 'success')));
for (const [name, mutate] of Object.entries({
 missing: x => x.pop(), empty: x => x[0].report.suites = [], duplicate: x => x[0].report.suites.push(x[0].report.suites[0]), extra: x => x[0].report.suites[0].specs.push({...x[0].report.suites[0].specs[0], id:'extra'}),
 filtered: x => x[0].inventory.suites.pop(), stale: x => x[0].attempt='2', source: x => x[0].head='c'.repeat(40),
 staleRun: x => x[0].run='124', staleWorkflow: x => x[0].workflow='d'.repeat(40),
 failed: x => x[0].outcome='failure', cancelled: x => x[0].outcome='cancelled', skipped: x => x[0].outcome='skipped', missingReport: x => delete x[0].report,
 reporterIO: x => x[0].outcome='failure', error: x => x[0].report.errors.push({message:'error'}),
 failure: x => x[0].report.suites[0].specs[0].tests[0].results[0].status='failed',
 retry: x => x[0].report.suites[0].specs[0].tests[0].results[0].retry=1,
 expectedFailure: x => x[0].report.suites[0].specs[0].tests[0].expectedStatus='failed',
 fixme: x => x[0].report.suites[0].specs[0].tests[0].annotations=[{type:'fixme'}],
 skip: x => x[0].report.suites[0].specs[0].tests[0].results[0].status='skipped',
 partial: x => x[0].report.suites[0].specs[0].tests[0].results=[],
})) test(`reject ${name}`, () => { const x=fixture(); mutate(x); assert.throws(() => validate(x,meta,'success')); });
for (const status of ['failure','cancelled','skipped','']) test(`dependency ${status}`, () => assert.throws(() => validate(fixture(),meta,status)));
import { realpathSync, mkdtempSync, writeFileSync, symlinkSync, rmSync, truncateSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readJSON } from './e2e-browser-results.mjs';
test('bounded owned file reads', () => {
 const dir=mkdtempSync(join(realpathSync(tmpdir()),'e2e-accounting-'));
 try {
  writeFileSync(join(dir,'good.json'),'{}'); assert.deepEqual(readJSON(dir,'good.json'),{});
  assert.throws(()=>readJSON(dir,'../outside.json'),/outside/);
  assert.throws(()=>readJSON(dir,'missing.json'));
  writeFileSync(join(dir,'empty.json'),''); assert.throws(()=>readJSON(dir,'empty.json'),/size/);
  writeFileSync(join(dir,'large.json'),''); truncateSync(join(dir,'large.json'),16*1024*1024+1); assert.throws(()=>readJSON(dir,'large.json'),/size/);
  symlinkSync(join(dir,'good.json'),join(dir,'link.json')); assert.throws(()=>readJSON(dir,'link.json'),/symlink/);
  symlinkSync(dir,join(dir,'parent')); assert.throws(()=>readJSON(join(dir,'parent'),'good.json'),/symlink/);
 } finally {rmSync(dir,{recursive:true});}
});
test('only exact WebKit CDP skip is accepted', () => {
 const x=fixture(); const spec=x[0].report.suites[0].specs[0];
 spec.file='m2-panels.e2e.ts'; spec.title='a_dpr_change_redraws_sharp';
 x.forEach(r=> {r.inventory.suites[0].specs[0].file=spec.file;r.inventory.suites[0].specs[0].title=spec.title;});
 const t=spec.tests[0];t.expectedStatus='skipped'; t.status='skipped'; t.results[0].status='skipped';t.annotations=[{type:'skip',description:'changes the ratio through the Chrome DevTools Protocol'}];
 assert.doesNotThrow(()=>validate(x,meta,'success'));
 t.status='expected';assert.throws(()=>validate(x,meta,'success'));t.status='skipped';
 t.annotations[0].description='other';assert.throws(()=>validate(x,meta,'success'));
});
test('one passing case cannot cover two discovered cases', () => {
 const x=fixture();
 for (const r of x) { const spec=structuredClone(r.inventory.suites[0].specs[0]);spec.title='second case';r.inventory.suites[0].specs.push(spec); }
 assert.throws(()=>validate(x,meta,'success'),/coverage|missing/);
});
