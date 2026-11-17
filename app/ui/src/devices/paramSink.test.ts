import { afterEach, describe, expect, it } from "vitest";
import type { Edit } from "../api/project";
import { isProjectReplacing, projectReplaceFailed, projectReplaced, projectReplacing } from "../project/replacement";
import { editorSink } from "./paramSink";

function harness() {
  const sent: string[] = [];
  const queued: { edit: Edit; still: () => boolean }[] = [];
  const frames: (() => void)[] = [];
  const { sink, live, send } = editorSink({
    channel: 3,
    sendLive: async (id, v) => sent.push(`${id} ${v}`),
    apply: async (edit, still) => {
      queued.push({ edit, still });
      return true;
    },
    stored: () => 650,
    shown: () => {},
    onError: () => {},
    schedule: (cb) => frames.push(cb) - 1,
    unschedule: (h) => {
      frames[h] = () => {};
    },
  });
  const flush = () => {
    for (const f of frames.splice(0)) f();
  };
  return { sink, live, send, sent, queued, flush };
}

afterEach(() => {
  if (isProjectReplacing()) projectReplaceFailed();
});

describe("an editor's sink across a project replacement", () => {
  it("nothing_new_goes_out_while_a_replacement_is_on_its_way", async () => {
    const h = harness();
    projectReplacing();
    h.sink.live("cutoff", 900);
    h.flush();
    expect(await h.sink.edit("cutoff", 900)).toBe(false);
    expect(h.sent).toEqual([]);
    expect(h.queued).toEqual([]);
    // The cancel that starts it may still put the old project's engine back.
    h.sink.restore("cutoff", 650);
    expect(h.sent).toEqual(["cutoff 650"]);
  });

  it("a_failed_replacement_gives_the_editor_back", async () => {
    const h = harness();
    projectReplacing();
    projectReplaceFailed();
    h.sink.live("cutoff", 900);
    h.flush();
    expect(await h.sink.edit("cutoff", 900)).toBe(true);
    expect(h.sent).toEqual(["cutoff 900"]);
    expect(h.queued.map((q) => q.still())).toEqual([true]);
  });

  it("an_edit_queued_before_a_replacement_is_checked_again_when_it_runs", async () => {
    const h = harness();
    await h.sink.edit("cutoff", 900);
    const [q] = h.queued;
    expect(q.edit).toEqual({ kind: "setInstrumentParam", channel: 3, param: "cutoff", value: 900 });
    expect(q.still()).toBe(true);
    projectReplacing();
    expect(q.still()).toBe(false);
    projectReplaced();
    expect(q.still()).toBe(false);
  });

  it("after_a_replacement_nothing_goes_out_at_all", async () => {
    const h = harness();
    h.sink.live("cutoff", 900);
    projectReplacing();
    projectReplaced();
    // A live value pushed before, sent after: dropped at send time too.
    h.flush();
    h.sink.restore("cutoff", 650);
    expect(await h.sink.edit("cutoff", 1)).toBe(false);
    expect(h.sent).toEqual([]);
    expect(h.queued).toEqual([]);
  });
});

describe("every edit an editor sends", () => {
  it("a_preset_or_any_other_edit_takes_the_same_guard", async () => {
    const h = harness();
    const preset: Edit = { kind: "loadSynthPreset", channel: 3, preset: "Trap Lead" };
    expect(await h.send(preset)).toBe(true);
    expect(h.queued.map((q) => q.edit)).toEqual([preset]);
    // Checked again when it runs.
    projectReplacing();
    expect(h.queued[0].still()).toBe(false);
    // And refused while a replacement is on its way, or after it.
    expect(await h.send(preset)).toBe(false);
    projectReplaced();
    expect(await h.send({ kind: "setChoke", channel: 3, group: 2 })).toBe(false);
    expect(h.queued.length).toBe(1);
  });
});
