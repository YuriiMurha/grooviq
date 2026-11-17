/** One live note-on, from its request to its release. The engine ends the note a release names,
 * so the release waits for the press the note-on returned; a note-on that failed made no press
 * and has nothing to release. Each producer (typing keyboard, roll keys, a row's preview) keeps
 * its own, so one never ends another's note on the same key. */
export class Press {
  private readonly press: Promise<number | null>;

  /** `on` resolves to the press; a failure goes to `onError` (if given) and leaves no press. */
  constructor(on: Promise<number>, onError?: (e: unknown) => void) {
    this.press = on.then(
      (press) => press,
      (e: unknown) => {
        onError?.(e);
        return null;
      },
    );
  }

  /** Sends `off` for the press once it is known. The backend retains queue-full releases
   * until delivery. Other failures can mean the channel was removed, ending its notes. */
  release(off: (press: number) => Promise<unknown>): Promise<void> {
    return this.press.then(async (press) => {
      if (press === null) return;
      try {
        await off(press);
      } catch {
        /* best effort */
      }
    });
  }
}
