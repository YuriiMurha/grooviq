import { isProjectTransitioning } from "./pending";
// New, Open and Restore replace the whole project. Channel ids start again in the new one, so
// anything keyed by a channel id (a device window, a gesture in it) must not carry over: it is
// told before the replacing command is sent, and again once the new project is in. While the
// command is on its way the old project's editors must send nothing (their edits would queue
// behind it and land in the new project); a failed command gives them the old project back.

let generation = 0;
let replacingNow = false;
const replacing = new Set<() => void>();
const replaced = new Set<() => void>();
const failed = new Set<() => void>();

/** Runs `fn` just before a replacing command is sent (still the old project). */
export function onProjectReplacing(fn: () => void): () => void {
  replacing.add(fn);
  return () => replacing.delete(fn);
}

/** Runs `fn` once a replacing command succeeded (the new project is in). */
export function onProjectReplaced(fn: () => void): () => void {
  replaced.add(fn);
  return () => replaced.delete(fn);
}

/** Runs `fn` once a replacing command failed (the old project goes on). */
export function onProjectReplaceFailed(fn: () => void): () => void {
  failed.add(fn);
  return () => failed.delete(fn);
}

/** Bumped by every successful New, Open or Restore. */
export function projectGeneration(): number {
  return generation;
}

/** A New, Open or Restore is on its way: editors of the current project send nothing. */
export function isProjectReplacing(): boolean {
  return replacingNow || isProjectTransitioning();
}

/** Retire admitted work for New/Open/Restore, not for a Save waiting on that work. */
export const isProjectReplacementPending = (): boolean => replacingNow;

/** Call right before sending New, Open or Restore. Listeners run first, while they can still
 * reach the old project (to put the engine back where its project has it). */
export function projectReplacing() {
  for (const fn of [...replacing]) fn();
  replacingNow = true;
}

/** Call when New, Open or Restore succeeded, before the new view is shown. */
export function projectReplaced() {
  generation += 1;
  replacingNow = false;
  for (const fn of [...replaced]) fn();
}

/** Call when New, Open or Restore failed: the old project goes on, and so do its editors. */
export function projectReplaceFailed() {
  replacingNow = false;
  for (const fn of [...failed]) fn();
}
