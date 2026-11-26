import type { DragDropEvent } from "@tauri-apps/api/webview";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useEffect, useLayoutEffect, useRef } from "react";
import { modalIsOpen } from "../project/shortcuts";

type PositionedDrop = Exclude<DragDropEvent, { type: "leave" }>;
type DropOwner = (event: PositionedDrop) => boolean;
let playlistOwnsDrop: DropOwner | undefined;

/** Both webview listeners receive the same event, regardless of which subscribed first. */
export function registerPlaylistFileDrop(owns: DropOwner): () => void {
  playlistOwnsDrop = owns;
  return () => { if (playlistOwnsDrop === owns) playlistOwnsDrop = undefined; };
}

export function useNativeFileDrop(handle: (event: DragDropEvent) => void) {
  const handler = useRef(handle);
  useLayoutEffect(() => { handler.current = handle; }, [handle]);
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let gone = false;
    try {
      getCurrentWebview()
        .onDragDropEvent(event => { if (!gone) handler.current(event.payload); })
        .then(
          stop => { if (gone) stop(); else unlisten = stop; },
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

/** Files outside the playlist retain the rack's window-wide channel import. */
export function useFileDrop(onDrop: (paths: string[]) => void, onHover: (over: boolean) => void) {
  useNativeFileDrop(event => {
    if (modalIsOpen() || event.type === "leave" || playlistOwnsDrop?.(event)) {
      onHover(false);
    } else if (event.type === "drop") {
      onHover(false);
      onDrop(event.paths);
    } else {
      onHover(true);
    }
  });
}
