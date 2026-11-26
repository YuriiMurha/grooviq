import { useLayoutEffect, useRef, useState, type RefObject } from "react";
import { availableSends, type MixerError, type MixerProps, type MixerTrack } from "./mixerModel";
import { classifyRoute, finishRoute, moveRoute, routeScrollDistance, sendCommand, type Point, type RouteEnd, type RouteGesture } from "./routing";

type Drag = {
  nodeId: string;
  anchor: HTMLButtonElement;
  pointerId: number;
  point: Point;
  targetId?: string;
  gesture: RouteGesture;
  listeners: AbortController;
  frame: number;
  suppressClick: () => void;
};

type Preview = {
  sourceId: string;
  targetId?: string;
  kind: ReturnType<typeof classifyRoute>["kind"];
  message: string;
  start: Point;
  end: Point;
};

type SendFocus = {
  nodeId: string;
  destinationId: string;
  anchor: HTMLElement | null;
  priorError: MixerError | null | undefined;
};

const EDGE = 24;
const pointOf = (event: PointerEvent): Point => ({ x: event.clientX, y: event.clientY });

function revealControl(control: HTMLElement | null | undefined) {
  if (!control) return;
  control.focus({ preventScroll: true });
  control.scrollIntoView({ block: "nearest", inline: "nearest", behavior: "instant" });
}

function focusSend(anchor: HTMLElement, sendId: string) {
  const rows = anchor.closest(".mx-strip")?.querySelectorAll<HTMLElement>("[data-mx-send]");
  const row = Array.from(rows ?? []).find((el) => el.dataset.mxSend === sendId);
  revealControl(row?.querySelector<HTMLElement>('[role="slider"]'));
}

