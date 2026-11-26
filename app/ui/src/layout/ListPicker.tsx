import { useId, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { placeListPicker, stepListPicker, type ListPickerAction, type ListPickerAlign, type ListPickerOption, type ListPickerState } from "./listPickerState";
import "./listPicker.css";

type Props = {
  label: string;
  options: readonly ListPickerOption[];
  value: string | null;
  placeholder: string;
  onCommit: (id: string) => void;
  disabled?: boolean;
  reason?: string;
  className?: string;
  align?: ListPickerAlign;
};

// Leave Space to the app's playback shortcut while the listbox is closed.
const openingKeys = ["Enter", "ArrowDown", "ArrowUp"];

function listAction(event: KeyboardEvent<HTMLDivElement>): ListPickerAction | undefined {
  switch (event.key) {
    case "ArrowDown": return { type: "move", direction: 1 };
    case "ArrowUp": return { type: "move", direction: -1 };
    case "Home": return { type: "edge", fromEnd: false };
    case "End": return { type: "edge", fromEnd: true };
    case "Escape": return { type: "dismiss" };
    case "Enter":
    case " ": return { type: "choose" };
    default:
      if (event.key.length === 1 && !event.ctrlKey && !event.altKey && !event.metaKey && !event.nativeEvent.isComposing) {
        return { type: "search", key: event.key };
      }
  }
}

/** A controlled single-choice button with an independent, temporary listbox highlight. */
export function ListPicker({ label, options, value, placeholder, onCommit, disabled, reason, className, align }: Props) {
  const listId = useId();
  const reasonId = `${listId}-reason`;
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<ListPickerState>(null);
  const open = state !== null;
  const activeIndex = state?.highlight?.index;
  const displayedName = options.find((option) => option.id === value)?.name ?? placeholder;
  const description = disabled ? reason : undefined;

  if (state) {
    const reconciled = stepListPicker(state, { type: "options" }, options, value).state;
    if (reconciled !== state) setState(reconciled);
  }

  useLayoutEffect(() => {
    const anchor = trigger.current;
    const list = popup.current;
    if (!open || disabled || !anchor || !list) return;

    const position = () => {
      const rect = anchor.getBoundingClientRect();
      const viewport = { width: document.documentElement.clientWidth, height: document.documentElement.clientHeight };
      list.style.minWidth = `min(${rect.width}px, 24rem, calc(100vw - 16px))`;
      const placement = placeListPicker(rect, { width: list.offsetWidth, height: list.scrollHeight + 2 }, viewport, align);
      list.style.left = `${placement.left}px`;
      list.style.top = placement.top === undefined ? "" : `${placement.top}px`;
      list.style.bottom = placement.bottom === undefined ? "" : `${placement.bottom}px`;
      list.style.maxHeight = `${placement.maxHeight}px`;
    };

    position();
    const observer = new ResizeObserver(position);
    observer.observe(anchor);
    window.addEventListener("resize", position);
    window.addEventListener("scroll", position, true);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", position);
      window.removeEventListener("scroll", position, true);
    };
  }, [align, disabled, open, options]);

  useLayoutEffect(() => {
    const list = popup.current;
    if (!open || !list) return;
    if (disabled) {
      // Return focus before hiding the focused listbox.
      if (document.activeElement === list) trigger.current?.focus({ preventScroll: true });
      // eslint-disable-next-line react-hooks/set-state-in-effect -- Close only after transferring DOM focus.
      setState(null);
    } else {
      list.focus({ preventScroll: true });
    }
  }, [disabled, open]);

  useLayoutEffect(() => {
    const list = popup.current;
    if (!list || !open || disabled || activeIndex === undefined) return;
    const row = list.children.item(activeIndex);
    if (!row) return;
    const bounds = list.getBoundingClientRect();
    const active = row.getBoundingClientRect();
    if (active.top < bounds.top) list.scrollTop += active.top - bounds.top;
    else if (active.bottom > bounds.bottom) list.scrollTop += active.bottom - bounds.bottom;
  }, [activeIndex, disabled, open, options]);

  useLayoutEffect(() => {
    if (!open || disabled) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (!popup.current?.contains(target) && !trigger.current?.contains(target)) setState(null);
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    return () => window.removeEventListener("pointerdown", onPointerDown, true);
  }, [disabled, open]);

  function act(action: ListPickerAction) {
    if (disabled) return;
    const result = stepListPicker(state, action, options, value);
    setState(result.state);
    if (open && !result.state && action.type !== "blur") trigger.current?.focus({ preventScroll: true });
    if (result.commit !== null) onCommit(result.commit);
  }

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={className ? `list-picker ${className}` : "list-picker"}
        aria-label={`${label}: ${displayedName}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listId}
        aria-disabled={disabled || undefined}
        aria-describedby={description ? reasonId : undefined}
        title={disabled ? description : displayedName}
        onPointerDown={(event) => {
          if (open && event.button === 0) event.preventDefault();
        }}
        onClick={() => act({ type: open ? "dismiss" : "open" })}
        onKeyDown={(event) => {
          if (!openingKeys.includes(event.key)) return;
          event.preventDefault();
          event.stopPropagation();
          if (!event.repeat) act({ type: "open", fromEnd: event.key === "ArrowUp" });
        }}
      >
        <span className="list-picker-value">{displayedName}</span>
        <span className="list-picker-chevron" aria-hidden="true" />
      </button>
      {description && <span id={reasonId} hidden>{description}</span>}
      <div
        ref={popup}
        id={listId}
        className="list-picker-popup"
        hidden={!open}
        role="listbox"
        aria-label={label}
        aria-activedescendant={activeIndex === undefined || !open ? undefined : `${listId}-${activeIndex}`}
        tabIndex={-1}
        onBlur={() => act({ type: "blur" })}
        onKeyDown={(event) => {
          const action = listAction(event);
          if (!action) return;
          event.preventDefault();
          event.stopPropagation();
          if (action.type !== "choose" || !event.repeat) act(action);
        }}
      >
        {open && options.map((option, index) => {
          const optionId = `${listId}-${index}`;
          return (
            <div
              key={option.id}
              id={optionId}
              className="list-picker-option"
              role="option"
              aria-label={option.name}
              aria-describedby={option.description ? `${optionId}-description` : undefined}
              aria-selected={option.id === value}
              aria-disabled={option.disabled || undefined}
              data-active={index === activeIndex || undefined}
              onPointerMove={(event) => {
                if (event.pointerType !== "touch" && !option.disabled && index !== activeIndex) {
                  act({ type: "hover", id: option.id });
                }
              }}
              onMouseDown={(event) => {
                // The listbox keeps focus through the pointer's click.
                if (event.button === 0) event.preventDefault();
              }}
              onClick={() => act({ type: "choose", id: option.id })}
            >
              <span>{option.name}</span>
              {option.description && <span id={`${optionId}-description`} className="list-picker-description">{option.description}</span>}
            </div>
          );
        })}
      </div>
    </>
  );
}
