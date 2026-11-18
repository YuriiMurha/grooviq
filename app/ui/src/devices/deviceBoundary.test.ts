import { afterAll, expect, test } from "vitest";
import { ESLint } from "eslint";
import ts from "typescript";
import { mkdirSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
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
