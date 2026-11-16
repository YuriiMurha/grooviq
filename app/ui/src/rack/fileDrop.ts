import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useRef } from "react";

/** Follows files dragged from the OS onto the window: `onHover` while they are over it,
 * `onDrop` with their paths, in the order the OS gives them. Outside the Tauri shell it does
 * nothing. */
export function useFileDrop(onDrop: (paths: string[]) => void, onHover: (over: boolean) => void) {
  const handlers = useRef({ onDrop, onHover });
  useEffect(() => {
    handlers.current = { onDrop, onHover };
  }, [onDrop, onHover]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    try {
      getCurrentWebview()
        .onDragDropEvent((event) => {
          const p = event.payload;
          if (p.type === "enter") handlers.current.onHover(true);
          else if (p.type === "leave") handlers.current.onHover(false);
          else if (p.type === "drop") {
            handlers.current.onHover(false);
            handlers.current.onDrop(p.paths);
          }
        })
        .then(
          (stop) => {
            if (gone) stop();
            else unlisten = stop;
          },
          () => {},
        );
    } catch {
      /* not inside the Tauri shell (a plain browser) */
    }
    return () => {
      gone = true;
      unlisten?.();
    };
  }, []);
}
