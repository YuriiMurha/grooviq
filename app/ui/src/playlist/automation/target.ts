import type { AutomationTarget } from "../../api/arrangement";
import { SAMPLER_PARAMS, formatValue as formatSamplerValue } from "../../devices/sampler/samplerLayout";
import { SYNTH_PARAMS, formatValue as formatSynthValue } from "../../devices/synth/synthLayout";
import { formatPan, formatVolume } from "../../rack/mixFormat";
import type { TargetRange } from "./curve";

export type PointTarget = TargetRange & { name: string; default: number; format: (value: number) => string };

export function pointTarget(target: AutomationTarget): PointTarget | null {
  if (target.type === "channelVolume") {
    return { name: "Channel volume", min: 0, max: 2, default: 1, stepped: false, format: formatVolume };
  }
  if (target.type === "channelPan") {
    return { name: "Channel pan", min: -1, max: 1, default: 0, stepped: false, format: formatPan };
  }
  if (target.type !== "instrumentParam") return null;
  if (target.instrument === "sampler") {
    const param = SAMPLER_PARAMS.find(param => param.id === target.param);
    return param ? { ...param, format: value => formatSamplerValue(param, value) } : null;
  }
  const param = SYNTH_PARAMS.find(param => param.id === target.param);
  return param ? { ...param, format: value => formatSynthValue(param, value) } : null;
}
