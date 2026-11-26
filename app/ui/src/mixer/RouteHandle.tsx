import { useRef } from "react";
import { ListPicker } from "../layout/ListPicker";
import { Icon } from "./icons";
import { availableSends, type MixerTrack } from "./mixerModel";
import type { StripActions } from "./TrackStrip";

const openingKeys = ["Enter", "ArrowDown", "ArrowUp"];

export function RouteHandle({ track, routing }: { track: MixerTrack; routing: StripActions["routing"] }) {
  const options = availableSends(track.sendDestinations, track.sends);
  const empty = options.length === 0;
  const suppressClick = useRef(false);
  return (
    <div
      className="mx-route-handle"
      onPointerDownCapture={(event) => {
        if (!(event.target instanceof HTMLButtonElement)) return;
        suppressClick.current = false;
        if (event.target.getAttribute("aria-expanded") === "true") return;
        routing.begin(track.id, event.target, event.nativeEvent, () => { suppressClick.current = true; });
      }}
      onClickCapture={(event) => {
        if (!(event.target instanceof HTMLButtonElement)) return;
        if (event.detail !== 0 && suppressClick.current) {
          event.stopPropagation();
        } else if (empty && event.target.getAttribute("aria-expanded") !== "true") {
          event.stopPropagation();
          if (!track.pending) routing.announce("No destinations available.");
        }
      }}
      onKeyDownCapture={(event) => {
        if (!(event.target instanceof HTMLButtonElement) ||
          (!empty && !routing.dragging()) || !openingKeys.includes(event.key)) return;
        event.preventDefault();
        event.stopPropagation();
        if (empty && !track.pending) routing.announce("No destinations available.");
      }}
    >
      <ListPicker
        className="mx-route-trigger"
        label={`Add send from ${track.name}`}
        options={options}
        value={null}
        placeholder={empty ? "No destinations available" : "Drag or choose a destination"}
        disabled={track.pending}
        openingKeys={openingKeys}
        onCommit={(destinationId) => routing.pick(track.id, destinationId)}
      />
      <Icon name="route" />
    </div>
  );
}
