import { useEffect, useRef } from "react";
import { decodeSynthMeter, readSynthMeter } from "../../api/synthMeter";
import { isProjectReplacing, onProjectReplacing } from "../../project/replacement";
import { subscribeThemeChange } from "../../theme/changes";

import { envelopeMeter, lfoMeter } from "./meterGeometry";
import { SILENT_SCOPE_DISPLAY, scopeDisplayChanged, scopeSample, scopeStatus } from "./scopeWake";
import "./meterMotion.css";

const POLL_MS = 33;
const STALE_MS = 250;
// The IPC gate is shared by the editors, so keep the polling cadence across selection changes.
let lastMeterRead = -Infinity;

function setMeterAttribute(element: SVGElement, name: string, value: string) {
  if (element.getAttribute(name) !== value) element.setAttribute(name, value);
}

function setMeterVisibility(element: SVGElement, visible: boolean) {
  const visibility = visible ? "visible" : "hidden";
  if (element.style.visibility !== visibility) element.style.visibility = visibility;
}

/** One outstanding read, with display work queued only on invalidation. */
export function OutputScope({ channel, active, generation }: { channel: number; active: boolean; generation: number }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const status = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    const scopeCanvas = canvas.current;
    if (!scopeCanvas) return;
    const ctx = scopeCanvas.getContext("2d");
    const dialog = scopeCanvas.closest('[role="dialog"]');
    let strokeColour = getComputedStyle(scopeCanvas).color;
    let disposed = false;
    let frame: number | undefined;
    let pollTimer: number | undefined;
    let expiry: number | undefined;
    let epoch = 0;
    let instance: number | null = null;
    let lastClock = -1;
    let display = SILENT_SCOPE_DISPLAY;

    const draw = () => {
      frame = undefined;
      const { scope, levels, motion } = display;
      const { width, height } = scopeCanvas;
      if (ctx) {
        ctx.clearRect(0, 0, width, height);
        ctx.strokeStyle = strokeColour;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        scope.forEach((sample, index) => {
          const x = index * width / 255;
          const y = height / 2 - scopeSample(sample) * (height / 2 - 5);
          if (index) ctx.lineTo(x, y); else ctx.moveTo(x, y);
        });
        ctx.stroke();
      }
      dialog?.querySelectorAll<SVGEllipseElement>("[data-synth-meter]").forEach((dot) => {
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
          setMeterAttribute(dot, "cx", String(point.x));
          setMeterAttribute(dot, "cy", String(point.y));
        }
        setMeterVisibility(dot, point !== null);
        const highlight = dot.parentElement?.querySelector<SVGRectElement>(`[data-synth-stage="${kind}"]`);
        if (highlight) {
          setMeterVisibility(highlight, point !== null);
          if (point && "left" in point && "width" in point) {
            setMeterAttribute(highlight, "x", String(point.left));
            setMeterAttribute(highlight, "width", String(point.width));
          }
        }
      });
      const text = scopeStatus(levels);
      if (status.current && status.current.textContent !== text) status.current.textContent = text;
    };

    const wake = () => {
      if (frame === undefined) frame = requestAnimationFrame(draw);
    };
    const updateDisplay = (next: typeof display) => {
      if (!scopeDisplayChanged(display, next)) return;
      display = scopeDisplayChanged(SILENT_SCOPE_DISPLAY, next) ? next : SILENT_SCOPE_DISPLAY;
      wake();
    };
    const cancelExpiry = () => {
      window.clearTimeout(expiry);
      expiry = undefined;
    };
    const clear = () => {
      cancelExpiry();
      updateDisplay(SILENT_SCOPE_DISPLAY);
    };

    const canPoll = () => !disposed && active && !document.hidden;
    const untilNextRead = () => lastMeterRead + POLL_MS - performance.now();
    const queuePoll = (delay = untilNextRead()) => {
      if (canPoll() && pollTimer === undefined) {
        pollTimer = window.setTimeout(poll, Math.max(0, Math.ceil(delay)));
      }
    };
    function poll() {
      pollTimer = undefined;
      if (!canPoll()) return;
      const remaining = untilNextRead();
      if (remaining > 0) { queuePoll(remaining); return; }
      if (isProjectReplacing()) { clear(); queuePoll(POLL_MS); return; }
      const request = readSynthMeter(channel, generation, instance);
      if (!request) { queuePoll(POLL_MS); return; }
      lastMeterRead = performance.now();
      const requestEpoch = epoch;
      const acceptsReply = () => canPoll() && epoch === requestEpoch && !isProjectReplacing();
      void request.then((raw) => {
        if (!acceptsReply()) return;
        const meter = decodeSynthMeter(raw, instance);
        if (!meter) { clear(); return; }
        // Replies may repeat a frozen capture. Only a new capture extends its lifetime.
        if (meter.frames === lastClock) return;
        lastClock = meter.frames;
        instance = meter.instance;
        updateDisplay(meter);
        cancelExpiry();
        if (display !== SILENT_SCOPE_DISPLAY) expiry = window.setTimeout(clear, STALE_MS);
      }, () => {
        if (acceptsReply()) clear();
      }).finally(() => queuePoll());
    }

    const reset = () => {
      // A reply already in flight can settle after the scope becomes visible or usable again.
      epoch++;
      clear();
      wake();
    };
    const onVisibility = () => {
      window.clearTimeout(pollTimer);
      pollTimer = undefined;
      reset();
      queuePoll();
    };
    document.addEventListener("visibilitychange", onVisibility);
    const unsubscribeReplacement = onProjectReplacing(reset);
    const unsubscribeTheme = subscribeThemeChange(() => {
      strokeColour = getComputedStyle(scopeCanvas).color;
      wake();
    });
    const geometry = new MutationObserver(wake);
    geometry.observe(scopeCanvas, { attributeFilter: ["width", "height"] });
    if (dialog) geometry.observe(dialog, {
      subtree: true,
      attributeFilter: [
        "data-meter-left", "data-meter-right", "data-meter-top", "data-meter-bottom",
        "data-meter-attack", "data-meter-decay", "data-meter-sustain", "data-meter-amount",
      ],
    });
    wake();
    queuePoll();
    return () => {
      disposed = true;
      if (frame !== undefined) cancelAnimationFrame(frame);
      window.clearTimeout(pollTimer);
      cancelExpiry();
      document.removeEventListener("visibilitychange", onVisibility);
      unsubscribeReplacement();
      unsubscribeTheme();
      geometry.disconnect();
    };
  }, [channel, active, generation]);
  return <section className="dwell synth-output" aria-label="Live synth output"><div className="dcard-head"><b className="dcard-title">Output</b><span ref={status} className="synth-output-status" /></div><canvas ref={canvas} width={660} height={48} style={{ width: "100%", height: 48, color: "var(--kc, var(--accent))" }} aria-label="Synth output waveform" /></section>;
}
