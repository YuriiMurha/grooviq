import { afterAll, expect, test } from "vitest";
import { ESLint } from "eslint";
import ts from "typescript";
import { mkdirSync, mkdtempSync, writeFileSync, rmSync, symlinkSync, unlinkSync, rmdirSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

const root = process.cwd();
const temp = mkdtempSync(path.join(tmpdir(), "device-boundary-"));
afterAll(() => rmSync(temp, { recursive: true, force: true }));
const context = path.join(root, "src/project/context.ts");
const relay = path.join(temp, "relay.ts");
writeFileSync(relay, `export { useProject } from ${JSON.stringify(context)};`);
const eslint = new ESLint({ cwd: root });
for (const [name, code] of Object.entries({
  direct: 'import { useProject } from "../project/context";',
  extension: 'import { useProject } from "../project/context.ts";',
  javascript: 'import { useProject } from "../project/context.js";',
  relay: `import { useProject } from ${JSON.stringify(relay)};`,
  reexport: `export { useProject } from ${JSON.stringify(relay)};`,
  namespace: 'import * as project from "../project/context";',
  session: 'import { useSession } from "../project/useSession";',
  edit: 'import { projectEdit } from "../api/project";',
  factory: 'import { editorSink } from "./paramSink";',
  dynamic: 'void import("../project/context");',
  computed: 'const p = "../project/context"; void import(p);',
})) {
  test(`rejects ${name} raw project access`, async () => {
    const [result] = await eslint.lintText(code, { filePath: "src/devices/BoundaryProbe.ts" });
    expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
  });
}
test("allows type-only project imports and the trusted hook", async () => {
  const [result] = await eslint.lintText('import type { ChannelView, Edit } from "../api/project"; export type Probe = [ChannelView, Edit];', { filePath: "src/devices/BoundaryProbe.ts" });
  expect(result.errorCount).toBe(0);
  const results = await eslint.lintFiles(["src/devices/useDeviceParams.ts"]);
  expect(results.flatMap((r) => r.messages)).toEqual([]);
});

test("rejects symlink aliases of protected project modules", async () => {
  const link = path.join(temp, "project-link");
  symlinkSync(path.dirname(context), link, "junction");
  try {
    const source = path.join(link, "context.ts");
    const [result] = await eslint.lintText(`import { useProject } from ${JSON.stringify(source)};`, { filePath: "src/devices/BoundaryProbe.ts" });
    expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
  } finally {
    // Remove the link itself, never recurse into the real project directory.
    if (process.platform === "win32") rmdirSync(link);
    else unlinkSync(link);
  }
});

test("rejects local value declarations that hide a JavaScript relay", async () => {
  const js = path.join(temp, "declared-relay.js");
  writeFileSync(js, `export { useProject } from ${JSON.stringify(context)};`);
  writeFileSync(path.join(temp, "declared-relay.d.ts"), `export declare function useProject(): import(${JSON.stringify(context)}).ProjectContextValue;`);
  const [result] = await eslint.lintText(`import { useProject } from ${JSON.stringify(js)};`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
  const [types] = await eslint.lintText(`import type { useProject } from ${JSON.stringify(js)}; export type Probe = typeof useProject;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(types.errorCount).toBe(0);
});

test("follows default value imports alongside named type imports in relays", async () => {
  const inner = path.join(temp, "default-inner.ts");
  const outer = path.join(temp, "default-outer.ts");
  writeFileSync(inner, `import { useProject } from ${JSON.stringify(context)}; export default useProject; export type Tag = "t";`);
  writeFileSync(outer, `import rawUseProject, { type Tag } from ${JSON.stringify(inner)}; export const useRaw = rawUseProject; export type Label = Tag;`);
  const [result] = await eslint.lintText(`import { useRaw } from ${JSON.stringify(outer)}; void useRaw().apply;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
  writeFileSync(outer, `import { type Tag } from ${JSON.stringify(inner)}; export const label: Tag = "t";`);
  const [types] = await eslint.lintText(`import { label } from ${JSON.stringify(outer)}; void label;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(types.errorCount).toBe(0);
});

test("rejects computed imports inside relays", async () => {
  const computed = path.join(temp, "computed-relay.ts");
  writeFileSync(computed, `const p = ${JSON.stringify(context)}; export const load = async () => (await import(p)).useProject;`);
  const [result] = await eslint.lintText(`import { load } from ${JSON.stringify(computed)}; void load;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
});

test.each(["glob", '["glob"]'])("rejects import.meta.%s directly and inside relays", async (member) => {
  const access = member.startsWith("[") ? `import.meta${member}` : `import.meta.${member}`;
  const code = `export const modules = ${access}("../project/context.ts", { eager: true });`;
  const [direct] = await eslint.lintText(code, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(direct.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
  const glob = path.join(temp, `glob-${member.length}.ts`);
  writeFileSync(glob, code);
  const [indirect] = await eslint.lintText(`import { modules } from ${JSON.stringify(glob)}; void modules;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(indirect.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
});

test("allows data asset imports directly and through a relay", async () => {
  const assets = 'import icon from "./knob.svg"; import url from "./knob.svg?url"; import raw from "./knob.svg?raw"; import "./editor.css"; export const data = [icon, url, raw];';
  const [direct] = await eslint.lintText(assets, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(direct.errorCount).toBe(0);
  const assetRelay = path.join(temp, "asset-relay.ts");
  writeFileSync(assetRelay, assets);
  const [indirect] = await eslint.lintText(`import { data } from ${JSON.stringify(assetRelay)}; void data;`, { filePath: "src/devices/BoundaryProbe.ts" });
  expect(indirect.errorCount).toBe(0);
});

test("raw apply cannot be assigned or passed as a guarded send prop", () => {
  const file = path.join(temp, "capability.ts");
  writeFileSync(file, `import type { GuardedEdit } from ${JSON.stringify(path.join(root, "src/devices/paramSink.ts"))};
import type { ProjectContextValue } from ${JSON.stringify(context)};
declare const raw: ProjectContextValue['apply'];
const assigned: GuardedEdit = raw;
declare function editor(props: Pick<ReturnType<typeof import(${JSON.stringify(path.join(root, "src/devices/useDeviceParams.ts"))}).useDeviceParams>, 'send'>): void;
editor({ send: raw });
void assigned;`);
  const program = ts.createProgram([file], { strict: true, noEmit: true, skipLibCheck: true, jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler, allowImportingTsExtensions: true, target: ts.ScriptTarget.ES2022 });
  const diagnostics = ts.getPreEmitDiagnostics(program).filter((d) => d.file?.fileName === file);
  expect(diagnostics.map((d) => d.code)).toEqual([2322, 2322]);
});

test("resolves tsconfig aliases before enforcing the boundary", async () => {
  mkdirSync(path.join(temp, "src/project"), { recursive: true });
  mkdirSync(path.join(temp, "src/devices"), { recursive: true });
  writeFileSync(path.join(temp, "src/project/context.ts"), "export const useProject = () => {}; ");
  writeFileSync(path.join(temp, "tsconfig.json"), JSON.stringify({ compilerOptions: { moduleResolution: "bundler", paths: { "@project/*": ["./src/project/*"] } } }));
  const lint = new ESLint({ cwd: temp, overrideConfigFile: path.join(root, "eslint.config.js") });
  const [result] = await lint.lintText('import { useProject } from "@project/context.js";', { filePath: path.join(temp, "src/devices/Probe.ts") });
  expect(result.messages.some((m) => m.ruleId === "device-boundary/guarded-edits")).toBe(true);
});
