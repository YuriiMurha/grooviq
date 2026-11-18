import { useEffect, useRef, useState } from "react";
import { ParamGestures } from "../devices/paramGestures";
import { registerGestureCancellation } from "../project/pending";
import { amount, DEFAULT_AMOUNTS, type TransformAmount } from "./transformAmounts";

/** Per-window preferences; changing a feel control never writes project data. */
export function useTransformAmounts() {
  const stored = useRef({ ...DEFAULT_AMOUNTS });
  const [values, setValues] = useState({ ...DEFAULT_AMOUNTS });
  const owner = useRef<ParamGestures | null>(null);
  useEffect(() => {
    const show = (moving: Readonly<Record<string, number>>) => setValues({ ...stored.current, ...moving });
    const g = new ParamGestures({
      stored: id => stored.current[id as TransformAmount],
      live: () => {}, dropLive: () => {}, restore: () => {},
      edit: async (id, value) => {
        stored.current = { ...stored.current, [id]: amount(id as TransformAmount, value) };
        return true;
      },
      shown: show,
    });
    owner.current = g;
    const cancel = () => g.cancelAll();
    const hidden = () => { if (document.hidden) cancel(); };
    const escape = (e: KeyboardEvent) => { if (e.key === "Escape") cancel(); };
    const unregister = registerGestureCancellation(cancel);
    window.addEventListener("blur", cancel);
    window.addEventListener("keydown", escape, true);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      unregister(); g.dispose(); owner.current = null;
      window.removeEventListener("blur", cancel);
      window.removeEventListener("keydown", escape, true);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, []);
  const control = (id: TransformAmount) => ({
    begin: () => owner.current!.begin(id),
    set: (value: number) => owner.current?.set(id, amount(id, value)),
  });
  return { values, control };
}
