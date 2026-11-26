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
  report: (message: string) => void;
  commit: (stamp: ArrangementStamp, input: AudioImportInput, skipped: number) => void;
};

export function useAudioDrop(options: Options) {
  const latest = useRef(options);
  const drag = useRef<{ stamp: ArrangementStamp; replacements: number } | null>(null);
  useLayoutEffect(() => { latest.current = options; }, [options]);

  // Undefined gives the rack the drop. Null reserves a canvas drop while its snapshot loads.
  function destination(event: Exclude<DragDropEvent, { type: "leave" }>, modal: boolean) {
    const { canvas, snapshot, view, quantum } = latest.current;
    const element = canvas.current;
    if (!element) return undefined;
    const client = dropClientPoint(event.position, window.devicePixelRatio || 1, detectDropPlatform(navigator.platform));
    const onCanvas = modal ? document.elementsFromPoint(client.x, client.y).includes(element)
      : document.elementFromPoint(client.x, client.y) === element;
    if (!onCanvas) return undefined;
    if (!snapshot) return null;
    return hitAudioDrop(client, element.getBoundingClientRect(), view, snapshot.tracks, quantum) ?? undefined;
  }

  useLayoutEffect(() => registerPlaylistFileDrop(event => destination(event, false) !== undefined), []);
  useNativeFileDrop(event => {
    const { snapshot, quantum, busy, showPreview, report, commit } = latest.current;
    if (event.type === "leave") {
      drag.current = null;
      showPreview(null);
      return;
    }
    const replacing = isProjectReplacing();
    const modal = modalIsOpen();
    if (event.type === "enter") {
      drag.current = snapshot && !replacing && !modal
        ? { stamp: { generation: snapshot.generation, revision: snapshot.revision }, replacements: projectGeneration() }
        : null;
    }
    const captured = drag.current;
    if (event.type === "drop") drag.current = null;
    const target = destination(event, modal);
    if (!captured || !snapshot || busy || replacing || modal || !target ||
        captured.replacements !== projectGeneration() || captured.stamp.generation !== snapshot.generation ||
        captured.stamp.revision !== snapshot.revision) {
      showPreview(null);
      if (event.type === "drop" && target !== undefined && event.paths.length > 0) {
        report(modal ? "Close the dialog before dropping audio." : "The playlist couldn’t accept audio. Try dropping it again.");
      }
      return;
    }
    if (event.type === "drop") {
      showPreview(null);
      const path = event.paths[0];
      if (path) commit(captured.stamp, { ...target, name: audioClipName(path), path }, event.paths.length - 1);
      return;
    }
    const trackIndex = snapshot.tracks.findIndex(track => track.id === target.track);
    showPreview({ id: null, name: "", kind: "audio", trackIndex, startTick: target.startTick,
      lengthTicks: Math.min(quantum, MAX_SONG_TICKS - target.startTick),
      ink: snapshot.tracks[trackIndex].color, muted: false });
  });
}
