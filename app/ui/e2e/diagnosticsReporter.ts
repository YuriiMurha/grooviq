import { closeSync, constants, fsyncSync, lstatSync, mkdirSync, openSync, readSync, writeSync } from "node:fs";
import { resolve, parse, join } from "node:path";
import { randomUUID } from "node:crypto";
import { freemem, totalmem, loadavg } from "node:os";

type Case = { id: string; titlePath(): string[]; parent: { project(): { name: string } | undefined } };
type Result = { retry: number; workerIndex: number; parallelIndex: number; status?: string; duration?: number };
type Options = { directory?: string; maxBytes?: number; maxRecords?: number; sample?: () => unknown };

function readMetric(path: string) {
  let fd: number | undefined;
  try {
    fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    const buffer = Buffer.alloc(4097);
    const length = readSync(fd, buffer, 0, buffer.length, null);
    if (length > 4096) return { available: false };
    return { available: true, raw: buffer.toString("utf8", 0, length) };
  } catch { return { available: false }; }
  finally { if (fd !== undefined) closeSync(fd); }
}
export function resources() {
  return {
    process: { scope: "reporter-process-only", available: true, cpuMicroseconds: process.cpuUsage(), rssBytes: process.memoryUsage().rss },
    host: { scope: "os-host-view-not-container", available: true, totalBytes: totalmem(), freeBytes: freemem(), loadAverage: loadavg() },
    cgroup: { scope: "cgroup-v2-mounted-root-not-attributed-to-browser", cpu: readMetric("/sys/fs/cgroup/cpu.stat"), memory: readMetric("/sys/fs/cgroup/memory.current") },
  };
}

export default class DiagnosticsReporter {
  private options: Options;
  private fd: number | undefined;
  private timer: ReturnType<typeof setInterval> | undefined;
  private failed = false;
  private ended = false;
  private bytes = 0;
  private count = 0;
  constructor(options: Options = {}) { this.options = options; }
  get activeTimer() { return this.timer !== undefined; }
  printsToStdio() { return false; }
  private failure() {
    if (!this.failed) process.stderr.write("e2e diagnostics unavailable or capped; journal incomplete\n");
    this.failed = true;
    this.stop();
  }
  private stop() {
    if (this.timer) clearInterval(this.timer);
    this.timer = undefined;
    if (this.fd !== undefined) { const fd = this.fd; this.fd = undefined; try { closeSync(fd); } catch { this.failed = true; } }
  }
  private record(kind: string, fields: object = {}) {
    if (this.failed || this.ended) return;
    try {
      const line = Buffer.from(JSON.stringify({ kind, sequence: this.count, wallMs: Date.now(), monotonicMs: performance.now(), ...fields }) + "\n");
      if (line.length > 64 * 1024 || this.count >= (this.options.maxRecords ?? 20000) || this.bytes + line.length > (this.options.maxBytes ?? 8 * 1024 * 1024)) throw Error("cap");
      let offset = 0;
      while (offset < line.length) { const written = writeSync(this.fd!, line, offset); if (!written) throw Error("short write"); offset += written; }
      fsyncSync(this.fd!);
      this.bytes += line.length; this.count++;
    } catch { this.failure(); }
  }
  private sample() {
    let observation: unknown;
    try { observation = (this.options.sample ?? resources)(); }
    catch { observation = { scope: "sampler", available: false }; }
    this.record("resource", { observation });
  }
  onBegin(_config: unknown, suite: { allTests(): Case[] }) {
    try {
      // Separate from outputDir: Playwright removes project output trees during setup.
      const directory = resolve(this.options.directory ?? join("playwright-report", `diagnostics-${randomUUID()}`));
      let parent = parse(directory).root;
      const parts = directory.slice(parent.length).split(/[\\/]/);
      for (const part of parts.slice(0, -1)) {
        parent = join(parent, part);
        try { mkdirSync(parent); } catch (error) { if ((error as NodeJS.ErrnoException).code !== "EEXIST") throw error; }
        const stat = lstatSync(parent);
        if (stat.isSymbolicLink() || !stat.isDirectory()) throw Error("unsafe ancestor");
      }
      mkdirSync(directory, { mode: 0o700 }); // Exclusive ownership; never reuse or delete.
      this.fd = openSync(join(directory, "events.jsonl"), constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
      const cases = suite.allTests();
      this.record("selection", { selected: cases.length, schema: 1, runtime: { node: process.version, platform: process.platform, arch: process.arch } });
      for (const test of cases) this.record("inventory", this.identity(test));
      this.sample();
      if (!this.failed) { this.timer = setInterval(() => this.sample(), 5000); this.timer.unref(); }
    } catch { this.failure(); }
  }
  private identity(test: Case) { return { id: test.id, project: test.parent.project()?.name, title: test.titlePath() }; }
  onTestBegin(test: Case, result: Result) { this.record("begin", { ...this.identity(test), retry: result.retry, workerIndex: result.workerIndex, parallelIndex: result.parallelIndex }); }
  onTestEnd(test: Case, result: Result) { this.record("end", { ...this.identity(test), retry: result.retry, workerIndex: result.workerIndex, parallelIndex: result.parallelIndex, status: result.status, durationMs: result.duration }); }
  async onEnd(result: { status: string }): Promise<{ status: "failed" } | undefined> {
    if (!this.ended) this.record("terminal", { status: result.status });
    this.ended = true;
    this.stop();
    if (this.failed) return { status: "failed" };
  }
}
