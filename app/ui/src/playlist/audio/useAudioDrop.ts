import { useLayoutEffect, useRef } from "react";
import type { DragDropEvent } from "@tauri-apps/api/webview";
import type { ArrangementStamp } from "../../api/arrangement";
import { isProjectReplacing, projectGeneration } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import { registerPlaylistFileDrop, useNativeFileDrop } from "../../rack/fileDrop";
import { MAX_SONG_TICKS, type PlaylistView } from "../geometry";
import type { PlaylistSnapshot } from "../store";
import type { RenderClip } from "../viewport";
import { dropClientPoint, hitAudioDrop } from "./dropPosition";
import { audioClipName, detectDropPlatform, type AudioImportInput } from "./media";

type Options = {
  canvas: { readonly current: HTMLCanvasElement | null };
  snapshot: PlaylistSnapshot | null;
  view: PlaylistView;
  quantum: number;
  busy: boolean;
  showPreview: (clip: RenderClip | null) => void;
  commit: (stamp: ArrangementStamp, input: AudioImportInput) => void;
};

export function useAudioDrop(options: Options) {
  const latest = useRef(options);
  const drag = useRef<{ stamp: ArrangementStamp; replacements: number } | null>(null);
  useLayoutEffect(() => { latest.current = options; }, [options]);

  function destination(event: Exclude<DragDropEvent, { type: "leave" }>) {
    const { canvas, snapshot, view, quantum } = latest.current;
    const element = canvas.current;
    if (!element || !snapshot) return null;
    const client = dropClientPoint(event.position, window.devicePixelRatio || 1, detectDropPlatform(navigator.platform));
    if (document.elementFromPoint(client.x, client.y) !== element) return null;
    return hitAudioDrop(client, element.getBoundingClientRect(), view, snapshot.tracks, quantum);
  }

  useLayoutEffect(() => registerPlaylistFileDrop(event => destination(event) !== null), []);
  useNativeFileDrop(event => {
    const { snapshot, quantum, busy, showPreview, commit } = latest.current;
    if (event.type === "leave") {
      drag.current = null;
      showPreview(null);
      return;
    }
    if (event.type === "enter") {
      drag.current = snapshot && !isProjectReplacing() && !modalIsOpen()
        ? { stamp: { generation: snapshot.generation, revision: snapshot.revision }, replacements: projectGeneration() }
        : null;
    }
    const captured = drag.current;
    if (event.type === "drop") drag.current = null;
    const target = destination(event);
    if (!captured || !snapshot || busy || isProjectReplacing() || modalIsOpen() || !target ||
        captured.replacements !== projectGeneration() || captured.stamp.generation !== snapshot.generation ||
        captured.stamp.revision !== snapshot.revision) {
      showPreview(null);
      return;
    }
    if (event.type === "drop") {
      showPreview(null);
      const path = event.paths[0];
      if (path) commit(captured.stamp, { ...target, name: audioClipName(path), path });
      return;
    }
    const trackIndex = snapshot.tracks.findIndex(track => track.id === target.track);
    showPreview({ id: null, name: "", kind: "audio", trackIndex, startTick: target.startTick,
      lengthTicks: Math.min(quantum, MAX_SONG_TICKS - target.startTick),
      ink: snapshot.tracks[trackIndex].color, muted: false });
  });
}
