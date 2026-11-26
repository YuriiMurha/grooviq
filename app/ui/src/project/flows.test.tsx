import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { LiveFrame } from "../live/client";

type Args = Record<string, unknown> | undefined;

// vi.mock is hoisted above the imports, so what the mocks share is created in vi.hoisted.
const mocks = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: Args }[],
  handlers: new Map<string, (args: Args) => unknown>(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  frame: { current: null as ((frame: unknown) => void) | null },
  open: vi.fn(),
  save: vi.fn(),
  setTitle: vi.fn(),
  currentView: null as ProjectView | null,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: async (cmd: string, args?: Args) => {
    mocks.calls.push({ cmd, args });
    const handler = mocks.handlers.get(cmd);
    if (!handler) throw new Error(`no mock for ${cmd}`);
    const result: unknown = await handler(args);
    const candidate = result && typeof result === "object" && "view" in result ? result.view : result;
    if (candidate && typeof candidate === "object" && "pattern" in candidate && "generation" in candidate) mocks.currentView = candidate as ProjectView;
    return result;
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, cb: (event: { payload: unknown }) => void) => {
    mocks.listeners.set(name, cb);
    return () => mocks.listeners.delete(name);
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open, save: mocks.save }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTitle: mocks.setTitle }),
}));
vi.mock("../live/client", () => ({
  connectLive: (_endpoint: unknown, onFrame: (frame: unknown) => void) => {
    mocks.frame.current = onFrame;
    return () => {};
  },
}));

import { App } from "../App";
import { encodeNotes } from "../api/notes";
import type { ProjectView } from "../api/project";

const VIEW: ProjectView = {
  name: "Late Night Flip",
  path: "/music/Late Night Flip.grooviq",
  dirty: true,
  bpm: 140,
  timeSignature: { beats: 4, unit: 4 },
  channels: [],
  pattern: { id: 1, name: "Hook", steps: 16, swing: 0, cells: {} },
  autosavedAt: null,
  key: null,
  notesRevision: 1,
  generation: 0,
  recording: false,
  canUndo: false,
  canRedo: false,
};
const CLEAN: ProjectView = { ...VIEW, dirty: false };
const UNTITLED: ProjectView = { ...VIEW, name: "Untitled", path: null };
const FRESH: ProjectView = { ...UNTITLED, dirty: false };

function reply(cmd: string, value: unknown) {
  mocks.handlers.set(cmd, typeof value === "function" ? (value as (a: Args) => unknown) : () => value);
}
const called = (cmd: string) => mocks.calls.filter((c) => c.cmd === cmd);
const names = () => mocks.calls.map((c) => c.cmd);
/** The project commands only, in order (the audio ones are noise here). */
const projectCalls = () =>
  names().filter((n) => /^(project_|app_exit|recovery_|transport_)/.test(n) && n !== "project_view" && n !== "recovery_status");

async function start(view: ProjectView = VIEW) {
  reply("project_view", view);
  const user = userEvent.setup();
  render(<App />);
  await screen.findByRole("button", { name: view.name });
  return user;
}

async function chooseFromMenu(user: ReturnType<typeof userEvent.setup>, view: ProjectView, item: string) {
  await user.click(screen.getByRole("button", { name: view.name }));
  await user.click(screen.getByRole("menuitem", { name: item }));
}

const emit = (name: string, payload: unknown) =>
  act(() => {
    mocks.listeners.get(name)?.({ payload });
  });

