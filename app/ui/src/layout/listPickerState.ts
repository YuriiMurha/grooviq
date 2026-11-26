export type ListPickerOption = {
  id: string;
  name: string;
  disabled?: boolean;
  description?: string;
};

type Highlight = { id: string; index: number };
export type ListPickerState = { highlight: Highlight | null } | null;

export type ListPickerAction =
  | { type: "open"; fromEnd?: boolean }
  | { type: "move"; direction: 1 | -1 }
  | { type: "edge"; fromEnd: boolean }
  | { type: "search"; key: string }
  | { type: "hover"; id: string }
  | { type: "choose"; id?: string }
  | { type: "dismiss" }
  | { type: "blur" }
  | { type: "options" };

type Transition = { state: ListPickerState; commit: string | null };

export function stepListPicker(
  state: ListPickerState,
  action: ListPickerAction,
  options: readonly ListPickerOption[],
  value: string | null,
): Transition {
  const stay = { state, commit: null };
  if (action.type === "dismiss" || action.type === "blur") return { state: null, commit: null };
  if (action.type === "open" ? state !== null : state === null) return stay;

  const enabled = options.flatMap((option, index) => option.disabled ? [] : [{ id: option.id, index }]);
  const current = enabled.findIndex(({ id }) => id === state?.highlight?.id);
  const highlight = (next: Highlight | undefined): Transition => {
    if (state && state.highlight?.id === next?.id && state.highlight?.index === next?.index) return stay;
    return { state: { highlight: next ?? null }, commit: null };
  };

  switch (action.type) {
    case "open":
      return highlight(enabled.find(({ id }) => id === value) ?? enabled.at(action.fromEnd ? -1 : 0));
    case "edge":
      return highlight(enabled.at(action.fromEnd ? -1 : 0));
    case "move": {
      if (!enabled.length) return stay;
      if (current < 0) return highlight(enabled.at(action.direction === 1 ? 0 : -1));
      return highlight(enabled[(current + action.direction + enabled.length) % enabled.length]);
    }
    case "search": {
      const start = current + 1;
      const key = action.key.toLocaleLowerCase();
      const matches = ({ index }: Highlight) => options[index].name.trimStart().toLocaleLowerCase().startsWith(key);
      return highlight(enabled.slice(start).find(matches) ?? enabled.slice(0, start).find(matches) ?? enabled[current]);
    }
    case "hover": {
      const target = enabled.find(({ id }) => id === action.id);
      return target ? highlight(target) : stay;
    }
    case "choose": {
      const target = action.id === undefined ? enabled[current] : enabled.find(({ id }) => id === action.id);
      if (!target) return stay;
      return { state: null, commit: target.id === value ? null : target.id };
    }
    case "options": {
      if (current >= 0) return highlight(enabled[current]);
      const index = state?.highlight?.index ?? 0;
      // Ties favor the earlier option since enabled is in display order.
      let nearest: Highlight | undefined;
      for (const option of enabled) {
        if (!nearest || Math.abs(option.index - index) < Math.abs(nearest.index - index)) nearest = option;
      }
      return highlight(nearest);
    }
  }
}

const VIEWPORT_INSET = 8;
const ANCHOR_GAP = 6;
const MAX_POPUP_HEIGHT = 272;

type Size = { width: number; height: number };
type Anchor = { left: number; right: number; top: number; bottom: number };
export type ListPickerAlign = "start" | "end";

export function placeListPicker(anchor: Anchor, popup: Size, viewport: Size, align: ListPickerAlign = "start") {
  const width = Math.min(popup.width, Math.max(0, viewport.width - 2 * VIEWPORT_INSET));
  const preferredLeft = align === "start" ? anchor.left : anchor.right - width;
  const left = Math.max(VIEWPORT_INSET, Math.min(preferredLeft, viewport.width - width - VIEWPORT_INSET));

  const top = Math.min(viewport.height - VIEWPORT_INSET, Math.max(VIEWPORT_INSET, anchor.bottom + ANCHOR_GAP));
  const bottom = Math.min(viewport.height - VIEWPORT_INSET, Math.max(VIEWPORT_INSET, viewport.height - anchor.top + ANCHOR_GAP));
  const below = viewport.height - top - VIEWPORT_INSET;
  const above = viewport.height - bottom - VIEWPORT_INSET;
  const flip = Math.min(popup.height, MAX_POPUP_HEIGHT) > below && above > below;
  const maxHeight = Math.max(0, Math.min(MAX_POPUP_HEIGHT, flip ? above : below));

  return flip ? { left, bottom, maxHeight } : { left, top, maxHeight };
}
