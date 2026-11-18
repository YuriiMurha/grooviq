// Where everything an editor sends goes: the engine (live values) and the project (edits: a
// parameter, a preset, anything later), for the project the editor was opened in only. It is
// the standard device edit path. Lint checks resolved value dependencies and the send type
// prevents accidental substitution of raw apply; neither is a sandbox against casts or eval.
// Runtime generation checks also refuse queued edits after project replacement.

import type { Edit } from "../api/project";
import { isProjectReplacing, projectGeneration } from "../project/replacement";
import { LiveCoalescer } from "./controls/paramDrag";
import type { GestureSink } from "./paramGestures";

const guardedEdit: unique symbol = Symbol("guarded device edit");
export type GuardedEdit = ((edit: Edit) => Promise<boolean>) & { readonly [guardedEdit]: true };

export interface SinkDeps {
  channel: number;
  /** Sends a live value to the engine (`channelParamLive`). */
  sendLive: (id: string, value: number) => Promise<unknown>;
  /** Queues a project edit; `still` is asked again when the edit's turn comes. */
  apply: (edit: Edit, still: () => boolean) => Promise<boolean>;
  stored: (id: string) => number;
  shown: (moving: Readonly<Record<string, number>>) => void;
  onError: (message: string) => void;
  /** For tests: the animation-frame scheduler of the live coalescer. */
  schedule?: (cb: () => void) => number;
  unschedule?: (handle: number) => void;
}

export function editorSink(d: SinkDeps): {
  sink: GestureSink;
  live: LiveCoalescer;
  /** Sends any project edit under the same guard as the gestures' edits. */
  send: GuardedEdit;
} {
  const generation = projectGeneration();
  // The engine still holds the project this editor was opened in ...
  const ours = () => projectGeneration() === generation;
  // ... and nothing is about to replace it.
  const current = () => ours() && !isProjectReplacing();
  const report = (e: unknown) => d.onError(e instanceof Error ? e.message : String(e));
  // Checked now and again when the queue runs it (it may wait behind an Open).
  const send = (edit: Edit) => (current() ? d.apply(edit, current) : Promise.resolve(false));
  const live = new LiveCoalescer(
    (id, v) => {
      if (current()) d.sendLive(id, v).catch(report);
    },
    d.schedule,
    d.unschedule,
  );
  const sink: GestureSink = {
    stored: d.stored,
    live: (id, v) => {
      if (current()) live.push(id, v);
    },
    dropLive: (id) => live.settle(id),
    // Also while a replacement is on its way: the cancel that starts it puts the engine back on
    // the old project's value. Best effort: the channel may just have been removed.
    restore: (id, v) => {
      if (ours()) d.sendLive(id, v).catch(() => {});
    },
    edit: (id, v) => send({ kind: "setInstrumentParam", channel: d.channel, param: id, value: v }),
    shown: d.shown,
  };
  return { sink, live, send: Object.assign(send, { [guardedEdit]: true as const }) };
}
