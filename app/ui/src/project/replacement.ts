// New, Open and Restore replace the whole project. Channel ids start again in the new one, so
// anything keyed by a channel id (a device window, a gesture in it) must not carry over: it is
// told before the replacing command is sent, and again once the new project is in.

let generation = 0;
const replacing = new Set<() => void>();
const replaced = new Set<() => void>();

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

/** Bumped by every successful New, Open or Restore. */
export function projectGeneration(): number {
  return generation;
}

/** Call right before sending New, Open or Restore. */
export function projectReplacing() {
  for (const fn of [...replacing]) fn();
}

/** Call when New, Open or Restore succeeded, before the new view is shown. */
export function projectReplaced() {
  generation += 1;
  for (const fn of [...replaced]) fn();
}
