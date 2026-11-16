// A stand-in for the Tauri shell and the engine, injected into the web build before it loads
// (`page.addInitScript(installMockTauri, seed)`), so Playwright drives the real UI against an
// in-memory session that follows the shell's command contract (app/src-tauri/src/commands.rs,
// project_cmds.rs, project/src/session.rs). Playwright serialises the function, so it must not
// reference anything outside its own body.

/** A file in the mock's file system. */
export type MockFile =
  | { kind: "sample"; seconds?: number }
  | { kind: "folder" }
  | { kind: "text" }
  | { kind: "project"; project: MockProject };

/** A saved project, as the mock keeps it. */
export interface MockProject {
  name: string;
  bpm: number;
  timeSignature: { beats: number; unit: number };
  channels: MockChannel[];
  pattern: { id: number; name: string; steps: number; swing: number; notes: MockNote[] };
  nextId: number;
}

export interface MockChannel {
  id: number;
  name: string;
  color: number;
  /** The sample's path; `null` for a channel without one. */
  sample: string | null;
  /** The project holds the sample's bytes (every saved sample is embedded). `false`: they are
   * missing from the project file, so the channel shows "Missing" until relinked. */
  embedded?: boolean;
  volume: number;
  pan: number;
  mute: boolean;
  solo: boolean;
  sampler: Record<string, number>;
}

export interface MockNote {
  channel: number;
  tick: number;
  velocity: number;
}

export interface MockSeed {
  /** Paths the mock's file system holds. */
  files?: Record<string, MockFile>;
  /** The project the engine holds at start; untitled and empty if absent. */
  project?: MockProject;
  /** An autosave an earlier session left behind. */
  recovery?: { savedAt: number; original: string | null; project: MockProject } | null;
  /** How long an export takes, in ms. */
  exportMs?: number;
  /** The sampler's parameters (tests/fixtures/device-params.json); the fixture fills it in. */
  samplerParams?: { id: string; min: number; max: number; default: number }[];
}

