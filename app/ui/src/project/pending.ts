/** Shared settlement for project commands and roll edits. A refusal is consumed once by a
 * transition, including one that finished before Save/Open/Select started waiting. */
const sources = new Set<() => Promise<boolean>>();
export function registerSettlement(source: () => Promise<boolean>): () => void {
  sources.add(source);
  return () => sources.delete(source);
}
export async function settleProjectEdits(): Promise<boolean> {
  const answers = await Promise.all([...sources].map((source) => source()));
  return answers.every(Boolean);
}
export class ProjectCommandQueue {
  private tail: Promise<unknown> = Promise.resolve();
  private failed = 0;
  private acknowledged = 0;
  enqueue<T>(command: () => Promise<T>, mutation = true): Promise<T> {
    const result = this.tail.then(command);
    this.tail = result.then(() => undefined, () => { if (mutation) this.failed += 1; });
    return result;
  }
  async flush(): Promise<boolean> {
    const before = this.acknowledged;
    await this.tail;
    this.acknowledged = this.failed;
    return before === this.failed;
  }
}

/** Only acknowledgements of this queue may advance a captured gesture revision. Recording
 * or another editor's revisions deliberately have no link and are refused by Rust. */
export class PatternRevisionChain {
  private links = new Map<string, number>();
  private key(origin: { generation: number; patternId: number; revision: number }): string {
    return `${origin.generation}:${origin.patternId}:${origin.revision}`;
  }
  resolve<T extends { generation: number; patternId: number; revision: number }>(origin: T): T {
    const result = { ...origin };
    const seen = new Set<number>();
    while (this.links.has(this.key(result)) && !seen.has(result.revision)) {
      seen.add(result.revision); result.revision = this.links.get(this.key(result))!;
    }
    return result;
  }
  accepted(origin: { generation: number; patternId: number; revision: number }, revision: number) {
    if (origin.revision !== revision) this.links.set(this.key(origin), revision);
    // No gesture can remain valid across this many acknowledged changes; bound the cache.
    if (this.links.size > 200) this.links.delete(this.links.keys().next().value!);
  }
}

const cancellations = new Set<() => void>();
export function registerGestureCancellation(cancel: () => void): () => void {
  cancellations.add(cancel); return () => cancellations.delete(cancel);
}
let transitions = 0;
export const isProjectTransitioning = () => transitions > 0;
export function beginProjectTransition(): () => void {
  for (const cancel of [...cancellations]) cancel();
  transitions += 1;
  let finished = false;
  return () => { if (!finished) { finished = true; transitions -= 1; } };
}
