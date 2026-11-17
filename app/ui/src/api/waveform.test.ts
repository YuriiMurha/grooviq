import { readFileSync } from "node:fs";
import { describe, it, expect } from "vitest";
import { decodeWaveform } from "./waveform";
const source = readFileSync(new URL("../../../../tests/fixtures/waveform-v1.bin", import.meta.url));
const fixture = () => Uint8Array.from(source).buffer;
describe("waveform binary", () => {
  it("decodes the Rust fixture with stereo signs and frame provenance", () => {
    const result = decodeWaveform(fixture());
    expect(result).toMatchObject({ asset: 11, generation: 9n, request: 10n, frames: 3, start: 0, end: 3, rate: 8000, channels: 2, bins: 2 });
    expect([...result.peaks]).toEqual([-1,-1,1,1,0.25,0.5,-0.5,-0.25]);
  });
  it("rejects every truncation and trailing data", () => {
    for (let length = 0; length < source.length; length++) expect(() => decodeWaveform(fixture().slice(0,length))).toThrow();
    expect(() => decodeWaveform(new ArrayBuffer(source.length+1))).toThrow();
  });
  it("rejects malformed sizes, ranges, nonfinite and inverted peaks", () => {
    for (const [offset,value] of [[0,0],[48,7999],[52,1],[56,16385],[60,1],[64,0x7f800000],[64,0x3f800000]]) {
      const packet=fixture(); new DataView(packet).setUint32(offset,value,true);
      expect(() => decodeWaveform(packet)).toThrow();
    }
    for (const [offset,value] of ([[24,0n],[32,4n],[40,4n],[24,2n**63n]] satisfies [number, bigint][])) {
      const packet=fixture();new DataView(packet).setBigUint64(offset,value,true);expect(() => decodeWaveform(packet)).toThrow();
    }
  });
});

it("rejects an otherwise valid packet beyond the independent bin cap", () => {
  const packet = new ArrayBuffer(64 + 16385 * 16);
  new Uint8Array(packet).set(new Uint8Array(fixture(), 0, 64));
  const header = new DataView(packet);
  header.setBigUint64(24, 20000n, true);
  header.setBigUint64(40, 20000n, true);
  header.setUint32(56, 16385, true);
  expect(() => decodeWaveform(packet)).toThrow();
});
