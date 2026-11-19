import { useEffect } from "react";
import type { ChannelView } from "../api/project";
import { useSelection } from "../layout/selection";
import "./devices.css";
import { SamplerEditor } from "./sampler/SamplerEditor";
import { SynthEditor } from "./synth/SynthEditor";
import { closeDevice, raiseDevice, useOpenDevices } from "./windows";

interface Props {
  channels: ChannelView[];
  generation: number;
  onError: (message: string) => void;
  /** Asks for a file for a sampler channel's sample (missing, or none yet). */
  onRelink: (channel: ChannelView) => void;
}

/** The open instrument editors, one floating window per channel, above the workspace: the synth
 * editor or the sampler editor by the channel's kind. A window whose channel is gone (removed,
 * another project) closes. Focusing or pressing a window selects its channel. */
export function DeviceWindows({ generation, channels, onError, onRelink }: Props) {
  const open = useOpenDevices();
  const { channel: selected, select } = useSelection();

  useEffect(() => {
    for (const w of open) if (!channels.some((c) => c.id === w.channel)) closeDevice(w.channel);
  }, [open, channels]);

  return (
    <div className="device-layer">
      {open.map((w, i) => {
        const channel = channels.find((c) => c.id === w.channel);
        if (!channel) return null;
        const common = {
          channel,
          index: i,
          z: w.z,
          opener: w.opener,
          onClose: () => closeDevice(w.channel),
          onRaise: () => {
            raiseDevice(w.channel);
            // The typing keyboard (and a MIDI keyboard) play the window being worked in.
            if (selected !== w.channel) select(w.channel);
          },
          onError,
        };
        return channel.kind === "synth" ? (
          <SynthEditor key={w.channel} {...common} active={selected === channel.id} generation={generation} />
        ) : (
          <SamplerEditor key={w.channel} {...common} onRelink={onRelink} />
        );
      })}
    </div>
  );
}
