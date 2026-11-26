import type { MixerCommand, MixerOption, MixerTrack } from "./mixerModel";

export type RouteSource = Pick<MixerTrack, "id" | "name" | "pending" | "outputId" | "sends" | "sendDestinations">;
export type Point = { x: number; y: number };
type AddSend = Extract<MixerCommand, { type: "addSend" }>;

const SCROLL_PX_PER_MS = 0.54;
const MAX_SCROLL_FRAME_MS = 100;

/** Preserve the old 60 Hz pace and limit the jump after a stalled frame. */
export function routeScrollDistance(elapsedMs: number): number {
  return SCROLL_PX_PER_MS * Math.max(0, Math.min(elapsedMs, MAX_SCROLL_FRAME_MS));
}

type Route =
  | { kind: "idle"; message: string }
  | { kind: "bad"; message: string }
  | { kind: "main"; message: string }
  | { kind: "existing"; message: string; sendId: string }
  | { kind: "ready"; message: string; command: AddSend };

export function sendCommand(nodeId: string, destinationId: string): AddSend {
  return { type: "addSend", nodeId, destinationId, pre: false, amount: 0 };
}

export function classifyRoute(source: RouteSource, target: MixerOption | null): Route {
  if (!target) return { kind: "idle", message: "Drop on a mixer strip." };
  if (target.id === source.outputId) return { kind: "main", message: `${target.name} is already the main output.` };
  const send = source.sends.find((send) => send.destinationId === target.id);
  if (send) return { kind: "existing", sendId: send.id, message: `${target.name} send already exists. Drop to focus its knob.` };
  if (!source.sendDestinations.some(({ id }) => id === target.id)) {
    return { kind: "bad", message: target.id === source.id ? "A track can't send to itself." : `${target.name} isn't available as a send destination.` };
  }
  return { kind: "ready", message: `Add send · ${source.name} → ${target.name}`, command: sendCommand(source.id, target.id) };
}

export type RouteGesture = { start: Point; moving: boolean };
export type RouteEnd = "drop" | "escape" | "blur" | "pointercancel" | "lostpointercapture" | "pending" | "target-changed";

export function moveRoute(gesture: RouteGesture, point: Point): RouteGesture {
  if (gesture.moving || Math.hypot(point.x - gesture.start.x, point.y - gesture.start.y) < 5) return gesture;
  return { ...gesture, moving: true };
}

export function finishRoute(gesture: RouteGesture, reason: RouteEnd, source: RouteSource | undefined, target: MixerOption | null): Route | null {
  if (reason !== "drop" || !gesture.moving || !source || source.pending) return null;
  return classifyRoute(source, target);
}
