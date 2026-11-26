// A stand-in for the Tauri shell and the engine, injected into the web build before it loads
// (`page.addInitScript(installMockTauri, seed)`), so Playwright drives the real UI against an
// in-memory session that follows the shell's command contract (app/src-tauri/src/commands.rs,
// project_cmds.rs, project/src/session.rs). Playwright serialises the function, so it must not
// reference anything outside its own body.

import type { ArrangementPage, ArrangementReply, AudioContent, PatternContent } from "../src/api/arrangement";

/** A file in the mock's file system. */
export type MockFile =
  | { kind: "sample"; seconds?: number; wav?: number[] | null }
  | { kind: "folder" }
  | { kind: "text" }
  | { kind: "project"; project: MockProject };

/** A saved project, as the mock keeps it. */
export interface MockProject {
  name: string;
  bpm: number;
  timeSignature: { beats: number; unit: number };
  channels: MockChannel[];
  /** `nextNoteId`: the id the next note gets (model.rs `Pattern::next_note_id`); never lower than
   * one past the largest id. */
  pattern: { id: number; name: string; steps: number; swing: number; notes: MockNote[]; nextNoteId?: number };
  patterns?: MockProject["pattern"][];
  patternClips?: { id: number; pattern: number }[];
  playMode?: "pattern" | "song";
  arrangement?: { tracks: { id: number; name: string; color: number; mute: boolean }[];
    clips: { id: number; track: number; name: string; startTick: number; muted: boolean;
      content: PatternContent | AudioContent }[];
    audioAssets: ArrangementPage["audioAssets"][number][]; loopRegion: { startTick: number; endTick: number; enabled: boolean } | null };
  /** Embedded fixture WAV bytes keyed by asset ID, independent of the original paths. */
  arrangementMedia?: Record<number, number[]>;
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
  /** Encoded bytes exist but cannot be decoded. */
  decodeFailed?: boolean;
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
  order?: number;
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
  audioLog?: { id: number; atMs: number; level: "info" | "warn" | "error"; message: string }[];
  holdAudioLog?: boolean;
  holdProjectView?: boolean;
  /** Paths the mock's file system holds. */
  files?: Record<string, MockFile>;
  /** The project the engine holds at start; untitled and empty if absent. */
  project?: MockProject;
  /** An autosave an earlier session left behind. */
  recovery?: { savedAt: number; original: string | null; project: MockProject } | null;
  /** How long an export takes, in ms. */
  exportMs?: number;
  /** Pause after the first progress event until releaseExport is called. */
  holdExport?: boolean;
  /** The sampler's parameters (tests/fixtures/device-params.json); the fixture fills it in. */
  samplerParams?: { id: string; min: number; max: number; default: number }[];
  /** The synth's parameters (the same fixture). */
  synthParams?: { id: string; min: number; max: number; default: number }[];
  /** The MIDI keyboards plugged in at start, and the ones the person switched off (settings.json). */
  midi?: { inputs: string[]; disabled?: string[] };
  /** Every synth preset's 40 values in SPEC order (tests/fixtures/synth-presets.json). */
  synthPresets?: Record<string, number[]>;
  /** What `autosave_status` reports at start (autosave_notice.rs): autosave off for good in this
   * window, and a failing autosave. Both `null` if absent. */
  autosave?: { disabled?: string | null; failure?: string | null };
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
  /** model.rs `MAX_TICKS`: a note starts before it and lasts at most it. */
  const MAX_TICKS = MAX_STEPS * STEP_TICKS;
  /** model.rs `DEFAULT_KEY`: the key a channel's preview plays. */
  const DEFAULT_KEY = 60;
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
    p.pattern.nextNoteId = Math.max(p.pattern.nextNoteId ?? 1, next);
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

  type MidiPlayer = { down: Set<number>; held: Set<number>; pedal: boolean };
  type MidiHold = { owners: Set<string>; presses: number[] };
  const midiPlayer = (): MidiPlayer => ({ down: new Set(), held: new Set(), pedal: false });
  const midiPresent = [...new Set(seed.midi?.inputs ?? [])].sort();
  const midiDisabled = [...(seed.midi?.disabled ?? [])];

