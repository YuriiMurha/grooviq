import { isMac } from "../../project/shortcuts";
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
