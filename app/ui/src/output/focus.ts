/** The bit of an element the focus helpers need, so they can be tested without a DOM. */
export interface Focusable {
  focus(): void;
}

/** A container that can find its first focusable control. */
export interface FocusRoot extends Focusable {
  querySelector(selector: string): Focusable | null;
}

const FOCUSABLE =
  "input:not([disabled]), button:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex='-1'])";

/** Moves focus to the first focusable control inside `root`, or to `root` itself (give it
 * `tabIndex={-1}`) when it holds none. */
export function focusInto(root: FocusRoot): void {
  (root.querySelector(FOCUSABLE) ?? root).focus();
}

/** Gives focus back to `opener`, the element that had it when the dialog opened, unless
 * `movedElsewhere`: the user has since put focus on something outside the dialog (a click
 * elsewhere closes it), and that must not be stolen back. */
export function returnFocus(opener: Focusable | null, movedElsewhere: boolean): void {
  if (opener && !movedElsewhere) opener.focus();
}
