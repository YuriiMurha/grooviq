import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { NOTES_MAGIC, decodeNotes, encodeNotes, type WireNote } from "./notes";

/** The Global Constraints fixture: revision 5 and three notes. */
const FIXTURE_NOTES: WireNote[] = [
  { id: 1, channel: 1, tick: 0, length: 240, velocity: 0.8, key: 60, slide: false },
  { id: 2, channel: 1, tick: 480, length: 960, velocity: 1.0, key: 63, slide: true },
  { id: 7, channel: 4, tick: 3600, length: 120, velocity: 0.25, key: 36, slide: false },
];

const fixture = () => {
  const bytes = readFileSync(new URL("../../../../tests/fixtures/notes-v1.bin", import.meta.url));
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
};

describe("notes wire format v1", () => {
  it("decodes_the_wire_fixture", () => {
    const buf = fixture();
    // Every byte offset, read independently of the decoder.
    expect(buf.byteLength).toBe(16 + 3 * 24);
    const v = new DataView(buf);
    expect(v.getUint32(0, true)).toBe(0x314e5147);
    expect(new TextDecoder().decode(new Uint8Array(buf, 0, 4))).toBe("GQN1");
    expect(v.getUint32(4, true)).toBe(5);
    expect(v.getUint32(8, true)).toBe(3);
    expect(v.getUint32(12, true)).toBe(0);
    const expected = [
      [1, 1, 0, 240, Math.fround(0.8), 60, 0],
      [2, 1, 480, 960, 1, 63, 1],
      [7, 4, 3600, 120, 0.25, 36, 0],
    ];
    expected.forEach(([id, channel, tick, length, velocity, key, flags], i) => {
      const o = 16 + i * 24;
      expect(v.getUint32(o, true)).toBe(id);
      expect(v.getUint32(o + 4, true)).toBe(channel);
      expect(v.getUint32(o + 8, true)).toBe(tick);
      expect(v.getUint32(o + 12, true)).toBe(length);
      expect(v.getFloat32(o + 16, true)).toBe(velocity);
      expect(v.getUint8(o + 20)).toBe(key);
      expect(v.getUint8(o + 21)).toBe(flags);
      expect(v.getUint8(o + 22)).toBe(0);
      expect(v.getUint8(o + 23)).toBe(0);
    });

    const decoded = decodeNotes(buf);
    expect(decoded.revision).toBe(5);
    expect(decoded.notes).toEqual(
      FIXTURE_NOTES.map((n) => ({ ...n, velocity: Math.fround(n.velocity) })),
    );
    // The encoder (used by tests and the mock) writes the same bytes, sorted by (tick, channel, id).
    const shuffled = [FIXTURE_NOTES[2], FIXTURE_NOTES[0], FIXTURE_NOTES[1]];
    expect(new Uint8Array(encodeNotes(5, shuffled))).toEqual(new Uint8Array(buf));
  });

  it("rejects_a_wrong_magic_and_a_short_buffer", () => {
    const good = encodeNotes(1, FIXTURE_NOTES);
    const wrong = good.slice(0);
    new DataView(wrong).setUint32(0, NOTES_MAGIC + 1, true);
    expect(() => decodeNotes(wrong)).toThrow(/not a notes buffer/);
    expect(() => decodeNotes(good.slice(0, 15))).toThrow(/too short/);
    // The header promises three notes; the body holds two and a half.
    expect(() => decodeNotes(good.slice(0, 16 + 2 * 24 + 12))).toThrow(/3 notes/);
    // An empty pattern is a header alone.
    expect(decodeNotes(encodeNotes(9, []))).toEqual({ revision: 9, notes: [] });
  });
});
