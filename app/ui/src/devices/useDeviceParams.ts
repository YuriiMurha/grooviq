import { useCallback, useEffect, useRef, useState } from "react";
import { channelParamLive } from "../api/notes";
import { errorText } from "../api/project";
import { useProject } from "../project/context";
import { LiveCoalescer } from "./controls/paramDrag";

/** An instrument's parameter values as an editor shows them, and the three ways to change one:
 * `input` while a gesture moves (sent live to the engine at most once per frame, the project
 * unchanged), `commit` when it ends (one `SetInstrumentParam`), `set` for a one-shot change (a
 * click, a key). */
export function useDeviceParams(channel: number, stored: Record<string, number>, defaults: (id: string) => number, onError: (message: string) => void) {
  const { apply } = useProject();
  // Values a gesture moved that the project does not hold yet.
  const [moving, setMoving] = useState<Record<string, number>>({});
  const latest = useRef({ stored, moving, onError });
  useEffect(() => {
    latest.current = { stored, moving, onError };
  });
  // Ids the engine heard a live value for since their last commit.
  const heard = useRef(new Set<string>());
  const live = useRef<LiveCoalescer | null>(null);

  const value = (id: string) => moving[id] ?? stored[id] ?? defaults(id);

  const drop = useCallback((id: string) => {
    setMoving((m) => {
      if (!(id in m)) return m;
      const next = { ...m };
      delete next[id];
      return next;
    });
  }, []);

  const input = useCallback((id: string, v: number) => {
    setMoving((m) => (m[id] === v ? m : { ...m, [id]: v }));
    heard.current.add(id);
    live.current?.push(id, v);
  }, []);

  const commit = useCallback(
    (id: string, v: number) => {
      live.current?.settle(id);
      const engineHeard = heard.current.delete(id);
      const before = latest.current.stored[id];
      if (v === before) {
        // Back where it started: the project needs nothing, the engine its old value again.
        if (engineHeard) channelParamLive(channel, id, v).catch(() => {});
        drop(id);
        return;
      }
      void apply({ kind: "setInstrumentParam", channel, param: id, value: v }).then((ok) => {
        // A refused edit leaves the engine on the live value: put the stored one back.
        if (!ok && engineHeard) {
          const kept = latest.current.stored[id];
          if (kept !== undefined) channelParamLive(channel, id, kept).catch(() => {});
        }
        drop(id);
      });
    },
    [apply, channel, drop],
  );

  useEffect(() => {
    const sender = new LiveCoalescer((id, v) => {
      channelParamLive(channel, id, v).catch((e: unknown) => latest.current.onError(errorText(e)));
    });
    live.current = sender;
    return () => {
      sender.dispose();
      live.current = null;
      // An editor closed mid-gesture stores what the engine already plays.
      for (const [id, v] of Object.entries(latest.current.moving)) {
        if (v !== latest.current.stored[id]) void apply({ kind: "setInstrumentParam", channel, param: id, value: v });
      }
    };
  }, [apply, channel]);

  return { value, input, commit, set: commit };
}
