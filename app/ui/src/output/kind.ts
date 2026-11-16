/** What an output is, as the `audio_outputs` command reports it. */
export type OutputKind = "headphones" | "speakers" | "unknown";

/** The output chip's icon for a kind. */
export function kindIcon(kind: OutputKind): "🎧" | "🔊" | "🔈" {
  switch (kind) {
    case "headphones":
      return "🎧";
    case "speakers":
      return "🔊";
    case "unknown":
      return "🔈";
  }
}

/** Speakers get the warning style: the room hears them. */
export function kindWarns(kind: OutputKind): boolean {
  return kind === "speakers";
}
