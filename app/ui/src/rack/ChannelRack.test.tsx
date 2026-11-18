import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createRef } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { channel, createFakeEngine, project, type FakeEngine } from "../test/fakeEngine";

type DropHandler = (event: { payload: { type: string; paths?: string[] } }) => void;

const mocks = vi.hoisted(() => ({
  engine: null as FakeEngine | null,
  open: vi.fn(),
  drop: { current: null as DropHandler | null },
  unlisten: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: Record<string, unknown>) => mocks.engine!.invoke(cmd, args),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: mocks.open }));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: async (handler: DropHandler) => {
      mocks.drop.current = handler;
      return mocks.unlisten;
    },
  }),
}));

import type { ProjectView } from "../api/project";
import type { LiveFrame } from "../live/decode";
import { Session } from "../test/Session";
import { ChannelRack } from "./ChannelRack";
import type { GridRenderer } from "./palette";

const AUDIO = ["wav", "aif", "aiff", "flac", "mp3", "ogg"];
const quiet = (): GridRenderer => ({ draw: () => {}, dispose: () => {} });

const KICK = channel(1, "Kick", { sampleName: "Punchy 03", volume: 0.7, pan: -0.2 });
const SNARE = channel(2, "Snare", { sampleName: "Crack Room" });

function setup(view: ProjectView) {
  mocks.engine = createFakeEngine(view);
  const errors: string[] = [];
  const user = userEvent.setup();
  const result = render(
    <Session onError={(m) => errors.push(m)}>
      {() => <ChannelRack latest={createRef<LiveFrame | null>()} sampleRate={48000} createRenderer={quiet} />}
    </Session>,
  );
  return { user, errors, engine: mocks.engine, ...result };
}

const rack = () => screen.findByRole("region", { name: "Channel rack" });

beforeEach(() => {
  mocks.open.mockReset();
  mocks.unlisten.mockReset();
  mocks.drop.current = null;
  vi.stubGlobal("requestAnimationFrame", () => 1);
  vi.stubGlobal("cancelAnimationFrame", () => {});
});

