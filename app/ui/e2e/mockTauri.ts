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
  key?: { root: number; scale: string } | null;
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
  /** Sampler unless said otherwise. */
  kind?: "sampler" | "synth";
  synth?: Record<string, number>;
  preset?: string | null;
  choke?: number;
  /** The sample's length in frames (default: its file's seconds at 48 kHz). */
  frames?: number;
}

/** A note. A step hit needs only channel, tick and velocity: the rest defaults to a plain step
 * (length 240, key 60, no slide) and ids are given on load. */
export interface MockNote {
  channel: number;
  tick: number;
  velocity: number;
  id?: number;
  length?: number;
  key?: number;
  slide?: boolean;
}

/** One step of the undo history (undo.rs `NoteChange` and `Step`): the changed notes before
 * and after, by id, its id in the length log, and the recorded take it is open to (or null). */
interface NoteChange {
  before: MockNote[];
  after: MockNote[];
  id?: number;
  take?: number | null;
}

/** undo.rs `Growth`: a commit's latest note end, the length it grows the pattern to, and the
 * bar (ticks) that length was rounded to. */
interface Growth {
  end: number;
  to: number;
  bar: number;
}

/** undo.rs `Length`: a commit's growth owned by a step (null: a step that left the history), or
 * a length set by hand. */
