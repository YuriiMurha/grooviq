import { SPLIT_MAX, SPLIT_MIN } from "./panels";

export const NOTE_MIN = 112;
const RACK_MIN = 80;

export type PanelChrome = Readonly<{ rackHeader: number; firstRackRow: number; rackPadding: number;
  rollHeader: number; tools: number; velocity: number; splitter: number }>;
export type PanelLayoutInput = Readonly<{ viewportHeight: number; requestedRatio: number; chrome: PanelChrome }>;
export type PanelLayout = Readonly<{ contentHeight: number; rackHeight: number; rollHeight: number;
  lowerRatio: number; upperRatio: number; effectiveRatio: number }>;

const clamp = (value: number, lower: number, upper: number) => Math.min(upper, Math.max(lower, value));
function nonnegative(value: number) {
  if (!Number.isFinite(value) || value < 0) throw new RangeError("Panel geometry must be finite and nonnegative");
  return value;
}
/** Share conservative minima with single-pane contained scrolling. Measurements stay data. */
export function panelMinimums(chrome: PanelChrome): Readonly<{ rack: number; roll: number; splitter: number }> {
  for (const value of Object.values(chrome)) nonnegative(value);
  return {
    rack: Math.ceil(nonnegative(Math.max(RACK_MIN, chrome.rackHeader + chrome.firstRackRow + chrome.rackPadding))),
    roll: Math.ceil(nonnegative(chrome.rollHeader + chrome.tools + chrome.velocity + NOTE_MIN)),
    splitter: Math.ceil(chrome.splitter),
  };
}

/** Fit the displayed allocation without changing the caller's remembered ratio. */
export function fitPanelLayout({ viewportHeight, requestedRatio, chrome }: PanelLayoutInput): PanelLayout {
  nonnegative(viewportHeight);
  if (!Number.isFinite(requestedRatio)) throw new RangeError("The requested split must be finite");
  const minimum = panelMinimums(chrome);
  // Whole-pixel allocations keep WebKit's overflow clipping from trimming the final lane.
  // Round the available height down. Only the pane minima can require scrolling.
  const contentHeight = Math.max(Math.floor(viewportHeight), Math.ceil(Math.max(
    minimum.rack + minimum.splitter + minimum.roll,
    (minimum.roll + minimum.splitter) / (1 - SPLIT_MIN), minimum.rack / SPLIT_MAX)));
  if (!Number.isSafeInteger(contentHeight)) throw new RangeError("Panel content height must be a safe integer");
  // Round the available interval inward to preserve both the chrome and the split bounds.
  const lowerHeight = Math.ceil(Math.max(minimum.rack, contentHeight * SPLIT_MIN));
  const upperHeight = Math.floor(Math.min(contentHeight * SPLIT_MAX,
    contentHeight - minimum.splitter - minimum.roll));
  if (lowerHeight > upperHeight) throw new RangeError("No finite panel allocation interval");
  const preferredHeight = contentHeight * clamp(requestedRatio, SPLIT_MIN, SPLIT_MAX);
  const rackHeight = clamp(Math.round(preferredHeight), lowerHeight, upperHeight);
  return { contentHeight, rackHeight, rollHeight: contentHeight - minimum.splitter - rackHeight,
    lowerRatio: lowerHeight / contentHeight, upperRatio: upperHeight / contentHeight,
    effectiveRatio: rackHeight / contentHeight };
}

/** Only accepted input is stored; the UI ignores attempts which do not move the display. */
export function acceptedSplit(layout: PanelLayout, candidateRatio: number): number {
  if (!Number.isFinite(candidateRatio)) throw new RangeError("The split input must be finite");
  return clamp(candidateRatio, layout.lowerRatio, layout.upperRatio);
}