beforeEach(() => {
  // jsdom has no layout/ResizeObserver. These scenes prove project commands;
  // actual pane geometry is exercised by the browser fixture.
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
  mocks.calls.length = 0;
  mocks.handlers.clear();
  mocks.listeners.clear();
  mocks.frame.current = null;
  mocks.open.mockReset();
  mocks.save.mockReset();
  mocks.setTitle.mockReset();
  mocks.currentView = null;
  const patterns = () => {
    const view = mocks.currentView ?? VIEW;
    return { generation: view.generation, revision: view.patternRevision ?? 0, selected: view.pattern.id,
      arrangementRevision: 0, patterns: [{ id: view.pattern.id, name: view.pattern.name, revision: view.notesRevision, steps: view.pattern.steps }], view };
  };
  reply("pattern_list", patterns);
  // These project-flow scenes have an empty Pattern-mode arrangement; the picker reads it.
  reply("arrangement_view", () => ({ generation: (mocks.currentView ?? VIEW).generation, revision: 0,
    offset: 0, total: 0, clips: [], tracks: [], audioAssets: [], loopRegion: null, playMode: "pattern",
    songError: null, pendingRequests: [], warnings: [] }));
  reply("pattern_edit", (args: Args) => {
    if ((args?.edit as {kind?:string})?.kind !== "settle") throw new Error("This flow fixture only settles patterns.");
    return { status: "applied", state: patterns(), settlement: { recordingDisarmed: false, waiting: false } };
  });
  reply("audio_outputs", []);
  reply("audio_log", []);
  reply("audio_settings", { output: "default", guard: true });
  reply("live_endpoint", { port: 1, token: "t" });
  reply("recovery_status", null);
  reply("project_save", CLEAN);
  reply("project_new", FRESH);
  reply("project_save_as", (args: Args) => ({ ...UNTITLED, dirty: false, path: args?.path }));
  reply("project_edit", (args: Args) => ({ ...VIEW, bpm: (args?.edit as { bpm: number }).bpm }));
  reply("transport_stop", null);
  reply("app_exit", null);
  reply("pattern_notes", () => encodeNotes(1, []));
  reply("channel_select", null);
});

