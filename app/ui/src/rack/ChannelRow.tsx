import { useRef, type CSSProperties, type KeyboardEvent, type PointerEvent } from "react";
import { channelPreview, errorText, type ChannelView } from "../api/project";
import { Knob } from "./Knob";
import { formatPan, formatVolume } from "./mixFormat";

interface Mix {
  volume: number;
  pan: number;
  mute: boolean;
  solo: boolean;
}

interface Props {
  channel: ChannelView;
  /** Sends the channel's whole mix with these changes. */
  onMix: (change: Partial<Mix>) => Promise<unknown>;
  /** Asks for the file a missing sample should come from. */
  onRelink: () => void;
  onError: (message: string) => void;
}

/** One channel of the rack: mute, the name (press to hear it), volume, pan and solo. The steps
 * are drawn by the step grid beside the rows. */
export function ChannelRow({ channel, onMix, onRelink, onError }: Props) {
  const previewing = useRef(false);
  const { id, name } = channel;

  // A preview plays only while the person holds the name down.
  const preview = (on: boolean) => {
    if (previewing.current === on) return;
    previewing.current = on;
    channelPreview(id, on).catch((e: unknown) => onError(errorText(e)));
  };

  const onPointerDown = (e: PointerEvent<HTMLButtonElement>) => {
    if (e.button !== 0) return;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* no pointer capture here */
    }
    preview(true);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!e.repeat) preview(true);
    }
  };
  const onKeyUp = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "Enter") preview(false);
  };

  return (
    <div
      className={`channel-row${channel.mute ? " muted" : ""}`}
      role="group"
      aria-label={name}
      style={{ "--ink": `var(--ink-${channel.color + 1})` } as CSSProperties}
    >
      <button
        type="button"
        className="mute-led"
        aria-label={`Mute ${name}`}
        aria-pressed={channel.mute}
        data-hint={`Mutes ${name}. The light is on while it plays.`}
        onClick={() => void onMix({ mute: !channel.mute })}
      />
      <button
        type="button"
        className="channel-name"
        data-hint={`Press and hold to hear ${name}. Release to stop.`}
        onPointerDown={onPointerDown}
        onPointerUp={() => preview(false)}
        onPointerCancel={() => preview(false)}
        onLostPointerCapture={() => preview(false)}
        onKeyDown={onKeyDown}
        onKeyUp={onKeyUp}
        onBlur={() => preview(false)}
      >
        <span className="channel-title">{name}</span>
        <span className="channel-sample">{channel.sampleName ?? "No sample"}</span>
      </button>
      {channel.missing && (
        <>
          <span
            className="channel-missing"
            data-hint={`The sample for ${name} was not found when the project opened. Relink it to a file to hear it again.`}
          >
            Missing
          </span>
          <button
            type="button"
            className="relink"
            aria-label={`Relink ${name}`}
            data-hint={`Choose the file ${name} should play.`}
            onClick={onRelink}
          >
            Relink…
          </button>
        </>
      )}
      <Knob
        label={`${name} volume`}
        value={channel.volume}
        min={0}
        max={2}
        step={0.05}
        defaultValue={1}
        format={formatVolume}
        hint={`The volume of ${name}. Drag up or down, or use the arrow keys; double-click for 0 dB.`}
        onCommit={(volume) => onMix({ volume })}
      />
      <Knob
        label={`${name} pan`}
        value={channel.pan}
        min={-1}
        max={1}
        step={0.05}
        defaultValue={0}
        format={formatPan}
        hint={`Where ${name} sits between left and right. Drag up or down, or use the arrow keys; double-click to centre it.`}
        onCommit={(pan) => onMix({ pan })}
      />
      <button
        type="button"
        className="solo"
        aria-label={`Solo ${name}`}
        aria-pressed={channel.solo}
        data-hint={`Solo: hear ${name} and the other soloed channels only.`}
        onClick={() => void onMix({ solo: !channel.solo })}
      >
        S
      </button>
    </div>
  );
}
