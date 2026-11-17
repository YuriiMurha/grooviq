import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { channelParamLive } from "../api/notes";
import { errorText } from "../api/project";
import { useProject } from "../project/context";
import { onProjectReplaced, onProjectReplacing, projectGeneration } from "../project/replacement";
import { LiveCoalescer } from "./controls/paramDrag";
import { DEAD_GESTURE, ParamGestures, type Gesture } from "./paramGestures";

/** An instrument editor's parameter values as it shows them, and its one gesture owner
 * (`ParamGestures`): `begin(id)` for a gesture, `set(id, value)` for a click or a key,
 * `cancelAll()` before something replaces every value (a preset). Gestures are cancelled when the
 * window loses focus, when the editor closes and before the project is replaced; nothing from
 * this editor reaches a project that replaced the one it was opened in. */
export function useDeviceParams(channel: number, stored: Record<string, number>, defaults: (id: string) => number, onError: (message: string) => void) {
  const { apply } = useProject();
  const [moving, setMoving] = useState<Readonly<Record<string, number>>>({});
  const latest = useRef({ stored, defaults, onError });
  useLayoutEffect(() => {
    latest.current = { stored, defaults, onError };
  });
  const owner = useRef<ParamGestures | null>(null);

  useEffect(() => {
    const generation = projectGeneration();
    const current = () => projectGeneration() === generation;
    const sender = new LiveCoalescer((id, v) => {
      if (current()) channelParamLive(channel, id, v).catch((e: unknown) => latest.current.onError(errorText(e)));
    });
    const gestures = new ParamGestures({
      stored: (id) => latest.current.stored[id] ?? latest.current.defaults(id),
      live: (id, v) => sender.push(id, v),
      dropLive: (id) => sender.settle(id),
      // Best effort: the channel may just have been removed.
      restore: (id, v) => {
        if (current()) channelParamLive(channel, id, v).catch(() => {});
      },
      edit: (id, v) => (current() ? apply({ kind: "setInstrumentParam", channel, param: id, value: v }) : Promise.resolve(false)),
      shown: setMoving,
    });
    owner.current = gestures;
    const cancel = () => gestures.cancelAll();
    window.addEventListener("blur", cancel);
    const offReplacing = onProjectReplacing(cancel);
    const offReplaced = onProjectReplaced(() => {
      gestures.abandon();
      sender.dispose();
    });
    return () => {
      window.removeEventListener("blur", cancel);
      offReplacing();
      offReplaced();
      gestures.dispose();
      sender.dispose();
      if (owner.current === gestures) owner.current = null;
      setMoving({});
    };
  }, [apply, channel]);

  const value = (id: string) => moving[id] ?? stored[id] ?? defaults(id);
  const begin = useCallback((id: string): Gesture => owner.current?.begin(id) ?? DEAD_GESTURE, []);
  const set = useCallback((id: string, v: number) => owner.current?.set(id, v), []);
  const cancelAll = useCallback(() => owner.current?.cancelAll(), []);
  return { value, begin, set, cancelAll };
}
