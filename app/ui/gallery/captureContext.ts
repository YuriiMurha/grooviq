import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { lstatSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { homedir, platform, release } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gallerySeed } from "./seed";

const ui = fileURLToPath(new URL("..", import.meta.url));
const repo = resolve(ui, "../..");
const hash = (data: string | Buffer) => createHash("sha256").update(data).digest("hex");
const filePin = (path: string) => {
  const info = lstatSync(path);
  if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Not a regular gallery input: ${path}`);
  const data = readFileSync(path);
  return { bytes: data.length, sha256: hash(data) };
};

/** Read-only provenance, called only by the capture runner. Never inherit or log all env. */
export function captureContext() {
  const env = {
    PATH: process.env.PATH ?? "", HOME: homedir(), GIT_OPTIONAL_LOCKS: "0",
    DEVELOPER_DIR: process.env.DEVELOPER_DIR ?? "/Library/Developer/CommandLineTools",
    ...(process.env.SystemRoot ? { SystemRoot: process.env.SystemRoot } : {}),
  };
  const commands: { argv: string[]; cwd: string; startedUtc: string; finishedUtc: string;
    elapsedMs: number; status: number | null; signal: string | null; error: string | null;
    stdoutBytes: number; stdoutSha256: string; stderrBytes: number; stderrSha256: string;
    environmentKeys: string[]; originalWaitCompleted: boolean }[] = [];
  const git = (args: string[]) => {
    const startedUtc = new Date().toISOString();
    const started = process.hrtime.bigint();
    const result = spawnSync("git", args, { cwd: repo, env, encoding: "utf8", timeout: 10_000, maxBuffer: 8 * 1024 * 1024 });
    const stdout = result.stdout ?? "";
    const stderr = result.stderr ?? "";
    commands.push({ argv: ["git", ...args], cwd: repo, startedUtc, finishedUtc: new Date().toISOString(),
      elapsedMs: Number(process.hrtime.bigint() - started) / 1_000_000,
      status: result.status, signal: result.signal, error: result.error?.message ?? null,
      stdoutBytes: Buffer.byteLength(stdout), stdoutSha256: hash(stdout),
      stderrBytes: Buffer.byteLength(stderr), stderrSha256: hash(stderr),
      environmentKeys: Object.keys(env).sort(), originalWaitCompleted: result.status !== null });
    if (result.error || result.status !== 0 || result.signal !== null) {
      throw new Error(`Gallery metadata command failed: git ${args.join(" ")}: ${result.error?.message ?? stderr}`);
    }
    return stdout;
  };
  const head = git(["rev-parse", "HEAD"]).trim();
  const branch = git(["branch", "--show-current"]).trim();
  const paths = ["app/ui", "Makefile"];
  const names = git(["ls-files", "--cached", "--others", "--exclude-standard", "--", ...paths])
    .split("\n").filter(Boolean).sort();
  const sources = Object.fromEntries([...new Set(names)].map((name) => [name, filePin(resolve(repo, name))]));
  const dirty = git(["status", "--porcelain", "--", ...paths]);
  const patch = git(["diff", "--binary", "HEAD", "--", ...paths]);
  const fontRoot = resolve(ui, "node_modules/@fontsource-variable/manrope/files");
  const fonts = Object.fromEntries(readdirSync(fontRoot).filter((name) => name.endsWith(".woff2"))
    .sort().map((name) => [name, filePin(resolve(fontRoot, name))]));
  const seed = gallerySeed;
  const context = {
    head, branch, dirtyPaths: dirty.split("\n").filter(Boolean), dirtyPatchSha256: hash(patch),
    capturedSourceScope: paths, sourceFingerprint: hash(JSON.stringify(sources)), sources,
    fonts,
    seed: { ...seed, sha256: hash(JSON.stringify(seed)) },
    host: { platform: platform(), release: release() },
    readOnlyGitCommands: commands,
    originalSynchronousWaitsCompleted: commands.every((command) => command.originalWaitCompleted && command.status === 0 && command.signal === null && command.error === null),
    approvedReference: null,
    referenceStatus: "candidate; independent baseline approval required",
  };
  // Discovery evaluates this module too, before any test can attach a capture.
  // Preserve every evaluation separately; never backfill old missing receipts.
  const directory = resolve(ui, "gallery/.runtime/metadata");
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const metadataReceipt = resolve(directory, `${process.pid}-${process.hrtime.bigint()}.json`);
  writeFileSync(metadataReceipt, JSON.stringify({ schemaVersion: 1, pidEvidenceOnly: process.pid,
    recordedUtc: new Date().toISOString(), ...context }, null, 2) + "\n", { flag: "wx", mode: 0o600 });
  return { ...context, metadataReceipt };
}
