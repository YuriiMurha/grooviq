/** Shared settlement for project commands and roll edits. A refusal is consumed once by a
 * transition, including one that finished before Save/Open/Select started waiting. */
const sources = new Set<() => Promise<boolean>>();
export function registerSettlement(source: () => Promise<boolean>): () => void {
  sources.add(source);
  return () => sources.delete(source);
}
export async function settleProjectEdits(except?: () => Promise<boolean>): Promise<boolean> {
  const answers = await Promise.all([...sources].filter((source) => source !== except).map((source) => source()));
  return answers.every(Boolean);
}

/** A transition owns its intent before preflight; only its own preflight excludes that intent. */
export class TransitionWork {
  private active = new Set<Promise<void>>();
  private failures = 0;
  private acknowledged = 0;
  unsettled = () => this.active.size > 0 || this.failures > this.acknowledged;
  settle = async (): Promise<boolean> => {
    const before = this.acknowledged;
    await Promise.all([...this.active]);
    this.acknowledged = this.failures;
    return before === this.failures;
  };
  settleOthers = () => settleProjectEdits(this.settle);
  async run<Result>(command: () => Promise<Result>, accepted: (result: Result) => boolean): Promise<Result> {
    let finish!: () => void;
    const completion = new Promise<void>((resolve) => { finish = resolve; });
    this.active.add(completion);
    try {
      const result = await command();
      if (!accepted(result)) this.failures++;
      return result;
    } catch (error) {
      this.failures++;
      throw error;
    } finally {
      this.active.delete(completion);
      finish();
    }
  }
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

const drafts = new Map<() => void, () => boolean>();
/** Submit local drafts before the transition fence closes; the command queue awaits them. */
export function registerProjectDraft(commit: () => void, changed: () => boolean = () => false): () => void {
  drafts.set(commit, changed); return () => { drafts.delete(commit); };
}
/** Check intent without committing it before the person chooses Save, Don't save or Cancel. */
export const hasProjectDraft = () => [...drafts.values()].some((changed) => changed());

const cancellations = new Set<() => void>();
export function registerGestureCancellation(cancel: () => void): () => void {
  cancellations.add(cancel); return () => cancellations.delete(cancel);
}
let transitions = 0;
export const isProjectTransitioning = () => transitions > 0;
export function beginProjectTransition(): () => void {
  for (const cancel of [...cancellations]) cancel();
  for (const commit of [...drafts.keys()]) commit();
  transitions += 1;
  let finished = false;
  return () => { if (!finished) { finished = true; transitions -= 1; } };
}
