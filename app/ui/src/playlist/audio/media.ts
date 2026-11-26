import { isMac } from "../../project/shortcuts";
import type { PlaylistResult } from "../store";
import type { DropPlatform } from "./dropPosition";

export type AudioImportInput = Readonly<{ track: number; startTick: number; name: string; path: string }>;

export function detectDropPlatform(platform: string): DropPlatform {
  if (isMac(platform)) return "macos";
  return /^win/i.test(platform) ? "windows" : "linux";
}

export function audioClipName(path: string): string {
  const basename = path.split(/[/\\]/).at(-1) ?? "";
  const extension = basename.lastIndexOf(".");
  return extension > 0 ? basename.slice(0, extension) : basename || "Audio";
}

export function missingAudioAssets(warnings: readonly string[]): ReadonlySet<number> {
  const missing = new Set<number>();
  for (const warning of warnings) {
    const match = /^Audio asset (\d+) is missing;/.exec(warning);
    if (match) missing.add(Number(match[1]));
  }
  return missing;
}

/** What to tell the user after a playlist drop settles. A refusal or an unconfirmed readback is
 * reported by the playlist alert, and native may still have applied the first file, so then the
 * notice only covers the files that were never tried. */
export type DropNotice = "imported-and-skipped" | "skipped-only" | "retry" | null;
export function dropNotice(status: PlaylistResult["status"], skipped: number): DropNotice {
  if (status === "blocked" || status === "stale" || status === "unsettled") return "retry";
  if (skipped <= 0) return null;
  return status === "applied" ? "imported-and-skipped" : "skipped-only";
}