export function useSendRouting(
  props: MixerProps,
  nodes: ReadonlyMap<string, MixerTrack>,
  bank: RefObject<HTMLDivElement | null>,
  root: RefObject<HTMLDivElement | null>,
) {
  const host = useRef({ props, nodes });
  const active = useRef<Drag | null>(null);
  const focusNext = useRef<SendFocus | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [notice, setNotice] = useState({ message: "", sequence: 0 });

  const [routing] = useState(() => {
    function announce(message: string) {
      setNotice((previous) => ({ message, sequence: previous.sequence + 1 }));
    }

    function targetAt({ x, y }: Point) {
      const strip = document.elementFromPoint(x, y)?.closest<HTMLElement>(".mx-strip[data-node]");
      return strip && root.current?.contains(strip) ? host.current.nodes.get(strip.dataset.node ?? "") ?? null : null;
    }

    function add(command: ReturnType<typeof sendCommand>, anchor: HTMLElement | null) {
      const { nodeId, destinationId } = command;
      const { nodes, props } = host.current;
      const source = nodes.get(nodeId);
      if (!source || source.pending) return;
      const request = { nodeId, destinationId, anchor, priorError: props.error };
      focusNext.current = request;
      const result = props.onCommand(command);
      if (result) {
        const settled = () => { if (focusNext.current === request) focusNext.current = null; };
        result.then(settled, settled);
      }
    }

    function finish(reason: RouteEnd, point?: Point) {
      const drag = active.current;
      if (!drag) return;
      active.current = null;
      if (reason !== "drop") drag.suppressClick();
      drag.listeners.abort();
      cancelAnimationFrame(drag.frame);
      if (drag.anchor.hasPointerCapture(drag.pointerId)) drag.anchor.releasePointerCapture(drag.pointerId);
      setPreview(null);

      const target = point ? targetAt(point) : null;
      const source = host.current.nodes.get(drag.nodeId);
      // Only the last previewed strip can receive the drop. Admission still comes from the current host.
      const route = finishRoute(drag.gesture, reason, source, target?.id === drag.targetId ? target : null);
      if (!route || route.kind === "idle") {
        if (drag.gesture.moving) announce("Send drag canceled.");
      } else if (route.kind === "ready") {
        add(route.command, drag.anchor);
      } else if (route.kind === "bad") {
        announce(`Route refused: ${route.message}`);
      } else {
        if (route.kind === "main") {
          revealControl(drag.anchor.closest(".mx-strip")?.querySelector<HTMLElement>(".mx-route-choice"));
        } else {
          focusSend(drag.anchor, route.sendId);
        }
        announce(route.kind === "existing" ? `${target?.name} send focused.` : route.message);
      }
    }

    function draw(drag: Drag) {
      const scroller = bank.current;
      const source = host.current.nodes.get(drag.nodeId);
      if (!scroller || !source || source.pending) {
        finish("pending");
        return;
      }
      const bounds = scroller.getBoundingClientRect();
      const anchor = drag.anchor.getBoundingClientRect();
      const target = targetAt(drag.point);
      drag.targetId = target?.id;
      const { kind, message } = classifyRoute(source, target);
      const start = { x: Math.max(bounds.left + 3, Math.min(bounds.right - 3, anchor.x + anchor.width / 2)), y: anchor.y + anchor.height / 2 };
      const end = drag.point;
      setPreview((previous) => previous?.sourceId === drag.nodeId && previous.targetId === target?.id &&
        previous.kind === kind && previous.message === message && previous.end === end &&
        previous.start.x === start.x && previous.start.y === start.y
        ? previous
        : { sourceId: drag.nodeId, targetId: target?.id, kind, message, start, end });
    }

    function scroll(drag: Drag, previousFrame: number, now: number) {
      const scroller = bank.current;
      if (active.current !== drag || !scroller) return;
      const { x, y } = drag.point;
      const bounds = scroller.getBoundingClientRect();
      if (y >= bounds.top && y < bounds.bottom && x >= bounds.left && x < bounds.right) {
        const distance = routeScrollDistance(now - previousFrame);
        if (x < bounds.left + EDGE) scroller.scrollLeft -= distance;
        else if (x > bounds.right - EDGE) scroller.scrollLeft += distance;
      }
      draw(drag);
      if (active.current === drag) drag.frame = requestAnimationFrame((next) => scroll(drag, now, next));
    }

    function begin(nodeId: string, anchor: HTMLButtonElement, event: PointerEvent, suppressClick: () => void) {
      const source = host.current.nodes.get(nodeId);
      if (active.current || !source || source.pending || event.button !== 0 || !event.isPrimary) return;
      focusNext.current = null;
      event.preventDefault();
      anchor.focus({ preventScroll: true });
      anchor.setPointerCapture(event.pointerId);
      const point = pointOf(event);
      const listeners = new AbortController();
      const drag: Drag = {
        nodeId, anchor, pointerId: event.pointerId, point,
        gesture: { start: point, moving: false }, listeners, frame: 0, suppressClick,
      };
      active.current = drag;

      const options = { signal: listeners.signal, capture: true };
      window.addEventListener("pointermove", (e) => {
        if (e.pointerId !== drag.pointerId) return;
        const wasMoving = drag.gesture.moving;
        drag.point = pointOf(e);
        drag.gesture = moveRoute(drag.gesture, drag.point);
        if (!drag.gesture.moving) return;
        if (!wasMoving) {
          suppressClick();
          const startedAt = performance.now();
          drag.frame = requestAnimationFrame((now) => scroll(drag, startedAt, now));
        }
        draw(drag);
      }, options);
      window.addEventListener("pointerup", (e) => {
        if (e.pointerId === drag.pointerId && e.button === 0) finish("drop", pointOf(e));
      }, options);
      window.addEventListener("pointercancel", (e) => {
        if (e.pointerId === drag.pointerId) finish("pointercancel");
      }, options);
      anchor.addEventListener("lostpointercapture", (e) => {
        if (e.pointerId === drag.pointerId) finish("lostpointercapture");
      }, options);
      window.addEventListener("keydown", (e) => {
        if (e.key !== "Escape") return;
        e.preventDefault();
        e.stopPropagation();
        finish("escape");
      }, options);
      // Element blur reaches a capture listener on window, too. Only cancel for window blur.
      window.addEventListener("blur", () => finish("blur"), { signal: listeners.signal });
      document.addEventListener("visibilitychange", () => {
        if (document.hidden) finish("blur");
      }, options);
    }

    return {
      begin, finish, announce,
      dragging: () => active.current !== null,
      pick: (nodeId: string, destinationId: string) => {
        const source = host.current.nodes.get(nodeId);
        if (!source || !availableSends(source).some(({ id }) => id === destinationId)) return;
        const anchor = document.activeElement;
        add(sendCommand(nodeId, destinationId), anchor instanceof HTMLElement ? anchor : null);
      },
    };
  });

  useLayoutEffect(() => {
    host.current = { props, nodes };
    const drag = active.current;
    if (drag) {
      const source = nodes.get(drag.nodeId);
      if (!source || source.pending) routing.finish("pending");
      else if (drag.targetId !== undefined && !nodes.has(drag.targetId)) routing.finish("target-removed");
    }
    const request = focusNext.current;
    if (!request) return;
    const source = nodes.get(request.nodeId);
    const send = source?.sends.find((send) => send.destinationId === request.destinationId);
    if (!source) {
      focusNext.current = null;
    } else if (send && !source.pending) {
      focusNext.current = null;
      if (request.anchor && document.activeElement === request.anchor) focusSend(request.anchor, send.id);
      routing.announce(`${source.name} → ${nodes.get(request.destinationId)?.name ?? source.sendDestinations.find(({ id }) => id === request.destinationId)?.name ?? "Send"} added`);
    } else if (props.error && props.error !== request.priorError && (props.error.nodeId === null || props.error.nodeId === request.nodeId)) {
      focusNext.current = null;
      routing.announce(`Route refused: ${props.error.message}`);
    }
  });

  useLayoutEffect(() => () => routing.finish("blur"), [routing]);

  return { routing, preview, notice };
}
