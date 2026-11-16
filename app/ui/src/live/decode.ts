/** One frame of the engine's live state (wire format v1, see `grooviq_engine::encode_live`). */
export interface LiveFrame {
  /** Master peak per channel, linear. */
  peak: [number, number];
  playing: boolean;
  tone: boolean;
  /** Frames rendered since the engine started; a u64, so a bigint. */
  frames: bigint;
  faults: number;
  guardHits: number;
  outputLatencyFrames: number;
}

const LIVE_VERSION = 1;
const LIVE_LEN = 32;
const FLAG_PLAYING = 1;
const FLAG_TONE = 1 << 1;

/** Decodes one 32-byte little-endian frame; `null` for a wrong length or an unknown version. */
export function decodeLive(buf: ArrayBuffer): LiveFrame | null {
  if (buf.byteLength !== LIVE_LEN) return null;
  const v = new DataView(buf);
  if (v.getUint8(0) !== LIVE_VERSION) return null;
  const flags = v.getUint8(1);
  return {
    peak: [v.getFloat32(4, true), v.getFloat32(8, true)],
    playing: (flags & FLAG_PLAYING) !== 0,
    tone: (flags & FLAG_TONE) !== 0,
    frames: v.getBigUint64(12, true),
    faults: v.getUint32(20, true),
    guardHits: v.getUint32(24, true),
    outputLatencyFrames: v.getUint32(28, true),
  };
}
