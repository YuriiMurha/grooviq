import type { ReactNode } from "react";
import { Icon } from "./icons";
import { placeholderValue, type MixerOption, type MixerTrack } from "./mixerModel";
import type { StripActions } from "./TrackStrip";

interface OutputChoiceProps {
  track: MixerTrack;
  nodes: ReadonlyMap<string, MixerTrack>;
  command: StripActions["command"];
  compact?: boolean;
}

/** Keep the current output visible even when the host omits it from the available choices. */
export function OutputChoice({ track, nodes, command, compact }: OutputChoiceProps) {
  const { id, name, outputId, outputs, pending } = track;
  const current = outputId === null ? undefined : outputs.find((option) => option.id === outputId);
  const label = current?.name ?? (outputId === null ? "Unassigned" : nodes.get(outputId)?.name ?? "Unavailable output");
  const placeholder = current ? undefined : placeholderValue(outputs);
  const picker = (
    <select
      className={compact ? "mx-route-select" : undefined}
      aria-label={compact ? `Main output for ${name}` : "Main output"}
      title={label}
      value={current?.id ?? placeholder}
      disabled={pending || outputs.length === 0}
      onChange={(e) => {
        const option = outputs.find((output) => output.id === e.currentTarget.value);
        if (option && option.id !== outputId) command({ type: "setOutput", nodeId: id, outputId: option.id });
      }}
    >
      {!current && <option value={placeholder} disabled>{label}</option>}
      <Options options={outputs} />
    </select>
  );
  return compact ? (
    <div className="mx-route-picker">
      <Icon name="route" />
      <span aria-hidden="true">{label}</span>
      {picker}
    </div>
  ) : picker;
}

export function Options({ options }: { options: readonly MixerOption[] }): ReactNode {
  return options.map(({ id, name }) => <option key={id} value={id}>{name}</option>);
}
