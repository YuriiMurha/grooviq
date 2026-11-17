import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { channelParamLive } from "../api/notes";
import { useProject } from "../project/context";
import { isProjectReplacing, onProjectReplaced, onProjectReplacing } from "../project/replacement";
import { DEAD_GESTURE, DEAD_GROUP, ParamGestures, type Gesture, type GestureGroup } from "./paramGestures";
import { editorSink } from "./paramSink";

/** An instrument editor's parameter values as it shows them, and its one gesture owner
 * (`ParamGestures`): `begin(id)` for a gesture, `set(id, value)` for a click or a key,
 * `cancelAll()` before something replaces every value (a preset). Gestures are cancelled when the
 * window loses focus, when the editor closes and before the project is replaced. While a
 * replacement is on its way the editor takes no input, and nothing from it reaches a project
 * that replaced the one it was opened in: an edit is checked again when its turn in the queue
 * comes. */
export function useDeviceParams(channel: number, stored: Record<string, number>, defaults: (id: string) => number, onError: (message: string) => void) {
  const { apply } = useProject();
  const [moving, setMoving] = useState<Readonly<Record<string, number>>>({});
  const latest = useRef({ stored, defaults, onError });
  useLayoutEffect(() => {
    latest.current = { stored, defaults, onError };
  });
  const owner = useRef<ParamGestures | null>(null);

  useEffect(() => {
    const { sink, live: sender } = editorSink({
      channel,
      sendLive: (id, v) => channelParamLive(channel, id, v),
      apply,
      stored: (id) => latest.current.stored[id] ?? latest.current.defaults(id),
      shown: setMoving,
      onError: (m) => latest.current.onError(m),
    });
    // No input at all while a replacement is on its way.
    const gestures = new ParamGestures(sink, isProjectReplacing);
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
  const group = useCallback((): GestureGroup => owner.current?.group() ?? DEAD_GROUP, []);
  const set = useCallback((id: string, v: number) => owner.current?.set(id, v), []);
  const cancelAll = useCallback(() => owner.current?.cancelAll(), []);
  return { value, begin, group, set, cancelAll };
}
