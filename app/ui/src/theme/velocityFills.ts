type Rgb = readonly [number, number, number];

function canvasFills(ink: Rgb, quiet: number): Rgb[] {
  // Both roll renderers receive Float32 velocities. In [0, 1] the unsigned bits sort
  // in numeric order, so each Canvas2D byte transition can be found by binary search.
  const velocity = new Float32Array([1]);
  const bits = new Uint32Array(velocity.buffer);
  const full = bits[0];
  const byteAt = (channel: number, encoded: number) => {
    bits[0] = encoded;
    const strength = quiet + (1 - quiet) * velocity[0];
    return Math.round(channel * strength * 255);
  };
  const transitions = new Set([0]);
  for (const channel of ink) {
    const last = byteAt(channel, full);
    for (let target = byteAt(channel, 0) + 1; target <= last; target++) {
      let low = 0;
      let high = full;
      while (low < high) {
        const mid = (low + high) >>> 1;
        if (byteAt(channel, mid) < target) low = mid + 1;
        else high = mid;
      }
      transitions.add(low);
    }
  }
  return [...transitions].sort((a, b) => a - b).map((encoded) =>
    [byteAt(ink[0], encoded), byteAt(ink[1], encoded), byteAt(ink[2], encoded)]);
}

function webglFills(ink: Rgb, quiet: number): Rgb[] {
  // WebGL2 converts the highp shader output to framebuffer bytes. Float32 mix,
  // multiplication and conversion can split a shared half-byte boundary differently
  // across browsers. Allow either adjacent byte within four Float32 ulps at 1.0.
  const tolerance = 4 * 2 ** -23 * 255;
  const channels = ink.map((channel) => channel * 255);
  const boundaries = new Set([quiet, 1]);
  for (const channel of channels) {
    for (let byte = 0; byte < Math.ceil(channel); byte++) {
      for (const offset of [-tolerance, tolerance]) {
        const strength = (byte + 0.5 + offset) / channel;
        if (strength > quiet && strength < 1) boundaries.add(strength);
      }
    }
  }
  const strengths = [...boundaries].sort((a, b) => a - b);
  const fills = new Map<number, Rgb>();
  for (const [index, strength] of strengths.entries()) {
    const midpoint = (strength + (strengths[index + 1] ?? strength)) / 2;
    for (const sample of [strength, midpoint]) {
      const [red, green, blue] = channels.map((channel) => [
        Math.round(channel * sample - tolerance), Math.round(channel * sample + tolerance),
      ]);
      for (const r of red) for (const g of green) for (const b of blue) {
        fills.set((r << 16) | (g << 8) | b, [r, g, b]);
      }
    }
  }
  return [...fills.values()];
}

/** Opaque note fills as RGB bytes for normalized ink, over velocities 0 through 1. */
export function roundedVelocityFills(ink: Rgb, quiet: number, renderer: "canvas2d" | "webgl2"): Rgb[] {
  return renderer === "canvas2d" ? canvasFills(ink, quiet) : webglFills(ink, quiet);
}