describe("new_project_with_unsaved_changes_asks_first", () => {
  it("Cancel does nothing", async () => {
    const user = await start();
    await chooseFromMenu(user, VIEW, "New");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(projectCalls()).toEqual([]);
  });

  it("Escape is Cancel: no new project, nothing saved, the dialog closes", async () => {
    const user = await start();
    await chooseFromMenu(user, VIEW, "New");
    await screen.findByRole("dialog", { name: "Save changes?" });
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(projectCalls()).toEqual([]);
    expect(names()).not.toContain("project_new");
  });

  it("Don't save starts the new project without saving", async () => {
    const user = await start();
    await chooseFromMenu(user, VIEW, "New");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Don't save" }));
    await screen.findByRole("button", { name: "Untitled" });
    expect(projectCalls()).toEqual(["project_new"]);
  });

  it("Save saves first, then starts the new project", async () => {
    const user = await start();
    await chooseFromMenu(user, VIEW, "New");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await screen.findByRole("button", { name: "Untitled" });
    expect(projectCalls()).toEqual(["project_save", "project_new"]);
  });

  it("Save stops the flow when the path prompt is cancelled", async () => {
    const user = await start(UNTITLED);
    mocks.save.mockResolvedValue(null);
    await chooseFromMenu(user, UNTITLED, "New");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(mocks.save).toHaveBeenCalled());
    expect(projectCalls()).toEqual([]);
  });

  it("a clean project does not ask", async () => {
    const user = await start(CLEAN);
    await chooseFromMenu(user, CLEAN, "New");
    await screen.findByRole("button", { name: "Untitled" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(projectCalls()).toEqual(["project_new"]);
  });

  it("Open asks the same question, then opens with one toast per warning and never plays", async () => {
    const user = await start();
    mocks.open.mockResolvedValue("/music/other.grooviq");
    reply("project_open", {
      view: { ...CLEAN, name: "other", path: "/music/other.grooviq" },
      warnings: ["Kick: the sample is missing.", "Snare: the sample is missing."],
    });
    await chooseFromMenu(user, VIEW, "Open…");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Don't save" }));
    await screen.findByRole("button", { name: "other" });
    expect(called("project_open")[0]?.args).toEqual({ path: "/music/other.grooviq" });
    const toasts = screen.getByRole("status", { name: "Messages" });
    expect(within(toasts).getByText("Kick: the sample is missing.")).toBeTruthy();
    expect(within(toasts).getByText("Snare: the sample is missing.")).toBeTruthy();
    expect(names()).not.toContain("transport_play");
  });

  it("stops the transport before replacing a project that is playing", async () => {
    const user = await start(CLEAN);
    act(() => mocks.frame.current?.({ playing: true, tone: false, positionTicks: 0n, peak: [0, 0], outputLatencyFrames: 0 }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Play" }).getAttribute("aria-pressed")).toBe("true"));
    await chooseFromMenu(user, CLEAN, "New");
    await screen.findByRole("button", { name: "Untitled" });
    expect(projectCalls()).toEqual(["transport_stop", "project_new"]);
  });
});

describe("close_requested_shows_the_same_prompt", () => {
  it("Cancel keeps the app open", async () => {
    const user = await start();
    emit("close-requested", null);
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(projectCalls()).toEqual([]);
  });

  it("Escape is Cancel: no app_exit, the dialog closes", async () => {
    const user = await start();
    emit("close-requested", null);
    await screen.findByRole("dialog", { name: "Save changes?" });
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(projectCalls()).toEqual([]);
    expect(names()).not.toContain("app_exit");
  });

  it("Don't save quits through app_exit", async () => {
    const user = await start();
    emit("close-requested", null);
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Don't save" }));
    await waitFor(() => expect(projectCalls()).toEqual(["app_exit"]));
  });

  it("Save saves, then quits", async () => {
    const user = await start();
    emit("close-requested", null);
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(projectCalls()).toEqual(["project_save", "app_exit"]));
  });

  it("a failed save does not quit", async () => {
    const user = await start();
    reply("project_save", () => {
      throw new Error("Disk full.");
    });
    emit("close-requested", null);
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await screen.findByText(/Disk full\./);
    expect(projectCalls()).toEqual(["project_save"]);
  });
});

describe("a save that is refused because another is still writing", () => {
  const BUSY = "This project is already being saved. Try again in a moment.";

  it("shows one toast, keeps the app usable, and a later save works", async () => {
    const user = await start();
    let attempts = 0;
    reply("project_save", () => {
      attempts += 1;
      if (attempts === 1) throw new Error(BUSY);
      return CLEAN;
    });
    await user.keyboard("{Control>}s{/Control}");
    const toasts = screen.getByRole("status", { name: "Messages" });
    await within(toasts).findByText(BUSY);
    expect(within(toasts).getAllByText(BUSY)).toHaveLength(1);
    // Still dirty, still usable.
    expect(screen.getByText("Unsaved changes")).toBeTruthy();
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(screen.queryByText("Unsaved changes")).toBeNull());
    expect(called("project_save")).toHaveLength(2);
  });

  it("Save then quit does not quit when the save is refused", async () => {
    const user = await start();
    reply("project_save", () => {
      throw new Error(BUSY);
    });
    emit("close-requested", null);
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await screen.findByText(BUSY);
    expect(names()).not.toContain("app_exit");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("Save then New does not replace the project when the save is refused", async () => {
    const user = await start();
    reply("project_save", () => {
      throw new Error(BUSY);
    });
    await chooseFromMenu(user, VIEW, "New");
    const dialog = await screen.findByRole("dialog", { name: "Save changes?" });
    await user.click(within(dialog).getByRole("button", { name: "Save" }));
    await screen.findByText(BUSY);
    expect(names()).not.toContain("project_new");
  });
});

describe("save_on_an_untitled_project_asks_for_a_path", () => {
  it("Mod+S on an untitled project opens the save dialog and saves as the chosen path", async () => {
    const user = await start(UNTITLED);
    mocks.save.mockResolvedValue("/music/new.grooviq");
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(called("project_save_as")).toHaveLength(1));
    expect(called("project_save_as")[0]?.args).toEqual({ path: "/music/new.grooviq" });
    expect(names()).not.toContain("project_save");
    expect(mocks.save.mock.calls[0]?.[0]).toMatchObject({ defaultPath: "Untitled.grooviq" });
  });

  it("cancelling the dialog saves nothing", async () => {
    const user = await start(UNTITLED);
    mocks.save.mockResolvedValue(null);
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(mocks.save).toHaveBeenCalled());
    expect(projectCalls()).toEqual([]);
  });

  it("a saved project saves in place, and Mod+Shift+S always asks for a path", async () => {
    const user = await start();
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(called("project_save")).toHaveLength(1));
    expect(mocks.save).not.toHaveBeenCalled();
    mocks.save.mockResolvedValue("/music/copy.grooviq");
    await user.keyboard("{Control>}{Shift>}S{/Shift}{/Control}");
    await waitFor(() => expect(called("project_save_as")).toHaveLength(1));
    expect(called("project_save_as")[0]?.args).toEqual({ path: "/music/copy.grooviq" });
  });
});

