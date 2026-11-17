import { createContext, useContext } from "react";

/** The selected channel: what the typing keyboard, a MIDI keyboard and F7 play and open. */
export interface Selection {
  channel: number | null;
  /** Selects a channel (and tells the engine, for MIDI input). */
  select: (channel: number | null) => void;
}

export const SelectionContext = createContext<Selection>({ channel: null, select: () => {} });

export const useSelection = () => useContext(SelectionContext);
