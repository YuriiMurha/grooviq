import { useLayoutEffect, useRef } from "react";
import type { useSendRouting } from "./useSendRouting";

type Preview = NonNullable<ReturnType<typeof useSendRouting>["preview"]>;

export function RoutePreview({ preview }: { preview: Preview }) {
  const { start, end, kind, message } = preview;
  const bend = Math.min(68, 24 + Math.abs(end.x - start.x) / 5);
  const path = `M${start.x} ${start.y} C${start.x} ${start.y + bend} ${end.x} ${end.y + bend} ${end.x} ${end.y}`;
  const note = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = note.current;
    if (!el) return;
    el.style.left = `${Math.max(8, Math.min(end.x + 12, window.innerWidth - el.offsetWidth - 8))}px`;
    el.style.top = `${Math.max(8, Math.min(end.y - el.offsetHeight - 10, window.innerHeight - el.offsetHeight - 8))}px`;
  }, [end, message]);

  return (
    <>
      <svg className="mx-route-cable" data-route={kind} aria-hidden="true">
        <path className="mx-route-shadow" d={path} />
        <path d={path} />
        <circle cx={start.x} cy={start.y} r="3" />
        <circle cx={end.x} cy={end.y} r="3" />
      </svg>
      <div ref={note} className="mx-route-note" data-route={kind} aria-hidden="true">{message}</div>
    </>
  );
}
