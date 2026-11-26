function randomWords(): readonly [number, number] {
  const words = crypto.getRandomValues(new Uint32Array(2));
  return [words[0], words[1]];
}

/** A fresh native media ticket for one import or relink (#629): a random safe integer above 0.
 * Random rather than counted, so a reloaded UI can't reuse a number the native session still
 * holds as live or cancelled. The Cancel control that uses it is #658. */
export function nextMediaTicket(random: () => readonly [number, number] = randomWords): number {
  for (;;) {
    const [high, low] = random();
    // 21 high bits and 32 low bits: at most 2^53 - 1.
    const ticket = (high & 0x1f_ffff) * 0x1_0000_0000 + (low >>> 0);
    if (ticket > 0) return ticket;
  }
}
