import { describe, expect, it, vi } from "vitest";
import { arrangementCurve, arrangementLoop, audioImport, audioRelink, encodeArrangementBatch, validateAudioTicket, validateStamp } from "./arrangement";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

describe("arrangement input bounds", () => {
  it("rejects unsafe and negative generation/revision values before IPC", () => {
    for (const value of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => validateStamp({ generation: value, revision: 0 })).toThrow();
      expect(() => validateStamp({ generation: 0, revision: value })).toThrow();
    }
    expect(() => validateStamp({ generation: 0, revision: Number.MAX_SAFE_INTEGER })).not.toThrow();
  });
  it("bounds operation work and UTF-8 payload bytes", () => {
    expect(encodeArrangementBatch([{ kind: "setLoop", region: null }])).toBe('[{"kind":"setLoop","region":null}]');
    const operations = Array.from({ length: 100_001 }, () => ({ kind: "setLoop" as const, region: null }));
    expect(() => encodeArrangementBatch(operations)).toThrow("operation limit");
  });
  it("requires a positive safe integer for a media ticket", () => {
    for (const value of [0, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
      expect(() => validateAudioTicket(value)).toThrow("Invalid media ticket");
    }
    expect(() => validateAudioTicket(1)).not.toThrow();
    expect(() => validateAudioTicket(Number.MAX_SAFE_INTEGER)).not.toThrow();
  });
});

describe("arrangement IPC arguments", () => {
  const identity = { generation: 2, revision: 9 };
  const stamp = { ...identity, clips: [], audioAssets: [{ sample: { path: "/qa/audio/Extra.wav" } }] };
  const media = { track: 3, startTick: 960, name: "Break", path: "/qa/audio/Break.wav", ticket: 7 };

  it.each([
    { command: "arrangement_curve", send: () => arrangementCurve(stamp, 4), args: { clip: 4 } },
    { command: "arrangement_loop", send: () => arrangementLoop(stamp, null), args: { region: null } },
    { command: "audio_import", send: () => audioImport(stamp, media), args: media },
    { command: "audio_relink", send: () => audioRelink(stamp, 5, media.path, 7), args: { asset: 5, path: media.path, ticket: 7 } },
  ])("sends only stamp identity and command arguments for $command", ({ command, send, args }) => {
    invoke.mockClear();
    send();
    expect(invoke.mock.calls).toEqual([[command, { ...identity, ...args }]]);
  });
});