describe("a held shortcut", () => {
  it("a held Mod+S (key repeat) saves once", async () => {
    await start();
    const press = async (repeat: boolean) => {
      await act(async () => {
        fireEvent.keyDown(window, { key: "s", ctrlKey: true, repeat });
      });
    };
    await press(false);
    await waitFor(() => expect(called("project_save")).toHaveLength(1));
    await waitFor(() => expect(screen.queryByText("Unsaved changes")).toBeNull());
    // Flush each repeat's asynchronous flow after the first save has completed.
    // Sending every repeat while busy would also pass with the repeat guard removed.
    for (let i = 0; i < 10; i++) {
      await press(true);
      expect(called("project_save")).toHaveLength(1);
    }
    fireEvent.keyUp(window, { key: "s", ctrlKey: true });
    await press(false);
    await waitFor(() => expect(called("project_save")).toHaveLength(2));
  });
});

describe("recovery_is_offered_at_start", () => {
  const SAVED_AT = 1_794_000_000;
  const d = new Date(SAVED_AT * 1000);
  const hhmm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;

  beforeEach(() => reply("recovery_status", { savedAt: SAVED_AT, original: null }));

  it("asks, and Restore loads the autosave with its warnings", async () => {
    reply("recovery_restore", {
      view: { ...VIEW, name: "Recovered" },
      warnings: ["Kick: the sample is missing."],
    });
    const user = await start();
    const dialog = await screen.findByRole("dialog");
    expect(dialog.textContent).toContain(
      `Grooviq closed unexpectedly. Restore the project autosaved at ${hhmm}?`,
    );
    await user.click(within(dialog).getByRole("button", { name: "Restore" }));
    await screen.findByRole("button", { name: "Recovered" });
    expect(called("recovery_restore")).toHaveLength(1);
    await screen.findByText("Kick: the sample is missing.");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("Discard throws the autosave away", async () => {
    reply("recovery_discard", null);
    const user = await start();
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("button", { name: "Discard" }));
    await waitFor(() => expect(called("recovery_discard")).toHaveLength(1));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(names()).not.toContain("recovery_restore");
  });

  it("is not dismissed by Escape (that would discard silently)", async () => {
    const user = await start();
    await screen.findByRole("dialog");
    await user.keyboard("{Escape}");
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(names()).not.toContain("recovery_discard");
  });

  it("does not ask when there is nothing to recover", async () => {
    reply("recovery_status", null);
    await start();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("export", () => {
  const RESULT = { frames: 96_000, seconds: 2, peakDb: -0.3, clipped: 0 };

  async function openExport(user: ReturnType<typeof userEvent.setup>) {
    await chooseFromMenu(user, VIEW, "Export WAV…");
    return screen.findByRole("dialog", { name: "Export WAV" });
  }

  it("export_shows_progress_and_a_clipping_warning", async () => {
    const user = await start();
    mocks.save.mockResolvedValue("/music/out.wav");
    let finish!: (r: typeof RESULT) => void;
    reply("project_export", () => new Promise((resolve) => (finish = resolve)));

    const dialog = await openExport(user);
    await user.click(within(dialog).getByRole("radio", { name: "32-bit float" }));
    await user.click(within(dialog).getByRole("radio", { name: "96 kHz" }));
    await user.clear(within(dialog).getByRole("spinbutton", { name: "Loops" }));
    await user.type(within(dialog).getByRole("spinbutton", { name: "Loops" }), "4");
    await user.click(within(dialog).getByRole("button", { name: "Export…" }));

    await waitFor(() => expect(called("project_export")).toHaveLength(1));
    expect(called("project_export")[0]?.args).toEqual({
      path: "/music/out.wav",
      options: { sampleRate: 96000, format: "float32", loops: 4 },
    });

    emit("export-progress", 0.42);
    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("42");

    await act(async () => finish({ frames: 1, seconds: 1, peakDb: 2.1, clipped: 1204 }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Exported, but 1,204 samples clipped (peak +2.1 dBFS). Lower the channel volumes and export again.",
    );
    expect(screen.queryByRole("progressbar")).toBeNull();
  });

  it("reports a clean export with its peak", async () => {
    const user = await start();
    mocks.save.mockResolvedValue("/music/out.wav");
    reply("project_export", RESULT);
    const dialog = await openExport(user);
    await user.click(within(dialog).getByRole("button", { name: "Export…" }));
    const status = await within(dialog).findByText("Exported. Peak -0.3 dBFS.");
    expect(status.getAttribute("role")).toBe("status");
    expect(called("project_export")[0]?.args).toEqual({
      path: "/music/out.wav",
      options: { sampleRate: 48000, format: "pcm24", loops: 1 },
    });
  });

  it("Cancel during an export calls project_export_cancel", async () => {
    const user = await start();
    mocks.save.mockResolvedValue("/music/out.wav");
    let fail!: (e: Error) => void;
    reply("project_export", () => new Promise((_, reject) => (fail = reject)));
    reply("project_export_cancel", null);
    const dialog = await openExport(user);
    await user.click(within(dialog).getByRole("button", { name: "Export…" }));
    await user.click(await within(dialog).findByRole("button", { name: "Cancel export" }));
    expect(called("project_export_cancel")).toHaveLength(1);
    await act(async () => fail(new Error("Export cancelled.")));
    // A cancel is the person's own doing: no error toast, back to the options.
    expect(within(dialog).queryByRole("progressbar")).toBeNull();
    expect(screen.queryByText("Export cancelled.")).toBeNull();
    expect(within(dialog).getByRole("button", { name: "Export…" })).toBeTruthy();
  });

  it("shows why an export failed", async () => {
    const user = await start();
    mocks.save.mockResolvedValue("/music/out.wav");
    reply("project_export", () => {
      throw new Error("Could not write the file.");
    });
    const dialog = await openExport(user);
    await user.click(within(dialog).getByRole("button", { name: "Export…" }));
    expect((await within(dialog).findByRole("alert")).textContent).toBe("Could not write the file.");
  });

  it("asks nothing when the file prompt is cancelled", async () => {
    const user = await start();
    mocks.save.mockResolvedValue(null);
    const dialog = await openExport(user);
    await user.click(within(dialog).getByRole("button", { name: "Export…" }));
    await waitFor(() => expect(mocks.save).toHaveBeenCalled());
    expect(called("project_export")).toHaveLength(0);
  });

  it("opens with Mod+R", async () => {
    const user = await start();
    await user.keyboard("{Control>}r{/Control}");
    await screen.findByRole("dialog", { name: "Export WAV" });
  });
});

describe("the_window_title_marks_unsaved_changes", () => {
  it("puts a bullet in front while there are unsaved changes", async () => {
    const user = await start();
    expect(document.title).toBe("• Late Night Flip — Grooviq");
    await waitFor(() => expect(mocks.setTitle).toHaveBeenLastCalledWith("• Late Night Flip — Grooviq"));
    await user.keyboard("{Control>}s{/Control}");
    await waitFor(() => expect(document.title).toBe("Late Night Flip — Grooviq"));
    expect(mocks.setTitle).toHaveBeenLastCalledWith("Late Night Flip — Grooviq");
  });
});

describe("the save state", () => {
  it("says Unsaved changes while dirty and Saved N s ago once clean", async () => {
    await start();
    expect(screen.getByText("Unsaved changes")).toBeTruthy();
  });

  it("says Saved N s ago for a clean project", async () => {
    await start(CLEAN);
    expect(screen.getByText(/^Saved (just now|\d+ s ago)$/)).toBeTruthy();
  });

  it("says a project that was never saved has not been", async () => {
    await start(FRESH);
    expect(screen.getByText("Not saved yet")).toBeTruthy();
  });
});

describe("bpm_field_accepts_typing_and_clamps_to_20_400", () => {
  async function field() {
    const user = await start();
    return { user, input: screen.getByRole("textbox", { name: "BPM" }) as HTMLInputElement };
  }
  const tempoEdits = () => called("project_edit").map((c) => c.args?.edit);

  it("shows two decimals", async () => {
    const { input } = await field();
    expect(input.value).toBe("140.00");
  });

  it("commits typed text on Enter", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.type(input, "128.5{Enter}");
    expect(tempoEdits()).toEqual([{ kind: "setTempo", bpm: 128.5 }]);
    await waitFor(() => expect(input.value).toBe("128.50"));
  });

  it("clamps above 400 and below 20", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.type(input, "500{Enter}");
    await user.clear(input);
    await user.type(input, "5{Enter}");
    expect(tempoEdits()).toEqual([
      { kind: "setTempo", bpm: 400 },
      { kind: "setTempo", bpm: 20 },
    ]);
  });

  it("rounds to two decimals", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.type(input, "128.456{Enter}");
    expect(tempoEdits()).toEqual([{ kind: "setTempo", bpm: 128.46 }]);
  });

  it("puts the old value back for an empty field (it must not become 20 BPM)", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.keyboard("{Enter}");
    expect(tempoEdits()).toEqual([]);
    expect(input.value).toBe("140.00");
    // Leaving an emptied field does the same.
    await user.clear(input);
    await user.tab();
    expect(tempoEdits()).toEqual([]);
    expect(input.value).toBe("140.00");
  });

  it("puts the old value back for text that is not a number", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.type(input, "fast{Enter}");
    expect(tempoEdits()).toEqual([]);
    expect(input.value).toBe("140.00");
  });

  it("commits on blur, and Escape gives up", async () => {
    const { user, input } = await field();
    await user.clear(input);
    await user.type(input, "100");
    await user.tab();
    expect(tempoEdits()).toEqual([{ kind: "setTempo", bpm: 100 }]);
    await user.clear(input);
    await user.type(input, "90{Escape}");
    expect(tempoEdits()).toHaveLength(1);
  });

  it("does not send an edit when the value did not change", async () => {
    const { user, input } = await field();
    await user.click(input);
    await user.tab();
    expect(tempoEdits()).toEqual([]);
  });

  it("changes with the arrow keys", async () => {
    const { user, input } = await field();
    input.focus();
    await user.keyboard("{ArrowUp}");
    expect(tempoEdits()).toEqual([{ kind: "setTempo", bpm: 141 }]);
  });
});

describe("the position", () => {
  it("follows the live frames, shifted by the output latency", async () => {
    reply("audio_outputs", [
      { id: "d", name: "Out", kind: "builtIn", isDefault: true, current: true, sampleRate: 48000 },
    ]);
    await start(CLEAN);
    const frame: Partial<LiveFrame> = {
      playing: true,
      tone: false,
      peak: [0, 0],
      // Bar 9, beat 3, step 2 at 4/4, plus 20 ms of output latency at 140 BPM (≈ 44.8 ticks).
      positionTicks: 8n * 3840n + 2n * 960n + 240n + 45n,
      outputLatencyFrames: 960,
    };
    act(() => mocks.frame.current?.(frame));
    await waitFor(() => expect(screen.getByLabelText("Position").textContent).toBe("9.3.02"));
  });
});

describe("transport", () => {
  it("Play and Stop call the engine", async () => {
    const user = await start(CLEAN);
    reply("transport_play", null);
    await user.click(screen.getByRole("button", { name: "Play" }));
    await user.click(screen.getByRole("button", { name: "Stop" }));
    expect(projectCalls()).toEqual(["transport_play", "transport_stop"]);
  });

  it("Space toggles play and stop, but not while typing", async () => {
    const user = await start(CLEAN);
    reply("transport_play", null);
    await user.keyboard(" ");
    await waitFor(() => expect(called("transport_play")).toHaveLength(1));
    await waitFor(() => expect(screen.getByRole("button", { name: "Play" }).getAttribute("aria-pressed")).toBe("true"));
    await user.keyboard(" ");
    await waitFor(() => expect(called("transport_stop")).toHaveLength(1));

    const bpm = screen.getByRole("textbox", { name: "BPM" });
    await user.click(bpm);
    await user.keyboard(" ");
    expect(called("transport_play")).toHaveLength(1);
  });

  it("Space plays after a tempo is typed and Enter leaves the field", async () => {
    const user = await start(CLEAN);
    reply("transport_play", null);
    const bpm = screen.getByRole("textbox", { name: "BPM" });
    await user.click(bpm);
    await user.keyboard(" ");
    expect(called("transport_play")).toHaveLength(0);
    await user.clear(bpm);
    await user.type(bpm, "128{Enter}");
    expect(called("project_edit").map((c) => c.args?.edit)).toEqual([{ kind: "setTempo", bpm: 128 }]);
    expect(document.activeElement).not.toBe(bpm);
    await user.keyboard(" ");
    await waitFor(() => expect(called("transport_play")).toHaveLength(1));
  });

  it("Space plays after a pattern length is typed and Enter leaves the field", async () => {
    const user = await start(CLEAN);
    reply("transport_play", null);
    reply("project_edit", (args: Args) => ({
      ...CLEAN,
      pattern: { ...CLEAN.pattern, steps: (args?.edit as { steps: number }).steps },
    }));
    const length = screen.getByRole("spinbutton", { name: "Pattern length" });
    await user.clear(length);
    await user.type(length, "32{Enter}");
    expect(called("project_edit").map((c) => c.args?.edit)).toEqual([{ kind: "setPatternLength", steps: 32 }]);
    expect(document.activeElement).not.toBe(length);
    await user.keyboard(" ");
    await waitFor(() => expect(called("transport_play")).toHaveLength(1));
  });

  it("Space plays while the swing slider has focus without editing swing", async () => {
    const user = await start(CLEAN);
    reply("transport_play", null);
    const swing = screen.getByRole("slider", { name: "Swing" }) as HTMLInputElement;
    act(() => swing.focus());
    await user.keyboard(" ");
    await waitFor(() => expect(called("transport_play")).toHaveLength(1));
    expect(swing.value).toBe("0");
    expect(called("project_edit")).toHaveLength(0);
  });

  it("shortcuts are off while a dialog is open", async () => {
    const user = await start();
    emit("close-requested", null);
    await screen.findByRole("dialog");
    await user.keyboard("{Control>}n{/Control}");
    expect(projectCalls()).toEqual([]);
  });
});