export function installMockTauri(seed: MockSeed): void {
  const STEP_TICKS = 240;
  const PPQ = 960;
  const MAX_STEPS = 256;
  const MAX_CHANNELS = 256;
  const INKS = 6;
  const DEFAULT_VELOCITY = 0.8;
  if (!seed.samplerParams?.length) throw new Error("the mock needs the sampler's parameters");
  const PARAMS: [string, number, number, number][] = seed.samplerParams.map((p) => [p.id, p.min, p.max, p.default]);
  const SILENT_OUTPUT = {
    id: "null",
    name: "Silent (QA mode)",
    kind: "headphones",
    isDefault: true,
    current: true,
    sampleRate: 48000,
  };

  type Project = MockProject;
  const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x)) as T;
  const clamp = (x: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, x));
  const fileName = (path: string) => path.split("/").filter(Boolean).pop() ?? path;
  const stem = (path: string) => fileName(path).replace(/\.[^.]+$/, "");
  const extension = (path: string) => {
    const name = fileName(path);
    const dot = name.lastIndexOf(".");
    return dot > 0 ? name.slice(dot + 1) : "";
  };
  /** `.ext` added unless the path ends in it: exactly (a project, session.rs `begin_save`) or in
   * any case (an export, project_cmds.rs `with_extension`). */
  const withExtension = (path: string, ext: string, anyCase: boolean) => {
    const has = anyCase ? extension(path).toLowerCase() === ext : extension(path) === ext;
    return has ? path : `${path}.${ext}`;
  };
  const emptyProject = (): Project => ({
    name: "Untitled",
    bpm: 140,
    timeSignature: { beats: 4, unit: 4 },
    channels: [],
    pattern: { id: 0, name: "Pattern 1", steps: 16, swing: 0, notes: [] },
    nextId: 1,
  });

  // --- The mock's state, readable from the tests as `window.__grooviqMock`. ---
  const files = new Map<string, MockFile>(Object.entries(seed.files ?? {}));
  const calls: { cmd: string; args: unknown; at: number }[] = [];
  const s = {
    project: seed.project ? clone(seed.project) : emptyProject(),
    path: null as string | null,
    revision: 0,
    savedRevision: 0,
    autosavedAt: null as number | null,
    recovery: seed.recovery ? clone(seed.recovery) : null,
    /** Backups of overwritten project files, by project path. */
    backups: new Map<string, number>(),
    exitCount: 0,
    export: null as null | { cancel: boolean },
    /** `held`: a position the test pinned the clock at (`hold`), or null to run in real time. */
    transport: { playing: false, startMs: 0, startTicks: 0, held: null as number | null },
    tone: false,
    previews: new Map<number, boolean>(),
    dialogs: [] as unknown[],
    dialogCalls: [] as { kind: string; options: unknown }[],
  };

  // --- Events (the event plugin, as @tauri-apps/api/mocks implements it). ---
  const callbacks = new Map<number, (data: unknown) => void>();
  let nextCallback = 1;
  const listeners = new Map<string, number[]>();
  const transformCallback = (cb: (data: unknown) => void, once = false) => {
    const id = nextCallback++;
    callbacks.set(id, (data) => {
      if (once) callbacks.delete(id);
      cb(data);
    });
    return id;
  };
  // Like the shell: `id` is the listener's id (what `listen` returned), which `once` uses to
  // unlisten.
  const emit = (event: string, payload: unknown) => {
    for (const handler of [...(listeners.get(event) ?? [])]) {
      callbacks.get(handler)?.({ event, id: handler, payload });
    }
  };

  // --- The session: the same rules as project/src/session.rs. ---
  const dirty = () => s.revision !== s.savedRevision;
  const changed = () => {
    s.revision += 1;
  };
  const markClean = () => {
    s.revision += 1;
    s.savedRevision = s.revision;
    s.autosavedAt = null;
  };
  const samplerValues = (c: MockChannel) =>
    Object.fromEntries(PARAMS.map(([id, , , def]) => [id, c.sampler[id] ?? def]));
  // session.rs: missing = a sample reference without decoded bytes. The path is never opened.
  const missing = (c: MockChannel) => c.sample !== null && c.embedded === false;
  const view = () => {
    const p = s.project;
    const steps = p.pattern.steps;
    const cells: Record<string, (number | null)[]> = {};
    for (const c of p.channels) cells[String(c.id)] = Array(steps).fill(null);
    for (const n of p.pattern.notes) {
      const step = n.tick / STEP_TICKS;
      const row = cells[String(n.channel)];
      if (n.tick % STEP_TICKS === 0 && step < steps && row) row[step] = n.velocity;
    }
    return {
      name: s.path ? stem(s.path) : p.name,
      path: s.path,
      dirty: dirty(),
      bpm: p.bpm,
      timeSignature: { ...p.timeSignature },
      channels: p.channels.map((c) => ({
        id: c.id,
        name: c.name,
        color: c.color,
        sampleName: c.sample ? fileName(c.sample) : null,
        missing: missing(c),
        volume: c.volume,
        pan: c.pan,
        mute: c.mute,
        solo: c.solo,
        sampler: samplerValues(c),
      })),
      pattern: { id: p.pattern.id, name: p.pattern.name, steps, swing: p.pattern.swing, cells },
      autosavedAt: s.autosavedAt,
    };
  };
  const finite = (x: unknown, what: string): number => {
    if (typeof x !== "number" || !Number.isFinite(x)) throw `The ${what} must be a number.`;
    return x;
  };
  const channelIndex = (id: unknown) => {
    const i = s.project.channels.findIndex((c) => c.id === id);
    if (i < 0) throw `There is no channel ${String(id)} in this project.`;
    return i;
  };
  const stepTick = (channel: unknown, step: unknown) => {
    channelIndex(channel);
    const steps = s.project.pattern.steps;
    const n = Number(step);
    if (!(n >= 0 && n < steps)) throw `Step ${n + 1} is past the end of the pattern (${steps} steps).`;
    return n * STEP_TICKS;
  };
  /** Changes the pattern; `true` if anything changed. */
  const editPattern = (change: (p: Project["pattern"]) => void) => {
    const before = JSON.stringify(s.project.pattern);
    const next = clone(s.project.pattern);
    change(next);
    if (JSON.stringify(next) === before) return false;
    s.project.pattern = next;
    return true;
  };
  type Edit = Record<string, unknown> & { kind: string };
  const applyEdit = (e: Edit): boolean => {
    const p = s.project;
    switch (e.kind) {
      case "removeChannel": {
        const i = channelIndex(e.channel);
        p.pattern.notes = p.pattern.notes.filter((n) => n.channel !== e.channel);
        p.channels.splice(i, 1);
        return true;
      }
      case "setChannelMix": {
        const c = p.channels[channelIndex(e.channel)];
        const volume = clamp(finite(e.volume, "volume"), 0, 2);
        const pan = clamp(finite(e.pan, "pan"), -1, 1);
        const mute = Boolean(e.mute);
        const solo = Boolean(e.solo);
        if (c.volume === volume && c.pan === pan && c.mute === mute && c.solo === solo) return false;
        Object.assign(c, { volume, pan, mute, solo });
        return true;
      }
      case "setSamplerParam": {
        const c = p.channels[channelIndex(e.channel)];
        const spec = PARAMS.find(([id]) => id === e.param);
        if (!spec) throw `The sampler has no setting called "${String(e.param)}".`;
        const value = clamp(finite(e.value, "sampler value"), spec[1], spec[2]);
        if (c.sampler[spec[0]] === value) return false;
        c.sampler[spec[0]] = value;
        return true;
      }
      case "toggleStep": {
        const tick = stepTick(e.channel, e.step);
        return editPattern((pat) => {
          const before = pat.notes.length;
          pat.notes = pat.notes.filter((n) => !(n.channel === e.channel && n.tick === tick));
          if (pat.notes.length === before) {
            pat.notes.push({ channel: e.channel as number, tick, velocity: DEFAULT_VELOCITY });
          }
        });
      }
      case "setStepVelocity": {
        const tick = stepTick(e.channel, e.step);
        const velocity = clamp(finite(e.velocity, "velocity"), 0, 1);
        return editPattern((pat) => {
          let found = false;
          for (const n of pat.notes) {
            if (n.channel === e.channel && n.tick === tick) {
              n.velocity = velocity;
              found = true;
            }
          }
          if (!found) pat.notes.push({ channel: e.channel as number, tick, velocity });
        });
      }
      case "clearStep": {
        const tick = stepTick(e.channel, e.step);
        return editPattern((pat) => {
          pat.notes = pat.notes.filter((n) => !(n.channel === e.channel && n.tick === tick));
        });
      }
      case "setPatternLength": {
        const steps = clamp(Math.trunc(Number(e.steps)), 1, MAX_STEPS);
        const before = p.pattern.steps;
        const done = editPattern((pat) => {
          pat.steps = steps;
        });
        if (done && s.transport.playing) keepPlaceInLoop(before, steps);
        return done;
      }
      case "setSwing": {
        const swing = clamp(finite(e.swing, "swing"), 0, 1);
        return editPattern((pat) => {
          pat.swing = swing;
        });
      }
      case "setTempo": {
        const bpm = clamp(finite(e.bpm, "tempo"), 20, 400);
        if (p.bpm === bpm) return false;
        reanchor();
        p.bpm = bpm;
        return true;
      }
      case "setTimeSignature": {
        const beats = Number(e.beats);
        const unit = Number(e.unit);
        if (!(beats >= 1 && beats <= 16) || ![2, 4, 8, 16].includes(unit)) {
          throw `${beats}/${unit} is not a time signature Grooviq supports (1 to 16 beats of 2, 4, 8 or 16).`;
        }
        if (p.timeSignature.beats === beats && p.timeSignature.unit === unit) return false;
        p.timeSignature = { beats, unit };
        return true;
      }
      default:
        throw `unknown edit ${e.kind}`;
    }
  };

  /** Reads a sample file the way `read_and_decode` does, or throws its sentence. */
  const readSample = (path: string) => {
    const name = fileName(path);
    const f = files.get(path);
    if (!f) throw `"${name}" could not be read: No such file or directory (os error 2).`;
    if (f.kind === "folder") throw `"${name}" could not be read: not a regular file.`;
    // The decoder reads the content; the extension is only a hint (sample_file.rs).
    if (f.kind !== "sample") {
      throw `"${name}": This file is not a sound Grooviq can open; use WAV, AIFF, FLAC, MP3 or OGG.`;
    }
  };
  const saveTo = (target: string | null) => {
    const path = target === null ? s.path : withExtension(target, "grooviq", false);
    if (path === null) throw "Choose where to save this project.";
    if (files.get(path)?.kind === "project") s.backups.set(path, (s.backups.get(path) ?? 0) + 1);
    files.set(path, { kind: "project", project: clone(s.project) });
    s.path = path;
    markClean();
    return view();
  };
  const openProject = (p: Project, path: string | null, unsaved: boolean) => {
    stopTransport();
    s.project = clone(p);
    s.path = path;
    markClean();
    if (unsaved) changed();
    const warnings = p.channels
      .filter((c) => missing(c))
      .map((c) => `Channel "${c.name}": its sample is missing from the project file. Relink it to hear it.`);
    return { view: view(), warnings };
  };

  // --- The transport clock: ticks since play, as the engine reports them. ---
  const ticksNow = () => {
    if (!s.transport.playing) return 0;
    if (s.transport.held !== null) return s.transport.held;
    const seconds = (performance.now() - s.transport.startMs) / 1000;
    return Math.floor(s.transport.startTicks + (seconds * s.project.bpm * PPQ) / 60);
  };
  const reanchor = () => {
    if (!s.transport.playing) return;
    s.transport.startTicks = ticksNow();
    s.transport.startMs = performance.now();
  };
  /** A new pattern length moves the position to the same place in the new loop
   * (engine/src/transport.rs). */
  const keepPlaceInLoop = (oldSteps: number, newSteps: number) => {
    const now = ticksNow();
    const oldLen = oldSteps * STEP_TICKS;
    const newLen = newSteps * STEP_TICKS;
    const inLoop = now % oldLen;
    const loopStart = now - inLoop;
    let target = loopStart + (inLoop % newLen);
    while (target < now) target += newLen;
    s.transport.startTicks = target;
    s.transport.startMs = performance.now();
  };
  const stopTransport = () => {
    s.transport = { playing: false, startMs: 0, startTicks: 0, held: null };
  };

  // --- The live stream: a fake WebSocket that sends v2 frames at 60 Hz. ---
  const LIVE_PORT = 47999;
  const encodeLive = () => {
    const buf = new ArrayBuffer(40);
    const v = new DataView(buf);
    v.setUint8(0, 2);
    const sounding = s.tone || (s.transport.playing && s.project.channels.length > 0);
    v.setUint8(1, (s.transport.playing ? 1 : 0) | (s.tone ? 2 : 0));
    const level = sounding ? 0.5 : 0;
    v.setFloat32(4, level, true);
    v.setFloat32(8, level, true);
    v.setBigUint64(12, BigInt(Math.floor(performance.now() * 48)), true);
    v.setUint32(28, 0, true);
    v.setBigUint64(32, BigInt(ticksNow()), true);
    return buf;
  };
  const RealWebSocket = window.WebSocket;
  class LiveSocket {
    binaryType = "arraybuffer";
    readyState = 0;
    onopen: ((e: Event) => void) | null = null;
    onmessage: ((e: MessageEvent) => void) | null = null;
    onclose: ((e: CloseEvent) => void) | null = null;
    onerror: ((e: Event) => void) | null = null;
    private timer: ReturnType<typeof setInterval> | null = null;
    constructor() {
      setTimeout(() => {
        if (this.readyState !== 0) return;
        this.readyState = 1;
        this.onopen?.(new Event("open"));
        this.timer = setInterval(() => {
          this.onmessage?.(new MessageEvent("message", { data: encodeLive() }));
        }, 16);
      }, 0);
    }
    send() {}
    close() {
      if (this.timer) clearInterval(this.timer);
      this.readyState = 3;
    }
    addEventListener() {}
    removeEventListener() {}
  }
  const MockWebSocket = function (this: unknown, url: string | URL, protocols?: string | string[]) {
    if (String(url).startsWith(`ws://127.0.0.1:${LIVE_PORT}/`)) return new LiveSocket();
    return new RealWebSocket(url, protocols);
  } as unknown as typeof WebSocket;
  window.WebSocket = MockWebSocket;

  // --- Commands. ---
  const LOOPS_MAX = 999;
  const commands: Record<string, (a: Record<string, unknown>) => unknown> = {
    // commands.rs
    audio_outputs: () => [SILENT_OUTPUT],
    audio_select: (a) => {
      if (a.id !== "default" && a.id !== SILENT_OUTPUT.id) throw `No output with the id "${String(a.id)}"`;
      return null;
    },
    audio_settings: () => ({ output: "default", guard: true }),
    audio_set_guard: () => null,
    test_tone: (a) => {
      s.tone = Boolean(a.on);
      return null;
    },
    synth_note: () => null,
    live_endpoint: () => ({ port: LIVE_PORT, token: "e2e" }),
    audio_log: () => [
      { atMs: Date.now(), level: "warn", message: "Silent QA mode (mock engine): no audio device" },
    ],
    // project_cmds.rs
    project_view: () => view(),
    project_edit: (a) => {
      if (applyEdit(a.edit as Edit)) changed();
      return view();
    },
    channel_add: (a) => {
      const path = String(a.path);
      readSample(path);
      const p = s.project;
      if (p.channels.length >= MAX_CHANNELS) {
        throw `A project holds at most ${MAX_CHANNELS} channels; remove one to add another.`;
      }
      const last = p.channels[p.channels.length - 1];
      p.channels.push({
        id: p.nextId,
        name: stem(path) || "Sample",
        color: last ? (last.color + 1) % INKS : 0,
        sample: path,
        embedded: true,
        volume: 1,
        pan: 0,
        mute: false,
        solo: false,
        sampler: {},
      });
      p.nextId += 1;
      changed();
      return view();
    },
    channel_set_sample: (a) => {
      const path = String(a.path);
      readSample(path);
      const c = s.project.channels[channelIndex(a.channel)];
      c.sample = path;
      c.embedded = true;
      changed();
      return view();
    },
    channel_preview: (a) => {
      channelIndex(a.channel);
      s.previews.set(a.channel as number, Boolean(a.on));
      return null;
    },
    transport_play: () => {
      s.transport = { playing: true, startMs: performance.now(), startTicks: 0, held: null };
      return null;
    },
    transport_stop: () => {
      stopTransport();
      return null;
    },
    project_new: () => openProject(emptyProject(), null, false).view,
    project_open: (a) => {
      const f = files.get(String(a.path));
      if (!f) throw "No such file or directory (os error 2)";
      if (f.kind !== "project") throw "This is not a Grooviq project file.";
      return openProject(f.project, String(a.path), false);
    },
    project_save: () => saveTo(null),
    project_save_as: (a) => saveTo(String(a.path)),
    project_export: async (a) => {
      const o = a.options as { sampleRate: number; format: string; loops: number };
      if (!(o.sampleRate >= 8000 && o.sampleRate <= 384000)) {
        throw "The sample rate must be 8000 Hz to 384000 Hz.";
      }
      if (!(o.loops >= 1 && o.loops <= LOOPS_MAX)) throw `The pattern can play 1 to ${LOOPS_MAX} times.`;
      if (s.export) throw "An export is already running.";
      const path = withExtension(String(a.path), "wav", true);
      // export.rs: written to `<path>.part`, renamed over `path` only when complete; a cancel
      // or an error deletes the part and leaves `path` as it was.
      const part = `${path}.part`;
      const job = { cancel: false };
      s.export = job;
      const total = seed.exportMs ?? 1500;
      const steps = 20;
      try {
        files.set(part, { kind: "text" });
        for (let i = 1; i <= steps; i++) {
          await new Promise((r) => setTimeout(r, total / steps));
          if (job.cancel) throw "The export was cancelled.";
          emit("export-progress", i / steps);
        }
        files.set(path, { kind: "sample", seconds: 1 });
      } finally {
        files.delete(part);
        s.export = null;
      }
      const seconds = ((o.loops * s.project.pattern.steps * STEP_TICKS) / PPQ) * (60 / s.project.bpm);
      const sounding = s.project.pattern.notes.length > 0 && s.project.channels.length > 0;
      return {
        frames: Math.round(seconds * o.sampleRate),
        seconds,
        peakDb: sounding ? -3.2 : -120,
        clipped: 0,
      };
    },
    project_export_cancel: () => {
      if (s.export) s.export.cancel = true;
      return null;
    },
    recovery_status: () =>
      s.recovery ? { savedAt: s.recovery.savedAt, original: s.recovery.original } : null,
    recovery_restore: () => {
      const r = s.recovery;
      if (!r) throw "There is no project to recover.";
      s.recovery = null;
      return openProject(r.project, r.original, true);
    },
    recovery_discard: () => {
      s.recovery = null;
      return null;
    },
    app_exit: () => {
      s.exitCount += 1;
      return null;
    },
    // Plugins.
    "plugin:dialog|open": (a) => {
      s.dialogCalls.push({ kind: "open", options: a.options });
      return s.dialogs.length ? s.dialogs.shift() : null;
    },
    "plugin:dialog|save": (a) => {
      s.dialogCalls.push({ kind: "save", options: a.options });
      return s.dialogs.length ? s.dialogs.shift() : null;
    },
    "plugin:event|listen": (a) => {
      const event = String(a.event);
      listeners.set(event, [...(listeners.get(event) ?? []), a.handler as number]);
      return a.handler;
    },
    "plugin:event|unlisten": (a) => {
      const event = String(a.event);
      listeners.set(
        event,
        (listeners.get(event) ?? []).filter((h) => h !== a.eventId),
      );
      return null;
    },
    "plugin:window|set_title": () => null,
  };

  // Argument types as serde reads them from the payload (the command signatures, `Edit`,
  // `ExportRequest`): a wrong type is refused before the command runs, as in the shell.
  type Check = (v: unknown) => string | null;
  const isInt = (v: unknown, max: number) => typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max;
  const u8: Check = (v) => (isInt(v, 255) ? null : `invalid value: ${JSON.stringify(v)}, expected u8`);
  const u32: Check = (v) => (isInt(v, 0xffffffff) ? null : `invalid value: ${JSON.stringify(v)}, expected u32`);
  // JSON has no NaN or infinity: they arrive as null.
  const f32: Check = (v) => (typeof v === "number" && Number.isFinite(v) ? null : `invalid type: ${JSON.stringify(v ?? null)}, expected f32`);
  const bool: Check = (v) => (typeof v === "boolean" ? null : `invalid type: ${JSON.stringify(v ?? null)}, expected a boolean`);
  const str: Check = (v) => (typeof v === "string" ? null : `invalid type: ${JSON.stringify(v ?? null)}, expected a string`);
  const fields = (spec: Record<string, Check>): Check => (v) => {
    if (typeof v !== "object" || v === null) return "expected a map";
    for (const [key, check] of Object.entries(spec)) {
      const why = check((v as Record<string, unknown>)[key]);
      if (why) return `${key}: ${why}`;
    }
    return null;
  };
  const EDITS: Record<string, Check> = {
    removeChannel: fields({ channel: u32 }),
    setChannelMix: fields({ channel: u32, volume: f32, pan: f32, mute: bool, solo: bool }),
    setSamplerParam: fields({ channel: u32, param: str, value: f32 }),
    toggleStep: fields({ channel: u32, step: u32 }),
    setStepVelocity: fields({ channel: u32, step: u32, velocity: f32 }),
    clearStep: fields({ channel: u32, step: u32 }),
    setPatternLength: fields({ steps: u32 }),
    setSwing: fields({ swing: f32 }),
    setTempo: fields({ bpm: f32 }),
    setTimeSignature: fields({ beats: u8, unit: u8 }),
  };
  const edit: Check = (v) => {
    const kind = (v as { kind?: unknown } | null)?.kind;
    const check = typeof kind === "string" ? EDITS[kind] : undefined;
    return check ? check(v) : `unknown variant ${JSON.stringify(kind ?? null)}`;
  };
  const exportOptions: Check = (v) =>
    fields({ sampleRate: u32, loops: u32 })(v) ??
    (["pcm24", "float32"].includes((v as { format?: unknown }).format as string)
      ? null
      : `format: unknown variant ${JSON.stringify((v as { format?: unknown }).format ?? null)}, expected \`pcm24\` or \`float32\``);
  const ARGS: Record<string, Record<string, Check>> = {
    audio_select: { id: str },
    audio_set_guard: { on: bool },
    test_tone: { on: bool },
    synth_note: { key: u8, on: bool },
    project_edit: { edit },
    channel_add: { path: str },
    channel_set_sample: { channel: u32, path: str },
    channel_preview: { channel: u32, on: bool },
    project_open: { path: str },
    project_save_as: { path: str },
    project_export: { path: str, options: exportOptions },
  };

  const invoke = async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args: clone(args ?? {}), at: performance.now() });
    const handler = commands[cmd];
    if (!handler) throw `The mock engine has no command ${cmd}`;
    for (const [name, check] of Object.entries(ARGS[cmd] ?? {})) {
      const why = check((args ?? {})[name]);
      if (why) throw `invalid args \`${name}\` for command \`${cmd}\`: ${why}`;
    }
    return clone(await handler(args ?? {}));
  };

  const w = window as unknown as Record<string, unknown>;
  w.__TAURI_INTERNALS__ = {
    invoke,
    transformCallback,
    unregisterCallback: (id: number) => callbacks.delete(id),
    runCallback: (id: number, data: unknown) => callbacks.get(id)?.(data),
    callbacks,
    convertFileSrc: (path: string) => path,
    metadata: {
      currentWindow: { label: "main" },
      currentWebview: { windowLabel: "main", label: "main" },
    },
  };
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (_event: string, id: number) => callbacks.delete(id),
  };
  w.__grooviqMock = {
    calls,
    files,
    state: s,
    view,
    emit,
    ticks: ticksNow,
    /** Pins the playing transport at `ticks` (the live frames report it), or lets it run on. */
    hold: (ticks: number | null) => {
      s.transport.held = ticks;
    },
    /** Answers for the next open/save dialogs, in order (`null`: the person cancels). */
    queueDialog: (...answers: unknown[]) => s.dialogs.push(...answers),
    /** What the shell does on a window close or Cmd+Q: ask the UI while there are unsaved
     * changes, else end. Returns whether it asked. */
    requestClose: () => {
      if (dirty()) {
        emit("close-requested", null);
        return true;
      }
      s.exitCount += 1;
      return false;
    },
    /** Files dragged from the OS: enter, then drop (or leave). */
    drag: (paths: string[], drop: boolean) => {
      emit("tauri://drag-enter", { paths, position: { x: 400, y: 300 } });
      emit("tauri://drag-over", { position: { x: 400, y: 300 } });
      if (drop) emit("tauri://drag-drop", { paths, position: { x: 400, y: 300 } });
      else emit("tauri://drag-leave", null);
    },
    autosave: () => {
      s.autosavedAt = Math.floor(Date.now() / 1000);
      emit("project-autosaved", s.autosavedAt);
    },
  };
}
