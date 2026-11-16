import { describe, expect, it } from "vitest";
import { FALLBACK, PALETTE_TOKENS, parseColor, readPalette } from "./palette";

/** An element whose computed style answers from this map. */
function rootWith(tokens: Record<string, string>): Element {
  const style = { getPropertyValue: (name: string) => tokens[name] ?? "" };
  const root = {} as Element;
  globalThis.getComputedStyle = ((el: Element) => {
    if (el !== root) throw new Error("read from another element");
    return style;
  }) as unknown as typeof getComputedStyle;
  return root;
}

/** Every token the palette reads, set to something valid. */
function allTokens(): Record<string, string> {
  return Object.fromEntries(PALETTE_TOKENS.map((t) => [t, t.startsWith("--reg") ? "1px" : "#000000"]));
}

describe("palette", () => {
  it("reads_hex_rgb_and_rgba_tokens", () => {
    expect(parseColor("#ff48b0")).toEqual([1, 0x48 / 255, 0xb0 / 255, 1]);
    expect(parseColor(" #fff ")).toEqual([1, 1, 1, 1]);
    expect(parseColor("#00000080")).toEqual([0, 0, 0, 0x80 / 255]);
    expect(parseColor("rgb(255, 0, 51)")).toEqual([1, 0, 0.2, 1]);
    expect(parseColor("rgba(243, 238, 228, 0.06)")).toEqual([243 / 255, 238 / 255, 228 / 255, 0.06]);
    expect(parseColor("rgb(255 0 0 / 50%)")).toEqual([1, 0, 0, 0.5]);
    expect(parseColor("var(--nope)")).toBeNull();
    expect(parseColor("#12")).toBeNull();
    expect(parseColor("")).toBeNull();

    const palette = readPalette(
      rootWith({
        ...allTokens(),
        "--ink-1": "#ff48b0",
        "--ink-1b": "rgb(47, 123, 216)",
        "--ink-6": "#9d7ad2",
        "--step-a": "#1a1e40",
        "--grid-playhead": "rgba(255, 72, 176, 0.5)",
        "--reg-x": " 2px",
        "--reg-y": "3px",
      }),
    );
    expect(palette.missing).toEqual([]);
    expect(palette.inks).toHaveLength(6);
    expect(palette.inks[0]).toEqual(parseColor("#ff48b0"));
    expect(palette.inks[5]).toEqual(parseColor("#9d7ad2"));
    expect(palette.inksB[0]).toEqual([47 / 255, 123 / 255, 216 / 255, 1]);
    expect(palette.stepA).toEqual(parseColor("#1a1e40"));
    expect(palette.playhead).toEqual([1, 72 / 255, 176 / 255, 0.5]);
    expect(palette.offset).toEqual([2, 3]);
  });

  it("a_missing_token_falls_back_and_says_which", () => {
    const tokens = allTokens();
    delete tokens["--ink-3"];
    tokens["--grid-focus"] = "not a colour";
    delete tokens["--reg-y"];
    const palette = readPalette(rootWith(tokens));
    expect(palette.missing).toEqual(["--ink-3", "--grid-focus", "--reg-y"]);
    expect(palette.inks[2]).toEqual(FALLBACK);
    expect(palette.focus).toEqual(FALLBACK);
    // The misregistration falls back to 2 px.
    expect(palette.offset).toEqual([1, 2]);
    // What was there is still read.
    expect(palette.inks[1]).toEqual([0, 0, 0, 1]);
  });
});