describe("ChannelRack", () => {
  it("an_empty_rack_shows_the_drop_hint", async () => {
    setup(project([]));
    const region = await rack();
    expect(within(region).getByText("Drop a sound here to make a channel")).toBeTruthy();
    expect(within(region).queryByLabelText("Step grid")).toBeNull();
    // With a channel the grid is there and the drop line stays as a reminder.
    mocks.engine = null;
  });

  it("rows show each channel's name and sample", async () => {
    setup(project([KICK, SNARE]));
    await rack();
    expect(screen.getByRole("button", { name: /^Kick/ }).textContent).toContain("Punchy 03");
    expect(screen.getByRole("button", { name: /^Snare/ }).textContent).toContain("Crack Room");
    expect(screen.getByLabelText("Step grid")).toBeTruthy();
    // Every control says what it does.
    for (const el of screen.getByRole("region", { name: "Channel rack" }).querySelectorAll("button, input, [role=slider], canvas")) {
      expect(el.closest("[data-hint]"), el.outerHTML).not.toBeNull();
    }
  });

  it("mute_and_solo_send_the_whole_mix", async () => {
    const { user, engine } = setup(project([KICK, SNARE]));
    await rack();
    await user.click(screen.getByRole("button", { name: "Mute Kick" }));
    await waitFor(() =>
      expect(engine.edits()).toEqual([
        { kind: "setChannelMix", channel: 1, volume: 0.7, pan: -0.2, mute: true, solo: false },
      ]),
    );
    await waitFor(() => expect(screen.getByRole("button", { name: "Mute Kick" }).getAttribute("aria-pressed")).toBe("true"));
    await user.click(screen.getByRole("button", { name: "Solo Kick" }));
    await waitFor(() => expect(engine.edits()).toHaveLength(2));
    expect(engine.edits()[1]).toEqual({ kind: "setChannelMix", channel: 1, volume: 0.7, pan: -0.2, mute: true, solo: true });
  });

  it("knobs_change_volume_and_pan_and_double_click_resets", async () => {
    const { user, engine } = setup(project([KICK]));
    await rack();
    const volume = screen.getByRole("slider", { name: "Kick volume" });
    const pan = screen.getByRole("slider", { name: "Kick pan" });
    expect(volume.getAttribute("aria-valuenow")).toBe("0.7");
    expect(pan.getAttribute("aria-valuetext")).toBe("20% left");

    act(() => volume.focus());
    await user.keyboard("{ArrowUp}");
    await waitFor(() => expect(engine.edits()).toHaveLength(1));
    expect(engine.edits()[0]).toMatchObject({ kind: "setChannelMix", channel: 1, pan: -0.2, mute: false, solo: false });
    expect((engine.edits()[0] as { volume: number }).volume).toBeCloseTo(0.75);

    act(() => pan.focus());
    await user.keyboard("{ArrowRight}");
    await waitFor(() => expect(engine.edits()).toHaveLength(2));
    expect((engine.edits()[1] as { pan: number }).pan).toBeCloseTo(-0.15);
    expect((engine.edits()[1] as { volume: number }).volume).toBeCloseTo(0.75);

    // A drag edits locally and sends one edit on release: 100 px is the whole range.
    fireEvent.pointerDown(volume, { clientY: 300, pointerId: 1, button: 0 });
    fireEvent.pointerMove(volume, { clientY: 280, pointerId: 1, buttons: 1 });
    fireEvent.pointerMove(volume, { clientY: 275, pointerId: 1, buttons: 1 });
    expect(Number(volume.getAttribute("aria-valuenow"))).toBeCloseTo(1.25);
    expect(engine.edits()).toHaveLength(2);
    fireEvent.pointerUp(volume, { clientY: 275, pointerId: 1 });
    await waitFor(() => expect(engine.edits()).toHaveLength(3));
    expect((engine.edits()[2] as { volume: number }).volume).toBeCloseTo(1.25);

    await user.dblClick(volume);
    await waitFor(() => expect(engine.edits().at(-1)).toMatchObject({ volume: 1 }));
    await user.dblClick(pan);
    await waitFor(() => expect(engine.edits().at(-1)).toMatchObject({ volume: 1, pan: 0 }));
    expect(volume.getAttribute("aria-valuetext")).toBe("0.0 dB");
  });

  it("pressing_a_channel_name_previews_it_and_release_stops", async () => {
    const { user, engine } = setup(project([KICK, SNARE]));
    await rack();
    const name = screen.getByRole("button", { name: /^Snare/ });
    // jsdom has no pointer capture; this test exercises the browser-supported path.
    name.setPointerCapture = () => {};
    await user.pointer({ keys: "[MouseLeft>]", target: name });
    await waitFor(() => expect(engine.argsOf("channel_preview")).toEqual([{ channel: 2, on: true }]));
    await user.pointer({ keys: "[/MouseLeft]", target: name });
    await waitFor(() =>
      expect(engine.argsOf("channel_preview")).toEqual([
        { channel: 2, on: true },
        { channel: 2, on: false, press: 1 },
      ]),
    );
    // From the keyboard: Enter held down previews, its release stops; a key repeat sends nothing.
    act(() => name.focus());
    fireEvent.keyDown(name, { key: "Enter" });
    fireEvent.keyDown(name, { key: "Enter", repeat: true });
    fireEvent.keyUp(name, { key: "Enter" });
    await waitFor(() => expect(engine.argsOf("channel_preview")).toHaveLength(4));
    expect(engine.argsOf("channel_preview").slice(2)).toEqual([
      { channel: 2, on: true },
      { channel: 2, on: false, press: 2 },
    ]);
    // Nothing else plays.
    expect(engine.calls.filter((c) => c.cmd.startsWith("transport_"))).toEqual([]);
  });

  it("a_missing_sample_is_marked", async () => {
    const { user, engine } = setup(project([channel(1, "Kick", { missing: true, sampleName: "Punchy 03" }), SNARE]));
    await rack();
    const row = screen.getByRole("group", { name: "Kick" });
    expect(within(row).getByText("Missing")).toBeTruthy();
    expect(within(screen.getByRole("group", { name: "Snare" })).queryByText("Missing")).toBeNull();

    // Relinking asks for a file; nothing is read until the person picks one.
    mocks.open.mockResolvedValueOnce(null);
    await user.click(within(row).getByRole("button", { name: "Relink Kick" }));
    await waitFor(() => expect(mocks.open).toHaveBeenCalledTimes(1));
    expect(engine.argsOf("channel_set_sample")).toEqual([]);
    expect(mocks.open.mock.calls[0][0]).toMatchObject({
      multiple: false,
      directory: false,
      filters: [{ name: "Audio", extensions: AUDIO }],
    });

    mocks.open.mockResolvedValueOnce("/kit/Punchy 03.wav");
    await user.click(within(row).getByRole("button", { name: "Relink Kick" }));
    await waitFor(() =>
      expect(engine.argsOf("channel_set_sample")).toEqual([{ channel: 1, path: "/kit/Punchy 03.wav", generation: 0 }]),
    );
    await waitFor(() => expect(within(row).queryByText("Missing")).toBeNull());
  });

  it("swing_and_pattern_length_send_their_edits", async () => {
    const { user, engine } = setup(project([KICK]));
    await rack();
    const swing = screen.getByRole("slider", { name: "Swing" });
    fireEvent.change(swing, { target: { value: "20" } });
    fireEvent.change(swing, { target: { value: "34" } });
    expect(screen.getByText("34%")).toBeTruthy();
    expect(engine.edits()).toEqual([]); // edited locally while dragging
    fireEvent.pointerUp(swing);
    await waitFor(() => expect(engine.edits()).toEqual([{ kind: "setSwing", swing: 0.34 }]));

    const length = screen.getByRole("spinbutton", { name: "Pattern length" });
    await user.clear(length);
    await user.type(length, "32{Enter}");
    await waitFor(() => expect(engine.edits().at(-1)).toEqual({ kind: "setPatternLength", steps: 32 }));
    await waitFor(() => expect((length as HTMLInputElement).value).toBe("32"));
    // Out of range is clamped; nonsense is ignored.
    await user.clear(length);
    await user.type(length, "999{Enter}");
    await waitFor(() => expect(engine.edits().at(-1)).toEqual({ kind: "setPatternLength", steps: 256 }));
    const count = engine.edits().length;
    await user.clear(length);
    await user.type(length, "{Enter}");
    await act(async () => {});
    expect(engine.edits()).toHaveLength(count);
    expect(screen.getByText("steps")).toBeTruthy();
  });

  it("add_channel_opens_an_audio_file_dialog", async () => {
    const { user, engine } = setup(project([]));
    await rack();
    mocks.open.mockResolvedValueOnce(["/kit/Kick.wav", "/kit/Hat.flac"]);
    await user.click(screen.getByRole("button", { name: "+ Add channel" }));
    await user.click(screen.getByRole("menuitem", { name: "Sample…" }));
    await waitFor(() => expect(engine.argsOf("channel_add")).toHaveLength(2));
    expect(mocks.open.mock.calls[0][0]).toMatchObject({
      multiple: true,
      directory: false,
      filters: [{ name: "Audio", extensions: AUDIO }],
    });
    expect(engine.argsOf("channel_add")).toEqual([{ path: "/kit/Kick.wav", generation: 0 }, { path: "/kit/Hat.flac", generation: 0 }]);
    await screen.findByRole("group", { name: "Hat" });
  });

  it("files_dropped_on_the_window_become_channels_in_drop_order", async () => {
    const { engine, errors, unmount } = setup(project([]));
    const region = await rack();
    await waitFor(() => expect(mocks.drop.current).not.toBeNull());
    engine.refuse.set("/kit/notes.txt", "Grooviq can't read notes.txt: it is not a sound file.");

    act(() => mocks.drop.current!({ payload: { type: "enter", paths: ["/kit/a.wav"] } }));
    expect(region.classList.contains("drop-over")).toBe(true);
    act(() => mocks.drop.current!({ payload: { type: "leave" } }));
    expect(region.classList.contains("drop-over")).toBe(false);

    act(() =>
      mocks.drop.current!({ payload: { type: "drop", paths: ["/kit/Kick.wav", "/kit/notes.txt", "/kit/Snare.wav"] } }),
    );
    await waitFor(() => expect(engine.argsOf("channel_add")).toHaveLength(3));
    expect(engine.argsOf("channel_add").map((a) => a?.path)).toEqual([
      "/kit/Kick.wav",
      "/kit/notes.txt",
      "/kit/Snare.wav",
    ]);
    await waitFor(() => expect(errors).toEqual(["Grooviq can't read notes.txt: it is not a sound file."]));
    await screen.findByRole("group", { name: "Snare" });
    expect(screen.getAllByRole("group").map((g) => g.getAttribute("aria-label"))).toEqual(["Kick", "Snare"]);
    unmount();
    expect(mocks.unlisten).toHaveBeenCalled();
  });
});
