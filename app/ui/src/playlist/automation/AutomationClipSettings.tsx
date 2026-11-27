import { useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import type { ArrangementOperation, ArrangementStamp, ClipSummary, CurvePoint } from "../../api/arrangement";
import { useProject } from "../../project/context";
import { registerGestureCancellation } from "../../project/pending";
import { isProjectReplacing } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import { useAutomationCreation } from "./creationContext";
import { automationTargetCatalog, automationTargetKey } from "./target";

export type AutomationClipSummary = Omit<ClipSummary, "content"> & {
  content: Extract<ClipSummary["content"], { type: "automation" }>;
};
type Props = {
  clip: AutomationClipSummary;
  points?: readonly CurvePoint[];
  stamp: ArrangementStamp;
  disabled: boolean;
  onEdit: (operation: Extract<ArrangementOperation, { kind: "setAutomationClip" }>, still: () => boolean) => void;
};

type Draft = {
  revision: number;
  key: string;
  priority: string;
  waitingForDevice?: { clip: AutomationClipSummary; points: readonly CurvePoint[] };
};

function sameClip(left: AutomationClipSummary, right: AutomationClipSummary): boolean {
  return left.id === right.id && left.track === right.track && left.name === right.name &&
    left.startTick === right.startTick && left.muted === right.muted &&
    left.content.offsetTick === right.content.offsetTick && left.content.lengthTicks === right.content.lengthTicks &&
    left.content.pointCount === right.content.pointCount && left.content.priority === right.content.priority &&
    automationTargetKey(left.content.target) === automationTargetKey(right.content.target);
}

export function AutomationClipSettings({ clip, points, stamp, disabled, onEdit }: Props) {
  const { view } = useProject();
  const automation = useAutomationCreation();
  const [draft, setDraft] = useState<Draft | null>(null);
  const applyButton = useRef<HTMLButtonElement>(null);
  const { target, priority } = clip.content;
  const currentKey = automationTargetKey(target);
  const active = !disabled && points !== undefined && view?.generation === stamp.generation;
  const revalidating = draft !== null && draft.revision !== stamp.revision;
  if (revalidating) {
    const previous = draft.waitingForDevice;
    if (!previous || !sameClip(previous.clip, clip)) setDraft(null);
    else if (points) {
      const unchanged = previous.points.length === points.length && points.every((point, index) => {
        const prior = previous.points[index];
        return point.tick === prior.tick && point.value === prior.value && point.mode === prior.mode;
      });
      setDraft(unchanged ? { ...draft, revision: stamp.revision } : null);
    }
  }
  const key = draft?.key ?? currentKey;
  const text = draft?.priority ?? String(priority);
  const channels = view?.generation === stamp.generation ? view.channels : undefined;
  const catalog = useMemo(() => automationTargetCatalog(channels ?? []), [channels]);
  const choice = catalog.flatMap(group => group.targets).find(option => option.id === key);
  const next = /^[0-9]+$/.test(text) ? Number(text) : NaN;
  const valid = Number.isSafeInteger(next) && next <= 0xffffffff;
  const changed = key !== currentKey || next !== priority;
  const canApply = active && !revalidating && choice && valid && changed;

  useEffect(() => {
    if (!draft) return;
    const reset = () => setDraft(null);
    const observer = new MutationObserver(() => { if (modalIsOpen()) reset(); });
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["aria-modal"] });
    const unregister = registerGestureCancellation(reset);
    return () => {
      observer.disconnect();
      unregister();
    };
  }, [draft]);

  const blocked = () => !active || revalidating || isProjectReplacing() || modalIsOpen();
  const stage = (key: string, priority: string) => {
    if (!blocked()) setDraft({ revision: stamp.revision, key, priority });
  };
  const apply = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!canApply || blocked()) return;
    // The transition fence would reject device edits still waiting for their turn.
    const still = automation?.captureEditGuard(stamp);
    if (still === "devicePending") {
      if (draft && points && !draft.waitingForDevice) setDraft({ ...draft, waitingForDevice: { clip, points } });
      return;
    }
    if (!still) return;
    if (event.currentTarget.contains(document.activeElement)) applyButton.current?.focus({ preventScroll: true });
    setDraft(null);
    onEdit({ kind: "setAutomationClip", clip: clip.id, target: choice.target, priority: next }, still);
  };
  return <form className="automation-actions automation-settings" aria-label="Automation clip settings" onSubmit={apply} onKeyDown={event => {
    if (event.key === "Escape" && draft) {
      event.stopPropagation();
      setDraft(null);
    }
  }}>
    <label>Target
      <select aria-label="Automation target" value={key} disabled={!active || revalidating}
        onChange={event => stage(event.currentTarget.value, text)}>
        {!choice && <option value={key}>{key === currentKey ? "Current target" : "Unavailable target"}</option>}
        {catalog.map(group => <optgroup key={group.channel} label={group.name}>
          {group.targets.map(option => <option key={option.id} value={option.id}>{option.name}</option>)}
        </optgroup>)}
      </select>
    </label>
    <label>Overlap priority
      <input aria-label="Overlap priority" inputMode="numeric" aria-invalid={!valid || undefined} value={text}
        disabled={!active || revalidating} onChange={event => stage(key, event.currentTarget.value)} />
    </label>
    <button ref={applyButton} type="submit" aria-disabled={!canApply}>Apply settings</button>
  </form>;
}