  // --- The mock's state, readable from the tests as `window.__grooviqMock`. ---
  const files = new Map<string, MockFile>(Object.entries(seed.files ?? {}));
  const calls: { cmd: string; args: unknown; at: number }[] = [];
  let releaseExport: (() => void) | null = null;
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
    /** Counts projects (Session::generation): New, Open and Restore move it on. */
    generation: 0,
    recording: false,
    metronome: false,
    /** The channel MIDI plays (`channel_select`). */
    selected: null as number | null,
    midi: {
      present: midiPresent,
      disabled: midiDisabled,
      players: new Map(midiPresent.filter((id) => !midiDisabled.includes(id)).map((id) => [id, midiPlayer()])),
      keys: new Map<number, MidiHold>(),
      // Object identity stands for Controller::channel_instance. Reopening can reuse numeric IDs.
      owed: new Map<MockChannel, Map<number, number[]>>(),
      stalled: false,
    },
    /** Live notes sounding now, "channel:key" -> velocity (`channel_note`). */
    live: new Map<string, number>(),
    /** Live presses handed out and not released yet: press -> "channel:key" (notes_cmds.rs
     * `channel_note`, project_cmds.rs `channel_preview`: a note-on returns its press, a release
     * names it). */
    presses: new Map<number, string>(),
    /** The presses `channel_preview` made, of those. */
    previewPresses: new Set<number>(),
    nextPress: 1,
    /** Notes being recorded, by press: where and how hard they started (recorder.rs pairs a
     * note-off with its own press's note-on). */
    recordOpen: new Map<number, { channel: number; key: number; tick: number; velocity: number; take: number }>(),
    /** The running take's number (engine/src/record.rs): one more after every take ends. */
    take: 0,
    /** Live frames are held back (a stalled stream), as `pauseLive` sets. */
    livePaused: false,
    /** `channel_param_live` calls, last value per "channel:param". */
    liveParams: new Map<string, number>(),
    dialogCalls: [] as { kind: string; options: unknown }[],
    /** Note changes for undo and redo (undo.rs): the notes and length before and after each. */
    history: { undo: [] as NoteChange[], redo: [] as NoteChange[], notes: 0, base: 0, log: [] as LengthEntry[], nextId: 0 },
    /** The saved output (`"default"`: follow the system) and speaker guard (settings.json). */
    audio: { output: "default", guard: true },
    /** What autosave owes the person (autosave_notice.rs `AutosaveStatus`). */
    autosave: { disabled: seed.autosave?.disabled ?? null, failure: seed.autosave?.failure ?? null },
  };

  type History = typeof s.history;
  const emptyHistory = (): History => ({ undo: [], redo: [], notes: 0, base: 0, log: [], nextId: 0 });
  type PatternEntry = { pattern: Project["pattern"]; revision: number; history: History };
  const patternBank = new Map<number, PatternEntry>();
  let patternsRevision = 0;
  let historyOrder = 0;
  let arrangementRevision = 0;
  type TrimChange = { channel: number; order: number; stamp: string;
    before: [number | undefined, number | undefined]; after: [number | undefined, number | undefined] };
  let trimUndo: TrimChange[] = [];
  let trimRedo: TrimChange[] = [];
  type Arrangement = NonNullable<Project["arrangement"]>;
  type AddedPattern = { initial: Project["pattern"]; dormant?: PatternEntry };
  type AudioSource = { bytes: number[]; data: DataView; rate: number; frames: number; channels: number; fromWorker: boolean };
  type ClipChange = { order: number; before: Arrangement; after: Arrangement; addedPatterns: AddedPattern[];
    media?: { asset: number; before: AudioSource | undefined; after: AudioSource } };
  const audioSources = new Map<number, AudioSource>();
  const pendingMedia = new Map<number, number>();
  let nextMediaRequest = 1;
  type MediaHold = { promise: Promise<boolean>; release: (decode: boolean) => void };
  const mediaHolds = new Map<string, MediaHold>();
  const waveformHolds = new Map<number, MediaHold>();
  const holdMedia = <K,>(holds: Map<K, MediaHold>, key: K) => {
    if (holds.has(key)) throw new Error("Media is already held");
    let release: MediaHold["release"] = () => {};
    const promise = new Promise<boolean>(resolve => { release = resolve; });
    holds.set(key, { promise, release });
  };
  const releaseMedia = <K,>(holds: Map<K, MediaHold>, key: K, decode = true) => {
    holds.get(key)?.release(decode);
    holds.delete(key);
  };
  let clipUndo: ClipChange[] = [], clipRedo: ClipChange[] = [];
  let workspaceRefusal: string | null = null;
  const workspaceTrace: { command: string; generation: number; revision: number; status: string; effect?: unknown }[] = [];

  const initialisePatterns = () => {
    // Session keeps its structural counter across replacement; generation fences its scope.
    patternBank.clear();
    for (const pattern of s.project.patterns ?? [s.project.pattern]) {
      const normalized = normalise({ ...s.project, pattern: clone(pattern) }).pattern;
      patternBank.set(pattern.id, { pattern: normalized, revision: s.notesRevision, history: emptyHistory() });
    }
    if (!patternBank.has(s.project.pattern.id)) patternBank.set(s.project.pattern.id, { pattern: s.project.pattern, revision: s.notesRevision, history: s.history });
  };
  initialisePatterns();
  const saveActivePattern = () => patternBank.set(s.project.pattern.id, { pattern: s.project.pattern, revision: s.notesRevision, history: s.history });
  const allPatterns = () => { saveActivePattern(); return [...patternBank.values()].map((entry) => clone(entry.pattern)); };

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
    arrangementRevision += 1;
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
  const missing = (c: MockChannel) => c.sample !== null && (c.embedded === false || c.decodeFailed === true);
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
        sampleNameBasis: c.sample === null ? null : c.embedded === false ? "storedReference" : "availableFile",
        sampleReference: c.sample,
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
      projectCanUndo: noteHistories().some((history) => history.undo.length > 0) || trimUndo.length > 0 || clipUndo.length > 0,
      projectCanRedo: noteHistories().some((history) => history.redo.length > 0) || trimRedo.length > 0 || clipRedo.length > 0,
      generation: s.generation,
      patternRevision: patternsRevision,
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
    } else if (growth && next.steps !== prev.steps) {
      // session.rs: notes resubmitted as they were, growing a pattern shortened by hand, are
      // a step holding only that growth.
      pushHistory({ before: [], after: [] }, prev.steps, growth);
    }
    notesChanged();
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
    // Notes contain only scalar fields; copy each snapshot without a JSON round trip.
    for (const n of next.notes) {
      const o = was.get(n.id);
      was.delete(n.id);
      if (!o) after.push({ ...n });
      else if (!sameNote(o, n)) {
        before.push({ ...o });
        after.push({ ...n });
      }
    }
    for (const o of was.values()) before.push({ ...o });
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
    for (const l of h.log) if ("owner" in l && l.owner !== null && dropped.has(l.owner)) l.owner = null;
    if (dropped.size) compactLog();
    const histories = [h, ...[...patternBank.entries()].filter(([id]) => id !== s.project.pattern.id).map(([,e]) => e.history)];
    while (histories.reduce((n,h) => n+h.undo.length+h.redo.length,0) > MAX_UNDO || histories.reduce((n,h) => n+h.notes,0) > MAX_UNDO_NOTES) {
      const choices = histories.flatMap((history) => [...history.undo, ...history.redo].map((step) => ({ history, step })));
      choices.sort((a,b) => (a.step.order ?? 0)-(b.step.order ?? 0));
      const oldest=choices[0]; if (!oldest) break;
      const active=oldest.history.undo.includes(oldest.step);
      const list=active ? oldest.history.undo : oldest.history.redo;
      list.splice(list.indexOf(oldest.step),1); oldest.history.notes-=sizeOf(oldest.step);
      if (active) { for (const entry of oldest.history.log) if ("owner" in entry && entry.owner===oldest.step.id) entry.owner=null; }
      else oldest.history.log=oldest.history.log.filter((entry) => !("owner" in entry) || entry.owner!==oldest.step.id);
      s.history=oldest.history; compactLog(); s.history=h;
    }
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
  const foldLog = (h: History = s.history) => {
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
  const compactLog = (h: History = s.history) => {
    const log: LengthEntry[] = [];
    for (const l of h.log) appendLength(log, l);
    h.log = log;
    foldLog(h);
  };
  const historyEmpty = () => !s.history.undo.length && !s.history.redo.length;
  /** undo.rs `NoteHistory::close`: every open take step closes. */
  const closeTakes = (h: History = s.history) => {
    for (const c of h.undo) c.take = null;
  };
  const noteHistories = () => [s.history, ...[...patternBank.entries()]
    .filter(([id]) => id !== s.project.pattern.id).map(([, entry]) => entry.history)];
  const pruneWorkspaceHistory = () => {
    const histories = noteHistories();
    while (histories.reduce((count, history) => count + history.undo.length + history.redo.length, 0) + trimUndo.length + trimRedo.length + clipUndo.length + clipRedo.length > MAX_UNDO) {
      const note = histories.flatMap((history) => [...history.undo,...history.redo].map((step) => ({ history, step })))
        .sort((a,b) => (a.step.order ?? 0)-(b.step.order ?? 0))[0];
      const trim = [...trimUndo,...trimRedo].sort((a,b) => a.order-b.order)[0];
      const clip = [...clipUndo,...clipRedo].sort((a,b) => a.order-b.order)[0];
      if (clip && (!note || clip.order < (note.step.order ?? 0)) && (!trim || clip.order < trim.order)) {
        clipUndo = clipUndo.filter(entry => entry !== clip); clipRedo = clipRedo.filter(entry => entry !== clip);
      } else if (trim && (!note || trim.order < (note.step.order ?? 0))) {
        trimUndo = trimUndo.filter((entry) => entry !== trim); trimRedo = trimRedo.filter((entry) => entry !== trim);
      } else if (note) {
        const active = note.history.undo.includes(note.step); const list = active ? note.history.undo : note.history.redo;
        list.splice(list.indexOf(note.step),1); note.history.notes -= sizeOf(note.step);
        if (active) { for (const entry of note.history.log) if ("owner" in entry && entry.owner === note.step.id) entry.owner = null; }
        else note.history.log = note.history.log.filter((entry) => !("owner" in entry) || entry.owner !== note.step.id);
        compactLog(note.history);
      } else break;
    }
  };
  const clearRedo = (h: History) => {
    const gone = new Set(h.redo.map((change) => change.id));
    for (const change of h.redo) h.notes -= sizeOf(change);
    h.redo = [];
    if (gone.size) {
      h.log = h.log.filter((entry) => !("owner" in entry) || entry.owner === null || !gone.has(entry.owner));
      compactLog(h);
    }
  };
  const clearOtherNoteRedo = () => {
    for (const h of noteHistories()) if (h !== s.history) clearRedo(h);
    clipRedo = [];
  };
  const clearWorkspaceRedo = () => {
    for (const h of noteHistories()) clearRedo(h);
    trimRedo = [];
    clipRedo = [];
  };
  /** undo.rs `NoteHistory::add`: a note change clears redo (and its growth leaves the log); the
   * first step starts the log at `steps`. */
  const addHistory = (c: NoteChange, take: number | null, steps: number) => {
    clearOtherNoteRedo();
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
    c.order = historyOrder++;
    h.notes += sizeOf(c);
    h.undo.push(c);
    trimHistory(); pruneWorkspaceHistory();
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
    clearOtherNoteRedo();
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
        trimHistory(); pruneWorkspaceHistory();
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
  const lengthAfter = (undo: boolean, h: History = s.history) => {
    const live = new Set(h.undo.map((c) => c.id!));
    if (undo) live.delete(h.undo[h.undo.length - 1]?.id ?? -1);
    else if (h.redo.length) live.add(h.redo[h.redo.length - 1].id!);
    return playLength(h.base, h.log, (id) => live.has(id));
  };
  /** Undo or redo (session.rs `undo_notes`): the notes go back by id, the length is the log
   * played with the steps in effect; the project is changed and the UI fetches the notes. */
  const stepPatternHistory = (pattern: number, undo: boolean) => {
    saveActivePattern();
    const entry = patternBank.get(pattern);
    if (!entry) throw "Pattern removed";
    const h = entry.history;
    if (!(undo ? h.undo : h.redo).length) return false;
    const steps = lengthAfter(undo, h);
    const change = (undo ? h.undo : h.redo).pop()!;
    const pat = entry.pattern;
    const [out, put] = undo ? [change.after, change.before] : [change.before, change.after];
    const ids = new Set([...out, ...put].map((note) => note.id));
    pat.notes = [...pat.notes.filter((note) => !ids.has(note.id)), ...clone(put)].sort((a,b) => (a.id ?? 0)-(b.id ?? 0));
    pat.steps = steps;
    for (const history of noteHistories()) closeTakes(history);
    change.take = null;
    (undo ? h.redo : h.undo).push(change);
    if (pattern === s.project.pattern.id) { notesChanged(); entry.revision = s.notesRevision; }
    else entry.revision++;
    changed(); return true;
  };
  const stepHistory = (undo: boolean) => {
    if (!stepPatternHistory(s.project.pattern.id, undo)) return null;
    return { revision: s.notesRevision, addedIds: [], ignoredIds: [], reload: true, view: view() };
  };
  const clearHistory = () => {
    s.history.undo = [];
    s.history.redo = [];
    s.history.notes = 0;
    s.history.base = 0;
    s.history.log = [];
  };
  /** model.rs `Pattern::take_note_id`: the pattern's next id, never handed out before (an
   * undone or deleted note's id is not reused). */
  const takeNoteId = (pat: Project["pattern"]) => {
    const id = Math.max(1, pat.nextNoteId ?? 1);
    if (id >= 0xffffffff) throw "This pattern has used up its note ids. Save the project and open it again to renumber its notes.";
    pat.nextNoteId = id + 1;
    return id;
  };
  const noteBase = (pat: Project["pattern"], channel: number, tick: number, velocity: number): MockNote => ({
    id: takeNoteId(pat),
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
  /** Clamps a note's values as notes.rs `clamp_note` does; `true` if any changed. */
  const clampNote = (n: MockNote) => {
    const before = JSON.stringify(n);
    n.tick = clamp(Math.trunc(n.tick), 0, MAX_TICKS - 1);
    n.length = clamp(Math.trunc(n.length ?? 1), 1, MAX_TICKS);
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
  // #181 sampler-only capability bookkeeping; no legacy note/history semantics here.
  let trimEpoch = 0n;
  const trimIdentities = new Map<number, { sample: bigint; edit: bigint }>();
  const trimInstances = new WeakMap<MockChannel, bigint>();
  let trimRefusal: string | null = null;
  let trimResponseHold: { promise: Promise<void>; release: () => void } | null = null;
  const invalidateTrim = (channel: number, sample = false) => {
    const state = trimIdentities.get(channel) ?? {sample:0n,edit:0n};
    state.edit = ++trimEpoch;
    if (sample) state.sample = trimEpoch;
    trimIdentities.set(channel,state);
    trimUndo = trimUndo.filter((entry) => entry.channel !== channel);
    trimRedo = trimRedo.filter((entry) => entry.channel !== channel);
  };
  const trimView = (channel: number) => {
    const c = s.project.channels[channelIndex(channel)];
    if (c.kind === "synth") throw "Not a sampler channel";
    if (!trimInstances.has(c)) trimInstances.set(c,++trimEpoch);
    const identity = trimIdentities.get(channel) ?? {sample:0n,edit:0n};
    const stamp = `${s.generation}:${trimInstances.get(c)}:${identity.sample}:${identity.edit}`;
    const inverse = [...trimUndo].reverse().find((entry) => entry.channel === channel && entry.stamp === stamp);
    return { stamp,start:c.sampler.start ?? 0,end:c.sampler.end ?? 1,
      frames:c.sample === null || missing(c) ? null : c.frames ?? Math.round(48000*sampleSeconds(c.sample)),
      undo:inverse?.stamp === stamp ? String(inverse.order) : null };
  };
  const advanceTrimIdentity = (channel: number) => {
    const identity = trimIdentities.get(channel) ?? { sample: 0n, edit: 0n };
    identity.edit = ++trimEpoch; trimIdentities.set(channel, identity);
  };
  const putTrimPair = (channel: number, pair: TrimChange["before"]) => {
    const c = s.project.channels[channelIndex(channel)];
    for (const [index, key] of ["start", "end"].entries()) {
      const value = pair[index]; if (value === undefined) delete c.sampler[key]; else c.sampler[key] = value;
    }
  };
  const moveTrimHistory = (entry: TrimChange, undo: boolean) => {
    if (trimView(entry.channel).stamp !== entry.stamp) throw "Sample or trim changed; reload trim";
    putTrimPair(entry.channel, undo ? entry.before : entry.after);
    // History movement must not call invalidateTrim: that removes the entry being moved.
    advanceTrimIdentity(entry.channel); entry.stamp = trimView(entry.channel).stamp;
    if (undo) { trimUndo = trimUndo.filter((item) => item !== entry); trimRedo.push(entry); }
    else { trimRedo = trimRedo.filter((item) => item !== entry); trimUndo.push(entry); }
    trimUndo.sort((a,b) => a.order-b.order); trimRedo.sort((a,b) => b.order-a.order);
    changed();
  };
  const trimMutation = async (channel: number, stamp: string, token?: string) => {
    let reply;
    try {
      const before = trimView(channel);
      if (before.stamp !== stamp) throw "Sample or trim changed; reload trim";
      if (trimRefusal !== null) { const message=trimRefusal;trimRefusal=null;throw message; }
      const c=s.project.channels[channelIndex(channel)];
      if (token === undefined) {
        if (!before.frames) throw "No decoded sample to repair";
        if (before.start === 0 && before.end === 1) return {status:"applied",state:before,changed:false};
        const original: TrimChange["before"] = [c.sampler.start,c.sampler.end];
        const candidate = clone(s.project); candidate.channels[channelIndex(channel)].sampler = { ...c.sampler, start: 0, end: 1 };
        workspaceAdmit(candidate, false);
        clearWorkspaceRedo();
        trimUndo = trimUndo.filter((entry) => entry.channel !== channel);
        putTrimPair(channel, [0,1]); advanceTrimIdentity(channel);
        trimUndo.push({ channel, order: historyOrder++, stamp: trimView(channel).stamp, before: original, after: [0,1] });
        trimHistory(); pruneWorkspaceHistory(); changed();
      } else {
        const inverse = trimUndo.find((entry) => entry.channel === channel && String(entry.order) === token && entry.stamp === stamp);
        if (!inverse) throw "Undo reset is no longer available";
        admitTrimInverse(inverse, true, false); moveTrimHistory(inverse, true);
      }
      reply={status:"applied",state:trimView(channel),changed:true};
    } catch (error) {
      let state=null;try {state=trimView(channel);} catch { /* removed target */ }
      reply={status:"refused",state,message:String(error)};
    }
    if (trimResponseHold) await trimResponseHold.promise;
    return reply;
  };

  const applyEdit = (e: Edit): boolean => {
    const p = s.project;
    switch (e.kind) {
      case "removeChannel": {
        const i = channelIndex(e.channel);
        editPattern((pat) => {
          pat.notes = pat.notes.filter((n) => n.channel !== e.channel);
        });
        invalidateTrim(Number(e.channel));
        trimIdentities.delete(Number(e.channel));
        p.channels.splice(i, 1);
        clearHistory();
        // The engine drops the channel's live notes with it (Task 5).
        const prefix = `${String(e.channel)}:`;
        for (const key of s.live.keys()) if (key.startsWith(prefix)) s.live.delete(key);
        for (const [press, key] of s.presses) {
          if (!key.startsWith(prefix)) continue;
          s.presses.delete(press);
          s.previewPresses.delete(press);
          s.recordOpen.delete(press);
        }
        if (s.selected === e.channel) midiDiscardKeys();
        return true;
      }
      case "setInstrumentParam": {
        const c = p.channels[channelIndex(e.channel)];
        const { spec, value } = instrumentParam(c, e.param, e.value);
        const store = c.kind === "synth" ? (c.synth ??= {}) : c.sampler;
        if (store[spec[0]] === value) return false;
        store[spec[0]] = value;
        if (c.kind !== "synth" && (spec[0] === "start" || spec[0] === "end")) invalidateTrim(c.id);
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
        if (spec[0] === "start" || spec[0] === "end") invalidateTrim(c.id);
        return true;
      }
      case "toggleStep": {
        const tick = stepTick(e.channel, e.step);
        return editPattern((pat) => {
          const before = pat.notes.length;
          pat.notes = pat.notes.filter((n) => !(n.channel === e.channel && n.tick === tick));
          if (pat.notes.length === before) pat.notes.push(noteBase(pat, e.channel as number, tick, DEFAULT_VELOCITY));
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
          if (!found) pat.notes.push(noteBase(pat, e.channel as number, tick, velocity));
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
  let unconfirmNextSave = false;
  // project/file.rs::entry_name. Saving assigns ZIP entry names only to the saved copy.
  const saveArrangementMedia = (project: Project) => {
    if (!project.arrangement?.audioAssets.length) return;
    const entries: { bytes?: number[]; name: string }[] = [];
    const entryFor = (path: string, bytes?: number[]) => {
      const shared = bytes && entries.find(entry => entry.bytes && sameAudioBytes(entry.bytes, bytes));
      if (shared) return shared.name;
      const name = Array.from(path.split(/[/\\]/).at(-1) ?? "", char =>
        /[\p{Cc}:*?"<>|]/u.test(char) ? "_" : char,
      ).slice(0, 120).join("").replace(/^[. ]+|[. ]+$/g, "") || "sample";
      const entry = { bytes, name: `samples/${entries.length + 1}-${name}` };
      entries.push(entry);
      return entry.name;
    };
    for (const channel of project.channels) {
      if (channel.sample === null || channel.embedded === false) continue;
      const file = files.get(channel.sample);
      entryFor(channel.sample, file?.kind === "sample" ? file.wav ?? undefined : undefined);
    }
    project.arrangementMedia = {};
    for (const asset of project.arrangement.audioAssets) {
      const source = audioSources.get(asset.id);
      if (!source) continue;
      asset.sample.file = entryFor(asset.sample.path, source.bytes);
      project.arrangementMedia[asset.id] = source.bytes.slice();
    }
  };
  const saveTo = (target: string | null) => {
    const path = target === null ? s.path : withExtension(target, "grooviq", false);
    if (path === null) throw "Choose where to save this project.";
    if (files.get(path)?.kind === "project") s.backups.set(path, (s.backups.get(path) ?? 0) + 1);
    const saved = clone({ ...s.project, patterns: allPatterns() });
    saveArrangementMedia(saved);
    files.set(path, { kind: "project", project: saved });
    s.path = path;
    if (unconfirmNextSave) {
      unconfirmNextSave = false;
      if (!dirty()) changed();
      throw "The file was written, but directory durability could not be confirmed. Keep recovery copies and try saving again.";
    }
    markClean();
    return view();
  };
  const openProject = (p: Project, path: string | null, unsaved: boolean) => {
    stopTransport();
    // Like Controller::load_state, replacing the graph cuts live notes without sending offs.
    s.live.clear();
    s.presses.clear();
    s.previewPresses.clear();
    midiDiscardKeys();
    s.generation += 1;
    trimIdentities.clear(); trimUndo = []; trimRedo = []; clipUndo = []; clipRedo = []; arrangementRevision = 0; workspaceRefusal = null;
    s.project = normalise(clone(p));
    pendingMedia.clear();
    openArrangementMedia();
    clearHistory();
    notesChanged();
    initialisePatterns();
    s.path = path;
    markClean();
    if (unsaved) changed();
    const warnings = p.channels
      .filter((c) => missing(c))
      .map((c) => `Channel "${c.name}": its sample is missing from the project file. Relink it to hear it.`);
    for (const asset of s.project.arrangement?.audioAssets ?? []) {
      const source = audioSources.get(asset.id);
      if (!source) warnings.push(`Audio asset ${asset.id}: its sample is missing from the project file. Relink it to hear it.`);
      else if (source.rate !== asset.sourceRate || source.frames !== asset.sourceFrames) {
        warnings.push(`Audio asset ${asset.id}: decoded geometry differs from saved source metadata; saved trim duration is preserved.`);
      }
    }
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
    for (const press of [...s.recordOpen.keys()]) closeRecorded(press, end);
    s.recordOpen.clear();
    // The take ends with the transport: the next one has the next number.
    if (s.recording && s.transport.playing) s.take += 1;
    s.transport = { playing: false, startMs: 0, startTicks: 0, held: null };
  };

  // --- The live stream: a fake WebSocket that sends v2 frames at 60 Hz. ---
  const LIVE_PORT = 47999;
  let synthObservation: number[] | null = null;
  type SynthMotionSnapshot = { ampStage: number; filterStage: number; lfoPhase: number; ampLevel: number; filterLevel: number; lfoValue: number; voices: number; frames?: number };
  let synthMotion: SynthMotionSnapshot | null = null;
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
  /** The ack of a note request made for a project that was replaced since (session.rs
   * `refused_ack`): nothing changes, nothing is announced, the UI fetches the notes. */
  /** session.rs `SessionError::ProjectReplaced`: a channel request made in a replaced project. */
  const sameProject = (generation: unknown) => {
    if (generation !== s.generation) throw "The project changed before this finished, so it was not applied.";
  };
  const refusedAck = () => ({ revision: s.notesRevision, addedIds: [], ignoredIds: [], reload: true, view: view() });
  const editNotes = (e: {
    generation: number;
    patternId: number;
    baseRevision: number;
    add: Omit<WireNote, "id">[];
    update: WireNote[];
    delete: number[];
  }) => {
    if (e.generation !== s.generation || e.patternId !== s.project.pattern.id) return refusedAck();
    const stale = e.baseRevision !== s.notesRevision;
    if (stale) throw "The pattern changed before this note edit arrived. Reload its notes.";
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
      for (const a of e.add) {
        if (pat.notes.length >= MAX_NOTES) throw `A pattern holds at most ${MAX_NOTES} notes.`;
        channelIndex(a.channel);
        const n: MockNote = { ...a, id: takeNoteId(pat) };
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
  /** session.rs `disarm`: recording switches itself off because `dropped` recorded notes did not
   * fit (the note cap, or the pattern's used-up note ids); the notes held now are refused too,
   * and counted with them. */
  const stopsItself = (dropped: number, reason: "noteCap" | "noteIds" = "noteCap") => {
    const lost = dropped + s.recordOpen.size;
    s.recordOpen.clear();
    if (s.recording && s.transport.playing) s.take += 1;
    s.recording = false;
    const notes = lost === 1 ? "1 recorded note was" : `${lost} recorded notes were`;
    emit("recording-stopped", {
      reason,
      dropped: lost,
      message:
        reason === "noteIds"
          ? `Recording stopped: this pattern has used up its note ids (save and reopen the project to renumber them). ${notes} not added.`
          : `Recording stopped: this pattern holds ${MAX_NOTES} notes, the most a pattern can hold. ${notes} not added.`,
    });
  };
  /** The recorder (recorder.rs): a press's note-on and note-off become a note at the tick it
   * was heard, in the loop; an open note ends when recording stops. Notes are overdubbed. */
  const closeRecorded = (press: number, now: number) => {
    const open = s.recordOpen.get(press);
    if (!open) return;
    s.recordOpen.delete(press);
    const { channel, key: note } = open;
    if (!s.project.channels.some((c) => c.id === channel)) return;
    // session.rs `collect_recorded`: what fits is the note cap and the ids left (u32::MAX is
    // never handed out); a note that does not fit is not added, and recording switches off
    // and says why.
    const pat = s.project.pattern;
    const room = MAX_NOTES - pat.notes.length;
    const ids = 0xffff_ffff - Math.max(1, pat.nextNoteId ?? 1);
    if (Math.min(room, ids) < 1) {
      stopsItself(1, ids < room ? "noteIds" : "noteCap");
      return;
    }
    const loop = s.project.pattern.steps * STEP_TICKS;
    editPattern((pat) => {
      const n: MockNote = {
        id: takeNoteId(pat),
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
  const checkStamp = (origin: unknown) => {
    const stamp = origin as { generation?: number; patternId?: number; revision?: number } | undefined;
    if (stamp && stamp.generation !== s.generation) throw "The project changed before this finished, so it was not applied.";
    if (!stamp || stamp.patternId !== s.project.pattern.id || stamp.revision !== s.notesRevision) {
      throw "The pattern changed before this gesture finished. Reload its notes.";
    }
  };
  const patternsView = () => { saveActivePattern(); return { generation: s.generation, revision: patternsRevision, arrangementRevision,
    selected: s.project.pattern.id, patterns: [...patternBank.entries()].map(([id, entry]) => ({ id, name: entry.pattern.name, revision: entry.revision, steps: entry.pattern.steps })), view: view() }; };
  const managePattern = (a: Record<string, unknown>) => {
    type Request = { kind: string; pattern?: number; name?: string; clip?: number };
    const edit = a.edit as Request;
    const settlement = { recordingDisarmed: false, waiting: false };
    const refuse = (reason: string, message: string) => ({ status: "refused", reason, message, state: patternsView(), settlement });
    if (a.generation !== s.generation) return refuse("projectReplaced", "The project changed before this finished, so it was not applied.");
    if (a.revision !== patternsRevision) return refuse("staleRevision", "Patterns changed. Reload the pattern list.");
    saveActivePattern();
    const disarmForSettlement = () => {
      for (const press of [...s.recordOpen.keys()]) closeRecorded(press, ticksNow());
      s.recording = false; settlement.recordingDisarmed = true; settlement.waiting = true;
      return refuse("recordingSettling", "Recording is ending. Wait for its final notes, then retry.");
    };
    if (edit.kind === "settle") {
      if (s.recording) return disarmForSettlement();
      return { status: "applied", state: patternsView(), settlement };
    }
    const candidate = new Map([...patternBank].map(([id, entry]) => [id, { pattern: clone(entry.pattern), revision: entry.revision, history: entry.history }]));
    let nextId = s.project.nextId;
    const allocate = () => {
      while (nextId < 0xffffffff && (candidate.has(nextId) || s.project.channels.some((c) => c.id === nextId) || s.project.patternClips?.some((c) => c.id === nextId) || s.project.arrangement?.tracks.some(t => t.id === nextId) || s.project.arrangement?.clips.some(c => c.id === nextId) || s.project.arrangement?.audioAssets.some(a => a.id === nextId))) nextId++;
      if (nextId >= 0xffffffff) return null;
      return nextId++;
    };
    let selected = s.project.pattern.id;
    const clips = clone(s.project.patternClips ?? []);
    if (edit.kind === "delete" && s.project.arrangement?.clips.some(c => c.content.type === "pattern" && c.content.pattern === edit.pattern)) return refuse("referencedPattern", "This pattern is still used by a clip.");
    if (["select", "delete", "rename", "duplicate"].includes(edit.kind) && !candidate.has(edit.pattern!)) return refuse("unknownPattern", `Pattern ${edit.pattern} no longer exists. Reload the pattern list.`);
    if ((edit.kind === "select" && edit.pattern === selected) || (edit.kind === "rename" && candidate.get(edit.pattern!)!.pattern.name === edit.name)) return { status:"applied", state:patternsView(), settlement };
    if (edit.kind === "create" || edit.kind === "duplicate" || edit.kind === "makeUnique") {
      let source = edit.pattern;
      let clip: { id: number; pattern: number } | undefined;
      if (edit.kind === "makeUnique") { clip = clips.find((c) => c.id === edit.clip); if (!clip) return refuse("unknownClip", "Clip no longer exists."); source = clip.pattern; }
      if (edit.kind !== "create" && !candidate.has(source!)) return refuse("unknownPattern", `Pattern ${source} no longer exists. Reload the pattern list.`);
      const id = allocate(); if (id === null) return refuse("idExhausted", "Project ids are exhausted.");
      const pattern = edit.kind === "create" ? { id, name: edit.name!, steps: 16, swing: 0, notes: [], nextNoteId: 1 } : { ...clone(candidate.get(source!)!.pattern), id };
      candidate.set(id, { pattern, revision: 1, history: emptyHistory() });
      if (clip) clip.pattern = id;
    } else if (edit.kind === "rename") candidate.get(edit.pattern!)!.pattern.name = edit.name!;
    else if (edit.kind === "select") selected = edit.pattern!;
    else if (edit.kind === "delete") {
      if (candidate.size === 1) return refuse("lastPattern", "Keep at least one pattern.");
      if (clips.some((c) => c.pattern === edit.pattern)) return refuse("referencedPattern", "Remove this pattern's playlist clips before deleting it.");
      candidate.delete(edit.pattern!);
      if (selected === edit.pattern) selected = candidate.keys().next().value!;
    } else return refuse("validation", "Unknown pattern operation.");
    // Match writer admission before committing any candidate, including the allocator.
    if (new TextEncoder().encode(JSON.stringify({ ...s.project, patterns: [...candidate.values()].map((e) => e.pattern) })).byteLength > 64 * 1024 * 1024) return refuse("fileBudget", "The project is too big to save.");
    if (selected !== s.project.pattern.id && s.recording) return disarmForSettlement();
    if (selected !== s.project.pattern.id) candidate.get(selected)!.revision++;
    patternBank.clear(); for (const [id, entry] of candidate) patternBank.set(id, entry);
    const current = patternBank.get(selected)!;
    s.project.pattern = current.pattern; s.notesRevision = current.revision; s.history = current.history;
    s.project.nextId = nextId; s.project.patternClips = clips;
    if (edit.kind === "delete" || edit.kind === "makeUnique") { trimUndo = []; trimRedo = []; }
    patternsRevision++; changed();
    return { status: "applied", state: patternsView(), settlement };
  };

  const nextChannelIdentity = () => {
    let id=s.project.nextId;
    while (id < 0xffffffff && (s.project.channels.some((c) => c.id===id) || patternBank.has(id) || s.project.patternClips?.some((c) => c.id===id) || s.project.arrangement?.tracks.some(t => t.id===id) || s.project.arrangement?.clips.some(c => c.id===id) || s.project.arrangement?.audioAssets.some(a => a.id===id))) id++;
    if (id>=0xffffffff) throw "Project ids are exhausted.";
    return id;
  };
  const requireEmptyArrangement = () => {
    const arrangement = s.project.arrangement;
    if ((s.project.patternClips?.length ?? 0) || (arrangement && (arrangement.tracks.length || arrangement.clips.length || arrangement.audioAssets.length || arrangement.loopRegion))) {
      throw "Nonempty arrangement is outside the reduced workspace fixture";
    }
  };
  const workspaceStamp = (args: Record<string, unknown>) => {
    if (args.generation !== s.generation) throw "Project replaced; reload arrangement";
    if (args.revision !== arrangementRevision) throw "Stale arrangement revision; reload arrangement";
    if (arrangementRevision >= Number.MAX_SAFE_INTEGER) throw "Arrangement revisions exhausted; reopen project";
  };
  const workspaceAdmit = (candidate: Project, scoped = true, patterns = allPatterns()) => {
    if (scoped && workspaceRefusal !== null) { const error = workspaceRefusal; workspaceRefusal = null; throw error; }
    if (new TextEncoder().encode(JSON.stringify({ ...candidate, patterns })).length > 64 * 1024 * 1024) throw "The project is too big to save.";
  };
  const admitTrimInverse = (entry: TrimChange, undo: boolean, scoped = true) => {
    if (trimView(entry.channel).stamp !== entry.stamp) throw "Sample or trim changed; reload trim";
    const candidate = clone(s.project); const c = candidate.channels[channelIndex(entry.channel)];
    for (const [index, key] of ["start", "end"].entries()) {
      const value = (undo ? entry.before : entry.after)[index];
      if (value === undefined) delete c.sampler[key]; else c.sampler[key] = value;
    }
    workspaceAdmit(candidate, scoped);
  };
  const emptyArrangement = (): Arrangement => ({ tracks: [], clips: [], audioAssets: [], loopRegion: null });
  const mediaWarnings = () => (s.project.arrangement?.audioAssets ?? []).flatMap(asset => {
    const source = audioSources.get(asset.id);
    if (!source) return [`Audio asset ${asset.id} is missing; relink it to hear its clips.`];
    if (source.rate !== asset.sourceRate || source.frames !== asset.sourceFrames) {
      return [`Audio asset ${asset.id}: decoded geometry differs from saved provenance; trim duration is preserved and unavailable decoded frames are silent.`];
    }
    return [];
  });
  const arrangementReply = (request: number | null, changed: boolean, createdIds: number[], message: string | null = null): ArrangementReply => ({
    status: message === null ? "applied" : "refused", generation: s.generation, revision: arrangementRevision,
    request, createdIds, changed, reload: message !== null, message, warnings: mediaWarnings(),
  });
  const workspaceView = (args: Record<string, unknown>) => {
    const limit = Number(args.limit); if (limit < 1 || limit > 1024) throw "Read 1..1024 clips per page";
    if (s.project.patternClips?.length) requireEmptyArrangement();
    const arrangement = s.project.arrangement ?? emptyArrangement();
    const offset = Number(args.offset);
    return { generation: s.generation, revision: arrangementRevision, offset, total: arrangement.clips.length,
      clips: clone(arrangement.clips.slice(offset, offset + limit)), tracks: clone(arrangement.tracks), audioAssets: clone(arrangement.audioAssets), loopRegion: arrangement.loopRegion, playMode: s.project.playMode ?? "pattern",
      songError: null, pendingRequests: [...pendingMedia.values()], warnings: mediaWarnings() };
  };
  const workspaceMode = (args: Record<string, unknown>) => {
    try {
      workspaceStamp(args);
      const mode = args.mode as "pattern" | "song"; const changedMode = (s.project.playMode ?? "pattern") !== mode;
      if (changedMode) { workspaceAdmit({ ...s.project, playMode: mode }); s.project.playMode = mode; changed(); }
      return arrangementReply(null, changedMode, []);
    } catch (error) { return arrangementReply(null, false, [], String(error)); }
  };
  const stepProjectHistory = (args: Record<string, unknown>) => {
    try {
      if (s.project.patternClips?.length) requireEmptyArrangement();
      workspaceStamp(args); saveActivePattern();
      const undo = args.undo === true;
      const notes = [...patternBank].flatMap(([pattern, entry]) => {
        const next = (undo ? entry.history.undo : entry.history.redo).at(-1);
        return next ? [{ pattern, change: next }] : [];
      }).sort((a,b) => (a.change.order ?? 0)-(b.change.order ?? 0));
      const note = undo ? notes.at(-1) : notes[0];
      const trim = (undo ? trimUndo : trimRedo).at(-1);
      const clip = (undo ? clipUndo : clipRedo).at(-1);
      const earlier = (a: number, b: number) => undo ? a > b : a < b;
      let effect: { kind: "empty" } | { kind: "arrangement" } | { kind: "notes"; pattern: number } | { kind: "samplerTrim"; channel: number } = { kind: "empty" };
      if (clip && (!note || earlier(clip.order, note.change.order ?? 0)) && (!trim || earlier(clip.order, trim.order))) {
        const candidate = clone(s.project); candidate.arrangement = clone(undo ? clip.before : clip.after);
        const bank = new Map(patternBank);
        for (const added of clip.addedPatterns) {
          const id = added.initial.id;
          if (undo) {
            if (!bank.delete(id)) throw "Unique source no longer exists";
          } else {
            if (bank.has(id)) throw "Unique source already exists";
            const floor = added.dormant;
            const pattern = clone(added.initial);
            pattern.nextNoteId = Math.max(pattern.nextNoteId ?? 1, floor?.pattern.nextNoteId ?? 1);
            bank.set(id, { pattern, revision: (floor?.revision ?? 1) + 1, history: floor?.history ?? emptyHistory() });
          }
        }
        const selected = bank.get(candidate.pattern.id) ?? bank.values().next().value;
        if (!selected) throw "No pattern remains";
        candidate.pattern = selected.pattern;
        workspaceAdmit(candidate, true, [...bank.values()].map(entry => entry.pattern));
        if (clip.addedPatterns.length) {
          for (const added of clip.addedPatterns) {
            if (undo) added.dormant = patternBank.get(added.initial.id);
          }
          patternBank.clear();
          for (const [id, entry] of bank) patternBank.set(id, entry);
          s.project.pattern = selected.pattern;
          s.notesRevision = selected.revision;
          s.history = selected.history;
          patternsRevision++;
        }
        s.project.arrangement = candidate.arrangement;
        if (clip.media) {
          const source = undo ? clip.media.before : clip.media.after;
          if (source) audioSources.set(clip.media.asset, source);
          else audioSources.delete(clip.media.asset);
        }
        if (undo) { clipUndo = clipUndo.filter(entry => entry !== clip); clipRedo.push(clip); }
        else { clipRedo = clipRedo.filter(entry => entry !== clip); clipUndo.push(clip); }
        changed(); effect = { kind: "arrangement" };
      } else if (trim && (!note || earlier(trim.order, note.change.order ?? 0))) {
        admitTrimInverse(trim, undo); moveTrimHistory(trim, undo); effect = { kind: "samplerTrim", channel: trim.channel };
      } else if (note) {
        const entry = patternBank.get(note.pattern)!;
        const candidate = clone(s.project); const [out, put] = undo ? [note.change.after, note.change.before] : [note.change.before, note.change.after];
        const ids = new Set([...out,...put].map((item) => item.id));
        const pattern = { ...entry.pattern, notes: [...entry.pattern.notes.filter((item) => !ids.has(item.id)), ...clone(put)], steps: lengthAfter(undo, entry.history) };
        const patterns = allPatterns().map((item) => item.id === note.pattern ? pattern : item);
        candidate.patterns = patterns;
        if (note.pattern === candidate.pattern.id) candidate.pattern = pattern;
        if (workspaceRefusal !== null) { const error=workspaceRefusal;workspaceRefusal=null;throw error; }
        if (new TextEncoder().encode(JSON.stringify(candidate)).length > 64 * 1024 * 1024) throw "The project is too big to save.";
        stepPatternHistory(note.pattern, undo); effect = { kind: "notes", pattern: note.pattern };
      }
      return { status: "applied", generation: s.generation, revision: arrangementRevision, effect };
    } catch (error) { return { status: "refused", generation: s.generation, revision: arrangementRevision, message: String(error), reload: true }; }
  };
  /** Finite pattern-clip counterpart to Session::edit_arrangement, not an audio/automation
   * engine. Candidate data, native-shaped refusal and real inverse snapshots are compared
   * to the offline Session fixture before browser outcomes earn integration grade. */
  const editPatternClips = (args: Record<string, unknown>) => {
    try {
      workspaceStamp(args); saveActivePattern();
      if (s.project.patternClips?.length) throw "Legacy clip records are outside the finite playlist fixture";
      const raw = String(args.editsJson); if (new TextEncoder().encode(raw).length > 64 * 1024 * 1024) throw "Arrangement request too large";
      const operations: Record<string, unknown>[] = JSON.parse(raw);
      if (!Array.isArray(operations) || operations.length > 100_000) throw "Invalid arrangement batch";
      const before: Arrangement = clone(s.project.arrangement ?? emptyArrangement());
      const candidate = clone(s.project); const arrangement = clone(before); candidate.arrangement = arrangement;
      const addedPatterns = new Map<number, PatternEntry>();
      const sourcePattern = (id: number) => addedPatterns.get(id) ?? patternBank.get(id);
      let next = candidate.nextId; const issued = new Set([...candidate.channels.map(c => c.id), ...patternBank.keys(), ...arrangement.tracks.map(t => t.id), ...arrangement.clips.map(c => c.id), ...arrangement.audioAssets.map(a => a.id)]);
      const ids: number[] = [], tracksByOperation = new Map<number, number>();
      const allocate = () => { while (next < 0xffffffff && issued.has(next)) next++; if (next >= 0xffffffff) throw "Project ids exhausted"; const id = next++; issued.add(id); return id; };
      const int = (value: unknown, max = 38_400_000): number => { if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0 || value > max) throw "Invalid arrangement integer"; return value; };
      const name = (value: unknown): string => { if (typeof value !== "string") throw "Invalid clip name"; return value; };
      const content = (value: unknown): PatternContent => {
        if (!value || typeof value !== "object" || !("type" in value) || value.type !== "pattern" || !("pattern" in value) || !("offsetTick" in value) || !("lengthTicks" in value)) throw "Only pattern content is supported by this finite fixture";
        const pattern = int(value.pattern, 0xffffffff), offsetTick = int(value.offsetTick), lengthTicks = int(value.lengthTicks);
        if (!sourcePattern(pattern) || lengthTicks < 1) throw "Invalid pattern source or length";
        return { type: "pattern", pattern, offsetTick, lengthTicks };
      };
      const track = (value: unknown) => { const id = int(value, 0xffffffff); if (!arrangement.tracks.some(t => t.id === id)) throw "Unknown track"; return id; };
      const clip = (value: unknown) => {
        const id = int(value, 0xffffffff);
        const result = arrangement.clips.find(c => c.id === id);
        if (!result) throw "Unknown clip";
        // Session::edit_arrangement allows pattern operations in the presence of audio assets.
        if (result.content.type !== "pattern") throw "Only pattern content is supported by this finite fixture";
        return result;
      };
      operations.forEach((op, index) => {
        if (!op || typeof op !== "object") throw "Invalid arrangement operation";
        if (op.kind === "setLoop") {
          if (op.region === null) arrangement.loopRegion = null;
          else {
            const region = op.region as Record<string, unknown>;
            const startTick = int(region.startTick), endTick = int(region.endTick);
            if (startTick >= endTick || typeof region.enabled !== "boolean") throw "Invalid loop region";
            arrangement.loopRegion = { startTick, endTick, enabled: region.enabled };
          }
        } else if (op.kind === "createTrack") {
          const id = allocate(); arrangement.tracks.push({ id, name: name(op.name), color: int(op.color, 5), mute: false }); ids.push(id); tracksByOperation.set(index, id);
        } else if (op.kind === "createClip" || op.kind === "createClipOnCreatedTrack") {
          const target = op.kind === "createClip" ? track(op.track) : tracksByOperation.get(int(op.trackOperation, 100_000));
          if (target === undefined) throw "Track must refer to an earlier CreateTrack";
          if (typeof op.muted !== "boolean") throw "Invalid muted flag";
          const id = allocate(); arrangement.clips.push({ id, track: target, name: name(op.name), startTick: int(op.startTick), muted: op.muted, content: content(op.content) }); ids.push(id);
        } else if (op.kind === "move" || op.kind === "copy") {
          const source = clip(op.clip), destination = track(op.track), start = int(op.startTick);
          if (op.kind === "copy") { const copy = clone(source); copy.id = allocate(); copy.track = destination; copy.startTick = start; arrangement.clips.push(copy); ids.push(copy.id); }
          else { source.track = destination; source.startTick = start; }
        } else if (op.kind === "slice") {
          const source = clip(op.clip), tick = int(op.tick), cut = tick - source.startTick;
          if (source.content.type !== "pattern" || op.audioFirstFrames != null || cut <= 0 || cut >= source.content.lengthTicks) throw "Invalid musical slice";
          const period = sourcePattern(source.content.pattern)!.pattern.steps * STEP_TICKS;
          const right = { ...source, id: allocate(), startTick: tick, content: clone(source.content) };
          right.content.offsetTick = (source.content.offsetTick + cut) % period;
          right.content.lengthTicks -= cut; source.content.lengthTicks = cut;
          arrangement.clips.push(right); ids.push(right.id);
        } else if (op.kind === "resize") {
          const source = clip(op.clip), resized = content(op.content);
          if (source.content.type !== "pattern" || source.content.pattern !== resized.pattern) throw "Resize cannot retarget its source";
          const duration = sourcePattern(source.content.pattern)!.pattern.steps * STEP_TICKS;
          const phase = ((source.content.offsetTick + int(op.startTick) - source.startTick) % duration + duration) % duration;
          if (resized.offsetTick !== phase) throw "Resize must preserve pattern source phase";
          source.content = resized; source.startTick = int(op.startTick);
        } else if (op.kind === "setClip") {
          const source = clip(op.clip);
          if (typeof op.muted !== "boolean") throw "Invalid muted flag";
          source.name = name(op.name); source.muted = op.muted;
        } else if (op.kind === "deleteClip") {
          const source = clip(op.clip);
          arrangement.clips = arrangement.clips.filter(candidate => candidate.id !== source.id);
        } else if (op.kind === "makeUnique") {
          const target = clip(op.clip);
          if (target.content.type !== "pattern") throw "Only pattern content is supported by this finite fixture";
          const source = sourcePattern(target.content.pattern);
          if (!source) throw "Unknown pattern source";
          const id = allocate();
          addedPatterns.set(id, { pattern: { ...clone(source.pattern), id }, revision: 1, history: emptyHistory() });
          target.content.pattern = id;
          ids.push(id);
        } else throw "This arrangement operation is outside the finite pattern fixture";
      });
      if (arrangement.tracks.length > 256 || arrangement.clips.length > 100_000 || arrangement.clips.some(c => c.content.type === "pattern" && c.startTick + c.content.lengthTicks > 38_400_000)) throw "Arrangement bounds exceeded";
      if (!addedPatterns.size && JSON.stringify(before) === JSON.stringify(arrangement)) return arrangementReply(null, false, []);
      candidate.nextId = next;
      workspaceAdmit(candidate, true, [...allPatterns(), ...Array.from(addedPatterns.values(), entry => entry.pattern)]);
      clearWorkspaceRedo();
      clipUndo.push({ order: historyOrder++, before, after: clone(arrangement), addedPatterns: Array.from(addedPatterns.values(), entry => ({ initial: clone(entry.pattern) })) });
      for (const [id, entry] of addedPatterns) patternBank.set(id, entry);
      if (addedPatterns.size) patternsRevision++;
      s.project.arrangement = arrangement; s.project.nextId = next; pruneWorkspaceHistory(); changed();
      return arrangementReply(null, true, ids);
    } catch (error) { return arrangementReply(null, false, [], String(error)); }
  };
  // Canonical RIFF/WAVE only: 44-byte header, 16-byte PCM fmt, then mono or stereo PCM16 data.
  // No metadata chunks; media.rs initializes tempo/key to null on import and preserves them on relink.
  const decodeAudio = (bytes: number[] | null | undefined, fromWorker: boolean): AudioSource => {
    const invalid = 'Media preparation failed: Decode("This file is not a sound Grooviq can open; use WAV, AIFF, FLAC, MP3 or OGG.")';
    if (!bytes || bytes.length < 44) throw invalid;
    const data = new DataView(Uint8Array.from(bytes).buffer);
    const channels = data.getUint16(22, true);
    const rate = data.getUint32(24, true);
    const frames = data.getUint32(40, true) / (channels * 2);
    if (data.getUint32(0) !== 0x52494646 || data.getUint32(8) !== 0x57415645 ||
        data.getUint32(12) !== 0x666d7420 || data.getUint32(16, true) !== 16 ||
        data.getUint16(20, true) !== 1 || data.getUint16(34, true) !== 16 ||
        data.getUint32(36) !== 0x64617461 || (channels !== 1 && channels !== 2) ||
        rate < 8000 || rate > 384000 || !Number.isInteger(frames) || frames < 1 ||
        44 + frames * channels * 2 !== bytes.length) throw invalid;
    return { bytes, data, rate, frames, channels, fromWorker };
  };
  const sameAudioBytes = (left: number[], right: number[]) =>
    left === right || (left.length === right.length && left.every((value, index) => value === right[index]));
  const retainedAudioSource = (bytes: number[]) => {
    const retained = [...audioSources.values()];
    for (const change of [...clipUndo, ...clipRedo]) {
      if (change.media?.before) retained.push(change.media.before);
      if (change.media) retained.push(change.media.after);
    }
    // Match the worker's equal-byte reuse while a worker-decoded source is still owned.
    // Open decodes archive bytes separately from the worker.
    return retained.find(source => source.fromWorker && sameAudioBytes(source.bytes, bytes));
  };
  const openArrangementMedia = () => {
    audioSources.clear();
    for (const asset of s.project.arrangement?.audioAssets ?? []) {
      const bytes = s.project.arrangementMedia?.[asset.id];
      if (asset.sample.file !== null && bytes) audioSources.set(asset.id, decodeAudio(bytes, false));
      else asset.sample.file = null;
    }
  };
  openArrangementMedia();

  const prepareAudio = async (
    args: Record<string, unknown>,
    target: { kind: "import"; track: number; startTick: number; name: string } | { kind: "relink"; asset: number },
  ) => {
    const path = String(args.path);
    const key = target.kind === "import" ? 0xffffffff : target.asset;
    let request: number | null = null;
    try {
      workspaceStamp(args);
      const before = s.project.arrangement ?? emptyArrangement();
      if (target.kind === "import") {
        if (!before.tracks.some(track => track.id === target.track) || target.startTick >= 38_400_000) {
          throw "Invalid import destination";
        }
      } else if (!before.audioAssets.some(asset => asset.id === target.asset)) {
        throw "Audio asset removed";
      }
      if (nextMediaRequest >= Number.MAX_SAFE_INTEGER) throw "Request identities exhausted";
      request = nextMediaRequest++;
      const file = files.get(path);
      if (!file) throw "No such file or directory (os error 2)";
      if (file.kind === "folder") throw "not a regular file";
      pendingMedia.set(key, request);
      // Pause at the worker's receive/publication boundary. Outcome is deterministic per path.
      const held = mediaHolds.get(path);
      const decoded = held ? await held.promise : true;
      const prepared = decodeAudio(decoded && file.kind === "sample" ? file.wav : undefined, true);
      workspaceStamp(args);
      if (pendingMedia.get(key) !== request) throw "Media request cancelled or superseded";
      const candidate = clone(s.project);
      const arrangement = candidate.arrangement ?? emptyArrangement();
      candidate.arrangement = arrangement;
      const name = path.split(/[/\\]/).at(-1) || "sample";
      const sample = { path: name, file: null };
      const createdIds: number[] = [];
      let asset: number;
      if (target.kind === "import") {
        const issued = new Set([
          ...candidate.channels.map(channel => channel.id), ...patternBank.keys(),
          ...arrangement.tracks.map(track => track.id), ...arrangement.clips.map(clip => clip.id),
          ...arrangement.audioAssets.map(entry => entry.id),
        ]);
        const allocate = () => {
          while (candidate.nextId < 0xffffffff && issued.has(candidate.nextId)) candidate.nextId++;
          if (candidate.nextId >= 0xffffffff) throw "Project ids exhausted";
          return candidate.nextId++;
        };
        asset = allocate();
        const clip = allocate();
        if (arrangement.audioAssets.length >= 4096 || arrangement.clips.length >= 100_000) {
          throw "Invalid arrangement: bounds, identity, reference or native parameter range.";
        }
        const ticks = Math.ceil(prepared.frames / prepared.rate * candidate.bpm * 16);
        if (target.startTick + ticks > 38_400_000) {
          throw `Clip ${clip} (${target.name}): invalid track/source reference, bounds or automation value.`;
        }
        arrangement.audioAssets.push({ id: asset, sample, sourceRate: prepared.rate, sourceFrames: prepared.frames,
          originalTempo: null, originalKey: null });
        arrangement.clips.push({ id: clip, track: target.track, name: target.name, startTick: target.startTick, muted: false,
          content: { type: "audio", asset, sourceStart: 0, sourceEnd: prepared.frames, gain: 1, reverse: false, fadeIn: 0, fadeOut: 0 } });
        createdIds.push(asset, clip);
      } else {
        asset = target.asset;
        const entry = arrangement.audioAssets.find(entry => entry.id === asset);
        if (!entry) throw "Relink asset removed";
        entry.sample = sample;
      }
      workspaceAdmit(candidate);
      const source = retainedAudioSource(prepared.bytes) ?? prepared;
      clearWorkspaceRedo();
      clipUndo.push({ order: historyOrder++, before, after: arrangement, addedPatterns: [],
        media: { asset, before: audioSources.get(asset), after: source } });
      s.project.arrangement = arrangement;
      s.project.nextId = candidate.nextId;
      audioSources.set(asset, source);
      pruneWorkspaceHistory();
      changed();
      return arrangementReply(request, true, createdIds);
    } catch (error) {
      return arrangementReply(request, false, [], String(error));
    } finally {
      if (request !== null && pendingMedia.get(key) === request) pendingMedia.delete(key);
    }
  };

  const waveformSource = (generation: number, asset: number) => {
    if (generation !== s.generation) throw "Project replaced";
    const source = audioSources.get(asset);
    if (!source) throw "Audio is missing; relink it";
    return source;
  };
  const arrangementWaveform = async (args: Record<string, unknown>) => {
    const { generation, request, asset, start, end, bins } = args as Record<string, number>;
    const source = waveformSource(generation, asset);
    if (start >= end || end > source.frames || bins === 0 || bins > 16384 || bins > end - start) {
      throw "Invalid waveform range or bin count";
    }
    if (!Number.isSafeInteger(request)) throw "Invalid waveform identity";
    const held = waveformHolds.get(request);
    if (held) await held.promise;
    if (waveformSource(generation, asset) !== source) throw "Waveform source replaced; reload";
    // engine/src/waveform.rs::PeakPyramid::encode: WFP1, little-endian, 64-byte header.
    const packet = new DataView(new ArrayBuffer(64 + bins * 16));
    packet.setUint32(0, 0x31504657, true);
    packet.setUint32(4, asset, true);
    [generation, request, source.frames, start, end].forEach((value, index) =>
      packet.setBigUint64(8 + index * 8, BigInt(value), true));
    [source.rate, 2, bins, 0].forEach((value, index) => packet.setUint32(48 + index * 4, value, true));
    for (let bin = 0; bin < bins; bin++) {
      const from = start + Math.floor((end - start) * bin / bins);
      const to = start + Math.floor((end - start) * (bin + 1) / bins);
      let leftMin = Infinity, leftMax = -Infinity, rightMin = Infinity, rightMax = -Infinity;
      for (let frame = from; frame < to; frame++) {
        const offset = 44 + frame * source.channels * 2;
        const left = source.data.getInt16(offset, true) / 32768;
        const right = source.channels === 2 ? source.data.getInt16(offset + 2, true) / 32768 : left;
        leftMin = Math.min(leftMin, left); leftMax = Math.max(leftMax, left);
        rightMin = Math.min(rightMin, right); rightMax = Math.max(rightMax, right);
      }
      [leftMin, leftMax, rightMin, rightMax].forEach((peak, index) =>
        packet.setFloat32(64 + bin * 16 + index * 4, peak, true));
    }
    return packet.buffer;
  };

  const seekArrangementTransport = (args: Record<string, unknown>) => {
    try {
      workspaceStamp(args);
      if (workspaceRefusal !== null) { const error = workspaceRefusal; workspaceRefusal = null; throw error; }
      if (s.project.playMode !== "song") throw "Seek requires Song mode";
      let tick = Number(args.tick);
      if (!Number.isSafeInteger(tick) || tick < 0) throw "Invalid seek tick";
      const arrangement = s.project.arrangement;
      const loop = arrangement?.loopRegion;
      if (loop?.enabled) tick = Math.max(loop.startTick, Math.min(loop.endTick - 1, tick));
      const end = Math.max(0, ...(arrangement?.clips ?? []).map(c => c.startTick + (c.content.type === "pattern" ? c.content.lengthTicks : 0)));
      if (tick > end) throw "Seek outside Song extent";
      s.transport.startTicks = tick; s.transport.startMs = performance.now();
      if (s.transport.held !== null) s.transport.held = tick;
      return { status: "applied", generation: s.generation, revision: arrangementRevision, changed: false, request: null, createdIds: [], reload: false, message: null, warnings: [] };
    } catch (error) { return { status: "refused", generation: s.generation, revision: arrangementRevision, changed: false, request: null, createdIds: [], reload: true, message: String(error), warnings: [] }; }
  };
  const nativeArrangementOnly = () => { throw "Arrangement/media commands are not simulated by this browser fixture; use native offline acceptance."; };
  const commands: Record<string, (a: Record<string, unknown>) => unknown> = {
    arrangement_view: workspaceView,
    arrangement_edit: editPatternClips,
    arrangement_history: stepProjectHistory,
    arrangement_curve: nativeArrangementOnly,
    audio_import: (a) => prepareAudio(a, { kind: "import", track: Number(a.track), startTick: Number(a.startTick), name: String(a.name) }),
    audio_relink: (a) => prepareAudio(a, { kind: "relink", asset: Number(a.asset) }),
    audio_cancel: nativeArrangementOnly,
    arrangement_waveform: arrangementWaveform,
    arrangement_mode: workspaceMode,
    arrangement_loop: nativeArrangementOnly,
    arrangement_seek: seekArrangementTransport,
    // Native menu state has no platform menu in this headless fixture.
    edit_history_enabled: () => null,
    // commands.rs
    audio_outputs: () => [SILENT_OUTPUT],
    audio_select: (a) => {
      if (a.id !== "default" && a.id !== SILENT_OUTPUT.id) throw `No output with the id "${String(a.id)}"`;
      s.audio.output = String(a.id);
      return null;
    },
    audio_settings: () => ({ ...s.audio }),
    audio_set_guard: (a) => {
      s.audio.guard = Boolean(a.on);
      return null;
    },
    test_tone: (a) => {
      s.tone = Boolean(a.on);
      return null;
    },
    synth_note: () => null,
    live_endpoint: () => ({ port: LIVE_PORT, token: "e2e" }),
    audio_log: () => seed.audioLog ?? [
      { id: 1, atMs: Date.now(), level: "warn", message: "Silent QA mode (mock engine): no audio device" },
    ],
    // project_cmds.rs
    pattern_list: () => patternsView(),
    pattern_edit: (a) => managePattern(a),
    project_view: () => view(),
    project_edit: (a) => {
      // session.rs `apply_in`: an edit made for a replaced project changes nothing.
      if (a.generation !== s.generation) return view();
      const kind = (a.edit as Edit).kind;
      if (["toggleStep", "setStepVelocity", "clearStep", "setPatternLength", "setSwing"].includes(kind)) checkStamp(a.stamp);
      if (applyEdit(a.edit as Edit)) changed();
      return view();
    },
    channel_add: (a) => {
      // session.rs `add_channel_in`: checked after the file is read, at the change.
      const path = String(a.path);
      readSample(path);
      sameProject(a.generation);
      const p = s.project;
      if (p.channels.length >= MAX_CHANNELS) {
        throw `A project holds at most ${MAX_CHANNELS} channels; remove one to add another.`;
      }
      const id = nextChannelIdentity();
      const last = p.channels[p.channels.length - 1];
      p.channels.push({
        id,
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
      p.nextId = id + 1;
      changed();
      return view();
    },
    channel_set_sample: (a) => {
      const path = String(a.path);
      readSample(path);
      sameProject(a.generation);
      const c = s.project.channels[channelIndex(a.channel)];
      c.sample = path;
      c.embedded = true;
      delete c.decodeFailed;
      // session.rs: the length is the new file's (a seeded length belonged to the old one).
      delete c.frames;
      invalidateTrim(c.id,true);
      changed();
      return view();
    },
    pattern_notes: () => encodeNotes(),
    notes_edit: (a) => editNotes(a.edit as Parameters<typeof editNotes>[0]),
    notes_undo: (a) => { checkStamp(a.stamp); return stepHistory(true); },
    notes_redo: (a) => { checkStamp(a.stamp); return stepHistory(false); },
    channel_add_synth: (a) => {
      sameProject(a.generation);
      const preset = String(a.preset);
      if (!PRESETS.includes(preset)) throw `There is no synth preset called "${preset}".`;
      const p = s.project;
      if (p.channels.length >= MAX_CHANNELS) {
        throw `A project holds at most ${MAX_CHANNELS} channels; remove one to add another.`;
      }
      const id = nextChannelIdentity();
      const last = p.channels[p.channels.length - 1];
      p.channels.push({
        id,
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
      p.nextId = id + 1;
      changed();
      return view();
    },
    channel_note: (a) => (a.on ? liveOn(a.channel, a.key, a.velocity) : liveOff(a.channel, a.key, a.press)),
    synth_meter: (a) => {
      if (a.generation !== s.generation) return null;
      const channel = s.project.channels.find((c) => c.id === a.channel && c.kind === "synth");
      if (!channel) return null;
      const instance = (s.generation + 1) * 100000 + channel.id;
      if (a.instance != null && a.instance !== instance) return null;
      if (s.livePaused) return synthObservation?.[0] === instance ? synthObservation : null;
      const sounding = [...s.live.keys()].some((key) => key.startsWith(`${channel.id}:`));
      const t = performance.now() / 1000;
      synthObservation = [instance, Math.floor(performance.now() * 48), sounding ? 0.5 : 0, sounding ? 0.5 : 0, sounding ? 1 : 0, sounding ? Math.sin(t) : 0, sounding ? 0.7 : 0, sounding ? 0.4 : 0, ...Array.from({ length: 256 }, (_, i) => sounding ? Math.sin(i / 10 + t * 9) * 0.4 : 0)];
      if (synthMotion) {
        synthObservation[1] = synthMotion.frames ?? synthObservation[1];
        synthObservation.splice(4, 4, synthMotion.voices, synthMotion.lfoValue, synthMotion.ampLevel, synthMotion.filterLevel);
      }
      synthObservation.push(synthMotion?.ampStage ?? (sounding ? 3 : 0), synthMotion?.filterStage ?? (sounding ? 3 : 0), synthMotion?.lfoPhase ?? (t % 1));
      return synthObservation;
    },
    sampler_trim_view: (a) => trimView(Number(a.channel)),
    sampler_trim_reset: (a) => trimMutation(Number(a.channel),String(a.stamp)),
    sampler_trim_undo: (a) => trimMutation(Number(a.channel),String(a.stamp),String(a.token)),
    channel_param_live: (a) => {
      const c = s.project.channels[channelIndex(a.channel)];
      const { spec, value } = instrumentParam(c, a.param, a.value);
      s.liveParams.set(`${c.id}:${spec[0]}`, value);
      if (c.kind !== "synth" && (spec[0] === "start" || spec[0] === "end")) invalidateTrim(c.id);
      return null;
    },
    channel_select: (a) => {
      if (a.channel !== null) channelIndex(a.channel);
      if (s.selected !== a.channel) {
        for (const port of [...s.midi.players.keys()].sort()) midiReleasePlayer(port, true);
      }
      s.selected = a.channel as number | null;
      return null;
    },
    channel_waveform: (a) => {
      const c = s.project.channels[channelIndex(a.channel)];
      if (c.kind === "synth" || c.sample === null || missing(c)) throw `${c.name} has no sample to draw.`;
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
      if (!on) for (const press of [...s.recordOpen.keys()]) closeRecorded(press, ticksNow());
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
    // Unique fixture port strings serve as both opaque IDs and display names.
    midi_inputs: () => s.midi.present.map((id) => ({ id, name: id, enabled: !s.midi.disabled.includes(id) })),
    midi_set_enabled: (a) => {
      const id = a.id as string;
      const was = !s.midi.disabled.includes(id);
      s.midi.disabled = s.midi.disabled.filter((n) => n !== id);
      if (!a.enabled) {
        s.midi.disabled.push(id);
        midiRetire(id);
      } else {
        midiAttach(id);
      }
      midiRetry();
      if (was !== Boolean(a.enabled)) emit("midi-inputs-changed", null);
      return null;
    },
    // session.rs `preview_on`: the channel's live note on its preview key, recorded like any.
    channel_preview: (a) => {
      if (a.on) {
        const id = liveOn(a.channel, DEFAULT_KEY, DEFAULT_VELOCITY);
        s.previewPresses.add(id);
        s.previews.set(a.channel as number, true);
        return id;
      }
      liveOff(a.channel, DEFAULT_KEY, a.press);
      s.previewPresses.delete(a.press as number);
      const where = `${String(a.channel)}:${DEFAULT_KEY}`;
      if (![...s.previewPresses].some((p) => s.presses.get(p) === where)) s.previews.set(a.channel as number, false);
      return null;
    },
    transport_play: () => {
      if (s.transport.playing) return null;
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
          if (i === 1 && seed.holdExport) {
            await new Promise<void>((resolve) => { releaseExport = resolve; });
            releaseExport = null;
            if (job.cancel) throw "The export was cancelled.";
          }
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
    autosave_status: () => ({ ...s.autosave }),
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

  /** session.rs `checked_key`. */
  const checkedKey = (key: unknown) => {
    if ((key as number) > 127) throw `Key ${String(key)} is outside 0 to 127.`;
    return key as number;
  };
  /** session.rs `live_note_on`, the one path of every live note (a key, a click, a preview): a
   * new press, sounding now and recorded while a take runs. */
  const liveOn = (channel: unknown, key: unknown, velocity: unknown) => {
    const c = s.project.channels[channelIndex(channel)];
    const k = checkedKey(key);
    const v = clamp(finite(velocity, "velocity"), 0, 1);
    const id = s.nextPress++;
    const where = `${c.id}:${k}`;
    s.presses.set(id, where);
    s.live.set(where, v);
    if (s.recording && s.transport.playing) s.recordOpen.set(id, { channel: c.id, key: k, tick: ticksNow(), velocity: v, take: s.take });
    return id;
  };
  /** session.rs `live_note_off`: the engine ends only the press the release names (of this
   * channel and key); a press it never made, or already ended, ends nothing. */
  const liveOff = (channel: unknown, key: unknown, id: unknown) => {
    const c = s.project.channels[channelIndex(channel)];
    const where = `${c.id}:${checkedKey(key)}`;
    if (id === null || id === undefined) throw "A release names the press it ends.";
    if (s.presses.get(id as number) !== where) return null;
    s.presses.delete(id as number);
    if (![...s.presses.values()].includes(where)) s.live.delete(where);
    closeRecorded(id as number, ticksNow());
    return null;
  };

  const midiTarget = () => s.project.channels.find((channel) => channel.id === s.selected);

  // hub.rs Player::release and Ledger::release: retire one owner, then send every accepted
  // press, oldest first, when no port still holds or sustains the key.
  const midiDisown = (port: string, key: number) => {
    const held = s.midi.keys.get(key);
    if (!held?.owners.delete(port) || held.owners.size) return;
    s.midi.keys.delete(key);
    const target = midiTarget();
    if (!target) return;
    const debt = s.midi.owed.get(target) ?? new Map<number, number[]>();
    const presses = debt.get(key) ?? [];
    presses.push(...held.presses);
    debt.set(key, presses);
    s.midi.owed.set(target, debt);
    midiPay(target, key);
  };

  // sink.rs ControllerSink::release/retry: retry by instance and by press ID. Only MIDI
  // admission stalls here. The fixture controls full/available explicitly, with no timer.
  const midiPay = (target: MockChannel, key: number) => {
    if (!s.project.channels.includes(target)) {
      s.midi.owed.delete(target);
      return;
    }
    if (s.midi.stalled) return;
    const debt = s.midi.owed.get(target);
    if (!debt) return;
    for (const press of debt.get(key) ?? []) liveOff(target.id, key, press);
    debt.delete(key);
    if (!debt.size) s.midi.owed.delete(target);
  };
  const midiRetry = () => {
    for (const [target, debt] of [...s.midi.owed].sort(([a], [b]) => a.id - b.id)) {
      for (const key of [...debt.keys()].sort((a, b) => a - b)) midiPay(target, key);
    }
  };

  const midiReleasePlayer = (port: string, includeDown: boolean) => {
    const player = s.midi.players.get(port);
    if (!player) return;
    const keys = [...player.held, ...(includeDown ? player.down : [])].sort((a, b) => a - b);
    player.held.clear();
    if (includeDown) player.down.clear();
    for (const key of keys) midiDisown(port, key);
  };
  const midiAttach = (port: string) => {
    if (s.midi.present.includes(port) && !s.midi.disabled.includes(port) && !s.midi.players.has(port)) {
      s.midi.players.set(port, midiPlayer());
    }
  };
  const midiRetire = (port: string) => {
    midiReleasePlayer(port, true);
    s.midi.players.delete(port);
  };
  const midiDiscardKeys = () => {
    // The graph cut these notes. Drop their ownership while keeping connected ports' pedals.
    s.midi.keys.clear();
    for (const player of s.midi.players.values()) {
      player.down.clear();
      player.held.clear();
    }
  };

  const midiMessage = (port: string, bytes: number[]) => {
    const [status, data, value] = bytes;
    const kind = status & 0xf0;
    const player = s.midi.players.get(port);
    if (!player || bytes.length !== 3 || !isInt(status, 255) || !isInt(data, 127) || !isInt(value, 127)) return null;

    let press = null;
    if (kind === 0x90 && value > 0) {
      const target = midiTarget();
      // A refused note-on never acquires ownership or changes an existing hold.
      if (target && !s.midi.stalled) {
        press = liveOn(target.id, data, value / 127);
        player.held.delete(data);
        player.down.add(data);
        const held = s.midi.keys.get(data) ?? { owners: new Set(), presses: [] };
        held.owners.add(port);
        held.presses.push(press);
        s.midi.keys.set(data, held);
      }
    } else if (kind === 0x80 || kind === 0x90) {
      if (player.down.delete(data)) {
        if (player.pedal) player.held.add(data);
        else midiDisown(port, data);
      }
    } else if (kind === 0xb0 && data === 64) {
      player.pedal = value >= 64;
      if (!player.pedal) midiReleasePlayer(port, false);
    } else if (kind === 0xb0 && (data === 120 || data === 123)) {
      midiReleasePlayer(port, true);
    } else {
      return null;
    }
    // hub.rs callback retries after every parsed message from an open port.
    midiRetry();
    return press;
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
    generation: u64,
    patternId: u32,
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
    arrangement_view: { offset: u64, limit: u64 },
    arrangement_mode: { generation: u64, revision: u64, mode: (value) => value === "pattern" || value === "song" ? null : "expected pattern or song" },
    arrangement_history: { generation: u64, revision: u64, undo: bool },
    arrangement_edit: { generation: u64, revision: u64, editsJson: str },
    audio_import: { generation: u64, revision: u64, track: u32, startTick: u64, name: str, path: str, ticket: optional(u64) },
    audio_relink: { generation: u64, revision: u64, asset: u32, path: str, ticket: optional(u64) },
    arrangement_waveform: { generation: u64, asset: u32, start: u64, end: u64, bins: u64,
      request: (v) => isInt(v, 2 ** 64) && Number(v) < 2 ** 64 ? null : "expected u64" },
    sampler_trim_view: { channel:u32 },
    sampler_trim_reset: { channel:u32,stamp:str },
    sampler_trim_undo: { channel:u32,stamp:str,token:str },
    audio_select: { id: str },
    audio_set_guard: { on: bool },
    test_tone: { on: bool },
    synth_note: { key: u8, on: bool },
    pattern_edit: { generation: u64, revision: u64, edit: (v) => typeof (v as {kind?:unknown})?.kind === "string" ? null : "expected tagged pattern operation" },
    project_edit: { edit, generation: u64, stamp: optional(fields({ generation: u64, patternId: u32, revision: u32 })) },
    channel_add: { path: str, generation: u64 },
    channel_set_sample: { channel: u32, path: str, generation: u64 },
    channel_preview: { channel: u32, on: bool, press: optional(u64) },
    notes_edit: { edit: notesEditArg },
    notes_undo: { stamp: fields({ generation: u64, patternId: u32, revision: u32 }) },
    notes_redo: { stamp: fields({ generation: u64, patternId: u32, revision: u32 }) },
    channel_add_synth: { preset: str, generation: u64 },
    channel_note: { channel: u32, key: u8, velocity: f32, on: bool, press: optional(u64) },
    synth_meter: { channel: u32, generation: u64, instance: optional(u64) },
    channel_param_live: { channel: u32, param: str, value: f32 },
    channel_select: { channel: optional(u32) },
    midi_set_enabled: { id: str, enabled: bool },
    channel_waveform: { channel: u32, points: u32 },
    record_set: { on: bool },
    metronome_set: { on: bool },
    project_open: { path: str },
    project_save_as: { path: str },
    project_export: { path: str, options: exportOptions },
  };

  /** Commands the test holds back (`holdCommand`), each until `releaseCommand`. */
  const holds = new Map<string, { promise: Promise<void>; release: () => void }>();
  for (const [command, held] of Object.entries({ audio_log: seed.holdAudioLog, project_view: seed.holdProjectView })) {
    if (!held) continue;
    let release!: () => void;
    const promise = new Promise<void>((resolve) => { release = resolve; });
    holds.set(command, { promise, release });
  }
  const replyHolds = new Map<string, { promise: Promise<void>; release: () => void }>();
  const replyHoldCommands = new Set(["pattern_list", "pattern_edit", "arrangement_view", "arrangement_mode", "arrangement_history", "arrangement_edit"]);
  const refusals = new Map<string, string>();
  const invoke = async (cmd: string, args: Record<string, unknown> = {}) => {
    calls.push({ cmd, args: clone(args ?? {}), at: performance.now() });
    // A held command waits here, before the engine sees it, until the test releases it.
    const held = holds.get(cmd);
    if (held) await held.promise;
    const refused = refusals.get(cmd);
    if (refused !== undefined) {
      refusals.delete(cmd);
      if (cmd === "pattern_edit") return clone({ status: "refused", reason: "engineBusy", message: refused,
        state: patternsView(), settlement: { recordingDisarmed: false, waiting: false } });
      throw refused;
    }
    const handler = commands[cmd];
    if (!handler) throw `The mock engine has no command ${cmd}`;
    for (const [name, check] of Object.entries(ARGS[cmd] ?? {})) {
      const why = check((args ?? {})[name]);
      if (why) throw `invalid args \`${name}\` for command \`${cmd}\`: ${why}`;
    }
    const result = await handler(args ?? {});
    const admitted = result instanceof ArrayBuffer ? result.slice(0) : clone(result);
    if (replyHoldCommands.has(cmd)) {
      workspaceTrace.push({ command: cmd, generation: typeof admitted === "object" && admitted && "generation" in admitted ? Number(admitted.generation) : s.generation,
        revision: typeof admitted === "object" && admitted && "revision" in admitted ? Number(admitted.revision) : arrangementRevision,
        status: typeof admitted === "object" && admitted && "status" in admitted ? String(admitted.status) : "read",
        effect: typeof admitted === "object" && admitted && "effect" in admitted ? clone(admitted.effect) : undefined });
      if (workspaceTrace.length > 200) workspaceTrace.shift();
      await replyHolds.get(cmd)?.promise;
    }
    // A raw `tauri::ipc::Response` arrives as an ArrayBuffer.
    return admitted;
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
    setSynthMotion: (snapshot: SynthMotionSnapshot | null) => { synthMotion = snapshot; },
    trimState: trimView,
    trimInverseCount: () => trimUndo.length,
    refuseTrim: (message: string) => { trimRefusal=message; },
    holdTrimResponse: () => {
      if (trimResponseHold) throw new Error("Trim response already held");
      let release=()=>{};
      const promise=new Promise<void>(resolve=>{release=resolve;});
      trimResponseHold={promise,release};
    },
    releaseTrimResponse: () => { trimResponseHold?.release();trimResponseHold=null; },
    releaseExport: () => {
      if (!releaseExport) throw new Error("No export is held.");
      releaseExport();
    },
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
    /** A file as the mock holds it (a saved project's notes, ...), or null. */
    file: (path: string) => clone(files.get(path) ?? null),
    /** Next save writes its file/path, retains dirty state, then reports uncertainty. */
    unconfirmNextSave: () => { unconfirmNextSave = true; },
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
    holdAudio: (path: string) => holdMedia(mediaHolds, path),
    releaseAudio: (path: string) => releaseMedia(mediaHolds, path),
    refuseAudio: (path: string) => releaseMedia(mediaHolds, path, false),
    holdWaveform: (request: number) => holdMedia(waveformHolds, request),
    releaseWaveform: (request: number) => releaseMedia(waveformHolds, request),
    /** Files dragged from the OS at CSS client coordinates. */
    drag: (paths: string[], drop: boolean, client = { x: 400, y: 300 }) => {
      // Model Tauri’s documented physical-pixel position. macOS may deliver points (#636).
      const position = { x: client.x * window.devicePixelRatio, y: client.y * window.devicePixelRatio };
      emit("tauri://drag-enter", { paths, position });
      emit("tauri://drag-over", { position });
      if (drop) emit("tauri://drag-drop", { paths, position });
      else emit("tauri://drag-leave", null);
    },
    /** Delivers one frame now that says `playing` whatever the transport does: a frame the
     * engine rendered earlier and the stream delivers late. */
    injectLive: (playing: boolean) => {
      const data = encodeLive();
      const v = new DataView(data);
      v.setUint8(1, (v.getUint8(1) & ~1) | (playing ? 1 : 0));
      for (const socket of sockets) socket.onmessage?.(new MessageEvent("message", { data }));
      return sockets.size;
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
    holdReply: (cmd: string) => {
      if (!replyHoldCommands.has(cmd) || replyHolds.has(cmd)) throw "Unsupported or already held workspace reply";
      let release = () => {}; const promise = new Promise<void>((resolve) => { release = resolve; });
      replyHolds.set(cmd, { promise, release });
    },
    releaseReply: (cmd: string) => { replyHolds.get(cmd)?.release(); replyHolds.delete(cmd); },
    refuseWorkspaceNext: (message: string) => { workspaceRefusal = message; },
    workspaceTrace: () => clone(workspaceTrace),
    workspaceSnapshot: () => ({ generation: s.generation, revision: arrangementRevision, nextId: s.project.nextId, selected: s.project.pattern.id,
      patterns: allPatterns().map((pattern) => ({ ...pattern, revision: patternBank.get(pattern.id)!.revision })),
      dirty: dirty(), playMode: s.project.playMode ?? "pattern",
      storedTrim: s.project.channels.filter((channel) => channel.kind !== "synth").map((channel) => ({ channel: channel.id,
        start: Object.hasOwn(channel.sampler, "start") ? new Uint32Array(new Float32Array([channel.sampler.start]).buffer)[0] : null,
        end: Object.hasOwn(channel.sampler, "end") ? new Uint32Array(new Float32Array([channel.sampler.end]).buffer)[0] : null })),
      trim: s.project.channels.filter((channel) => channel.kind !== "synth").map((channel) => [channel.id, trimView(channel.id)]) }),
    releaseCommand: (cmd: string) => {
      holds.get(cmd)?.release();
      holds.delete(cmd);
    },
    /** Holds back live frames (`true`) or lets them flow again. */
    pauseLive: (on: boolean) => {
      s.livePaused = on;
    },
    disconnectLive: () => {
      const connected = [...sockets];
      for (const socket of connected) {
        socket.close();
        socket.onclose?.(new CloseEvent("close"));
      }
      return connected.length;
    },
    /** A note that arrives from outside the UI's edits (the recorder, as `collect_recorded`
     * adds it): the engine bumps the notes revision and announces it. */
    addNote: (n: { channel: number; tick: number; length: number; key: number; velocity: number }) => {
      if (editPattern((pat) => {
        const note = { ...n, id: takeNoteId(pat), slide: false };
        pat.notes.push(note);
        growToNotes(pat, [note]);
      })) changed();
    },
    /** A plug or unplug includes the hub poll that observes it. */
    midiPlug: (port: string) => {
      const changed = !s.midi.present.includes(port);
      if (changed) s.midi.present = [...s.midi.present, port].sort();
      midiAttach(port);
      midiRetry();
      if (changed) emit("midi-inputs-changed", null);
    },
    midiUnplug: (port: string) => {
      const changed = s.midi.present.includes(port);
      s.midi.present = s.midi.present.filter((id) => id !== port);
      midiRetire(port);
      midiRetry();
      if (changed) emit("midi-inputs-changed", null);
    },
    midiMessage,
    midiNote: (port: string, key: number, velocity: number, on: boolean) =>
      midiMessage(port, [on ? 0x90 : 0x80, key, velocity]),
    midiStall: (stalled: boolean) => { s.midi.stalled = stalled; },
    midiRetry,
    midiOwed: () => [...s.midi.owed.values()].reduce((total, debt) =>
      total + [...debt.values()].reduce((count, presses) => count + presses.length, 0), 0),
    /** The names of the MIDI inputs switched off. */
    midiDisabled: () => [...s.midi.disabled],
    /** Recorded notes did not fit (the note cap): recording switches itself off, as the
     * shell's recorder thread does between two polls. */
    recordingStopsItself: (dropped: number) => stopsItself(dropped),
    /** The notes as the engine holds them. */
    notes: () => clone(s.project.pattern.notes),
    patterns: allPatterns,
    /** Live notes sounding now, as "channel:key". */
    live: () => [...s.live.keys()],
    autosave: () => {
      // autosave_notice.rs `succeeded`: the trouble is over.
      s.autosave.failure = null;
      s.autosavedAt = Math.floor(Date.now() / 1000);
      emit("project-autosaved", s.autosavedAt);
    },
  };
}
