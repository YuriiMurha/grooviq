/** What an output is, as the `audio_outputs` command reports it. */
export type OutputKind = "headphones" | "speakers" | "unknown";

/** Speakers get the warning style: the room hears them. */
export function kindWarns(kind: OutputKind): boolean {
  return kind === "speakers";
}
