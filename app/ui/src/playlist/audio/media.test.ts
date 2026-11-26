import { expect, it } from "vitest";
import { audioClipName, detectDropPlatform, missingAudioAssets } from "./media";

it.each([
  ["/qa/Break.Take.2.wav", "Break.Take.2"],
  ["C:\\QA\\Kick.WAV", "Kick"],
  ["\\\\box\\share\\Loop", "Loop"],
  ["/qa/.hidden", ".hidden"],
])("uses the basename of %s without its final extension", (path, name) => {
  expect(audioClipName(path)).toBe(name);
});

it.each([
  ["MacIntel", "macos"], ["macOS", "macos"], ["Win32", "windows"],
  ["Windows", "windows"], ["Linux x86_64", "linux"],
] as const)("detects the drop platform %s", (platform, expected) => {
  expect(detectDropPlatform(platform)).toBe(expected);
});

it("identifies missing assets without treating a provenance warning as missing", () => {
  expect(missingAudioAssets([
    "Audio asset 17 is missing; relink it to hear its clips.",
    "Audio asset 22: decoded geometry differs from saved provenance; trim duration is preserved and unavailable decoded frames are silent.",
  ])).toEqual(new Set([17]));
});
