import { ListPicker } from "../layout/ListPicker";
import { outputChoices, type MixerTrack } from "./mixerModel";
import type { StripActions } from "./TrackStrip";

interface OutputChoiceProps {
  track: MixerTrack;
  nodes: ReadonlyMap<string, MixerTrack>;
  command: StripActions["command"];
  compact?: boolean;
}

/** Keep the current output visible even when the host omits it from the available choices. */
export function OutputChoice({ track, nodes, command, compact }: OutputChoiceProps) {
  const { id, name, outputId, pending } = track;
  const { options, label } = outputChoices(track.outputs, outputId, nodes);
  return (
    <ListPicker
      className={compact ? "mx-route-choice" : undefined}
      label={compact ? `Main output for ${name}` : "Main output"}
      options={options}
      value={outputId}
      placeholder={label}
      disabled={pending || options.length === 0}
      onCommit={(outputId) => command({ type: "setOutput", nodeId: id, outputId })}
    />
  );
}
