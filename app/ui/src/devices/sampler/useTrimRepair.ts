import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { TrimState } from "../../api/samplerTrim";
import { onProjectReplacing, onProjectReplaced } from "../../project/replacement";
import type { GuardedTrim } from "../paramSink";

/** A local offer is only a hint; Session checks the capability again at queue execution. */
export function useTrimRepair(context: string, storedStart: number, storedEnd: number, observed: Readonly<Record<string, number>>, send: GuardedTrim, onError: (message: string) => void) {
  const [state, setState] = useState<TrimState | null>(null);
  const [busy, setBusy] = useState(false);
  const [offer, setOffer] = useState<string | null>(null);
  const [previousContext, setPreviousContext] = useState(context);
  if (previousContext !== context) {
    setPreviousContext(context); setState(null); setOffer(null); setBusy(false);
  }
  const request = useRef(0);
  const current = useRef(context);
  const token = useRef<string | null>(null);
  const invalidate = () => { request.current++; token.current = null; setOffer(null); };
  useLayoutEffect(() => {
    current.current = context;
    request.current++;
    token.current = null;
  }, [context]);
  useEffect(() => {
    const clear = () => { invalidate(); setState(null); setBusy(false); };
    const counter = request;
    const a = onProjectReplacing(clear); const b = onProjectReplaced(clear);
    return () => { a(); b(); counter.current++; };
  }, []);
  useEffect(() => {
    let live = true;
    void send({ kind: "view" }).then(reply => {
      if (!live || reply?.status !== "applied") return;
      setState(reply.state);
      if (token.current !== reply.state.undo) { token.current = null; setOffer(null); }
    }).catch(e => { if (live) onError(String(e)); });
    return () => { live = false; };
  }, [context, storedStart, storedEnd, observed, send, onError]);
  const act = async (undo: boolean) => {
    if (busy || !state || (undo && (!offer || offer !== state.undo))) return;
    const mine = ++request.current;
    const origin = current.current;
    setBusy(true);
    try {
      const reply = await send(undo ? {kind:"undo",stamp:state.stamp,token:offer!} : {kind:"reset",stamp:state.stamp});
      if (mine !== request.current || origin !== current.current) return;
      if (!reply) { setState(null); return; }
      if (reply.status === "refused") { setState(reply.state); onError(reply.message); return; }
      // Re-read in the same guarded queue: an intervening edit or relink must retire the offer.
      const fresh = await send({kind:"view"});
      if (mine !== request.current || origin !== current.current) return;
      const accepted = fresh?.status === "applied" && fresh.state.stamp === reply.state.stamp;
      setState(fresh?.status === "applied" ? fresh.state : null);
      token.current = accepted && !undo && reply.changed ? reply.state.undo : null;
      setOffer(token.current);
    } catch (e) { if (mine === request.current) onError(String(e)); }
    finally { if (mine === request.current && origin === current.current) setBusy(false); }
  };
  return { ready: state !== null, busy, offer, invalidate, reset: () => void act(false), undo: () => void act(true) };
}
