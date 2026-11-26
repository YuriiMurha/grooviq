import { useEffect, useRef } from "react";
import { decodeSynthMeter, readSynthMeter, type SynthMeter } from "../../api/synthMeter";
import { isProjectReplacing, onProjectReplacing } from "../../project/replacement";

import { envelopeMeter, lfoMeter } from "./meterGeometry";
import "./meterMotion.css";

const SILENT_SCOPE = Array<number>(256).fill(0);
const SILENT_LEVELS = Array<number>(6).fill(0);

/** One outstanding read, no accumulating queue, no synthesized display motion. */
export function OutputScope({ channel, active, generation }: { channel: number; active: boolean; generation: number }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const status = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    let disposed = false;
    let frame = 0;
    let lastRead = -Infinity;
    let lastReply = -Infinity;
    let instance: number | null = null;
    let lastClock = -1;
    let scope = SILENT_SCOPE;
    let levels = SILENT_LEVELS;
    let motion: SynthMeter["motion"] = null;
    const clear = () => { scope = SILENT_SCOPE; levels = SILENT_LEVELS; motion = null; };
    const unsubscribe = onProjectReplacing(clear);
    const draw = (now: number) => {
      const visible = active && !document.hidden && !isProjectReplacing();
      if (!visible || now - lastReply > 250) clear();
      if (visible && generation !== undefined && now - lastRead >= 33) {
        const request = readSynthMeter(channel, generation, instance);
        if (request) {
          lastRead = now;
          void request.then((raw) => {
            if (disposed || isProjectReplacing()) return;
            const meter = decodeSynthMeter(raw, instance);
            if (!meter) { clear(); return; }
            if (meter.frames === lastClock) return;
            lastClock = meter.frames;
            instance = meter.instance;
            scope = meter.scope;
            levels = meter.levels;
            motion = meter.motion;
            lastReply = performance.now();
          }, clear);
        }
      }
      const c = canvas.current;
      const ctx = c?.getContext("2d");
      if (c && ctx) {
        ctx.clearRect(0, 0, c.width, c.height);
        ctx.strokeStyle = getComputedStyle(c).color;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        scope.forEach((v, i) => {
          const x = i * c.width / 255;
          const y = c.height / 2 - Math.max(-1, Math.min(1, v)) * (c.height / 2 - 5);
          if (i) ctx.lineTo(x, y); else ctx.moveTo(x, y);
        });
        ctx.stroke();
      }
      canvas.current?.closest('[role="dialog"]')?.querySelectorAll<SVGEllipseElement>("[data-synth-meter]").forEach((dot) => {
        const kind = dot.dataset.synthMeter;
        const box = {
          left: Number(dot.dataset.meterLeft), right: Number(dot.dataset.meterRight),
          top: Number(dot.dataset.meterTop), bottom: Number(dot.dataset.meterBottom),
        };
        const live = levels[2] > 0 && motion !== null;
        const point = kind === "lfo"
          ? lfoMeter(motion?.lfoPhase ?? 0, levels[3], Number(dot.dataset.meterAmount), box, live)
          : envelopeMeter((kind === "amp" ? motion?.ampStage : motion?.filterStage) ?? "idle", kind === "amp" ? levels[4] : levels[5], {
            ...box, attack: Number(dot.dataset.meterAttack), decay: Number(dot.dataset.meterDecay), sustain: Number(dot.dataset.meterSustain),
          }, live);
        if (point) {
          dot.setAttribute("cx", String(point.x));
          dot.setAttribute("cy", String(point.y));
        }
        dot.style.visibility = point ? "visible" : "hidden";
        const highlight = dot.parentElement?.querySelector<SVGRectElement>(`[data-synth-stage="${kind}"]`);
        if (highlight) {
          highlight.style.visibility = point ? "visible" : "hidden";
          if (point && "left" in point && "width" in point) {
            highlight.setAttribute("x", String(point.left));
            highlight.setAttribute("width", String(point.width));
          }
        }
      });
      if (status.current) status.current.textContent = `${Math.round(levels[2])} ${levels[2] === 1 ? "voice" : "voices"} · Peak ${Math.max(levels[0], levels[1]).toFixed(2)}`;
      if (active) frame = requestAnimationFrame(draw);
    };
    draw(performance.now());
    return () => { disposed = true; cancelAnimationFrame(frame); unsubscribe(); };
  }, [channel, active, generation]);
  return <section className="dwell synth-output" aria-label="Live synth output"><div className="dcard-head"><b className="dcard-title">Output</b><span ref={status} className="synth-output-status" /></div><canvas ref={canvas} width={660} height={48} style={{ width: "100%", height: 48, color: "var(--kc, var(--accent))" }} aria-label="Synth output waveform" /></section>;
}
