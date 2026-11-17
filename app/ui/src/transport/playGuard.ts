// When the UI may believe a live frame that says the transport is stopped. `transport_play` only
// queues the command, so frames rendered before the engine applied it can still arrive after the
// reply and say "stopped". After a Play, a stopped frame is believed only once a playing frame
// has arrived since (frames come in order on one socket, so anything after it is newer), or once
// PLAY_GRACE_MS have passed without one (the engine really stopped, or never started).

/** How long after a Play a "stopped" frame is doubted, in ms. */
export const PLAY_GRACE_MS = 300;

export class PlayGuard {
  /** When the last Play was confirmed by its reply, while no playing frame has come since. */
  private waitingSince: number | null = null;

  /** A Play command succeeded. */
  played(): void {
    this.waitingSince = performance.now();
  }

  /** Feeds one live frame; `true` when what it says may be believed. A playing frame always is,
   * and ends the wait. */
  believe(playing: boolean): boolean {
    if (playing) {
      this.waitingSince = null;
      return true;
    }
    if (this.waitingSince === null) return true;
    if (performance.now() - this.waitingSince < PLAY_GRACE_MS) return false;
    this.waitingSince = null;
    return true;
  }
}
