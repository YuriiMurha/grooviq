import { quantize, type ParamRange } from "../devices/controls/paramDrag";
export const DEFAULT_AMOUNTS = { spread: 30, timing: 20, velocity: 0.1 };
export type TransformAmount = keyof typeof DEFAULT_AMOUNTS;
export const AMOUNT_RANGES: Record<TransformAmount, ParamRange> = {
  spread: { min: 0, max: 120, default: 30, curve: "lin", stepped: true },
  timing: { min: 0, max: 120, default: 20, curve: "lin", stepped: true },
  velocity: { min: 0, max: 1, default: 0.1, curve: "lin", stepped: false },
};
export const amount = (id: TransformAmount, value: number) => quantize(AMOUNT_RANGES[id], value);
