import { describe, expect, test } from "vitest";
import { acceptedSplit, fitPanelLayout, NOTE_MIN, panelMinimums, type PanelChrome } from "./panelLayout";
import { SPLIT_MAX, SPLIT_MIN } from "./panels";

// Actual baseline observations: header/row/padding/lane/separator and each engine's
// font-dependent toolbar height. Production receives live DOM measurements.
const chrome: PanelChrome = Object.freeze({ rackHeader: 40, firstRackRow: 28, rackPadding: 16,
  rollHeader: 40, tools: 105.625, velocity: 72, splitter: 6 });
const noteRoom = (height: number, measured: PanelChrome) => height - measured.rollHeader - measured.tools - measured.velocity;

describe("measured panel layout", () => {
  test.each([105.625, 105.5625, 105.5859375])("compact fit preserves actual note space with fractional tools %s", (tools) => {
    const measured = { ...chrome, tools };
    const layout = fitPanelLayout({ viewportHeight: 472, requestedRatio: 0.5, chrome: measured });
    expect(layout.contentHeight).toBe(472);
    expect(layout.rackHeight).toBeGreaterThanOrEqual(84);
    expect(noteRoom(layout.rollHeight, measured)).toBeGreaterThanOrEqual(112);
    expect(layout.effectiveRatio).toBeLessThan(0.5);
    expect(layout.rackHeight + layout.rollHeight + measured.splitter).toBe(layout.contentHeight);
    expect(layout.effectiveRatio).toBe(layout.rackHeight / layout.contentHeight);
  });

  test("fractional measurements leave a whole-pixel roll at and above its minimum", () => {
    const measured = { ...chrome, tools: 106.0625 };
    const minimum = panelMinimums(measured);
    const fit = (requestedRatio: number) => fitPanelLayout({ viewportHeight: 472, requestedRatio, chrome: measured });
    expect.soft(minimum.roll).toBe(331);
    expect.soft(fit(0.5).rollHeight).toBe(331);
    expect(fit(0.253).rollHeight).toBe(347);
  });

  test("short viewports expand contained content instead of erasing notes", () => {
    const layout = fitPanelLayout({ viewportHeight: 272, requestedRatio: 0.5, chrome });
    expect(layout.contentHeight).toBe(420);
    expect(layout.rackHeight).toBe(84);
    expect(layout.rollHeight).toBe(330);
    expect(layout.lowerRatio).toBe(layout.upperRatio);
    expect(noteRoom(layout.rollHeight, chrome)).toBeGreaterThanOrEqual(NOTE_MIN);
  });

  test("single-panel minima include measured chrome and visible velocity", () => {
    expect(panelMinimums(chrome)).toEqual({ rack: 84, roll: 330, splitter: 6 });
    expect(panelMinimums({ ...chrome, velocity: 0 }).roll).toBe(258);
    expect(panelMinimums({ ...chrome, firstRackRow: 0, rackHeader: 0, rackPadding: 0 }).rack).toBe(80);
  });

  test("fit does not overwrite a requested preference when compacted", () => {
    const input = Object.freeze({ viewportHeight: 992, requestedRatio: 0.65, chrome });
    const large = fitPanelLayout(input);
    const compact = fitPanelLayout({ ...input, viewportHeight: 472 });
    const grown = fitPanelLayout(input);
    expect(compact.effectiveRatio).toBeLessThan(large.effectiveRatio);
    expect(grown).toEqual(large);
    expect(Math.abs(grown.rackHeight - grown.contentHeight * 0.65)).toBeLessThanOrEqual(0.5);
    expect(input.requestedRatio).toBe(0.65);
  });

  test.each([0, 1, 272, 472, 812, 4096])("global shares and measured minima stay feasible at height %s", (viewportHeight) => {
    for (const requestedRatio of [SPLIT_MIN, 0.5, 0.65, SPLIT_MAX]) {
      const layout = fitPanelLayout({ viewportHeight, requestedRatio, chrome });
      expect(layout.rackHeight).toBeGreaterThanOrEqual(84);
      expect(noteRoom(layout.rollHeight, chrome)).toBeGreaterThanOrEqual(112);
      expect(layout.lowerRatio).toBeGreaterThanOrEqual(SPLIT_MIN);
      expect(layout.upperRatio).toBeLessThanOrEqual(SPLIT_MAX);
      expect(layout.lowerRatio).toBeLessThanOrEqual(layout.effectiveRatio);
      expect(layout.effectiveRatio).toBeLessThanOrEqual(layout.upperRatio);
    }
  });

  test("large wrapped tools and large rack chrome respect both global bounds", () => {
    const tallTools = { ...chrome, tools: 1000 };
    const rollLimited = fitPanelLayout({ viewportHeight: 0, requestedRatio: 0.15, chrome: tallTools });
    expect(rollLimited.lowerRatio).toBeGreaterThanOrEqual(0.15);
    expect(noteRoom(rollLimited.rollHeight, tallTools)).toBeGreaterThanOrEqual(112);
    const tallRack = { ...chrome, rackHeader: 1000, firstRackRow: 1000, rackPadding: 0, tools: 0, rollHeader: 0, velocity: 0, splitter: 0 };
    const rackLimited = fitPanelLayout({ viewportHeight: 0, requestedRatio: 0.85, chrome: tallRack });
    expect(rackLimited.rackHeight).toBeGreaterThanOrEqual(2000);
    expect(rackLimited.upperRatio).toBeLessThanOrEqual(0.85);
    expect(noteRoom(rackLimited.rollHeight, tallRack)).toBeGreaterThanOrEqual(112);
  });

  test("rounding fractional chrome never spends the note minimum", () => {
    const measured = { rackHeader: 40.001, firstRackRow: 28.007, rackPadding: 16.002,
      rollHeader: 40.003, tools: 105.5859375, velocity: 72.006, splitter: 6.004 };
    const layout = fitPanelLayout({ viewportHeight: 300.001, requestedRatio: 0.85, chrome: measured });
    expect(layout.rackHeight).toBeGreaterThanOrEqual(measured.rackHeader + measured.firstRackRow + measured.rackPadding);
    expect(noteRoom(layout.rollHeight, measured)).toBeGreaterThanOrEqual(112);
    expect(layout.contentHeight - layout.rackHeight - layout.rollHeight).toBeGreaterThanOrEqual(measured.splitter);
  });

  test("accepted input uses feasible bounds rather than the remembered request", () => {
    const layout = Object.freeze(fitPanelLayout({ viewportHeight: 472, requestedRatio: 0.65, chrome }));
    expect(acceptedSplit(layout, 0.65)).toBe(layout.upperRatio);
    expect(acceptedSplit(layout, -1)).toBe(layout.lowerRatio);
    const middle = (layout.lowerRatio + layout.upperRatio) / 2;
    expect(acceptedSplit(layout, middle)).toBe(middle);
  });

  test("invalid observations cannot produce nonfinite CSS", () => {
    for (const viewportHeight of [-1, NaN, Infinity]) {
      expect(() => fitPanelLayout({ viewportHeight, requestedRatio: 0.5, chrome })).toThrow(RangeError);
    }
    expect(() => fitPanelLayout({ viewportHeight: 472, requestedRatio: NaN, chrome })).toThrow(RangeError);
    expect(() => fitPanelLayout({ viewportHeight: 472, requestedRatio: 0.5, chrome: { ...chrome, tools: -1 } })).toThrow(RangeError);
    expect(() => fitPanelLayout({ viewportHeight: Number.MAX_VALUE, requestedRatio: 0.5, chrome })).toThrow(RangeError);
    expect(() => acceptedSplit(fitPanelLayout({ viewportHeight: 472, requestedRatio: 0.5, chrome }), NaN)).toThrow(RangeError);
  });
});
