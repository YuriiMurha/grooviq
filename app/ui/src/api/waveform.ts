/** WFP1 is little endian. All source positions are half-open decoded frames. */
export function decodeWaveform(buffer: ArrayBuffer) {
  const view = new DataView(buffer);
  const invalid = () => { throw new Error("Invalid waveform packet"); };
  if (view.byteLength < 64 || view.getUint32(0, true) !== 0x31504657) invalid();
  const integer = (offset: number) => {
    const value = view.getBigUint64(offset, true);
    if (value > BigInt(Number.MAX_SAFE_INTEGER)) invalid();
    return Number(value);
  };
  const asset = view.getUint32(4, true);
  const generation = view.getBigUint64(8, true);
  const request = view.getBigUint64(16, true);
  const frames = integer(24), start = integer(32), end = integer(40);
  const rate = view.getUint32(48, true), channels = view.getUint32(52, true);
  const bins = view.getUint32(56, true);
  if (rate < 8000 || rate > 384000 || frames > rate * 600 || channels !== 2 ||
      start >= end || end > frames || bins === 0 || bins > 16384 || bins > end - start ||
      view.getUint32(60, true) !== 0 || buffer.byteLength !== 64 + bins * 16) invalid();
  const peaks = new Float32Array(bins * 4);
  for (let index = 0; index < peaks.length; index++) {
    const value = view.getFloat32(64 + index * 4, true);
    if (!Number.isFinite(value)) invalid();
    peaks[index] = value;
  }
  for (let index = 0; index < peaks.length; index += 4) {
    if (peaks[index] > peaks[index + 1] || peaks[index + 2] > peaks[index + 3]) invalid();
  }
  return { asset, generation, request, frames, start, end, rate, channels, bins, peaks };
}
