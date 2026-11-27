import { useEffect, useMemo, useState, type FormEvent } from "react";
import type { ArrangementOperation, AutomationTarget } from "../../api/arrangement";
import { useProject } from "../../project/context";
import { registerGestureCancellation } from "../../project/pending";
import { isProjectReplacing } from "../../project/replacement";
import { modalIsOpen } from "../../project/shortcuts";
import { automationTargetCatalog, automationTargetKey } from "./target";

type Props = {
  clip: number;
  target: AutomationTarget;
  priority: number;
  generation: number;
  disabled: boolean;
  onEdit: (operation: Extract<ArrangementOperation, { kind: "setAutomationClip" }>) => void;
};

type Draft = { key: string; priority: string };

export function AutomationClipSettings({ clip, target, priority, generation, disabled, onEdit }: Props) {
  const { view } = useProject();
  const [draft, setDraft] = useState<Draft | null>(null);
  const currentKey = automationTargetKey(target);
  const active = !disabled && view?.generation === generation;
  if (!active && draft) setDraft(null);
  const key = draft?.key ?? currentKey;
  const text = draft?.priority ?? String(priority);
  const channels = view?.generation === generation ? view.channels : undefined;
  const catalog = useMemo(() => automationTargetCatalog(channels ?? []), [channels]);
  const choice = catalog.flatMap(group => group.targets).find(option => option.id === key);
  const next = /^[0-9]+$/.test(text) ? Number(text) : NaN;
  const valid = Number.isSafeInteger(next) && next <= 0xffffffff;
  const changed = key !== currentKey || next !== priority;
  const canApply = active && choice && valid && changed;

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

  const blocked = () => !active || isProjectReplacing() || modalIsOpen();
  const stage = (next: Draft) => { if (!blocked()) setDraft(next); };
  const apply = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!canApply || blocked()) return;
    setDraft(null);
    onEdit({ kind: "setAutomationClip", clip, target: choice.target, priority: next });
  };
  return <form className="automation-actions" aria-label="Automation clip settings" onSubmit={apply} onKeyDown={event => {
    if (event.key === "Escape" && draft) {
      event.stopPropagation();
      setDraft(null);
    }
  }}>
    <label>Target
      <select aria-label="Automation target" value={key} disabled={!active}
        onChange={event => stage({ key: event.currentTarget.value, priority: text })}>
        {!choice && <option value={key}>{key === currentKey ? "Current target" : "Unavailable target"}</option>}
        {catalog.map(group => <optgroup key={group.channel} label={group.name}>
          {group.targets.map(option => <option key={option.id} value={option.id}>{option.name}</option>)}
        </optgroup>)}
      </select>
    </label>
    <label>Overlap priority
      <input aria-label="Overlap priority" inputMode="numeric" aria-invalid={!valid || undefined} value={text}
        disabled={!active} onChange={event => stage({ key, priority: event.currentTarget.value })} />
    </label>
    <button type="submit" aria-disabled={!canApply}>Apply settings</button>
  </form>;
}
