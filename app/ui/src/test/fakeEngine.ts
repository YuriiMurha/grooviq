import { encodeNotes, type NotesAck, type NotesEdit, type WireNote } from "../api/notes";
import type { ChannelView, Edit, ProjectView } from "../api/project";

type Args = Record<string, unknown> | undefined;

export interface Call {
  cmd: string;
  args: Args;
}

/** A stand-in for the engine's project commands (the contract of Task 6), enough to drive the
 * rack in tests: it keeps a view, applies edits to it and records every call. */
export function createFakeEngine(initial: ProjectView) {
  let view: ProjectView = structuredClone(initial);
  const calls: Call[] = [];
  /** The pattern's notes (`pattern_notes`, `notes_edit`), empty unless a test sets them. */
  let notes: WireNote[] = [];
  /** Paths `channel_add` refuses, with the loader's sentence. */
  const refuse = new Map<string, string>();

  const touch = () => {
    view = { ...view, dirty: true };
    return structuredClone(view);
  };

  function applyEdit(edit: Edit) {
    const cells = view.pattern.cells;
    const row = (channel: number) => (cells[String(channel)] ??= Array(view.pattern.steps).fill(null));
    switch (edit.kind) {
      case "toggleStep": {
        const r = row(edit.channel);
        r[edit.step] = r[edit.step] == null ? 0.8 : null;
        break;
      }
      case "setStepVelocity":
        row(edit.channel)[edit.step] = edit.velocity;
        break;
      case "clearStep":
        row(edit.channel)[edit.step] = null;
        break;
      case "setChannelMix":
        view.channels = view.channels.map((c) =>
          c.id === edit.channel ? { ...c, volume: edit.volume, pan: edit.pan, mute: edit.mute, solo: edit.solo } : c,
        );
        break;
      case "setPatternLength":
        view.pattern.steps = edit.steps;
        for (const key of Object.keys(cells)) {
          cells[key] = Array.from({ length: edit.steps }, (_, i) => cells[key][i] ?? null);
        }
        break;
      case "setSwing":
        view.pattern.swing = edit.swing;
        break;
      default:
        break;
    }
  }

  /** Deletes, then updates, then adds, by id (Task 7's contract, without clamping). */
  function editNotes(edit: NotesEdit): NotesAck {
    const reload = edit.baseRevision !== view.notesRevision;
    const byId = new Map(notes.map((n) => [n.id, n]));
    const ignoredIds: number[] = [];
    for (const id of edit.delete) if (!byId.delete(id)) ignoredIds.push(id);
    for (const u of edit.update) {
      if (byId.has(u.id)) byId.set(u.id, { ...u });
      else ignoredIds.push(u.id);
    }
    let next = Math.max(0, ...byId.keys()) + 1;
    const addedIds = edit.add.map((a) => {
      const id = next++;
      byId.set(id, { ...a, id });
      return id;
    });
    notes = [...byId.values()];
    view = { ...view, dirty: true, notesRevision: view.notesRevision + 1 };
    return { revision: view.notesRevision, addedIds, ignoredIds, reload, view: structuredClone(view) };
  }

  async function invoke(cmd: string, args?: Args): Promise<unknown> {
    calls.push({ cmd, args });
    switch (cmd) {
      case "project_view":
        return structuredClone(view);
      case "project_edit":
        applyEdit(args?.edit as Edit);
        return touch();
      case "channel_add": {
        const path = String(args?.path);
        const refusal = refuse.get(path);
        if (refusal) throw refusal;
        const id = Math.max(0, ...view.channels.map((c) => c.id)) + 1;
        const file = path.split("/").pop() ?? path;
        const name = file.replace(/\.[^.]+$/, "");
        const channel: ChannelView = {
          id,
          name,
          color: (id - 1) % 6,
          sampleName: name,
          sampleFrames: 48_000,
          missing: false,
          volume: 1,
          pan: 0,
          mute: false,
          solo: false,
          sampler: {},
          kind: "sampler",
          synth: {},
          preset: null,
          choke: 0,
          roll: false,
        };
        view.channels = [...view.channels, channel];
        view.pattern.cells[String(id)] = Array(view.pattern.steps).fill(null);
        return touch();
      }
      case "channel_set_sample": {
        const path = String(args?.path);
        const name = (path.split("/").pop() ?? path).replace(/\.[^.]+$/, "");
        view.channels = view.channels.map((c) =>
          c.id === args?.channel ? { ...c, missing: false, sampleName: name } : c,
        );
        return touch();
      }
      case "channel_preview":
      case "channel_select":
      case "channel_note":
        return null;
      case "pattern_notes":
        return encodeNotes(view.notesRevision, notes);
      case "notes_edit":
        return editNotes(args?.edit as NotesEdit);
      default:
        throw new Error(`no fake for ${cmd}`);
    }
  }

  return {
    calls,
    invoke,
    refuse,
    get view() {
      return view;
    },
    get notes() {
      return notes;
    },
    set notes(next: WireNote[]) {
      notes = next;
    },
    /** The edits sent, in order. */
    edits: () => calls.filter((c) => c.cmd === "project_edit").map((c) => c.args?.edit as Edit),
    /** The calls of one command, their arguments in order. */
    argsOf: (cmd: string) => calls.filter((c) => c.cmd === cmd).map((c) => c.args),
  };
}

export type FakeEngine = ReturnType<typeof createFakeEngine>;

/** A channel with sensible defaults. */
export function channel(id: number, name: string, extra: Partial<ChannelView> = {}): ChannelView {
  return {
    id,
    name,
    color: (id - 1) % 6,
    sampleName: `${name} 01`,
    missing: false,
    sampleFrames: 48_000,
    volume: 1,
    pan: 0,
    mute: false,
    solo: false,
    sampler: {},
    kind: "sampler",
    synth: {},
    preset: null,
    choke: 0,
    roll: false,
    ...extra,
  };
}

/** A project with these channels and, per channel id, these cells (the rest empty). */
export function project(
  channels: ChannelView[],
  cells: Record<number, (number | null)[]> = {},
  steps = 16,
): ProjectView {
  return {
    name: "Late Night Flip",
    path: null,
    dirty: false,
    bpm: 120,
    timeSignature: { beats: 4, unit: 4 },
    channels,
    pattern: {
      id: 1,
      name: "Hook",
      steps,
      swing: 0,
      cells: Object.fromEntries(
        channels.map((c) => [
          String(c.id),
          Array.from({ length: steps }, (_, i) => cells[c.id]?.[i] ?? null),
        ]),
      ),
    },
    autosavedAt: null,
    key: null,
    notesRevision: 1,
    recording: false,
    canUndo: false,
    canRedo: false,
  };
}
