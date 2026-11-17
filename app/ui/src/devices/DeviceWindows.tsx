import { useEffect } from "react";
import type { ChannelView } from "../api/project";
import { useSelection } from "../layout/selection";
import "./devices.css";
import { SynthEditor } from "./synth/SynthEditor";
import { closeDevice, raiseDevice, useOpenDevices } from "./windows";

/** The open instrument editors, one floating window per channel, above the workspace. A window
 * whose channel is gone (removed, another project) closes. Focusing or pressing a window selects
 * its channel. */
export function DeviceWindows({ channels, onError }: { channels: ChannelView[]; onError: (message: string) => void }) {
  const open = useOpenDevices();
  const { channel: selected, select } = useSelection();

  useEffect(() => {
    for (const w of open) if (!channels.some((c) => c.id === w.channel && c.kind === "synth")) closeDevice(w.channel);
  }, [open, channels]);

  return (
    <div className="device-layer">
      {open.map((w, i) => {
        const channel = channels.find((c) => c.id === w.channel);
        if (channel?.kind !== "synth") return null;
        return (
          <SynthEditor
            key={w.channel}
            channel={channel}
            index={i}
            z={w.z}
            opener={w.opener}
            onClose={() => closeDevice(w.channel)}
            onRaise={() => {
              raiseDevice(w.channel);
              // The typing keyboard (and a MIDI keyboard) play the window being worked in.
              if (selected !== w.channel) select(w.channel);
            }}
            onError={onError}
          />
        );
      })}
    </div>
  );
}
