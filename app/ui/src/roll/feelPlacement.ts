export type FeelRect = Readonly<{ left: number; top: number; right: number; bottom: number }>;
export type FeelSize = Readonly<{ width: number; height: number }>;
export type FeelPlacement = Readonly<{
  left: number;
  top: number;
  maxWidth: number;
  maxHeight: number;
  side: "below" | "above";
}>;

/** Six pixels separate the summary from its panel; oversize panels scroll within the usable box. */
export function placeFeelPanel(anchor: FeelRect, panel: FeelSize, usable: FeelRect): FeelPlacement {
  const maxWidth = Math.max(0, usable.right - usable.left);
  const belowTop = Math.min(usable.bottom, Math.max(usable.top, anchor.bottom + 6));
  const aboveBottom = Math.max(usable.top, Math.min(usable.bottom, anchor.top - 6));
  const below = Math.max(0, usable.bottom - belowTop);
  const above = Math.max(0, aboveBottom - usable.top);
  const side = panel.height <= below || (panel.height > above && below >= above) ? "below" : "above";
  const maxHeight = side === "below" ? below : above;
  return {
    left: Math.max(usable.left, Math.min(anchor.left, usable.right - Math.min(panel.width, maxWidth))),
    top: side === "below" ? belowTop : aboveBottom - Math.min(panel.height, maxHeight),
    maxWidth,
    maxHeight,
    side,
  };
}
