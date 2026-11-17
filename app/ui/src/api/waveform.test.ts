import { readFileSync } from "node:fs";
import { describe, it, expect } from "vitest";
import { decodeWaveform } from "./waveform";
const source = readFileSync(new URL("../../../../tests/fixtures/waveform-v1.bin", import.meta.url));
const fixture = () => Uint8Array.from(source).buffer;
function packet(bins: number) {
  const buffer = new ArrayBuffer(64 + bins * 16);
  new Uint8Array(buffer).set(new Uint8Array(fixture(), 0, 64));
  new DataView(buffer).setUint32(56, bins, true);
  return buffer;
}
describe("waveform binary", () => {
  it("decodes the Rust partial-window fixture with independent frame provenance", () => {
    const result = decodeWaveform(fixture());
    expect(result).toMatchObject({ asset: 11, generation: 9n, request: 10n, frames: 5, start: 1, end: 3, rate: 8000, channels: 2, bins: 2 });
    expect([...result.peaks]).toEqual([0.5, 0.5, -0.5, -0.5, 0.25, 0.25, -0.25, -0.25]);
  });
  it("rejects every truncation", () => {
    for (let length = 0; length < source.length; length++) expect(() => decodeWaveform(fixture().slice(0, length))).toThrow();
  });
  it("rejects trailing data on an otherwise valid packet", () => {
    const buffer = new ArrayBuffer(source.length + 1);
    new Uint8Array(buffer).set(source);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  const invalidFields: [string, number, number][] = [
    ["magic", 0, 0], ["lower rate", 48, 7999], ["upper rate", 48, 384001],
    ["channels", 52, 1], ["reserved word", 60, 1],
    ["left order", 64, 0x3f800000], ["right order", 72, 0x3f000000],
    ["positive infinity in maximum", 68, 0x7f800000],
    ["negative infinity in minimum", 64, 0xff800000], ["NaN", 64, 0x7fc00000],
  ];
  it.each(invalidFields)("rejects %s independently", (_name, offset, value) => {
    const buffer = fixture(); new DataView(buffer).setUint32(offset, value, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  it("rejects a duration beyond 600 seconds with JS-safe frame counts", () => {
    const buffer = fixture(); new DataView(buffer).setBigUint64(24, 4800001n, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  it("rejects end beyond source frames", () => {
    const buffer = fixture(); new DataView(buffer).setBigUint64(40, 6n, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  it("rejects bins exceeding the nonempty range with exact packet length", () => {
    const buffer = fixture(); new DataView(buffer).setBigUint64(40, 2n, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  it("rejects zero bins with exact packet length", () => {
    expect(() => decodeWaveform(packet(0))).toThrow();
  });
  it("rejects unsafe frame integers", () => {
    const buffer = fixture(); new DataView(buffer).setBigUint64(24, 2n ** 63n, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
  it("rejects an otherwise valid packet beyond the independent bin cap", () => {
    const buffer = packet(16385); const header = new DataView(buffer);
    header.setBigUint64(24, 20000n, true); header.setBigUint64(32, 0n, true); header.setBigUint64(40, 20000n, true);
    expect(() => decodeWaveform(buffer)).toThrow();
  });
});
