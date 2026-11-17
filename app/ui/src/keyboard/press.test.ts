import { describe, expect, it, vi } from "vitest";
import { Press } from "./press";

describe("Press", () => {
  it("releases the press its note-on returned, even when the release comes first", async () => {
    let resolve!: (press: number) => void;
    const p = new Press(new Promise<number>((r) => (resolve = r)));
    const off = vi.fn(() => Promise.resolve(null));
    const done = p.release(off);
    expect(off).not.toHaveBeenCalled();
    resolve(7);
    await done;
    expect(off).toHaveBeenCalledExactlyOnceWith(7);
  });

  it("has nothing to release when the note-on failed, and reports the failure", async () => {
    const onError = vi.fn();
    const p = new Press(Promise.reject(new Error("full")), onError);
    const off = vi.fn(() => Promise.resolve(null));
    await p.release(off);
    expect(off).not.toHaveBeenCalled();
    expect(onError).toHaveBeenCalledOnce();
  });

  it("keeps two presses on one key apart", async () => {
    const first = new Press(Promise.resolve(1));
    const second = new Press(Promise.resolve(2));
    const off = vi.fn(() => Promise.resolve(null));
    await second.release(off);
    await first.release(off);
    expect(off.mock.calls).toEqual([[2], [1]]);
  });

  it("drops a failed release", async () => {
    const p = new Press(Promise.resolve(3));
    await expect(p.release(() => Promise.reject(new Error("gone")))).resolves.toBeUndefined();
  });
});
