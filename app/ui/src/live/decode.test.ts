import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test } from "vitest";
import { decodeLive } from "./decode";

/** The shared fixture, copied into a fresh buffer: `readFileSync` may hand back a view into a
 * pooled ArrayBuffer at a non-zero offset. */
function fixture(): ArrayBuffer {
  const path = fileURLToPath(new URL("../../../../tests/fixtures/live-v1.bin", import.meta.url));
  return new Uint8Array(readFileSync(path)).buffer;
}

/** A v1 frame built by hand (the layout `encode_live` documents). */
function frame(fields: { flags?: number; frames?: bigint; peak?: [number, number] }): ArrayBuffer {
  const buf = new ArrayBuffer(32);
  const v = new DataView(buf);
  v.setUint8(0, 1);
  v.setUint8(1, fields.flags ?? 0);
  v.setFloat32(4, fields.peak?.[0] ?? 0, true);
  v.setFloat32(8, fields.peak?.[1] ?? 0, true);
  v.setBigUint64(12, fields.frames ?? 0n, true);
  return buf;
}

test("decodes the shared v1 fixture", () => {
  const f = decodeLive(fixture())!;
  expect(f).toEqual({
    peak: [0.5, 0.25],
    playing: true,
    tone: false,
    frames: 123456789n,
    faults: 2,
    guardHits: 3,
    outputLatencyFrames: 7680,
  });
  expect(decodeLive(new Uint8Array(32).fill(2).buffer)).toBeNull();
});

test("reads the tone bit on its own", () => {
  const f = decodeLive(frame({ flags: 0b10 }))!;
  expect([f.playing, f.tone]).toEqual([false, true]);
  const both = decodeLive(frame({ flags: 0b11 }))!;
  expect([both.playing, both.tone]).toEqual([true, true]);
});

test("keeps a frame count above 2^32 exact", () => {
  const big = (1n << 40n) + 12345n;
  expect(decodeLive(frame({ frames: big }))!.frames).toBe(big);
  const max = (1n << 64n) - 1n;
  expect(decodeLive(frame({ frames: max }))!.frames).toBe(max);
});

test("rejects a wrong length even when the version byte and content are valid", () => {
  // Both buffers carry the valid v1 fixture bytes, so only the length check can reject them.
  const valid = new Uint8Array(fixture());
  expect(valid[0]).toBe(1);
  expect(decodeLive(valid.slice(0, 31).buffer)).toBeNull();
  const long = new Uint8Array(33);
  long.set(valid);
  expect(decodeLive(long.buffer)).toBeNull();
  // Control: the same bytes at the right length decode.
  expect(decodeLive(valid.slice().buffer)).not.toBeNull();
});
