import { describe, expect, it } from "vitest";
import { encodeArrangementBatch, validateStamp } from "./arrangement";

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
});
