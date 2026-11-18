import { useEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type MouseEvent, type PointerEvent } from "react";
import { channelNoteOff, channelNoteOn, TYPING_VELOCITY } from "../api/notes";
import { channelPreviewOff, channelPreviewOn, errorText, type ChannelView } from "../api/project";
import { Press } from "../keyboard/press";
import { openDevice } from "../devices/windows";
import { isTypingTarget } from "../project/shortcuts";
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
  selected: boolean;
  onSelect: () => void;
  /** Asks whether to remove the channel. */
  onRemove: () => void;
}

/** The note a synth channel's name plays while held (C4). */
const PREVIEW_KEY = 60;

/** One channel of the rack: mute, the name (press to hear it), volume, pan and solo. The steps
 * are drawn by the step grid beside the rows. Pressing a row selects it; Delete on a focused row
 * or its context menu asks to remove it. */
export function ChannelRow({ channel, onMix, onRelink, onError, selected, onSelect, onRemove }: Props) {
  /** The preview sounding now (its press), while the name is held. */
  const previewing = useRef<Press | null>(null);
  const pointer = useRef<number | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const menuItem = useRef<HTMLButtonElement>(null);
  const { id, name } = channel;
  const synth = channel.kind === "synth";

  // A preview plays only while the person holds the name down: the sample, or the synth's C4.
  const preview = (on: boolean) => {
    if (!on) pointer.current = null;
    const sounding = previewing.current;
    if ((sounding !== null) === on) return;
    if (on) {
      const sent = synth ? channelNoteOn(id, PREVIEW_KEY, TYPING_VELOCITY) : channelPreviewOn(id);
      previewing.current = new Press(sent, (e: unknown) => onError(errorText(e)));
      return;
    }
    previewing.current = null;
    // Stopping names this preview's own press; best effort: the channel may just have gone.
    void sounding?.release((press) =>
      synth ? channelNoteOff(id, PREVIEW_KEY, press) : channelPreviewOff(id, press),
    );
  };

  // A preview held while the row goes away (F6 hides the rack, the channel is removed) or the
  // window loses focus gets no keyup or pointerup: stop it then.
  const stopPreview = useRef(() => {});
  useEffect(() => {
    stopPreview.current = () => preview(false);
  });
  useEffect(() => {
    const stop = () => stopPreview.current();
    window.addEventListener("blur", stop);
    return () => {
      window.removeEventListener("blur", stop);
      stop();
    };
  }, []);

  useEffect(() => {
    if (!menu) return;
    menuItem.current?.focus();
    const close = (e: Event) => {
      if (!(e.target instanceof Node && menuItem.current?.contains(e.target))) setMenu(null);
    };
    window.addEventListener("pointerdown", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("pointerdown", close);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const onContextMenu = (e: MouseEvent<HTMLDivElement>) => {
    e.preventDefault();
    onSelect();
    setMenu({ x: e.clientX, y: e.clientY });
  };
  const onRowKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if ((e.key === "Delete" || e.key === "Backspace") && !e.repeat && !isTypingTarget(e.target)) {
      e.preventDefault();
      onRemove();
    }
  };

  const onPointerDown = (e: PointerEvent<HTMLButtonElement>) => {
    if (e.button !== 0 || !e.isPrimary || pointer.current !== null) return;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      return; // Without capture, an outside release cannot be guaranteed.
    }
    pointer.current = e.pointerId;
    preview(true);
  };
  const onPointerEnd = (e: PointerEvent<HTMLButtonElement>) => {
    if (e.pointerId === pointer.current) preview(false);
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
      className={`channel-row${channel.mute ? " muted" : ""}${selected ? " selected" : ""}`}
      role="group"
      aria-label={name}
      style={{ "--ink": `var(--ink-${channel.color + 1})` } as CSSProperties}
      onPointerDownCapture={(e) => {
        if (e.button === 0) onSelect();
      }}
      onFocus={onSelect}
      onKeyDown={onRowKeyDown}
      onContextMenu={onContextMenu}
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
        aria-current={selected ? "true" : undefined}
        data-hint={`Press and hold to hear ${name}. Release to stop. The typing keyboard plays the selected channel; Delete removes it.`}
        onPointerDown={onPointerDown}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onLostPointerCapture={onPointerEnd}
        onKeyDown={onKeyDown}
        onKeyUp={onKeyUp}
        onBlur={(e) => {
          // WebKit blurs an already focused button after pointerdown. Its captured hold
          // still owns the preview; keyboard previews continue to end on focus leaving.
          if (pointer.current !== null && e.currentTarget.hasPointerCapture(pointer.current)) return;
          preview(false);
        }}
      >
        <span className="channel-title">{name}</span>
        <span className="channel-sample">{synth ? (channel.preset ?? "Synth") : (channel.sampleName ?? "No sample")}</span>
      </button>
      <button
        type="button"
        className="instrument-button"
        aria-label={`Edit the ${name} ${synth ? "synth" : "sampler"}`}
        data-hint={
          synth
            ? `Opens the synth editor for ${name}: its sound, presets and envelopes.`
            : `Opens the sampler editor for ${name}: its sample's region and loop, pitch, envelopes and filter.`
        }
        onClick={(e) => openDevice(id, e.currentTarget)}
      >
        <svg viewBox="0 0 16 12" aria-hidden="true">
          <path d={synth ? "M1 6C3 0.5 5 0.5 7 6S11 11.5 13 6L15 3" : "M2 4.5V7.5M5 2V10M8 3.5V8.5M11 1V11M14 4.5V7.5"} />
        </svg>
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
      {menu && (
        <div className="menu row-menu" role="menu" aria-label={`${name} channel`} style={{ left: menu.x, top: menu.y }}>
          <button
            ref={menuItem}
            type="button"
            role="menuitem"
            className="menu-item"
            data-hint={`Removes ${name} with its steps and notes. It asks first.`}
            onKeyDown={(e) => {
              if (e.key === "Escape" || e.key === "Tab") {
                e.stopPropagation();
                e.preventDefault();
                setMenu(null);
              }
            }}
            onClick={() => {
              setMenu(null);
              onRemove();
            }}
          >
            Remove channel…
          </button>
        </div>
      )}
    </div>
  );
}
