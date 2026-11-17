import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { channelParamLive } from "../api/notes";
import type { Edit } from "../api/project";
import { useProject } from "../project/context";
import { isProjectReplacing, onProjectReplaced, onProjectReplacing } from "../project/replacement";
import { DEAD_GESTURE, DEAD_GROUP, ParamGestures, type Gesture, type GestureGroup } from "./paramGestures";
import { editorSink } from "./paramSink";

/** An instrument editor's parameter values as it shows them, its one gesture owner
 * (`ParamGestures`): `begin(id)` for a gesture, `group()` for a drag over several parameters,
 * `set(id, value)` for a click or a key, and `send(edit)` for any other edit (a preset). This hook
 * is the editor's only way to the project: device components never see `apply`. Gestures are cancelled when the
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
  const sendRef = useRef<((edit: Edit) => Promise<boolean>) | null>(null);

  useEffect(() => {
    const { sink, live: sender, send } = editorSink({
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
    sendRef.current = send;
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
      if (sendRef.current === send) sendRef.current = null;
      setMoving({});
    };
  }, [apply, channel]);

  const value = (id: string) => moving[id] ?? stored[id] ?? defaults(id);
  const begin = useCallback((id: string): Gesture => owner.current?.begin(id) ?? DEAD_GESTURE, []);
  const group = useCallback((): GestureGroup => owner.current?.group() ?? DEAD_GROUP, []);
  const set = useCallback((id: string, v: number) => owner.current?.set(id, v), []);
  const cancelAll = useCallback(() => owner.current?.cancelAll(), []);
  /** Any other edit (a preset, ...): running gestures end first, then it goes through the sink's
   * guard (refused while a replacement is on its way, checked again when it runs). */
  const send = useCallback(async (edit: Edit): Promise<boolean> => {
    owner.current?.cancelAll();
    return (await sendRef.current?.(edit)) ?? false;
  }, []);
  return { value, begin, group, set, cancelAll, send };
}
