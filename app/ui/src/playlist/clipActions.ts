import type { ArrangementOperation, AudioContent, ClipSummary, PatternContent } from "../api/arrangement";
import { audioDurationTicks } from "./audio/geometry";
import { MAX_SONG_TICKS } from "./geometry";

export type ClipAction = "delete" | "mute" | "duplicate";
type EditableClip = ClipSummary & { content: PatternContent | AudioContent };

export function selectedEditableClips(clips: readonly ClipSummary[], selection: ReadonlySet<number>): EditableClip[] {
  if (!selection.size) return [];
  return clips.filter((clip): clip is EditableClip => selection.has(clip.id) && clip.content.type !== "automation");
}

export function selectionIsMuted(clips: readonly EditableClip[]): boolean {
  return clips.length > 0 && clips.every(clip => clip.muted);
}

export function buildClipAction(
  action: ClipAction, clips: readonly EditableClip[], audioAssets: ReadonlyMap<number, { sourceRate: number }>, bpm: number,
): ArrangementOperation[] {
  if (!clips.length) return [];
  switch (action) {
    case "delete":
      return clips.map(clip => ({ kind: "deleteClip", clip: clip.id }));
    case "mute": {
      const muted = !selectionIsMuted(clips);
      return clips.map(clip => ({ kind: "setClip", clip: clip.id, name: clip.name, muted }));
    }
    case "duplicate": {
      let firstTick = MAX_SONG_TICKS;
      let endTick = 0;
      for (const clip of clips) {
        firstTick = Math.min(firstTick, clip.startTick);
        const content = clip.content;
        let length: number;
        if (content.type === "audio") {
          const asset = audioAssets.get(content.asset);
          if (!asset) return [];
          length = audioDurationTicks(content.sourceEnd - content.sourceStart, asset.sourceRate, bpm);
        } else length = content.lengthTicks;
        endTick = Math.max(endTick, clip.startTick + length);
      }
      const offset = Math.min(endTick - firstTick, MAX_SONG_TICKS - endTick);
      // At the song horizon a copy would land exactly on its original and be invisible.
      if (offset <= 0) return [];
      return clips.map(clip => ({ kind: "copy", clip: clip.id, track: clip.track, startTick: clip.startTick + offset }));
    }
  }
}