type LengthEntry = { owner: number | null; growth: Growth } | { set: number };

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
  /** The synth's parameters (the same fixture). */
  synthParams?: { id: string; min: number; max: number; default: number }[];
  /** The MIDI keyboards plugged in at start, and the ones the person switched off (settings.json). */
  midi?: { inputs: string[]; disabled?: string[] };
  /** Every synth preset's 40 values in SPEC order (tests/fixtures/synth-presets.json). */
  synthPresets?: Record<string, number[]>;
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
  const SYNTH_PARAMS: [string, number, number, number][] = (seed.synthParams ?? []).map((p) => [p.id, p.min, p.max, p.default]);
  if (!seed.synthPresets) throw new Error("the mock needs the synth presets");
  const PRESET_VALUES = seed.synthPresets;
  /** All 40 values of a factory preset, by parameter id (`LoadSynthPreset`, Task 7). */
  const presetValues = (name: string) =>
    Object.fromEntries(SYNTH_PARAMS.map(([id], i) => [id, PRESET_VALUES[name][i]]));
  // project/src (Task 6 and 7): presets (grooviq_dsp::synth::PRESETS), ranges, the notes wire format.
  const PRESETS = ["Init", "808 Glide", "Sub Bass", "Reese", "Bell Keys", "Dark Pad", "Pluck", "Trap Lead"];
  const SCALES = [
    "major", "minor", "harmonic_minor", "melodic_minor", "dorian", "phrygian", "lydian", "mixolydian",
    "locrian", "major_pentatonic", "minor_pentatonic", "blues",
  ];
  const MAX_CHOKE = 16;
  const MAX_NOTES = 100_000;
  /** undo.rs `MAX_UNDO` and `MAX_UNDO_NOTES`. */
  const MAX_UNDO = 200;
  const MAX_UNDO_NOTES = 2_000_000;
  const NOTES_MAGIC = 0x314e5147;
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
  /** Ids for notes without one, and the plain-step defaults (a project as Task 6 loads it). */
  const normalise = (p: Project): Project => {
    let next = Math.max(0, ...p.pattern.notes.map((n) => n.id ?? 0)) + 1;
    p.pattern.notes = p.pattern.notes.map((n) => ({
      id: n.id ?? next++,
      channel: n.channel,
      tick: n.tick,
      length: n.length ?? STEP_TICKS,
      key: n.key ?? 60,
      velocity: n.velocity,
      slide: n.slide ?? false,
    }));
    return p;
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
    project: normalise(seed.project ? clone(seed.project) : emptyProject()),
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
    /** Bumped by every note change (Pattern.revision, starts at 1 on load). */
    notesRevision: 1,
    recording: false,
    metronome: false,
    /** The channel MIDI plays (`channel_select`). */
    selected: null as number | null,
    /** The MIDI inputs plugged in (sorted, no duplicates: midi/hub.rs `poll`) and the names
     * switched off (`midiDisabled` in settings.json, kept while a device is unplugged). */
    midi: { present: [...new Set(seed.midi?.inputs ?? [])].sort(), disabled: [...(seed.midi?.disabled ?? [])] },
    /** Live notes sounding now, "channel:key" -> velocity (`channel_note`). */
    live: new Map<string, number>(),
    /** Live presses handed out and not released yet: press -> "channel:key" (notes_cmds.rs
     * `channel_note`, project_cmds.rs `channel_preview`: a note-on returns its press, a release
     * names it). */
    presses: new Map<number, string>(),
    nextPress: 1,
    /** Notes being recorded: "channel:key" -> where and how hard they started. */
    recordOpen: new Map<string, { tick: number; velocity: number; take: number }>(),
    /** The running take's number (engine/src/record.rs): one more after every take ends. */
    take: 0,
    /** Live frames are held back (a stalled stream), as `pauseLive` sets. */
    livePaused: false,
    /** `channel_param_live` calls, last value per "channel:param". */
    liveParams: new Map<string, number>(),
    dialogCalls: [] as { kind: string; options: unknown }[],
    /** Note changes for undo and redo (undo.rs): the notes and length before and after each. */
    history: { undo: [] as NoteChange[], redo: [] as NoteChange[], notes: 0, base: 0, log: [] as LengthEntry[], nextId: 0 },
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
    c.kind === "synth" ? {} : Object.fromEntries(PARAMS.map(([id, , , def]) => [id, c.sampler[id] ?? def]));
  const synthValues = (c: MockChannel) =>
    c.kind === "synth" ? Object.fromEntries(SYNTH_PARAMS.map(([id, , , def]) => [id, c.synth?.[id] ?? def])) : {};
  /** Global Constraints: a plain step is on a step, a step long, key 60, no slide. */
  const plain = (n: MockNote) => n.tick % STEP_TICKS === 0 && n.length === STEP_TICKS && n.key === 60 && !n.slide;
  const isRoll = (id: number) => s.project.pattern.notes.some((n) => n.channel === id && !plain(n));
  // session.rs: missing = a sample reference without decoded bytes. The path is never opened.
  const missing = (c: MockChannel) => c.sample !== null && c.embedded === false;
  /** A sample file's length (a second unless the file says otherwise). */
  const sampleSeconds = (path: string) => {
    const f = files.get(path);
    return f?.kind === "sample" ? (f.seconds ?? 1) : 1;
  };
  const view = () => {
    const p = s.project;
    const steps = p.pattern.steps;
    const cells: Record<string, (number | null)[]> = {};
    for (const c of p.channels) cells[String(c.id)] = Array(steps).fill(null);
    for (const n of p.pattern.notes) {
      const step = n.tick / STEP_TICKS;
      const row = cells[String(n.channel)];
      if (plain(n) && step < steps && row) row[step] = n.velocity;
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
        // session.rs: the decoded sample's length; none while it is missing.
        sampleFrames: c.sample === null || missing(c) ? null : (c.frames ?? Math.round(48_000 * sampleSeconds(c.sample))),
        volume: c.volume,
        pan: c.pan,
        mute: c.mute,
        solo: c.solo,
        sampler: samplerValues(c),
        kind: c.kind ?? "sampler",
        synth: synthValues(c),
        preset: c.preset ?? null,
        choke: c.choke ?? 0,
        roll: isRoll(c.id),
      })),
      pattern: { id: p.pattern.id, name: p.pattern.name, steps, swing: p.pattern.swing, cells },
      autosavedAt: s.autosavedAt,
      key: p.key ?? null,
      notesRevision: s.notesRevision,
      recording: s.recording,
      canUndo: s.history.undo.length > 0,
      canRedo: s.history.redo.length > 0,
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
    const c = s.project.channels[channelIndex(channel)];
    // session.rs (Task 7): SessionError::RollChannel.
    if (isRoll(c.id)) throw `${c.name} has piano-roll notes. Edit them in the piano roll (F7).`;
    const steps = s.project.pattern.steps;
    const n = Number(step);
    if (!(n >= 0 && n < steps)) throw `Step ${n + 1} is past the end of the pattern (${steps} steps).`;
    return n * STEP_TICKS;
  };
  /** Every note change, from any path, bumps the revision and tells the UI (Task 7). */
  const notesChanged = () => {
    s.notesRevision += 1;
    emit("notes-changed", s.notesRevision);
  };
  /** Changes the pattern; `true` if anything changed. A note change bumps the notes revision
   * and is one step of the undo history (undo.rs). With `take`, it is a note recorded in that
   * take: it joins the take's open step. */
  const editPattern = (change: (p: Project["pattern"]) => void, take: number | null = null) => {
    const before = JSON.stringify(s.project.pattern);
    const notesBefore = JSON.stringify(s.project.pattern.notes);
    const prev = s.project.pattern;
    const next = clone(prev);
    lastGrowth = null;
    change(next);
    const growth = lastGrowth as Growth | null;
    if (JSON.stringify(next) === before) return false;
    s.project.pattern = next;
    if (JSON.stringify(next.notes) !== notesBefore) {
      const step = noteChange(prev, next);
      if (take === null) pushHistory(step, prev.steps, growth);
      else recordHistory(step, take, prev.steps, growth);
      notesChanged();
    } else if (growth && next.steps !== prev.steps) {
      // session.rs: notes resubmitted as they were, growing a pattern shortened by hand, are
      // a step holding only that growth.
      pushHistory({ before: [], after: [] }, prev.steps, growth);
    }
    return true;
  };
  /** The growth the last `growToNotes` worked out, for the length log. */
  let lastGrowth: Growth | null = null;
  const sameNote = (a: MockNote, b: MockNote) =>
    a.channel === b.channel && a.tick === b.tick && a.length === b.length && a.key === b.key &&
    a.velocity === b.velocity && Boolean(a.slide) === Boolean(b.slide);
  /** undo.rs `NoteChange::between`. */
  const noteChange = (old: Project["pattern"], next: Project["pattern"]): NoteChange => {
    const was = new Map(old.notes.map((n) => [n.id, n]));
    const before: MockNote[] = [];
    const after: MockNote[] = [];
    for (const n of next.notes) {
      const o = was.get(n.id);
      was.delete(n.id);
      if (!o) after.push(clone(n));
      else if (!sameNote(o, n)) {
        before.push(clone(o));
        after.push(clone(n));
      }
    }
    for (const o of was.values()) before.push(clone(o));
    return { before, after };
  };
  const sizeOf = (c: NoteChange) => c.before.length + c.after.length;
  /** undo.rs `play`: the log from `base`, with the growth of the steps `live` holds. */
  const playLength = (base: number, log: LengthEntry[], live: (id: number) => boolean) =>
    log.reduce((steps, l) => {
      if ("set" in l) return l.set;
      if (l.owner !== null && !live(l.owner)) return steps;
      return l.growth.end > steps * STEP_TICKS ? Math.max(steps, l.growth.to) : steps;
    }, base);
  /** undo.rs `NoteHistory::trim`: the oldest steps go past the step count or the note budget;
   * their growth stays for good, folded into the log's start. */
  const trimHistory = () => {
    const h = s.history;
    const dropped = new Set<number>();
    while (h.undo.length > MAX_UNDO || (h.notes > MAX_UNDO_NOTES && h.undo.length > 1)) {
      const old = h.undo.shift()!;
      h.notes -= sizeOf(old);
      dropped.add(old.id!);
    }
    if (!dropped.size) return;
    for (const l of h.log) if ("owner" in l && l.owner !== null && dropped.has(l.owner)) l.owner = null;
    compactLog();
  };
  /** undo.rs `append`: a length set by hand replaces one set right before it; a step's growth
   * after its own growth with the same bar keeps the one reaching further. */
  const appendLength = (log: LengthEntry[], entry: LengthEntry) => {
    const last = log[log.length - 1];
    if (last && "set" in last && "set" in entry) last.set = entry.set;
    else if (last && "owner" in last && "owner" in entry && last.owner === entry.owner && last.growth.bar === entry.growth.bar) {
      if (entry.growth.end > last.growth.end) last.growth = entry.growth;
    } else log.push(entry);
  };
  /** undo.rs `NoteHistory::fold`: leading entries no step can take back play into the start. */
  const foldLog = () => {
    const h = s.history;
    let fixed = 0;
    while (fixed < h.log.length) {
      const l = h.log[fixed];
      if ("owner" in l && l.owner !== null) break;
      fixed++;
    }
    if (!fixed) return;
    h.base = playLength(h.base, h.log.slice(0, fixed), () => true);
    h.log.splice(0, fixed);
  };
  /** undo.rs `NoteHistory::compact`. */
  const compactLog = () => {
    const log: LengthEntry[] = [];
    for (const l of s.history.log) appendLength(log, l);
    s.history.log = log;
    foldLog();
  };
  const historyEmpty = () => !s.history.undo.length && !s.history.redo.length;
  /** undo.rs `NoteHistory::close`: every open take step closes. */
  const closeTakes = () => {
    for (const c of s.history.undo) c.take = null;
  };
  /** undo.rs `NoteHistory::add`: a note change clears redo (and its growth leaves the log); the
   * first step starts the log at `steps`. */
  const addHistory = (c: NoteChange, take: number | null, steps: number) => {
    const h = s.history;
    if (historyEmpty()) {
      h.base = steps;
      h.log = [];
    }
    const gone = new Set(h.redo.map((r) => r.id));
    for (const r of h.redo) h.notes -= sizeOf(r);
    h.redo = [];
    if (gone.size) {
      h.log = h.log.filter((l) => !("owner" in l) || l.owner === null || !gone.has(l.owner));
      compactLog();
    }
    c.take = take;
    c.id = h.nextId++;
    h.notes += sizeOf(c);
    h.undo.push(c);
    trimHistory();
    return c.id;
  };
  const logGrowth = (owner: number, growth: Growth | null) => {
    if (growth) appendLength(s.history.log, { owner, growth: { ...growth } });
  };
  /** undo.rs `NoteHistory::push`. */
  const pushHistory = (c: NoteChange, steps: number, growth: Growth | null) => {
    closeTakes();
    logGrowth(addHistory(c, null, steps), growth);
  };
  /** undo.rs `NoteHistory::record`: a take's notes join its open step, wherever it sits. */
  const recordHistory = (c: NoteChange, take: number, steps: number, growth: Growth | null) => {
    const h = s.history;
    const open = [...h.undo].reverse().find((step) => step.take === take);
    if (open && c.before.length === 0) {
      let newest = -1;
      for (const n of open.before) newest = Math.max(newest, n.id ?? 0);
      for (const n of open.after) newest = Math.max(newest, n.id ?? 0);
      if (c.after.every((n) => (n.id ?? 0) > newest)) {
        open.after.push(...c.after);
        h.notes += sizeOf(c);
        logGrowth(open.id!, growth);
        trimHistory();
        return;
      }
    }
    logGrowth(addHistory(c, take, steps), growth);
  };
  /** undo.rs `NoteHistory::set_length`: undo never takes a length set by hand back. */
  const setLengthByHand = (steps: number) => {
    if (historyEmpty()) return;
    appendLength(s.history.log, { set: steps });
    foldLog();
  };
  /** undo.rs `NoteHistory::length_after`. */
  const lengthAfter = (undo: boolean) => {
    const h = s.history;
    const live = new Set(h.undo.map((c) => c.id!));
    if (undo) live.delete(h.undo[h.undo.length - 1]?.id ?? -1);
    else if (h.redo.length) live.add(h.redo[h.redo.length - 1].id!);
    return playLength(h.base, h.log, (id) => live.has(id));
  };
  /** Undo or redo (session.rs `undo_notes`): the notes go back by id, the length is the log
   * played with the steps in effect; the project is changed and the UI fetches the notes. */
  const stepHistory = (undo: boolean) => {
    const h = s.history;
    if (!(undo ? h.undo : h.redo).length) return null;
    const steps = lengthAfter(undo);
    const change = (undo ? h.undo : h.redo).pop()!;
    const pat = s.project.pattern;
    const [out, put] = undo ? [change.after, change.before] : [change.before, change.after];
    const ids = new Set([...out, ...put].map((n) => n.id));
    pat.notes = [...pat.notes.filter((n) => !ids.has(n.id)), ...clone(put)].sort((a, b) => (a.id ?? 0) - (b.id ?? 0));
    pat.steps = steps;
    closeTakes();
    change.take = null;
    (undo ? h.redo : h.undo).push(change);
    notesChanged();
    changed();
    return { revision: s.notesRevision, addedIds: [], ignoredIds: [], reload: true, view: view() };
  };
  const clearHistory = () => {
    s.history.undo = [];
    s.history.redo = [];
    s.history.notes = 0;
    s.history.base = 0;
    s.history.log = [];
  };
  const nextNoteId = () => Math.max(0, ...s.project.pattern.notes.map((n) => n.id ?? 0)) + 1;
  const noteBase = (channel: number, tick: number, velocity: number): MockNote => ({
    id: nextNoteId(),
    channel,
    tick,
    length: STEP_TICKS,
    key: 60,
    velocity,
    slide: false,
  });
  /** Grows the pattern to the end of the bar holding the latest end of `touched` (the notes an
   * edit added or moved; never shrinks, at most 256 steps). Notes left past a shortened end do
   * not grow it back (project/src/notes.rs). */
  const growToNotes = (pat: Project["pattern"], touched: MockNote[]) => {
    const { beats, unit } = s.project.timeSignature;
    const barTicks = (beats * 3840) / unit;
    let end = 0;
    for (const n of touched) end = Math.max(end, n.tick + (n.length ?? STEP_TICKS));
    const to = Math.min(MAX_STEPS, Math.ceil(end / barTicks) * (barTicks / STEP_TICKS));
    // notes.rs `growth`: every commit's growth goes into the history's length log.
    if (end > 0) lastGrowth = { end, to, bar: barTicks };
    if (end <= pat.steps * STEP_TICKS) return;
    pat.steps = Math.max(pat.steps, to);
  };
  /** Clamps a note's values as `validate` does; `true` if any changed. */
  const clampNote = (n: MockNote) => {
    const before = JSON.stringify(n);
    n.tick = Math.max(0, Math.trunc(n.tick));
    n.length = Math.max(1, Math.trunc(n.length ?? 1));
    n.key = clamp(Math.trunc(n.key ?? 60), 0, 127);
    n.velocity = clamp(n.velocity, 0, 1);
    n.slide = Boolean(n.slide);
    return JSON.stringify(n) !== before;
  };
  const instrumentParam = (c: MockChannel, param: unknown, value: unknown) => {
    const synth = c.kind === "synth";
    const spec = (synth ? SYNTH_PARAMS : PARAMS).find(([id]) => id === param);
    if (!spec) throw `The ${synth ? "synth" : "sampler"} has no setting called "${String(param)}".`;
    return { spec, value: clamp(finite(value, `${synth ? "synth" : "sampler"} value`), spec[1], spec[2]) };
  };
  type Edit = Record<string, unknown> & { kind: string };
  const applyEdit = (e: Edit): boolean => {
    const p = s.project;
    switch (e.kind) {
      case "removeChannel": {
        const i = channelIndex(e.channel);
        editPattern((pat) => {
          pat.notes = pat.notes.filter((n) => n.channel !== e.channel);
        });
        p.channels.splice(i, 1);
        clearHistory();
        // The engine drops the channel's live notes with it (Task 5).
        for (const k of [...s.live.keys()]) if (k.startsWith(`${String(e.channel)}:`)) s.live.delete(k);
        return true;
      }
      case "setInstrumentParam": {
        const c = p.channels[channelIndex(e.channel)];
        const { spec, value } = instrumentParam(c, e.param, e.value);
        const store = c.kind === "synth" ? (c.synth ??= {}) : c.sampler;
        if (store[spec[0]] === value) return false;
        store[spec[0]] = value;
        return true;
      }
      case "loadSynthPreset": {
        const c = p.channels[channelIndex(e.channel)];
        if (c.kind !== "synth") throw `${c.name} is not a synth channel.`;
        if (!PRESETS.includes(String(e.preset))) throw `There is no synth preset called "${String(e.preset)}".`;
        c.synth = presetValues(String(e.preset));
        c.preset = String(e.preset);
        return true;
      }
      case "setChoke": {
        const c = p.channels[channelIndex(e.channel)];
        const group = clamp(Math.trunc(Number(e.group)), 0, MAX_CHOKE);
        if ((c.choke ?? 0) === group) return false;
        c.choke = group;
        return true;
      }
      case "setProjectKey": {
        const k = e.key as { root: number; scale: string } | null;
        if (k !== null && (!(k.root >= 0 && k.root <= 11) || !SCALES.includes(k.scale))) {
          throw `${String(k.scale)} on ${String(k.root)} is not a key Grooviq knows.`;
        }
        if (JSON.stringify(p.key ?? null) === JSON.stringify(k)) return false;
        p.key = k ? { root: k.root, scale: k.scale } : null;
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
          if (pat.notes.length === before) pat.notes.push(noteBase(e.channel as number, tick, DEFAULT_VELOCITY));
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
          if (!found) pat.notes.push(noteBase(e.channel as number, tick, velocity));
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
        // Set by hand, even to the length it has: undo never takes it back.
        setLengthByHand(steps);
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
    s.project = normalise(clone(p));
    clearHistory();
    notesChanged();
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
  /** Stops the clock. Recording ends at the stop tick first: every open note closes there
   * (the recorder's `End`), so a key let go later records nothing more. */
  const stopTransport = () => {
    const end = ticksNow();
    for (const key of [...s.recordOpen.keys()]) closeRecorded(key, end);
    s.recordOpen.clear();
    // The take ends with the transport: the next one has the next number.
    if (s.recording && s.transport.playing) s.take += 1;
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
  const sockets = new Set<LiveSocket>();
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
        sockets.add(this);
        this.onopen?.(new Event("open"));
        this.timer = setInterval(() => {
          if (s.livePaused) return;
          this.onmessage?.(new MessageEvent("message", { data: encodeLive() }));
        }, 16);
      }, 0);
    }
    send() {}
    close() {
      if (this.timer) clearInterval(this.timer);
      sockets.delete(this);
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

  // --- Notes (Task 7): the wire format, edits by id, live notes and the recorder. ---
  const encodeNotes = () => {
    const notes = [...s.project.pattern.notes].sort(
      (a, b) => a.tick - b.tick || a.channel - b.channel || (a.id ?? 0) - (b.id ?? 0),
    );
    const buf = new ArrayBuffer(16 + notes.length * 24);
    const v = new DataView(buf);
    v.setUint32(0, NOTES_MAGIC, true);
    v.setUint32(4, s.notesRevision, true);
    v.setUint32(8, notes.length, true);
    notes.forEach((n, i) => {
      const o = 16 + i * 24;
      v.setUint32(o, n.id ?? 0, true);
      v.setUint32(o + 4, n.channel, true);
      v.setUint32(o + 8, n.tick, true);
      v.setUint32(o + 12, n.length ?? STEP_TICKS, true);
      v.setFloat32(o + 16, n.velocity, true);
      v.setUint8(o + 20, n.key ?? 60);
      v.setUint8(o + 21, n.slide ? 1 : 0);
    });
    return buf;
  };
  type WireNote = Required<MockNote>;
  const editNotes = (e: { baseRevision: number; add: Omit<WireNote, "id">[]; update: WireNote[]; delete: number[] }) => {
    const stale = e.baseRevision !== s.notesRevision;
    const ignored: number[] = [];
    const added: number[] = [];
    let clamped = false;
    const touched: MockNote[] = [];
    const done = editPattern((pat) => {
      const has = (id: number) => pat.notes.some((n) => n.id === id);
      for (const id of e.delete) {
        if (has(id)) pat.notes = pat.notes.filter((n) => n.id !== id);
        else ignored.push(id);
      }
      // By id, so a 10,000-note move is not 10,000 scans.
      const slots = new Map(pat.notes.map((n, i) => [n.id, i]));
      for (const u of e.update) {
        const i = slots.get(u.id) ?? -1;
        if (i < 0) {
          ignored.push(u.id);
          continue;
        }
        const n = { ...u };
        clamped = clampNote(n) || clamped;
        channelIndex(n.channel);
        pat.notes[i] = n;
        touched.push(n);
      }
      let id = Math.max(0, ...pat.notes.map((n) => n.id ?? 0)) + 1;
      for (const a of e.add) {
        if (pat.notes.length >= MAX_NOTES) throw `A pattern holds at most ${MAX_NOTES} notes.`;
        channelIndex(a.channel);
        const n: MockNote = { ...a, id: id++ };
        clamped = clampNote(n) || clamped;
        pat.notes.push(n);
        touched.push(n);
        added.push(n.id!);
      }
      growToNotes(pat, touched);
    });
    if (done) changed();
    return { revision: s.notesRevision, addedIds: added, ignoredIds: ignored, reload: stale || clamped, view: view() };
  };
  /** The recorder (recorder.rs): a note-on and its note-off become a note at the tick it was
   * heard, in the loop; an open note ends when recording stops. Notes are overdubbed. */
  const closeRecorded = (key: string, now: number) => {
    const open = s.recordOpen.get(key);
    if (!open) return;
    s.recordOpen.delete(key);
    const [channel, note] = key.split(":").map(Number);
    if (!s.project.channels.some((c) => c.id === channel)) return;
    // At the note cap the note is not added: recording switches off and says so (session.rs).
    if (s.project.pattern.notes.length >= MAX_NOTES) {
      if (s.recording && s.transport.playing) s.take += 1;
      s.recording = false;
      emit("recording-stopped", {
        reason: "noteCap",
        dropped: 1,
        message: `Recording stopped: this pattern holds ${MAX_NOTES} notes, the most a pattern can hold. 1 recorded note was not added.`,
      });
      return;
    }
    const loop = s.project.pattern.steps * STEP_TICKS;
    editPattern((pat) => {
      const n: MockNote = {
        id: nextNoteId(),
        channel,
        tick: open.tick % loop,
        length: Math.max(1, now - open.tick),
        key: note,
        velocity: open.velocity,
        slide: false,
      };
      pat.notes.push(n);
      growToNotes(pat, [n]);
    }, open.take);
    changed();
  };

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
      // session.rs: the length is the new file's (a seeded length belonged to the old one).
      delete c.frames;
      changed();
      return view();
    },
    pattern_notes: () => encodeNotes(),
    notes_edit: (a) => editNotes(a.edit as Parameters<typeof editNotes>[0]),
    notes_undo: () => stepHistory(true),
    notes_redo: () => stepHistory(false),
    channel_add_synth: (a) => {
      const preset = String(a.preset);
      if (!PRESETS.includes(preset)) throw `There is no synth preset called "${preset}".`;
      const p = s.project;
      if (p.channels.length >= MAX_CHANNELS) {
        throw `A project holds at most ${MAX_CHANNELS} channels; remove one to add another.`;
      }
      const last = p.channels[p.channels.length - 1];
      p.channels.push({
        id: p.nextId,
        name: preset,
        color: last ? (last.color + 1) % INKS : 0,
        sample: null,
        volume: 1,
        pan: 0,
        mute: false,
        solo: false,
        sampler: {},
        kind: "synth",
        synth: presetValues(preset),
        preset,
        choke: 0,
      });
      p.nextId += 1;
      changed();
      return view();
    },
    channel_note: (a) => {
      const c = s.project.channels[channelIndex(a.channel)];
      const key = `${c.id}:${String(a.key)}`;
      if (a.on) {
        s.live.set(key, a.velocity as number);
        if (s.recording && s.transport.playing) s.recordOpen.set(key, { tick: ticksNow(), velocity: a.velocity as number, take: s.take });
        return press(key);
      }
      // The engine ends only the note the release names: a press it never made ends nothing.
      if (!release(a.press, key)) return null;
      if (![...s.presses.values()].includes(key)) s.live.delete(key);
      closeRecorded(key, ticksNow());
      return null;
    },
    channel_param_live: (a) => {
      const c = s.project.channels[channelIndex(a.channel)];
      const { spec, value } = instrumentParam(c, a.param, a.value);
      s.liveParams.set(`${c.id}:${spec[0]}`, value);
      return null;
    },
    channel_select: (a) => {
      if (a.channel !== null) channelIndex(a.channel);
      s.selected = a.channel as number | null;
      return null;
    },
    channel_waveform: (a) => {
      const c = s.project.channels[channelIndex(a.channel)];
      if (c.kind === "synth" || c.sample === null || c.embedded === false) throw `${c.name} has no sample to draw.`;
      // session.rs `waveform_peaks`: 1..=MAX_WAVEFORM_POINTS points.
      const points = clamp(a.points as number, 1, 16_384);
      const buf = new ArrayBuffer(points * 8);
      const v = new DataView(buf);
      for (let i = 0; i < points; i++) {
        const amp = Math.exp((-4 * i) / Math.max(1, points));
        v.setFloat32(i * 8, -amp, true);
        v.setFloat32(i * 8 + 4, amp, true);
      }
      return buf;
    },
    record_set: (a) => {
      const on = Boolean(a.on);
      if (!on) for (const key of [...s.recordOpen.keys()]) closeRecorded(key, ticksNow());
      // Switching recording off ends a running take; every switch on starts a new one.
      if (on || (s.recording && s.transport.playing)) s.take += 1;
      s.recording = on;
      return view();
    },
    metronome_set: (a) => {
      s.metronome = Boolean(a.on);
      return null;
    },
    // midi_cmds.rs: the list is what the last poll found, `midi_set_enabled` tells the UI only
    // when the switch moved (midi/hub.rs `set_enabled`).
    midi_inputs: () => s.midi.present.map((name) => ({ name, enabled: !s.midi.disabled.includes(name) })),
    midi_set_enabled: (a) => {
      const name = a.name as string;
      const was = !s.midi.disabled.includes(name);
      s.midi.disabled = s.midi.disabled.filter((n) => n !== name);
      if (!a.enabled) s.midi.disabled.push(name);
      if (was !== Boolean(a.enabled)) emit("midi-inputs-changed", null);
      return null;
    },
    channel_preview: (a) => {
      channelIndex(a.channel);
      const key = `${String(a.channel)}:preview`;
      if (a.on) {
        s.previews.set(a.channel as number, true);
        return press(key);
      }
      if (release(a.press, key) && ![...s.presses.values()].includes(key)) s.previews.set(a.channel as number, false);
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

  /** A new press for a note-on of `key`. */
  const press = (key: string) => {
    const id = s.nextPress++;
    s.presses.set(id, key);
    return id;
  };
  /** Ends `id` if it is a press of `key`; a release must name one. */
  const release = (id: unknown, key: string) => {
    if (id === null || id === undefined) throw "A release names the press it ends.";
    if (s.presses.get(id as number) !== key) return false;
    s.presses.delete(id as number);
    return true;
  };

  // Argument types as serde reads them from the payload (the command signatures, `Edit`,
  // `ExportRequest`): a wrong type is refused before the command runs, as in the shell.
  type Check = (v: unknown) => string | null;
  const isInt = (v: unknown, max: number) => typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max;
  const u8: Check = (v) => (isInt(v, 255) ? null : `invalid value: ${JSON.stringify(v)}, expected u8`);
  const u32: Check = (v) => (isInt(v, 0xffffffff) ? null : `invalid value: ${JSON.stringify(v)}, expected u32`);
  // A u64 as JavaScript numbers carry it exactly (the presses the shell hands out stay far below).
  const u64: Check = (v) => (isInt(v, Number.MAX_SAFE_INTEGER) ? null : `invalid value: ${JSON.stringify(v)}, expected u64`);
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
    setInstrumentParam: fields({ channel: u32, param: str, value: f32 }),
    loadSynthPreset: fields({ channel: u32, preset: str }),
    setChoke: fields({ channel: u32, group: u8 }),
    setProjectKey: (v) => {
      const key = (v as { key?: unknown } | null)?.key;
      return key === null ? null : fields({ root: u8, scale: str })(key);
    },
  };
  const optional = (check: Check): Check => (v) => (v === null || v === undefined ? null : check(v));
  const list = (check: Check): Check => (v) => {
    if (!Array.isArray(v)) return `invalid type: ${JSON.stringify(v ?? null)}, expected a sequence`;
    for (const item of v) {
      const why = check(item);
      if (why) return why;
    }
    return null;
  };
  const newNote = fields({ channel: u32, tick: u32, length: u32, key: u8, velocity: f32, slide: bool });
  const notesEditArg: Check = fields({
    baseRevision: u32,
    add: list(newNote),
    update: list(fields({ id: u32, channel: u32, tick: u32, length: u32, key: u8, velocity: f32, slide: bool })),
    delete: list(u32),
  });
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
    channel_preview: { channel: u32, on: bool, press: optional(u64) },
    notes_edit: { edit: notesEditArg },
    channel_add_synth: { preset: str },
    channel_note: { channel: u32, key: u8, velocity: f32, on: bool, press: optional(u64) },
    channel_param_live: { channel: u32, param: str, value: f32 },
    channel_select: { channel: optional(u32) },
    midi_set_enabled: { name: str, enabled: bool },
    channel_waveform: { channel: u32, points: u32 },
    record_set: { on: bool },
    metronome_set: { on: bool },
    project_open: { path: str },
    project_save_as: { path: str },
    project_export: { path: str, options: exportOptions },
  };

  /** Commands the test holds back (`holdCommand`), each until `releaseCommand`. */
  const holds = new Map<string, { promise: Promise<void>; release: () => void }>();
  /** Commands whose next call the engine refuses (`refuseNext`), with the error it gives. */
  const refusals = new Map<string, string>();
  const invoke = async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args: clone(args ?? {}), at: performance.now() });
    // A held command waits here, before the engine sees it, until the test releases it.
    const held = holds.get(cmd);
    if (held) await held.promise;
    const refused = refusals.get(cmd);
    if (refused !== undefined) {
      refusals.delete(cmd);
      throw refused;
    }
    const handler = commands[cmd];
    if (!handler) throw `The mock engine has no command ${cmd}`;
    for (const [name, check] of Object.entries(ARGS[cmd] ?? {})) {
      const why = check((args ?? {})[name]);
      if (why) throw `invalid args \`${name}\` for command \`${cmd}\`: ${why}`;
    }
    const result = await handler(args ?? {});
    // A raw `tauri::ipc::Response` arrives as an ArrayBuffer.
    return result instanceof ArrayBuffer ? result.slice(0) : clone(result);
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
    /** Delivers one frame now that says `playing` whatever the transport does: a frame the
     * engine rendered earlier and the stream delivers late. */
    injectLive: (playing: boolean) => {
      const data = encodeLive();
      const v = new DataView(data);
      v.setUint8(1, (v.getUint8(1) & ~1) | (playing ? 1 : 0));
      for (const socket of sockets) socket.onmessage?.(new MessageEvent("message", { data }));
    },
    /** The next call of `cmd` fails with `error`, as when the engine's queue is full. */
    refuseNext: (cmd: string, error: string) => {
      refusals.set(cmd, error);
    },
    /** Holds every later call of `cmd` before the engine handles it, until `releaseCommand`. */
    holdCommand: (cmd: string) => {
      let release = () => {};
      const promise = new Promise<void>((r) => (release = r));
      holds.set(cmd, { promise, release });
    },
    releaseCommand: (cmd: string) => {
      holds.get(cmd)?.release();
      holds.delete(cmd);
    },
    /** Holds back live frames (`true`) or lets them flow again. */
    pauseLive: (on: boolean) => {
      s.livePaused = on;
    },
    /** A note that arrives from outside the UI's edits (the recorder, as `collect_recorded`
     * adds it): the engine bumps the notes revision and announces it. */
    addNote: (n: { channel: number; tick: number; length: number; key: number; velocity: number }) => {
      editPattern((pat) => {
        const note = { ...n, id: nextNoteId(), slide: false };
        pat.notes.push(note);
        growToNotes(pat, [note]);
      });
    },
    /** A MIDI keyboard is plugged in or out; the shell's poll notices within 2 s and tells the UI. */
    midiPlug: (name: string) => {
      s.midi.present = [...new Set([...s.midi.present, name])].sort();
      emit("midi-inputs-changed", null);
    },
    midiUnplug: (name: string) => {
      s.midi.present = s.midi.present.filter((n) => n !== name);
      emit("midi-inputs-changed", null);
    },
    /** The names of the MIDI inputs switched off. */
    midiDisabled: () => [...s.midi.disabled],
    /** The notes as the engine holds them. */
    notes: () => clone(s.project.pattern.notes),
    /** Live notes sounding now, as "channel:key". */
    live: () => [...s.live.keys()],
    autosave: () => {
      s.autosavedAt = Math.floor(Date.now() / 1000);
      emit("project-autosaved", s.autosavedAt);
    },
  };
}
