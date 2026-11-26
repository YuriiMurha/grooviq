import { useCallback, useEffect, useRef, useState } from "react";
import type { ArrangementStamp } from "../../api/arrangement";
import { registerGestureCancellation } from "../../project/pending";
import { isProjectReplacing, projectGeneration } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import { audioInspectorEdit, audioInspectorInfo, clipFrames, formatFade, MAX_GAIN,
  type InspectorAsset, type InspectorClip, type InspectorEdit, type InspectorResize, type SliderField } from "./inspector";

type Props = {
  clip: InspectorClip;
  asset: InspectorAsset;
  stamp: ArrangementStamp;
  disabled: boolean;
  bpm: number;
  beats: number;
  unit: number;
  onEdit: (stamp: ArrangementStamp, operation: InspectorResize) => void;
};

type Origin = { stamp: ArrangementStamp; replacement: number };
type Draft = Origin & { field: SliderField; value: number };
type Drag = Origin & { id: number; cancelled: boolean };

const sliders = [["gain", "Gain"], ["fadeIn", "Fade in"], ["fadeOut", "Fade out"]] as const;

export function AudioClipInspector({ clip, asset, stamp, disabled, bpm, beats, unit, onEdit }: Props) {
  const [preview, setPreview] = useState<Draft | null>(null);
  const draft = useRef<Draft | null>(null);
  const drag = useRef<Drag | null>(null);

  const clear = useCallback(() => {
    draft.current = null;
    setPreview(null);
  }, []);
  const cancel = useCallback(() => {
    if (drag.current) drag.current.cancelled = true;
    clear();
  }, [clear]);
  useEffect(() => {
    const unregister = registerGestureCancellation(cancel);
    const observer = new MutationObserver(() => { if (modalIsOpen()) cancel(); });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["open", "aria-modal"] });
    window.addEventListener("blur", cancel);
    return () => {
      unregister();
      window.removeEventListener("blur", cancel);
      observer.disconnect();
    };
  }, [cancel]);

  const origin = (): Origin => ({ stamp: { generation: stamp.generation, revision: stamp.revision }, replacement: projectGeneration() });
  const currentStamp = (from: Origin) => from.stamp.generation === stamp.generation && from.stamp.revision === stamp.revision;
  const blocked = () => disabled || isProjectReplacing() || modalIsOpen();
  const current = (from: Origin) => currentStamp(from) && from.replacement === projectGeneration() && !blocked();

  const submit = (edit: InspectorEdit, from: Origin) => {
    if (!current(from)) return;
    const operation = audioInspectorEdit(clip, edit);
    if (operation) onEdit(from.stamp, operation);
  };
  const take = (field: SliderField) => {
    const pending = draft.current;
    if (pending?.field !== field) return;
    // Enter can be followed by blur. Consume the draft before either can submit it again.
    clear();
    submit(pending, pending);
  };

  const info = audioInspectorInfo(clip.content, asset, bpm, beats, unit);
  return <fieldset className="playlist-audio-inspector" disabled={disabled} aria-label="Audio clip inspector">
    <div className="playlist-audio-controls">
      {sliders.map(([field, label]) => {
        const gain = field === "gain";
        const value = !disabled && preview?.field === field && currentStamp(preview) ? preview.value : clip.content[field];
        const display = gain ? String(Number(value.toFixed(2))) : formatFade(value, asset.sourceRate);
        return <label key={field}>
          <span>{label}</span><span className="playlist-audio-value" aria-hidden="true">{display}</span>
          <input type="range" aria-label={label} aria-valuetext={display} min={0} max={gain ? MAX_GAIN : clipFrames(clip.content)}
            step={gain ? 0.01 : 1} value={value}
            onChange={event => {
              const held = drag.current;
              const from = held ?? (draft.current?.field === field ? draft.current : origin());
              if (held?.cancelled || !current(from)) { cancel(); return; }
              const next: Draft = { field, value: event.currentTarget.valueAsNumber, stamp: from.stamp, replacement: from.replacement };
              draft.current = next;
              setPreview(next);
            }}
            onPointerDown={event => {
              if (event.button !== 0 || !event.isPrimary || blocked()) return;
              drag.current = { ...origin(), id: event.pointerId, cancelled: false };
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerUp={event => {
              const held = drag.current;
              if (held?.id !== event.pointerId) return;
              drag.current = null;
              if (!held.cancelled && current(held)) take(field);
              else clear();
            }}
            onLostPointerCapture={event => {
              if (drag.current?.id !== event.pointerId) return;
              drag.current = null;
              clear();
            }}
            onBlur={() => take(field)}
            onKeyDown={event => {
              if (event.key === "Escape") cancel();
              if (event.key === "Enter") { event.preventDefault(); take(field); }
            }} />
        </label>;
      })}
      <button type="button" aria-pressed={clip.content.reverse}
        onKeyDown={event => { if (event.key === " ") event.stopPropagation(); }}
        onKeyUp={event => { if (event.key === " ") event.stopPropagation(); }}
        onClick={() => submit({ field: "reverse", value: !clip.content.reverse }, origin())}>Reverse</button>
    </div>
    <dl className="playlist-audio-info">
      <div><dt>Length</dt><dd>{info.seconds}</dd><dd>{info.musicalLength}</dd></div>
      <div><dt>Source</dt><dd>{asset.sourceRate.toLocaleString()} Hz</dd><dd>{asset.sourceFrames.toLocaleString()} frames</dd></div>
      <div><dt>Original tempo</dt><dd>{info.originalTempo}</dd></div>
      <div><dt>Original key</dt><dd>{info.originalKey}</dd></div>
    </dl>
  </fieldset>;
}
