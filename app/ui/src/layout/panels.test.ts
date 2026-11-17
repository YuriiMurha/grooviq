import { describe, expect, it } from "vitest";
import { DEFAULT_PANELS, SPLIT_MAX, SPLIT_MIN, panelFor, rollTarget, splitAfterDrag, togglePanel } from "./panels";

describe("panels", () => {
  it("f6_and_f7_toggle_their_panels", () => {
    expect(DEFAULT_PANELS).toEqual({ rack: true, roll: false, split: 0.5 });
    expect(panelFor("F6")).toBe("rack");
    expect(panelFor("F7")).toBe("roll");
    expect(panelFor("F8")).toBeNull();
    const rollOpen = togglePanel(DEFAULT_PANELS, "roll");
    expect(rollOpen).toEqual({ rack: true, roll: true, split: 0.5 });
    expect(togglePanel(rollOpen, "roll")).toEqual(DEFAULT_PANELS);
    const rackClosed = togglePanel(rollOpen, "rack");
    expect(rackClosed).toEqual({ rack: false, roll: true, split: 0.5 });
    expect(togglePanel(rackClosed, "rack")).toEqual(rollOpen);
  });

  it("f7_opens_the_roll_for_the_selected_channel", () => {
    expect(rollTarget(3, [1, 3, 5])).toBe(3);
    // Nothing selected, or a channel that is gone: the first channel.
    expect(rollTarget(null, [1, 3, 5])).toBe(1);
    expect(rollTarget(9, [1, 3, 5])).toBe(1);
    expect(rollTarget(null, [])).toBeNull();
  });

  it("the_splitter_stays_inside_its_limits", () => {
    expect(splitAfterDrag(0.5, 100, 1000)).toBeCloseTo(0.6);
    expect(splitAfterDrag(0.5, -1000, 1000)).toBe(SPLIT_MIN);
    expect(splitAfterDrag(0.5, 1000, 1000)).toBe(SPLIT_MAX);
    expect(splitAfterDrag(0.4, 50, 0)).toBe(0.4);
  });
});
