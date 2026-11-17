import { useEffect, useMemo } from "react";
import { useProject } from "../project/context";
import "./devices.css";
import { SynthEditor } from "./synth/SynthEditor";
import { closeDevice, raiseDevice, useOpenDevices } from "./windows";

/** The open instrument editors, one floating window per channel, above the workspace. A window
 * whose channel is gone (removed, another project) closes. */
export function DeviceWindows({ onError }: { onError: (message: string) => void }) {
  const open = useOpenDevices();
  const { view } = useProject();
  const channels = useMemo(() => view?.channels ?? [], [view?.channels]);

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
            onRaise={() => raiseDevice(w.channel)}
            onError={onError}
          />
        );
      })}
    </div>
  );
}
