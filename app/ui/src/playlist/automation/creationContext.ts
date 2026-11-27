import { createContext, useContext, useEffect } from "react";
import type { ArrangementStamp } from "../../api/arrangement";
import type { Point } from "../geometry";
import type { ControlTarget } from "./creation";

export type PlaylistSelection = {
  stamp: ArrangementStamp;
  track: number | null;
  selectCreated: (generation: number, clip: number) => void;
};

type AutomationCreationContext = {
  open: (target: ControlTarget, shown: number, anchor: HTMLElement, at: Point) => void;
  setSelection: (selection: PlaylistSelection | null) => void;
  registerDeviceGesture: (hasActiveChanges: () => boolean) => () => void;
  captureEditGuard: (stamp: ArrangementStamp) => (() => boolean) | "devicePending" | null;
};

export const AutomationCreationContext = createContext<AutomationCreationContext | null>(null);
export const useAutomationCreation = () => useContext(AutomationCreationContext);

export function useAutomationGestureGuard(hasActiveChanges: () => boolean) {
  const registerDeviceGesture = useAutomationCreation()?.registerDeviceGesture;
  useEffect(() => registerDeviceGesture?.(hasActiveChanges), [registerDeviceGesture, hasActiveChanges]);
}
