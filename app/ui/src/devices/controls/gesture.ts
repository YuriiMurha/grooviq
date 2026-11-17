import { useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import type { Gesture } from "../paramGestures";
import { dragNorm, fromNorm, toNorm, wheelValue, type ParamRange } from "./paramDrag";

/** How a control changes its parameter: a gesture from the editor's owner (live while it moves,
 * one commit or a cancel at its end), or a one-shot `set` (a key, a double-click). */
export interface ParamControl {
  begin: () => Gesture;
  set: (value: number) => void;
}

/** The gestures of one drag that moves several parameters (an envelope point, the filter curve):
 * each begins when the drag first moves it, and they all end together. */
export class GestureSet {
  private gestures = new Map<string, { gesture: Gesture; last: number }>();

  constructor(private readonly begin: (id: string) => Gesture) {}

  /** `false` once the editor ended the drag (Escape, focus lost, a preset): stop following it. */
  input(id: string, value: number): boolean {
    let g = this.gestures.get(id);
    if (!g) {
      g = { gesture: this.begin(id), last: value };
      this.gestures.set(id, g);
    }
    g.last = value;
    return g.gesture.input(value);
  }

  commit() {
    for (const g of this.gestures.values()) g.gesture.commit(g.last);
    this.gestures.clear();
  }

  cancel() {
    for (const g of this.gestures.values()) g.gesture.cancel();
    this.gestures.clear();
  }
}

/** A wheel gesture ends this long after its last event. */
const WHEEL_SETTLE_MS = 300;

/** The CSS scale an element is drawn at (a device window scales itself to fit). */
export function drawnScale(el: Element): number {
  const h = (el as HTMLElement).offsetHeight;
  return h > 0 ? el.getBoundingClientRect().height / h : 1;
}

/** Keeps a ref pointing at the latest value, for handlers that outlive a render. */
export function useLatest<T>(value: T): RefObject<T> {
  const ref = useRef(value);
  useLayoutEffect(() => {
    ref.current = value;
  });
  return ref;
}

/** A vertical drag (Shift for fine) and the wheel on one parameter. The drag keeps its own
 * unrounded position, so stepped values move after enough travel. A pointer cancel, or a capture
 * lost before the button came up, cancels the gesture; so does the editor (Escape, focus lost). */
export function useVerticalDrag(param: ParamRange, value: number, control: ParamControl, el: RefObject<HTMLElement | SVGSVGElement | null>) {
  const [dragging, setDragging] = useState(false);
  const drag = useRef<{ y: number; n: number; last: number; moved: boolean; scale: number; gesture: Gesture } | null>(null);
  const latest = useLatest({ param, value, control });

  const stop = () => {
    const d = drag.current;
    drag.current = null;
    setDragging(false);
    return d;
  };
  const onPointerDown = (e: ReactPointerEvent<Element>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    (e.currentTarget as HTMLElement).focus?.({ preventScroll: true });
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    drag.current?.gesture.cancel();
    drag.current = { y: e.clientY, n: toNorm(param, value), last: value, moved: false, scale: drawnScale(e.currentTarget), gesture: control.begin() };
    setDragging(true);
  };
  const onPointerMove = (e: ReactPointerEvent<Element>) => {
    const d = drag.current;
    if (!d) return;
    const dy = (d.y - e.clientY) / d.scale;
    d.y = e.clientY;
    d.n = dragNorm(param, d.n, dy, e.shiftKey);
    const v = fromNorm(param, d.n);
    if (v === d.last) return;
    d.last = v;
    d.moved = true;
    // The editor ended it (Escape, focus lost, a preset): stop following the pointer.
    if (!d.gesture.input(v)) stop();
  };
  const onPointerUp = () => {
    const d = stop();
    if (!d) return;
    if (d.moved) d.gesture.commit(d.last);
    else d.gesture.cancel();
  };
  const cancel = () => stop()?.gesture.cancel();

  // The wheel needs a listener that may prevent the page from scrolling: React's is passive. A
  // run of wheel events is one gesture, committed once it settles.
  useEffect(() => {
    const node = el.current as HTMLElement | null;
    if (!node) return;
    let wheel: { gesture: Gesture; value: number; timer: ReturnType<typeof setTimeout> | null } | null = null;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const { param: p, value: v, control: c } = latest.current;
      // A turn the editor already ended (Escape, focus lost, a preset) is over: start afresh.
      if (wheel && !wheel.gesture.alive()) {
        if (wheel.timer) clearTimeout(wheel.timer);
        wheel = null;
      }
      wheel ??= { gesture: c.begin(), value: v, timer: null };
      const next = wheelValue(p, wheel.value, e.deltaY || e.deltaX, e.shiftKey);
      if (next === wheel.value || !wheel.gesture.input(next)) return;
      wheel.value = next;
      if (wheel.timer) clearTimeout(wheel.timer);
      const mine = wheel;
      wheel.timer = setTimeout(() => {
        if (wheel === mine) wheel = null;
        mine.gesture.commit(mine.value);
      }, WHEEL_SETTLE_MS);
    };
    node.addEventListener("wheel", onWheel, { passive: false });
    return () => {
      node.removeEventListener("wheel", onWheel);
      // Unmounting never commits: the editor's owner cancels what is left.
      if (wheel?.timer) clearTimeout(wheel.timer);
      wheel?.gesture.cancel();
      wheel = null;
    };
  }, [el, latest]);

  return {
    dragging,
    pointer: {
      onPointerDown,
      onPointerMove,
      onPointerUp,
      onPointerCancel: cancel,
      // After a pointerup this finds nothing; without one (the element went away, the browser
      // took the pointer) it cancels.
      onLostPointerCapture: cancel,
    },
  };
}

/** The element's laid-out size in CSS px before any transform, kept up to date. */
export function useBoxSize(el: RefObject<Element | null>): { w: number; h: number } {
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const node = el.current as HTMLElement | null;
    if (!node) return;
    const read = () => setSize((s) => (s.w === node.clientWidth && s.h === node.clientHeight ? s : { w: node.clientWidth, h: node.clientHeight }));
    read();
    const ro = new ResizeObserver(read);
    ro.observe(node);
    return () => ro.disconnect();
  }, [el]);
  return size;
}
