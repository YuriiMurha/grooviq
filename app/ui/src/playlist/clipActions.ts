import type { ArrangementOperation, ClipSummary, PatternContent } from "../api/arrangement";
import { MAX_SONG_TICKS } from "./geometry";

export type ClipAction = "delete" | "mute" | "duplicate";
type PatternClip = ClipSummary & { content: PatternContent };

export function selectedPatternClips(clips: readonly ClipSummary[], selection: ReadonlySet<number>): PatternClip[] {
  if (!selection.size) return [];
  return clips.filter((clip): clip is PatternClip => selection.has(clip.id) && clip.content.type === "pattern");
}

export function selectionIsMuted(clips: readonly PatternClip[]): boolean {
  return clips.length > 0 && clips.every(clip => clip.muted);
}

export function buildClipAction(action: ClipAction, clips: readonly PatternClip[]): ArrangementOperation[] {
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
        endTick = Math.max(endTick, clip.startTick + clip.content.lengthTicks);
      }
      const offset = Math.min(endTick - firstTick, MAX_SONG_TICKS - endTick);
      return clips.map(clip => ({ kind: "copy", clip: clip.id, track: clip.track, startTick: clip.startTick + offset }));
    }
  }
}
